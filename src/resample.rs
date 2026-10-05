//! Windowed-sinc polyphase interpolation; no naive decimation/linear aliasing.
//! New version: intentionally not bit-identical to libswresample's defaults.
use crate::kernels::{Backend, DotKernel};
use std::f64::consts::PI;

pub struct FingerprintReport {
    pub engine: &'static str,
    pub resampler_version: &'static str,
    pub spec: crate::AudioSpec,
    pub frames: u64,
    pub fingerprint_windows: Vec<Window>,
}
crate::json_struct!(FingerprintReport {
    engine,
    resampler_version,
    spec,
    frames,
    fingerprint_windows
});

/// Decode once with continuous filter state, retaining only fingerprint windows.
/// The dedicated fallback avoids calculating unused loudness/structure metrics.
pub fn fingerprint(
    path: &std::path::Path,
    limits: crate::Limits,
    backend: Backend,
) -> crate::Result<FingerprintReport> {
    if !backend.available() {
        return Err(crate::Error::Unsupported("CPU backend"));
    }
    let mut reader = crate::audio::AudioReader::open(path, limits)?;
    let spec = reader.spec.clone();
    let count = spec.frames.ok_or(crate::Error::Unsupported(
        "fingerprint requires declared sample count",
    ))?;
    let mut tap = FingerprintTap::new(spec.sample_rate, count, backend);
    let mut samples = Vec::new();
    let channels = spec.channels as usize;
    while reader.next(&mut samples, backend)? {
        let rows = samples.len() / channels;
        let mut i = 0;
        while i < rows {
            // Rows that no retained window reads are counted, not mixed.
            let skip = tap.skippable().min((rows - i) as u64) as usize;
            if skip != 0 {
                tap.skip(skip as u64);
                i += skip;
                continue;
            }
            // Pushing is always exact; check again after a stretch.
            let end = rows.min(i + 4096);
            for row in samples[i * channels..end * channels].chunks_exact(channels) {
                let mono = row
                    .iter()
                    .map(|x| (*x as f64 / 2147483648.0) as f32 / spec.channels as f32)
                    .sum();
                tap.push(mono);
            }
            i = end;
        }
    }
    let frames = reader.decoded_frames();
    Ok(FingerprintReport {
        engine: crate::ENGINE_VERSION,
        resampler_version: crate::RESAMPLER_VERSION,
        spec,
        frames,
        fingerprint_windows: tap.finish(frames),
    })
}

pub fn sinc_kernel<const N: usize>(fraction: f64, cutoff: f64) -> [f32; N] {
    let mut out = [0f32; N];
    let mut sum = 0.0;
    for (i, v) in out.iter_mut().enumerate() {
        let x = i as f64 - (N / 2 - 1) as f64 - fraction;
        let z = PI * x * cutoff;
        let sinc = if z.abs() < 1e-12 {
            cutoff
        } else {
            cutoff * z.sin() / z
        };
        let w = 0.42 - 0.5 * (2.0 * PI * i as f64 / (N - 1) as f64).cos()
            + 0.08 * (4.0 * PI * i as f64 / (N - 1) as f64).cos();
        *v = (sinc * w) as f32;
        sum += *v as f64;
    }
    for v in &mut out {
        *v = (*v as f64 / sum) as f32;
    }
    out
}

