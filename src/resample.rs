//! Windowed-sinc polyphase interpolation; no naive decimation/linear aliasing.
//! New version: intentionally not bit-identical to libswresample's defaults.
use crate::kernels::{Backend, DotKernel};
use serde::Serialize;
use std::f64::consts::PI;

#[derive(Serialize)]
pub struct FingerprintReport {
    pub engine: &'static str,
    pub resampler_version: &'static str,
    pub spec: crate::AudioSpec,
    pub frames: u64,
    pub fingerprint_windows: Vec<Window>,
}

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
    while reader.next(&mut samples, backend)? {
        for row in samples.chunks_exact(spec.channels as usize) {
            let mono = row
                .iter()
                .map(|x| (*x as f64 / 2147483648.0) as f32 / spec.channels as f32)
                .sum();
            tap.push(mono);
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
        let peak = samples.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        self.peak = self.peak.max(peak as f64);
        if !self.interpolate || samples.is_empty() {
            return;
        }
        let buffer = &mut self.history[ch];
        buffer.extend_from_slice(samples);
        self.peak = self
            .peak
            .max(self.kernel.apply(buffer, &TP_COEFFICIENTS) as f64);
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

#[derive(Debug, Serialize)]
pub struct Window {
    pub start_secs: f64,
    pub samples: Vec<i16>,
}
/// 64-tap lowpass, 1024 phases; only the <=90 seconds needed for fingerprints
/// are convolved/stored, while input state remains continuous across the file.
pub struct FingerprintTap {
    ring: [f32; 128],
    pos: usize,
    input: u64,
    output: u64,
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
            rate,
            kernels: (0..1024)
                .map(|p| sinc_kernel(p as f64 / 1024.0, 11025.0 / rate as f64 * 0.94))
                .collect(),
            ranges,
            windows,
            dot: DotKernel::new(backend),
        }
    }
    pub fn push(&mut self, mono: f32) {
        self.ring[self.pos] = mono;
        self.ring[self.pos + 64] = mono;
        self.pos = (self.pos + 1) % 64;
        self.input += 1;
        loop {
            let numerator = self.output * self.rate as u64;
            let center = numerator / 11025;
            if center + 33 > self.input {
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
                    self.windows[i]
                        .samples
                        .push((value * 32768.0).round().clamp(-32768.0, 32767.0) as i16);
                }
            }
            self.output += 1;
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
