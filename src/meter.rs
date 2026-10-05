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
use std::{f64::consts::PI, path::Path};

#[derive(Debug)]
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
    pub fingerprint_windows: Option<Vec<Window>>,
}
crate::json_struct!(Analysis {
    engine,
    metric_version,
    backend,
    spec,
    samples_per_channel,
    duration_secs,
    peak,
    channel_peaks,
    integrated_lufs,
    true_peak_dbtp,
    clip_events,
    clipped_samples,
    blocks,
    silent_blocks,
    longest_silent_run,
    zero_crossing_rate,
    block_db,
    pcm_sha256,
    resampler_version,
    fingerprint_windows: skip_none,
});

struct KWeight {
    b: [f64; 5],
    a: [f64; 5],
    state: [[f64; 2]; 4],
    kernel: crate::kernels::WeightKernel,
    /// Stereo filtering may use `kernels::StereoWeight` (not `--scalar`).
    vector: bool,
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
            vector: backend != Backend::Scalar,
        }
    }
    #[cfg(test)]
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
/// Smallest `s` with `s as f64 / 2^31 > 1e-6` (zero-crossing sign).
const SIGN_THRESHOLD: i32 = 2148;
/// Smallest `|s|` with `|s| as f64 / 2^31 >= 0.999` (clipping).
const CLIP_THRESHOLD: u32 = 2_145_336_165;

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
    let mut hash = crate::pcm_hash::PcmHash::new(reader.spec.bits_per_sample, false);
    while reader.next_hashed(&mut samples, backend, &mut hash)? {
        analyzer.push(&samples);
    }
    Ok(analyzer.finish(reader.decoded_frames(), crate::hex(&hash.finish().0)))
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
        // `s as f32 * 2^-31` equals `(s as f64 / 2^31) as f32`: scaling by a
        // power of two commutes with rounding to f32.
        let scale = |s: i32| s as f32 * (1.0 / 2147483648.0);
        let [left, right] = &mut self.planar;
        left.clear();
        right.clear();
        if channels == 2 {
            let (frames, _) = samples.as_chunks::<2>();
            left.extend(frames.iter().map(|f| scale(f[0])));
            right.extend(frames.iter().map(|f| scale(f[1])));
        } else {
            left.extend(samples.iter().map(|&s| scale(s)));
        }
        self.tp.push_channel(0, left);
        if channels == 2 {
            self.tp.push_channel(1, right);
        }
        let mut rest = samples;
        loop {
            let frames = (block_frames - self.block_count).min(rest.len() / channels);
            if frames == 0 {
                break;
            }
            let (segment, tail) = rest.split_at(frames * channels);
            rest = tail;
            match (channels, self.tap.is_some()) {
                (2, false) => self.segment::<2, false>(segment),
                (2, true) => self.segment::<2, true>(segment),
                (_, false) => self.segment::<1, false>(segment),
                (_, true) => self.segment::<1, true>(segment),
            }
            self.block_count += frames;
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

    /// Frames within one 50 ms block. Running values stay in locals: the
    /// K-weighting, squares and weighted sums keep their per-sample f64
    /// operation order, and the peak, clip and sign tests use integer
    /// thresholds equal to the f64 comparisons on `s / 2^31` (an exact
    /// scaling), so every result is bit-identical to per-sample f64 code.
    #[inline(always)]
    fn segment<const C: usize, const TAP: bool>(&mut self, segment: &[i32]) {
        let (b, a) = (self.k.b, self.k.a);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        if C == 2 && self.k.vector {
            let mut weight = crate::kernels::StereoWeight::new(&b, &a, &self.k.state);
            self.frames::<C, TAP>(segment, |x| weight.step(x));
            self.k.state = weight.state();
            return;
        }
        let kernel = self.k.kernel;
        let mut state = self.k.state;
        self.frames::<C, TAP>(segment, |x| kernel.apply(&b, &a, &mut state, x));
        self.k.state = state;
    }

    #[inline(always)]
    fn frames<const C: usize, const TAP: bool>(
        &mut self,
        segment: &[i32],
        mut weight: impl FnMut([f64; 2]) -> [f64; 2],
    ) {
        let (frames, _) = segment.as_chunks::<C>();
        let mut squares = self.squares;
        let mut weighted = self.weighted;
        let mut peak = [0u32; C];
        let mut runs: [u64; C] = std::array::from_fn(|ch| self.runs[ch]);
        let mut signs: [i8; C] = std::array::from_fn(|ch| self.signs[ch]);
        let mut crossings = self.crossings;
        for frame in frames {
            let x = std::array::from_fn(|ch| {
                if ch < C {
                    frame[ch] as f64 / 2147483648.0
                } else {
                    0.0
                }
            });
            let y = weight(x);
            let mut mono = 0f32;
            for ch in 0..C {
                let s = frame[ch];
                let v = x[ch];
                squares += v * v;
                weighted += y[ch] * y[ch];
                let m = s.unsigned_abs();
                peak[ch] = peak[ch].max(m);
                // v > 1e-6 and v < -1e-6.
                let sign = (s >= SIGN_THRESHOLD) as i8 - (s <= -SIGN_THRESHOLD) as i8;
                crossings += (sign != 0 && signs[ch] != 0 && signs[ch] != sign) as u64;
                if sign != 0 {
                    signs[ch] = sign;
                }
                // |v| >= 0.999.
                if m >= CLIP_THRESHOLD {
                    runs[ch] += 1;
                    match runs[ch].cmp(&3) {
                        std::cmp::Ordering::Equal => {
                            self.a.clip_events += 1;
                            self.a.clipped_samples += 3;
                        }
                        std::cmp::Ordering::Greater => self.a.clipped_samples += 1,
                        std::cmp::Ordering::Less => (),
                    }
                } else {
                    runs[ch] = 0;
                }
                if TAP {
                    mono += v as f32 / C as f32;
                }
            }
            if TAP {
                if let Some(t) = &mut self.tap {
                    t.push(mono);
                }
            }
        }
        self.squares = squares;
        self.weighted = weighted;
        self.crossings = crossings;
        for ch in 0..C {
            self.runs[ch] = runs[ch];
            self.signs[ch] = signs[ch];
            let p = peak[ch] as f64 / 2147483648.0;
            self.a.channel_peaks[ch] = self.a.channel_peaks[ch].max(p);
            self.block_peak = self.block_peak.max(p);
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

    /// Both channels in one SSE2/NEON register give the scalar filter's bits,
    /// including the decay to zero after the input stops.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn vector_stereo_weighting_matches_scalar() {
        for rate in [44100, 48000, 96000, 192000] {
            let mut scalar = KWeight::new(rate, 2, Backend::Scalar);
            let mut vector = crate::kernels::StereoWeight::new(&scalar.b, &scalar.a, &scalar.state);
            let mut seed = 7u32;
            for frame in 0..60000 {
                let x = std::array::from_fn(|ch| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    if frame > 10000 {
                        0.0
                    } else if frame % 23 == ch {
                        -1.0
                    } else {
                        seed as i32 as f64 / 2147483648.0
                    }
                });
                let expected = scalar.push(x);
                let actual = vector.step(x);
                for ch in 0..2 {
                    assert_eq!(
                        actual[ch].to_bits(),
                        expected[ch].to_bits(),
                        "{rate} {frame}"
                    );
                }
            }
            for (a, b) in vector.state().iter().zip(&scalar.state) {
                assert_eq!(a.map(f64::to_bits), b.map(f64::to_bits));
            }
        }
    }

    /// The integer thresholds and the f32 conversion equal the per-sample
    /// f64 expressions they replace.
    #[test]
    fn integer_tests_equal_f64_comparisons() {
        let v = |s: i32| s as f64 / 2147483648.0;
        let mut values: Vec<i32> = vec![i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX];
        for t in [SIGN_THRESHOLD, CLIP_THRESHOLD as i32] {
            values.extend((t - 3..=t + 3).flat_map(|s| [s, -s]));
        }
        let mut seed = 0x1234_5678u32;
        values.extend((0..1_000_000).map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as i32 >> (seed % 31)
        }));
        for s in values {
            assert_eq!(s >= SIGN_THRESHOLD, v(s) > 1e-6, "{s}");
            assert_eq!(s <= -SIGN_THRESHOLD, v(s) < -1e-6, "{s}");
            assert_eq!(
                s.unsigned_abs() >= CLIP_THRESHOLD,
                v(s).abs() >= 0.999,
                "{s}"
            );
            assert_eq!(s.unsigned_abs() as f64 / 2147483648.0, v(s).abs(), "{s}");
            assert_eq!(
                (s as f32 * (1.0 / 2147483648.0)).to_bits(),
                (v(s) as f32).to_bits(),
                "{s}"
            );
        }
    }
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
