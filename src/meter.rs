// SPDX-License-Identifier: LGPL-2.1-or-later
// K-weighting coefficients adapted from FFmpeg libavfilter/ebur128.c.
// Copyright (c) 2011 Jan Kokemüller; derived from libebur128 (MIT).
// AUDENIQ: fused 50ms meters, four-scalar gate history, fixed-size histogram,
// explicit null for silence, polyphase true peak, reusable decode buffers.
use crate::{
    audio::AudioReader,
    kernels::Backend,
    resample::{FingerprintTap, TruePeak, Window},
    AudioSpec, Error, Limits, Result,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
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
    state: [[f64; 4]; 2],
}
impl KWeight {
    fn new(rate: u32) -> Self {
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
            state: [[0.0; 4]; 2],
        }
    }
    #[inline]
    fn push(&mut self, ch: usize, x: f64) -> f64 {
        let v = &mut self.state[ch];
        let mut n = x;
        for (i, old) in v.iter().enumerate() {
            n -= self.a[i + 1] * old;
        }
        let mut out = self.b[0] * n;
        for (i, old) in v.iter().enumerate() {
            out += self.b[i + 1] * old;
        }
        // A long silent tail must not spend seconds executing denormal arithmetic.
        v.copy_within(0..3, 1);
        v[0] = if n.abs() < 1e-30 { 0.0 } else { n };
        out
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
    if !backend.available() {
        return Err(Error::Unsupported("CPU backend"));
    }
    let mut reader = AudioReader::open(path, limits.clone())?;
    let spec = reader.spec.clone();
    let channels = spec.channels as usize;
    let mut k = KWeight::new(spec.sample_rate);
    let mut loud = Loudness::new();
    let mut tp = TruePeak::new(spec.sample_rate, channels, backend);
    let mut tap = if want_fingerprint {
        Some(FingerprintTap::new(
            spec.sample_rate,
            spec.frames
                .ok_or(Error::Unsupported("fingerprint requires declared duration"))?,
            backend,
        ))
    } else {
        None
    };
    let mut a = Analysis {
        engine: crate::ENGINE_VERSION,
        metric_version: crate::METRIC_VERSION,
        backend,
        spec: spec.clone(),
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
    };
    let block_frames = spec.sample_rate as usize / 20;
    let mut block_count = 0;
    let mut block_peak = 0f64;
    let mut squares = 0f64;
    let mut weighted = 0f64;
    let mut silent_run = 0u64;
    let mut runs = [0u64; 2];
    let mut signs = [0i8; 2];
    let mut crossings = 0u64;
    let mut samples = Vec::new();
    let mut hash = Sha256::new();
    let mut bytes = Vec::new();
    while reader.next(&mut samples, backend)? {
        bytes.clear();
        for x in &samples {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        hash.update(&bytes);
        for row in samples.chunks_exact(channels) {
            let mut f = [0f32; 2];
            let mut mono = 0f32;
            for (ch, &s) in row.iter().enumerate() {
                let v = s as f64 / 2147483648.0;
                let x = v.abs();
                f[ch] = v as f32;
                mono += f[ch] / channels as f32;
                a.channel_peaks[ch] = a.channel_peaks[ch].max(x);
                block_peak = block_peak.max(x);
                squares += v * v;
                let sign = if v > 1e-6 {
                    1
                } else if v < -1e-6 {
                    -1
                } else {
                    0
                };
                if sign != 0 {
                    if signs[ch] != 0 && signs[ch] != sign {
                        crossings += 1;
                    }
                    signs[ch] = sign;
                }
                if x >= 0.999 {
                    runs[ch] += 1;
                    match runs[ch].cmp(&3) {
                        std::cmp::Ordering::Equal => {
                            a.clip_events += 1;
                            a.clipped_samples += 3;
                        }
                        std::cmp::Ordering::Greater => a.clipped_samples += 1,
                        std::cmp::Ordering::Less => (),
                    }
                } else {
                    runs[ch] = 0;
                }
                let y = k.push(ch, v);
                weighted += y * y;
            }
            tp.push(&f[..channels]);
            if let Some(t) = &mut tap {
                t.push(mono);
            }
            block_count += 1;
            if block_count == block_frames {
                close_block(
                    &mut a,
                    block_count,
                    block_peak,
                    squares,
                    &mut silent_run,
                    channels,
                );
                loud.block(weighted / block_count as f64);
                block_count = 0;
                block_peak = 0.0;
                squares = 0.0;
                weighted = 0.0;
            }
        }
    }
    if block_count != 0 {
        close_block(
            &mut a,
            block_count,
            block_peak,
            squares,
            &mut silent_run,
            channels,
        );
    }
    tp.finish();
    a.samples_per_channel = reader.decoded_frames();
    a.duration_secs = a.samples_per_channel as f64 / spec.sample_rate as f64;
    a.peak = a.channel_peaks.iter().copied().fold(0.0, f64::max);
    a.integrated_lufs = loud.finish();
    a.true_peak_dbtp = if tp.peak > 0.0 {
        Some(20.0 * tp.peak.log10())
    } else {
        None
    };
    a.zero_crossing_rate = crossings as f64 / (a.samples_per_channel * channels as u64) as f64;
    a.pcm_sha256 = crate::hex(&hash.finalize());
    a.fingerprint_windows = tap.map(|t| t.finish(a.samples_per_channel));
    Ok(a)
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
