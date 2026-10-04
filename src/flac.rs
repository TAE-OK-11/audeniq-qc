// SPDX-License-Identifier: LGPL-2.1-or-later
// FLAC fixed predictors, signed Rice mapping and frame writer adapted from
// FFmpeg libavcodec/flacenc.c / lpc.c, Copyright (c) 2006 Justin Ruggles.
// Redesign: bounded 4096-frame work buffers; constant/fixed/verbatim choice;
// exact Rice costs near an estimated parameter; adaptive independent/mid-side;
// streaming MD5/PCM SHA256, verified output and atomic no-clobber publication.
use crate::{
    audio::{pcm_sha256, AudioReader},
    bits::{crc16, crc8, BeWriter},
    kernels::{Backend, Dot64Kernel, LpcKernel, RiceKernel},
    AudioSpec, Error, Limits, Result,
};
use md5::{Digest as _, Md5};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy)]
struct Profile {
    level: u8,
    block: usize,
    fixed: usize,
    lpc: usize,
}
impl Profile {
    fn new(level: u8) -> Result<Self> {
        let (block, fixed, lpc) = match level {
            0 => (1024, 0, 0),
            1 => (2048, 1, 0),
            2 => (4096, 2, 0),
            3 => (4096, 3, 0),
            4 => (4096, 4, 4),
            5 => (4096, 4, 8),
            6 => (8192, 4, 8),
            7 => (16384, 4, 8),
            8 => (32768, 4, 8),
            _ => return Err(Error::Invalid("compression level range 0..8")),
        };
        Ok(Self {
            level,
            block,
            fixed,
            lpc,
        })
    }
}
#[derive(Serialize)]
pub struct Conversion {
    pub spec: AudioSpec,
    pub frames: u64,
    pub pcm_sha256: String,
    pub output_bytes: u64,
    pub encoder: &'static str,
    pub compression_level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<crate::meter::Analysis>,
}

#[derive(Clone, Copy, Default)]
pub struct ConvertOptions {
    pub compression_level: Option<u8>,
    pub analyze: bool,
    pub fingerprint: bool,
}
// One normalizer per command; stack storage avoids an unnecessary heap box.
#[allow(clippy::large_enum_variant)]
enum Normalizer {
    Copy(File),
    Encode(Encoder),
}
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn convert(src: &Path, dst: &Path, limits: Limits, backend: Backend) -> Result<Conversion> {
    convert_with_level(src, dst, limits, backend, None)
}

/// Explicit levels always re-encode, including FLAC inputs. With no explicit
/// level, use profile 5 for encoding and preserve verified FLAC audio frames.
pub fn convert_with_level(
    src: &Path,
    dst: &Path,
    limits: Limits,
    backend: Backend,
    level: Option<u8>,
) -> Result<Conversion> {
    convert_with_options(
        src,
        dst,
        limits,
        backend,
        ConvertOptions {
            compression_level: level,
            ..Default::default()
        },
    )
}