// ITU-R BS.1770-5 Annex 2, published four-phase / twelve-tap FIR.
// https://www.itu.int/dms_pubrec/itu-r/rec/bs/R-REC-BS.1770-5-202311-I!!PDF-E.pdf
#[allow(clippy::excessive_precision)]
pub(crate) const TP_COEFFICIENTS: [[f32; 4]; 12] = [
    [
        0.001708984375,
        -0.0291748046875,
        -0.0189208984375,
        -0.00830078125,
    ],
    [0.010986328125, 0.029296875, 0.0330810546875, 0.014892578125],
    [
        -0.0196533203125,
        -0.0517578125,
        -0.0582275390625,
        -0.026611328125,
    ],
    [0.033203125, 0.089111328125, 0.1015625, 0.047607421875],
    [
        -0.0594482421875,
        -0.16650390625,
        -0.2003173828125,
        -0.102294921875,
    ],
    [
        0.1373291015625,
        0.465087890625,
        0.77978515625,
        0.97216796875,
    ],
    [
        0.97216796875,
        0.77978515625,
        0.465087890625,
        0.1373291015625,
    ],
    [
        -0.102294921875,
        -0.2003173828125,
        -0.16650390625,
        -0.0594482421875,
    ],
    [0.047607421875, 0.1015625, 0.089111328125, 0.033203125],
    [
        -0.026611328125,
        -0.0582275390625,
        -0.0517578125,
        -0.0196533203125,
    ],
    [0.014892578125, 0.0330810546875, 0.029296875, 0.010986328125],
    [
        -0.00830078125,
        -0.0189208984375,
        -0.0291748046875,
        0.001708984375,
    ],
];
/// Outputs per true-peak bound check.
const PEAK_SPAN: usize = 256;
/// Largest per-phase L1 norm of `TP_COEFFICIENTS` (2.0228...), with margin
/// for the f32 rounding of a 12-term sum.
const TP_L1_BOUND: f64 = 2.0228271484375 * 1.0001;
/// BS.1770 4x true-peak interpolation over whole PCM chunks. Each channel
/// keeps its previous 11 samples ahead of the chunk, and the kernel computes
/// every output phase with the original per-output order (oldest tap first,
/// separate multiply and add), vectorized across consecutive outputs. The
/// maximum is order-independent, so results equal the former per-frame path.
pub struct TruePeak {
    history: [Vec<f32>; 2],
    kernel: crate::kernels::PeakKernel,
    interpolate: bool,
    pub peak: f64,
    channels: usize,
}
impl TruePeak {
    pub fn new(rate: u32, channels: usize, backend: Backend) -> Self {
        // Use all four phases through 96kHz as well; no interpolation at >=192k.
        let interpolate = rate < 192000;
        Self {
            history: std::array::from_fn(|_| vec![0.0; 11]),
            kernel: crate::kernels::PeakKernel::new(backend, interpolate),
            interpolate,
            peak: 0.0,
            channels,
        }
    }
    /// Push one channel's next samples; call once per channel per chunk.
    pub fn push_channel(&mut self, ch: usize, samples: &[f32]) {
        // For non-NaN floats, |x| orders like its bit pattern, so this
        // integer maximum (which vectorizes) is the largest |x|.
        let peak = f32::from_bits(
            samples
                .iter()
                .fold(0u32, |m, &x| m.max(x.to_bits() & 0x7fff_ffff)),
        );
        self.peak = self.peak.max(peak as f64);
        if !self.interpolate || samples.is_empty() {
            return;
        }
        let buffer = &mut self.history[ch];
        buffer.extend_from_slice(samples);
        // Every output is a 12-tap sum, so |output| <= max|x| * L1 (largest
        // per-phase coefficient L1 norm); the f32 sum's rounding adds under
        // 1e-6 relative. A span whose bound is below the running maximum
        // cannot raise it and is skipped; the result is unchanged.
        let outputs = buffer.len() - 11;
        let mut start = 0;
        while start < outputs {
            let end = (start + PEAK_SPAN).min(outputs);
            let window = &buffer[start..end + 11];
            let largest = window
                .iter()
                .fold(0u32, |m, &x| m.max(x.to_bits() & 0x7fff_ffff));
            if (f32::from_bits(largest) as f64) * TP_L1_BOUND >= self.peak {
                self.peak = self
                    .peak
                    .max(self.kernel.apply(window, &TP_COEFFICIENTS) as f64);
            }
            start = end;
        }
        let keep = buffer.len() - 11;
        buffer.copy_within(keep.., 0);
        buffer.truncate(11);
    }
    pub fn finish(&mut self) {
        let zeros = [0.0f32; 12];
        for ch in 0..self.channels {
            self.push_channel(ch, &zeros);
        }
    }
}

#[derive(Debug)]
pub struct Window {
    pub start_secs: f64,
    pub samples: Vec<i16>,
}
crate::json_struct!(Window {
    start_secs,
    samples
});
/// 64-tap lowpass, 1024 phases; only the <=90 seconds needed for fingerprints
/// are convolved/stored, while input state remains continuous across the file.
/// `(value * 32768.0).round().clamp(-32768.0, 32767.0) as i16` without the
/// libm call for `round` (half away from zero): below 2^23 the truncation
/// and the fraction are exact; at and above it every f32 is an integer, so
/// the fraction is 0; saturation, infinities and NaN clamp or map as there.
#[inline]
fn quantize(value: f32) -> i16 {
    let x = value * 32768.0;
    let t = x as i32;
    let fraction = x - t as f32;
    // Branch-free: the rounding direction is unpredictable.
    let r = t as i64 + (fraction >= 0.5) as i64 - (fraction <= -0.5) as i64;
    r.clamp(-32768, 32767) as i16
}

