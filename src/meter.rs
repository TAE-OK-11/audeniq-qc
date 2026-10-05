// SPDX-License-Identifier: LGPL-2.1-or-later
// K-weighting coefficients adapted from FFmpeg libavfilter/ebur128.c.
// Copyright (c) 2011 Jan Kokemüller; derived from libebur128 (MIT).
// AUDENIQ: fused 50ms meters, four-scalar gate history, fixed-size histogram,
// explicit null for silence, polyphase true peak, reusable decode buffers.
use crate::sha256::Sha256;
use crate::{
    audio::AudioReader,
    kernels::Backend,
    resample::{FingerprintTap, TruePeak, Window},
    AudioSpec, Error, Limits, Result,
};
use serde::Serialize;
use std::{f64::consts::PI, path::Path};

#[derive(Debug, Serialize)]
pub struct Analysis {
    pub engine: &'static str,
    pub metric_version: &'static str,
    pub backend: Backend,
    pub spec: AudioSpec,
    pub samples_per_channel: u64,
    pub duration_secs: f64,
    pub peak: f64,
    pub channel_peaks: Vec<f64>,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub clip_events: u64,
    pub clipped_samples: u64,
    pub blocks: u64,
    pub silent_blocks: u64,
    pub longest_silent_run: u64,
    pub zero_crossing_rate: f64,
    pub block_db: Vec<f32>,
    pub pcm_sha256: String,
    pub resampler_version: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint_windows: Option<Vec<Window>>,
}