/// Compute QC while encoding the same source PCM, retaining independent output
/// re-decoding/hash verification before publishing either file or report.
pub fn convert_with_options(
    src: &Path,
    dst: &Path,
    limits: Limits,
    backend: Backend,
    options: ConvertOptions,
) -> Result<Conversion> {
    if options.fingerprint && !options.analyze {
        return Err(Error::Invalid("conversion fingerprint requires analysis"));
    }
    let profile = Profile::new(options.compression_level.unwrap_or(5))?;
    if !backend.available() {
        return Err(Error::Unsupported("CPU backend"));
    }
    let mut reader = AudioReader::open(src, limits.clone())?;
    let spec = reader.spec.clone();
    let mut analyzer = if options.analyze {
        Some(crate::meter::Analyzer::new(
            spec.clone(),
            backend,
            options.fingerprint,
        )?)
    } else {
        None
    };
    if dst.exists() {
        return Err(Error::Invalid("output already exists"));
    }
    let name = dst
        .file_name()
        .ok_or(Error::Invalid("output path"))?
        .to_string_lossy();
    let temp = Temp(dst.with_file_name(format!(
        ".{name}.{}.{}.partial",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )));
    let mut file_options = OpenOptions::new();
    file_options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        file_options.mode(0o600);
    }
    let mut f = file_options.open(&temp.0)?;
    // Already-valid FLAC needs metadata removal, not another prediction pass.
    // Preserve only STREAMINFO and CRC-checked audio packets, then independently
    // decode/hash the resulting file just like newly encoded inputs.
    let copied = if options.compression_level.is_none() {
        reader.prepare_flac_copy()
    } else {
        None
    };
    let mut writer = if let Some(info) = copied {
        f.write_all(b"fLaC\x80\x00\x00\x22")?;
        f.write_all(&info)?;
        Normalizer::Copy(f)
    } else {
        Normalizer::Encode(Encoder::new(
            f,
            spec.clone(),
            limits.clone(),
            backend,
            profile,
        )?)
    };
    let mut samples = Vec::new();
    let mut hash = Sha256::new();
    while reader.next(&mut samples, backend)? {
        {
            let _profile = crate::profile::scope(crate::profile::Stage::SourceHash);
            hash.update(crate::audio::pcm_bytes(&samples));
        }
        if let Some(analyzer) = &mut analyzer {
            let _profile = crate::profile::scope(crate::profile::Stage::Qc);
            analyzer.push(&samples);
        }
        match &mut writer {
            Normalizer::Encode(encoder) => encoder.push(&samples)?,
            Normalizer::Copy(file) => file.write_all(
                reader
                    .flac_frame()
                    .ok_or(Error::Invalid("missing FLAC frame"))?,
            )?,
        }
    }
    let frames = reader.decoded_frames();
    let source_hash = crate::hex(&hash.finalize());
    let f = match writer {
        Normalizer::Encode(encoder) => encoder.finish()?,
        Normalizer::Copy(file) => file,
    };
    f.sync_all()?;
    let output_bytes = f.metadata()?.len();
    if output_bytes > limits.max_file_bytes {
        return Err(Error::Limit("FLAC output bytes"));
    }
    drop(f);
    let (out, out_hash, out_frames) = {
        let _profile = crate::profile::scope(crate::profile::Stage::OutputVerify);
        pcm_sha256(&temp.0, limits.clone(), backend)?
    };
    if out_hash != source_hash
        || out_frames != frames
        || out.sample_rate != spec.sample_rate
        || out.channels != spec.channels
        || out.bits_per_sample != spec.bits_per_sample
    {
        return Err(Error::Invalid("lossless round-trip verification"));
    }
    let analysis = analyzer.map(|a| a.finish(frames, source_hash.clone()));
    // QC finalization/resampler flushing also belongs to the command deadline.
    limits.check()?;
    // Same-filesystem hard link is atomic and refuses to replace an existing name.
    std::fs::hard_link(&temp.0, dst)?;
    Ok(Conversion {
        spec: out,
        frames,
        pcm_sha256: source_hash,
        output_bytes,
        encoder: if copied.is_some() {
            "verified-flac-frame-copy-v1"
        } else {
            "adaptive-lpc8-rice-v1"
        },
        compression_level: if copied.is_some() {
            None
        } else {
            Some(profile.level)
        },
        analysis,
    })
}