pub struct FingerprintTap {
    ring: [f32; 128],
    pos: usize,
    input: u64,
    output: u64,
    /// Input count at which `output` is emitted: its center + 33.
    due: u64,
    rate: u32,
    kernels: Vec<[f32; 64]>,
    ranges: Vec<(u64, u64)>,
    windows: Vec<Window>,
    dot: DotKernel,
}
impl FingerprintTap {
    pub fn new(rate: u32, frames: u64, backend: Backend) -> Self {
        let duration = frames as f64 / rate as f64;
        let spans = if duration <= 90.0 {
            vec![(0.0, duration)]
        } else {
            vec![
                (0.0, 30.0),
                (duration / 2.0 - 15.0, 30.0),
                (duration - 30.0, 30.0),
            ]
        };
        // Derive cardinality with integer rational arithmetic. Floating-point
        // window ends and a floor at EOF previously lost one tail sample.
        let denominator = rate as u64;
        let numerator = frames * 11025;
        let end = (numerator + denominator / 2) / denominator;
        let ranges = if frames <= 90 * denominator {
            vec![(0, end)]
        } else {
            let len = 30 * 11025;
            let middle = (numerator - len * denominator + denominator) / (2 * denominator);
            vec![(0, len), (middle, middle + len), (end - len, end)]
        };
        let windows = spans
            .iter()
            .map(|(s, n)| Window {
                start_secs: *s,
                samples: Vec::with_capacity((n * 11025.0).ceil() as usize),
            })
            .collect();
        Self {
            ring: [0.0; 128],
            pos: 0,
            input: 0,
            output: 0,
            due: 33,
            rate,
            kernels: (0..1024)
                .map(|p| sinc_kernel(p as f64 / 1024.0, 11025.0 / rate as f64 * 0.94))
                .collect(),
            ranges,
            windows,
            dot: DotKernel::new(backend),
        }
    }
    #[inline]
    pub fn push(&mut self, mono: f32) {
        self.ring[self.pos] = mono;
        self.ring[self.pos + 64] = mono;
        self.pos = (self.pos + 1) % 64;
        self.input += 1;
        // About one input in rate / 11025 completes an output.
        if self.input >= self.due {
            self.emit();
        }
    }
    fn emit(&mut self) {
        loop {
            let numerator = self.output * self.rate as u64;
            let center = numerator / 11025;
            if center + 33 > self.input {
                self.due = center + 33;
                break;
            }
            let phase = ((numerator % 11025) * 1024 / 11025) as usize;
            for (i, (start, end)) in self.ranges.iter().enumerate() {
                if self.output >= *start && self.output < *end {
                    // The ring currently ends at center+32 for this output.
                    // Rational stepping emits at most one output per input at accepted rates.
                    let value = self
                        .dot
                        .apply(&self.ring[self.pos..self.pos + 64], &self.kernels[phase]);
                    self.windows[i].samples.push(quantize(value));
                }
            }
            self.output += 1;
        }
    }
    /// How many of the next inputs no retained output reads: the ring of
    /// the first retained output at or after `output` (center c) holds
    /// inputs c - 30 ..= c + 33 (counted from 1), and every output before it
    /// is discarded.
    pub fn skippable(&self) -> u64 {
        let Some(first) = self
            .ranges
            .iter()
            .filter(|(_, end)| self.output < *end)
            .map(|(start, _)| (*start).max(self.output))
            .min()
        else {
            return u64::MAX;
        };
        let center = first * self.rate as u64 / 11025;
        // Inputs up to count center - 31 are never read; keep a margin of
        // one ring length.
        center.saturating_sub(31 + 64).saturating_sub(self.input)
    }
    /// Advance over `n` inputs that [`Self::skippable`] allows, as if each
    /// had been pushed: the discarded outputs they complete are counted.
    pub fn skip(&mut self, n: u64) {
        self.input += n;
        self.pos = ((self.pos as u64 + n) % 64) as usize;
        if self.input >= self.due {
            // The first output whose center + 33 exceeds the input count.
            let first = (self.input - 32) * 11025;
            let rate = self.rate as u64;
            self.output = self.output.max(first.div_ceil(rate));
            self.due = self.output * rate / 11025 + 33;
        }
    }
    pub fn finish(mut self, frames: u64) -> Vec<Window> {
        let end = (frames * 11025 + self.rate as u64 / 2) / self.rate as u64;
        for _ in 0..33 {
            if self.output >= end {
                break;
            }
            self.push(0.0);
        }
        self.windows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_matches_round_and_clamp() {
        let reference = |v: f32| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        let mut seed = 0x0bad_5eedu32;
        let specials = [
            0.0f32,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            1.5 / 32768.0,
            -1.5 / 32768.0,
            2.5 / 32768.0,
            -2.5 / 32768.0,
            32767.5 / 32768.0,
            -32768.5 / 32768.0,
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::MIN_POSITIVE,
            65536.0,
            -65536.0,
            8388608.5 / 32768.0,
        ];
        for &v in &specials {
            assert_eq!(quantize(v), reference(v), "{v}");
        }
        for _ in 0..2_000_000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            // Random bit patterns and values near the output range, where
            // halves are common.
            let bits = f32::from_bits(seed);
            assert_eq!(quantize(bits), reference(bits), "{bits}");
            let near = (seed as i32 as f32 / 2147483648.0) * 1.2;
            assert_eq!(quantize(near), reference(near), "{near}");
            let half = ((seed % 70000) as f32 - 35000.0 + 0.5) / 32768.0;
            assert_eq!(quantize(half), reference(half), "{half}");
        }
    }