struct KWeight {
    b: [f64; 5],
    a: [f64; 5],
    state: [[f64; 2]; 4],
    kernel: crate::kernels::WeightKernel,
}
impl KWeight {
    fn new(rate: u32, channels: usize, backend: Backend) -> Self {
        let k = (PI * 1681.974450955533 / rate as f64).tan();
        let q = 0.7071752369554196;
        let vh = 10f64.powf(3.999843853973347 / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let pb = [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ];
        let pa = [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0];
        let k = (PI * 38.13547087602444 / rate as f64).tan();
        let q = 0.5003270373238773;
        let a0 = 1.0 + k / q + k * k;
        let ra = [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0];
        let rb = [1.0, -2.0, 1.0];
        let mut b = [0.0; 5];
        let mut a = [0.0; 5];
        for i in 0..3 {
            for j in 0..3 {
                b[i + j] += pb[i] * rb[j];
                a[i + j] += pa[i] * ra[j];
            }
        }
        Self {
            b,
            a,
            state: [[0.0; 2]; 4],
            kernel: crate::kernels::WeightKernel::new(backend, channels),
        }
    }
    #[inline]
    fn push(&mut self, x: [f64; 2]) -> [f64; 2] {
        self.kernel.apply(&self.b, &self.a, &mut self.state, x)
    }
}
struct Loudness {
    ring: [f64; 8],
    index: usize,
    filled: usize,
    hist: Vec<(u64, f64)>,
    absolute_count: u64,
    absolute_sum: f64,
}
impl Loudness {
    fn new() -> Self {
        Self {
            ring: [0.0; 8],
            index: 0,
            filled: 0,
            hist: vec![(0, 0.0); 8001],
            absolute_count: 0,
            absolute_sum: 0.0,
        }
    }
    fn block(&mut self, energy: f64) {
        self.ring[self.index] = energy;
        self.index = (self.index + 1) % 8;
        self.filled += 1;
        if self.filled < 8 || !self.filled.is_multiple_of(2) {
            return;
        }
        let e = self.ring.iter().sum::<f64>() / 8.0;
        let l = lufs(e);
        if l >= -70.0 {
            let bin = ((l + 70.0) * 100.0).floor().clamp(0.0, 8000.0) as usize;
            self.hist[bin].0 += 1;
            self.hist[bin].1 += e;
            self.absolute_count += 1;
            self.absolute_sum += e;
        }
    }
    fn finish(&self) -> Option<f64> {
        if self.absolute_count == 0 {
            return None;
        }
        let relative = lufs(self.absolute_sum / self.absolute_count as f64) - 10.0;
        let floor = ((relative.max(-70.0) + 70.0) * 100.0)
            .ceil()
            .clamp(0.0, 8000.0) as usize;
        let mut count = 0;
        let mut sum = 0.0;
        for (n, e) in &self.hist[floor..] {
            count += n;
            sum += e;
        }
        if count == 0 {
            None
        } else {
            Some(lufs(sum / count as f64))
        }
    }
}
fn lufs(e: f64) -> f64 {
    -0.691 + 10.0 * e.log10()
}

pub fn analyze(
    path: &Path,
    limits: Limits,
    backend: Backend,
    want_fingerprint: bool,
) -> Result<Analysis> {
    let mut reader = AudioReader::open(path, limits)?;
    let mut analyzer = Analyzer::new(reader.spec.clone(), backend, want_fingerprint)?;
    let mut samples = Vec::new();
    let mut hash = Sha256::new();
    while reader.next(&mut samples, backend)? {
        hash.update(crate::audio::pcm_bytes(&samples));
        analyzer.push(&samples);
    }
    Ok(analyzer.finish(reader.decoded_frames(), crate::hex(&hash.finalize())))
}

/// Shared streaming meters for standalone analysis and verified FLAC conversion.
/// Hashing remains with the caller so conversion + QC never hashes PCM twice.
pub(crate) struct Analyzer {
    k: KWeight,
    loud: Loudness,
    tp: TruePeak,
    planar: [Vec<f32>; 2],
    tap: Option<FingerprintTap>,
    a: Analysis,
    block_count: usize,
    block_peak: f64,
    squares: f64,
    weighted: f64,
    silent_run: u64,
    runs: [u64; 2],
    signs: [i8; 2],
    crossings: u64,
}
impl Analyzer {
    pub(crate) fn new(spec: AudioSpec, backend: Backend, want_fingerprint: bool) -> Result<Self> {
        if !backend.available() {
            return Err(Error::Unsupported("CPU backend"));
        }
        let channels = spec.channels as usize;
        let tap = if want_fingerprint {
            Some(FingerprintTap::new(
                spec.sample_rate,
                spec.frames
                    .ok_or(Error::Unsupported("fingerprint requires declared duration"))?,
                backend,
            ))
        } else {
            None
        };
        Ok(Self {
            k: KWeight::new(spec.sample_rate, channels, backend),
            loud: Loudness::new(),
            tp: TruePeak::new(spec.sample_rate, channels, backend),
            planar: std::array::from_fn(|_| Vec::new()),
            tap,
            a: Analysis {
                engine: crate::ENGINE_VERSION,
                metric_version: crate::METRIC_VERSION,
                backend,
                spec,
                samples_per_channel: 0,
                duration_secs: 0.0,
                peak: 0.0,
                channel_peaks: vec![0.0; channels],
                integrated_lufs: None,
                true_peak_dbtp: None,
                clip_events: 0,
                clipped_samples: 0,
                blocks: 0,
                silent_blocks: 0,
                longest_silent_run: 0,
                zero_crossing_rate: 0.0,
                block_db: Vec::with_capacity(36000),
                pcm_sha256: String::new(),
                resampler_version: want_fingerprint.then_some(crate::RESAMPLER_VERSION),
                fingerprint_windows: None,
            },
            block_count: 0,
            block_peak: 0.0,
            squares: 0.0,
            weighted: 0.0,
            silent_run: 0,
            runs: [0; 2],
            signs: [0; 2],
            crossings: 0,
        })
    }
    pub(crate) fn push(&mut self, samples: &[i32]) {
        let channels = self.a.spec.channels as usize;
        let block_frames = self.a.spec.sample_rate as usize / 20;
        for (ch, plane) in self.planar.iter_mut().enumerate().take(channels) {
            plane.clear();
            plane.extend(
                samples
                    .iter()
                    .skip(ch)
                    .step_by(channels)
                    .map(|&s| (s as f64 / 2147483648.0) as f32),
            );
            self.tp.push_channel(ch, plane);
        }
        for row in samples.chunks_exact(channels) {
            let weighted = self.k.push([
                row[0] as f64 / 2147483648.0,
                if channels == 2 {
                    row[1] as f64 / 2147483648.0
                } else {
                    0.0
                },
            ]);
            let mut f = [0f32; 2];
            for (ch, &s) in row.iter().enumerate() {
                let v = s as f64 / 2147483648.0;
                let x = v.abs();
                f[ch] = v as f32;
                self.a.channel_peaks[ch] = self.a.channel_peaks[ch].max(x);
                self.block_peak = self.block_peak.max(x);
                self.squares += v * v;
                let sign = if v > 1e-6 {
                    1
                } else if v < -1e-6 {
                    -1
                } else {
                    0
                };
                if sign != 0 {
                    if self.signs[ch] != 0 && self.signs[ch] != sign {
                        self.crossings += 1;
                    }
                    self.signs[ch] = sign;
                }
                if x >= 0.999 {
                    self.runs[ch] += 1;
                    match self.runs[ch].cmp(&3) {
                        std::cmp::Ordering::Equal => {
                            self.a.clip_events += 1;
                            self.a.clipped_samples += 3;
                        }
                        std::cmp::Ordering::Greater => self.a.clipped_samples += 1,
                        std::cmp::Ordering::Less => (),
                    }
                } else {
                    self.runs[ch] = 0;
                }
                let y = weighted[ch];
                self.weighted += y * y;
            }
            if let Some(t) = &mut self.tap {
                let mono = f[..channels]
                    .iter()
                    .fold(0.0, |sum, &v| sum + v / channels as f32);
                t.push(mono);
            }
            self.block_count += 1;
            if self.block_count == block_frames {
                close_block(
                    &mut self.a,
                    self.block_count,
                    self.block_peak,
                    self.squares,
                    &mut self.silent_run,
                    channels,
                );
                self.loud.block(self.weighted / self.block_count as f64);
                self.block_count = 0;
                self.block_peak = 0.0;
                self.squares = 0.0;
                self.weighted = 0.0;
            }
        }
    }
    pub(crate) fn finish(mut self, frames: u64, pcm_sha256: String) -> Analysis {
        let channels = self.a.spec.channels as usize;
        if self.block_count != 0 {
            close_block(
                &mut self.a,
                self.block_count,
                self.block_peak,
                self.squares,
                &mut self.silent_run,
                channels,
            );
        }
        self.tp.finish();
        self.a.samples_per_channel = frames;
        self.a.duration_secs = self.a.samples_per_channel as f64 / self.a.spec.sample_rate as f64;
        self.a.peak = self.a.channel_peaks.iter().copied().fold(0.0, f64::max);
        self.a.integrated_lufs = self.loud.finish();
        self.a.true_peak_dbtp = if self.tp.peak > 0.0 {
            Some(20.0 * self.tp.peak.log10())
        } else {
            None
        };
        self.a.zero_crossing_rate =
            self.crossings as f64 / (self.a.samples_per_channel * channels as u64) as f64;
        self.a.pcm_sha256 = pcm_sha256;
        self.a.fingerprint_windows = self.tap.map(|t| t.finish(self.a.samples_per_channel));
        self.a
    }
}
fn close_block(
    a: &mut Analysis,
    n: usize,
    peak: f64,
    squares: f64,
    silent_run: &mut u64,
    channels: usize,
) {
    a.blocks += 1;
    if peak < 0.001 {
        a.silent_blocks += 1;
        *silent_run += 1;
        a.longest_silent_run = a.longest_silent_run.max(*silent_run);
    } else {
        *silent_run = 0;
    }
    if a.block_db.len() < 36000 {
        a.block_db
            .push((20.0 * (squares / (n * channels) as f64).sqrt().max(1e-9).log10()) as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stereo_weighting_matches_independent_scalar_history() {
        for rate in [44100, 48000, 96000, 192000] {
            for backend in [Backend::Scalar, Backend::detect()] {
                let mut kernel = KWeight::new(rate, 2, backend);
                let mut history = [[0.0; 4]; 2];
                let mut seed = 1729u32;
                for frame in 0..60000 {
                    let input = std::array::from_fn::<_, 2, _>(|ch| {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        if frame > 10000 {
                            0.0
                        } else if frame % 17 == 0 {
                            if ch == 0 {
                                -1.0
                            } else {
                                0.99999
                            }
                        } else {
                            seed as i32 as f64 / 2147483648.0
                        }
                    });
                    let expected = std::array::from_fn::<_, 2, _>(|ch| {
                        let h = &mut history[ch];
                        let mut n = input[ch];
                        for (i, &v) in h.iter().enumerate() {
                            n -= kernel.a[i + 1] * v;
                        }
                        let mut y = kernel.b[0] * n;
                        for (i, &v) in h.iter().enumerate() {
                            y += kernel.b[i + 1] * v;
                        }
                        h.copy_within(0..3, 1);
                        h[0] = if n.abs() < 1e-30 { 0.0 } else { n };
                        y
                    });
                    let actual = kernel.push(input);
                    for ch in 0..2 {
                        assert_eq!(
                            actual[ch].to_bits(),
                            expected[ch].to_bits(),
                            "rate {rate} backend {backend:?} frame {frame} channel {ch}"
                        );
                    }
                }
            }
        }
    }
}