struct Encoder {
    file: File,
    spec: AudioSpec,
    pending: Vec<i32>,
    frames: u64,
    number: u64,
    md5: Md5,
    limits: Limits,
    bytes: u64,
    min_frame: usize,
    max_frame: usize,
    dot: Dot64Kernel,
    lpc: LpcKernel,
    rice: RiceKernel,
    profile: Profile,
    raw: Vec<u8>,
    frame_buffer: Vec<u8>,
    channel_buffers: [Vec<i32>; 4],
    planner: Planner,
}
impl Encoder {
    fn new(
        mut file: File,
        spec: AudioSpec,
        limits: Limits,
        backend: Backend,
        profile: Profile,
    ) -> Result<Self> {
        file.write_all(b"fLaC\x80\x00\x00\x22")?;
        file.write_all(&[0u8; 34])?;
        let capacity = profile.block * spec.channels as usize;
        let raw_capacity = capacity * (spec.bits_per_sample / 8) as usize;
        Ok(Self {
            file,
            spec,
            pending: Vec::new(),
            frames: 0,
            number: 0,
            md5: Md5::new(),
            limits,
            bytes: 42,
            min_frame: usize::MAX,
            max_frame: 0,
            dot: Dot64Kernel::new(backend),
            lpc: LpcKernel::new(backend),
            rice: RiceKernel::new(backend),
            profile,
            raw: Vec::with_capacity(raw_capacity),
            frame_buffer: Vec::with_capacity(raw_capacity + 128),
            channel_buffers: std::array::from_fn(|_| Vec::new()),
            planner: Planner::default(),
        })
    }
    fn push(&mut self, samples: &[i32]) -> Result<()> {
        let _profile = crate::profile::scope(crate::profile::Stage::Encoder);
        crate::audio::compact_pcm(samples, self.spec.bits_per_sample, &mut self.raw);
        self.md5.update(&self.raw);
        let block = self.profile.block * self.spec.channels as usize;
        let mut pos = 0;
        while pos < samples.len() {
            if self.pending.is_empty() && samples.len() - pos >= block {
                // ALAC packets and PCM reader batches commonly contain complete
                // encoder blocks. Borrow them directly instead of copying PCM.
                self.write_block(&samples[pos..pos + block])?;
                pos += block;
                continue;
            }
            let n = (block - self.pending.len()).min(samples.len() - pos);
            self.pending.extend_from_slice(&samples[pos..pos + n]);
            pos += n;
            if self.pending.len() == block {
                let mut pending = std::mem::take(&mut self.pending);
                self.write_block(&pending)?;
                pending.clear();
                self.pending = pending;
            }
        }
        Ok(())
    }
    fn write_block(&mut self, samples: &[i32]) -> Result<()> {
        self.limits.check()?;
        let channels = self.spec.channels as usize;
        let n = samples.len() / channels;
        let depth = self.spec.bits_per_sample as u32;
        let shift = 32 - depth;
        let [left, right, mid, side] = &mut self.channel_buffers;
        left.clear();
        right.clear();
        mid.clear();
        side.clear();
        for row in samples.chunks_exact(channels) {
            left.push(row[0] >> shift);
            if channels == 2 {
                right.push(row[1] >> shift);
            }
        }
        let l = Plan::new(
            left,
            depth,
            &self.dot,
            &self.lpc,
            &self.rice,
            self.profile,
            &mut self.planner,
        );
        let r = if channels == 2 {
            Some(Plan::new(
                right,
                depth,
                &self.dot,
                &self.lpc,
                &self.rice,
                self.profile,
                &mut self.planner,
            ))
        } else {
            None
        };
        let mut ms = None;
        // Uncorrelated stereo usually gains nothing from mid-side. A cheap
        // covariance check avoids planning two extra subframes in that case.
        let correlated = if channels == 2 && self.profile.level >= 3 {
            let (mut ll, mut rr, mut lr) = (0.0f64, 0.0f64, 0.0f64);
            for (&a, &b) in left.iter().zip(right.iter()) {
                ll += (a as f64) * (a as f64);
                rr += (b as f64) * (b as f64);
                lr += (a as f64) * (b as f64);
            }
            lr * lr > 0.015625 * ll * rr
        } else {
            false
        };
        if correlated {
            for (&a, &b) in left.iter().zip(right.iter()) {
                mid.push((a + b) >> 1);
                side.push(a - b);
            }
            let m = Plan::new(
                mid,
                depth,
                &self.dot,
                &self.lpc,
                &self.rice,
                self.profile,
                &mut self.planner,
            );
            let s = Plan::new(
                side,
                depth + 1,
                &self.dot,
                &self.lpc,
                &self.rice,
                self.profile,
                &mut self.planner,
            );
            if m.cost + s.cost < l.cost + r.as_ref().unwrap().cost {
                ms = Some((m, s));
            } else {
                m.recycle(&mut self.planner);
                s.recycle(&mut self.planner);
            }
        }
        let mut bw = BeWriter::reuse(std::mem::take(&mut self.frame_buffer));
        bw.put(16, 0xfff8);
        bw.put(4, 7);
        bw.put(4, 0);
        bw.put(
            4,
            if ms.is_some() {
                10
            } else {
                (channels - 1) as u64
            },
        );
        bw.put(3, if depth == 16 { 4 } else { 6 });
        bw.put(1, 0);
        utf8(&mut bw, self.number);
        bw.put(16, (n - 1) as u64);
        let crc = crc8(&bw.bytes);
        bw.put(8, crc as u64);
        if let Some((m, s)) = ms {
            l.recycle(&mut self.planner);
            if let Some(r) = r {
                r.recycle(&mut self.planner);
            }
            m.write(&mut bw, mid, depth, &mut self.planner);
            s.write(&mut bw, side, depth + 1, &mut self.planner);
        } else {
            l.write(&mut bw, left, depth, &mut self.planner);
            if let Some(r) = r {
                r.write(&mut bw, right, depth, &mut self.planner);
            }
        }
        bw.align();
        let crc = crc16(&bw.bytes);
        bw.put(16, crc as u64);
        self.bytes += bw.bytes.len() as u64;
        if self.bytes > self.limits.max_file_bytes {
            return Err(Error::Limit("FLAC output bytes"));
        }
        self.min_frame = self.min_frame.min(bw.bytes.len());
        self.max_frame = self.max_frame.max(bw.bytes.len());
        self.file.write_all(&bw.bytes)?;
        self.frame_buffer = bw.bytes;
        self.frames += n as u64;
        self.number += 1;
        Ok(())
    }
    fn finish(mut self) -> Result<File> {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.write_block(&pending)?;
        }
        if self.frames == 0 || self.frames > self.limits.max_frames {
            return Err(Error::Invalid("empty/oversized FLAC output"));
        }
        let md5 = self.md5.finalize();
        let mut b = BeWriter::new();
        b.put(16, self.profile.block as u64);
        b.put(16, self.profile.block as u64);
        b.put(24, self.min_frame as u64);
        b.put(24, self.max_frame as u64);
        b.put(20, self.spec.sample_rate as u64);
        b.put(3, (self.spec.channels - 1) as u64);
        b.put(5, (self.spec.bits_per_sample - 1) as u64);
        b.put(36, self.frames);
        b.bytes.extend_from_slice(&md5);
        self.file.seek(SeekFrom::Start(8))?;
        self.file.write_all(&b.bytes)?;
        Ok(self.file)
    }
}