    /// Skipping inputs that no retained window reads gives the same windows
    /// as pushing every input, for several rates, lengths around the
    /// three-window threshold and skip/push stretches of any size.
    #[test]
    fn skipped_inputs_leave_windows_unchanged() {
        let mut seed = 0x1357_9bdfu32;
        for (rate, seconds) in [
            (8000u32, 95.0),
            (11025, 90.5),
            (22050, 120.0),
            (44100, 91.0),
            (48000, 60.0),
        ] {
            let frames = (rate as f64 * seconds) as u64;
            let signal: Vec<f32> = (0..frames)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as i32 as f32 / 2147483648.0
                })
                .collect();
            let mut full = FingerprintTap::new(rate, frames, Backend::Scalar);
            for &x in &signal {
                full.push(x);
            }
            let expected = full.finish(frames);
            for stretch in [1usize, 7, 4096] {
                let mut tap = FingerprintTap::new(rate, frames, Backend::Scalar);
                let mut i = 0;
                while i < signal.len() {
                    let skip = tap.skippable().min((signal.len() - i) as u64) as usize;
                    tap.skip(skip as u64);
                    i += skip;
                    for &x in &signal[i..signal.len().min(i + stretch)] {
                        tap.push(x);
                    }
                    i = signal.len().min(i + stretch);
                }
                let actual = tap.finish(frames);
                assert_eq!(actual.len(), expected.len());
                for (a, e) in actual.iter().zip(&expected) {
                    assert_eq!(a.samples, e.samples, "rate {rate} stretch {stretch}");
                }
            }
        }
    }

    #[test]
    fn l1_bound_covers_every_phase() {
        for p in 0..4 {
            let l1: f64 = TP_COEFFICIENTS.iter().map(|c| c[p].abs() as f64).sum();
            assert!(l1 * 1.0001 <= TP_L1_BOUND, "phase {p}: {l1}");
        }
    }

    /// Span skipping never changes the true peak: compare with one
    /// unskipped pass over the whole signal, for signals whose level rises
    /// and falls, any chunking and every backend.
    #[test]
    fn skipped_spans_leave_true_peak_unchanged() {
        let mut seed = 0x2468_ace1u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as i32 as f32 / 2147483648.0
        };
        for case in 0..24 {
            let len = 3000 + case * 517;
            let signal: Vec<f32> = (0..len)
                .map(|i| {
                    let envelope = match (i / (200 + case * 37)) % 4 {
                        0 => 0.01,
                        1 => 0.9,
                        2 => 0.3,
                        _ => 0.05,
                    };
                    next() * envelope
                })
                .collect();
            let mut padded = vec![0.0f32; 11];
            padded.extend_from_slice(&signal);
            padded.extend_from_slice(&[0.0; 12]);
            let sample_peak = signal.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
            let interpolated = crate::kernels::PeakKernel::new(Backend::Scalar, true)
                .apply(&padded, &TP_COEFFICIENTS);
            let expected = (sample_peak as f64).max(interpolated as f64);
            for backend in [Backend::Scalar, Backend::detect()] {
                for chunk in [1, 7, 256, 300, 4096] {
                    let mut tp = TruePeak::new(48000, 1, backend);
                    for part in signal.chunks(chunk) {
                        tp.push_channel(0, part);
                    }
                    tp.finish();
                    assert_eq!(
                        tp.peak.to_bits(),
                        expected.to_bits(),
                        "case {case} chunk {chunk}"
                    );
                }
            }
        }
    }
}