enum Mode {
    Constant,
    Verbatim,
    Fixed {
        order: usize,
        k: u32,
        residual: Vec<u32>,
    },
    Lpc {
        coefficients: Vec<i32>,
        shift: u32,
        k: u32,
        residual: Vec<u32>,
    },
}
struct Plan {
    cost: u64,
    mode: Mode,
}
struct LpcCandidate {
    coefficients: Vec<i32>,
    shift: u32,
    estimate: u64,
}
#[derive(Default)]
struct Planner {
    diff: Vec<i64>,
    window: Vec<f64>,
    windowed: Vec<f64>,
    residuals: Vec<Vec<u32>>,
}
impl Planner {
    fn residual(&mut self) -> Vec<u32> {
        if let Some(buffer) = self.residuals.pop() {
            crate::profile::count(crate::profile::Counter::EncoderResidualReused, 1);
            buffer
        } else {
            crate::profile::count(crate::profile::Counter::EncoderResidualFresh, 1);
            Vec::new()
        }
    }
    fn recycle(&mut self, residual: Vec<u32>) {
        // At most four held stereo/mid-side plans plus one planning scratch.
        debug_assert!(self.residuals.len() < 5);
        self.residuals.push(residual);
    }
}
// Uniformly spaced integer residuals rank prediction models without repeatedly
// computing whole blocks. This changes compression choices only; reconstruction
// still uses checked exact residuals for every sample and verifies output PCM.
fn sampled_lpc_cost(samples: &[i32], coefficients: &[i32], shift: u32, depth: u32) -> u64 {
    let order = coefficients.len();
    let count = 128.min(samples.len() - order);
    let mut sampled = [0u32; 128];
    let residual = &mut sampled[..count];
    for (point, value) in residual.iter_mut().enumerate() {
        let i = order + point * (samples.len() - order - 1) / (count - 1).max(1);
        let prediction: i64 = coefficients
            .iter()
            .enumerate()
            .map(|(j, &c)| c as i64 * samples[i - j - 1] as i64)
            .sum();
        let delta = samples[i] as i64 - (prediction >> shift);
        if i32::try_from(delta).is_err() {
            return u64::MAX;
        }
        *value = ((delta << 1) ^ (delta >> 63)) as u32;
    }
    let mean = residual.iter().map(|&r| r as u64).sum::<u64>() / count as u64;
    let estimate = if mean == 0 {
        0
    } else {
        63 - mean.leading_zeros()
    };
    let overhead = 8 + order as u64 * depth as u64 + 4 + 5 + order as u64 * 12 + 11;
    (estimate.saturating_sub(1)..=(estimate + 1).min(30))
        .map(|k| {
            let bits = residual
                .iter()
                .map(|&r| (r as u64 >> k) + 1 + k as u64)
                .sum::<u64>();
            overhead + bits * (samples.len() - order) as u64 / count as u64
        })
        .min()
        .unwrap_or(u64::MAX)
}
impl Plan {
    fn new(
        samples: &[i32],
        depth: u32,
        dot: &Dot64Kernel,
        lpc: &LpcKernel,
        rice: &RiceKernel,
        profile: Profile,
        planner: &mut Planner,
    ) -> Self {
        let _profile = crate::profile::scope(crate::profile::Stage::EncoderPlan);
        if samples.iter().all(|x| *x == samples[0]) {
            return Self {
                cost: 8 + depth as u64,
                mode: Mode::Constant,
            };
        }
        let mut best = Self {
            cost: 8 + samples.len() as u64 * depth as u64,
            mode: Mode::Verbatim,
        };
        planner.diff.clear();
        planner.diff.extend(samples.iter().map(|x| *x as i64));
        for order in 0..=profile.fixed.min(samples.len() - 1) {
            if order > 0 {
                for i in (order..samples.len()).rev() {
                    planner.diff[i] -= planner.diff[i - 1];
                }
            }
            let mut residual = planner.residual();
            residual.clear();
            residual.extend(
                planner.diff[order..]
                    .iter()
                    .map(|x| ((*x << 1) ^ (*x >> 63)) as u32),
            );
            let cheapest = rice.choose(&residual, 8 + order as u64 * depth as u64 + 11);
            if cheapest.0 < best.cost {
                best.recycle(planner);
                best = Self {
                    cost: cheapest.0,
                    mode: Mode::Fixed {
                        order,
                        k: cheapest.1,
                        residual,
                    },
                };
            } else {
                planner.recycle(residual);
            }
        }
        // Welch-tapered autocorrelation and Levinson-Durbin, limited to order
        // eight. Integer residual costs decide; no lossy reconstruction.
        if samples.len() > 16 && profile.lpc > 0 {
            let n = samples.len();
            if planner.window.len() != n {
                planner.window = (0..n)
                    .map(|i| {
                        let d = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
                        1.0 - d * d
                    })
                    .collect();
            }
            let windowed = &mut planner.windowed;
            windowed.resize(n, 0.0);
            for ((v, &x), &w) in windowed.iter_mut().zip(samples).zip(&planner.window) {
                *v = x as f64 * w;
            }
            let mut r = [0.0f64; 9];
            for (lag, energy) in r[..=profile.lpc].iter_mut().enumerate() {
                *energy = dot.apply(&windowed[lag..], &windowed[..n - lag]);
            }
            let mut a = [0.0f64; 8];
            let mut error = r[0];
            let mut candidates = Vec::with_capacity(3);
            for index in 0..profile.lpc {
                if error <= r[0] * 1e-12 || !error.is_finite() {
                    break;
                }
                let reflection =
                    (r[index + 1] - (0..index).map(|j| a[j] * r[index - j]).sum::<f64>()) / error;
                if !reflection.is_finite() || reflection.abs() >= 1.0 {
                    break;
                }
                let old = a;
                for j in 0..index {
                    a[j] = old[j] - reflection * old[index - j - 1];
                }
                a[index] = reflection;
                error *= 1.0 - reflection * reflection;
                let order = index + 1;
                if !matches!(order, 2 | 4 | 8) {
                    continue;
                }
                let largest = a[..order].iter().fold(0.0f64, |v, x| v.max(x.abs()));
                if largest == 0.0 || largest > 2047.0 {
                    continue;
                }
                let shift = (2047.0 / largest).log2().floor().clamp(0.0, 15.0) as u32;
                let coefficients: Vec<i32> = a[..order]
                    .iter()
                    .map(|x| (x * (1u32 << shift) as f64).round() as i32)
                    .collect();
                let estimate = if profile.level <= 5 {
                    sampled_lpc_cost(samples, &coefficients, shift, depth)
                } else {
                    0
                };
                candidates.push(LpcCandidate {
                    coefficients,
                    shift,
                    estimate,
                });
            }
            if profile.level <= 5 {
                candidates.sort_by_key(|c| c.estimate);
                candidates.truncate(1);
                if candidates.first().is_some_and(|c| c.estimate >= best.cost) {
                    candidates.clear();
                }
            }
            for candidate in candidates {
                let LpcCandidate {
                    coefficients,
                    shift,
                    ..
                } = candidate;
                let order = coefficients.len();
                let mut residual = planner.residual();
                if lpc
                    .residual_into(samples, &coefficients, shift, &mut residual)
                    .is_none()
                {
                    planner.recycle(residual);
                    continue;
                }
                let overhead = 8 + order as u64 * depth as u64 + 4 + 5 + order as u64 * 12 + 11;
                let cheapest = rice.choose(&residual, overhead);
                if cheapest.0 < best.cost {
                    best.recycle(planner);
                    best = Self {
                        cost: cheapest.0,
                        mode: Mode::Lpc {
                            coefficients,
                            shift,
                            k: cheapest.1,
                            residual,
                        },
                    };
                } else {
                    planner.recycle(residual);
                }
            }
        }
        best
    }
    fn recycle(self, planner: &mut Planner) {
        match self.mode {
            Mode::Fixed { residual, .. } | Mode::Lpc { residual, .. } => planner.recycle(residual),
            _ => (),
        }
    }
    fn write(self, bw: &mut BeWriter, samples: &[i32], depth: u32, planner: &mut Planner) {
        match self.mode {
            Mode::Constant => {
                bw.put(8, 0);
                bw.put(depth, samples[0] as u64);
            }
            Mode::Verbatim => {
                bw.put(8, 2);
                for x in samples {
                    bw.put(depth, *x as u64);
                }
            }
            Mode::Fixed { order, k, residual } => {
                bw.put(8, ((8 + order) * 2) as u64);
                for x in &samples[..order] {
                    bw.put(depth, *x as u64);
                }
                bw.put(2, 1);
                bw.put(4, 0);
                bw.put(5, k as u64);
                bw.rice_block(&residual, k);
                planner.recycle(residual);
            }
            Mode::Lpc {
                coefficients,
                shift,
                k,
                residual,
            } => {
                let order = coefficients.len();
                bw.put(8, ((32 + order - 1) * 2) as u64);
                for x in &samples[..order] {
                    bw.put(depth, *x as u64);
                }
                bw.put(4, 11); // 12-bit coefficients.
                bw.put(5, shift as u64);
                for c in coefficients {
                    bw.put(12, c as u64);
                }
                bw.put(2, 1);
                bw.put(4, 0);
                bw.put(5, k as u64);
                bw.rice_block(&residual, k);
                planner.recycle(residual);
            }
        }
    }
}
fn utf8(bw: &mut BeWriter, n: u64) {
    if n < 128 {
        bw.put(8, n);
        return;
    }
    let bits = 64 - n.leading_zeros();
    let len = if bits <= 11 {
        2
    } else if bits <= 16 {
        3
    } else if bits <= 21 {
        4
    } else if bits <= 26 {
        5
    } else if bits <= 31 {
        6
    } else {
        7
    };
    let first = (0xffu64 << (8 - len)) & 0xff;
    let remaining = (len - 1) * 6;
    bw.put(8, first | (n >> remaining));
    for i in (0..len - 1).rev() {
        bw.put(8, 0x80 | ((n >> (i * 6)) & 63));
    }
}
