// SPDX-License-Identifier: LGPL-2.1-or-later
// FLAC fixed predictors, signed Rice mapping and frame writer adapted from
// FFmpeg libavcodec/flacenc.c / lpc.c, Copyright (c) 2006 Justin Ruggles.
// Redesign: bounded 4096-frame work buffers; constant/fixed/verbatim choice;
// exact Rice costs near an estimated parameter; adaptive independent/mid-side;
// streaming MD5/PCM SHA256, verified output and atomic no-clobber publication.
use crate::{
    audio::{pcm_sha256, AudioReader},
    bits::{crc16, crc8, BeWriter},
    kernels::{Backend, Dot64Kernel, LpcKernel},
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
    let profile = Profile::new(level.unwrap_or(5))?;
    if !backend.available() {
        return Err(Error::Unsupported("CPU backend"));
    }
    let mut reader = AudioReader::open(src, limits.clone())?;
    let spec = reader.spec.clone();
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
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&temp.0)?;
    // Already-valid FLAC needs metadata removal, not another prediction pass.
    // Preserve only STREAMINFO and CRC-checked audio packets, then independently
    // decode/hash the resulting file just like newly encoded inputs.
    let copied = if level.is_none() {
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
    let mut bytes = Vec::new();
    while reader.next(&mut samples, backend)? {
        bytes.clear();
        for x in &samples {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        hash.update(&bytes);
        match &mut writer {
            Normalizer::Encode(encoder) => encoder.push(&samples)?,
            Normalizer::Copy(file) => file.write_all(
                &reader
                    .take_flac_frame()
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
    let (out, out_hash, out_frames) = pcm_sha256(&temp.0, limits, backend)?;
    if out_hash != source_hash
        || out_frames != frames
        || out.sample_rate != spec.sample_rate
        || out.channels != spec.channels
        || out.bits_per_sample != spec.bits_per_sample
    {
        return Err(Error::Invalid("lossless round-trip verification"));
    }
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
    profile: Profile,
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
        Ok(Self {
            file,
            spec,
            pending: Vec::with_capacity(capacity),
            frames: 0,
            number: 0,
            md5: Md5::new(),
            limits,
            bytes: 42,
            min_frame: usize::MAX,
            max_frame: 0,
            dot: Dot64Kernel::new(backend),
            lpc: LpcKernel::new(backend),
            profile,
        })
    }
    fn push(&mut self, samples: &[i32]) -> Result<()> {
        let shift = 32 - self.spec.bits_per_sample;
        let nbytes = (self.spec.bits_per_sample / 8) as usize;
        let mut raw = Vec::with_capacity(samples.len() * nbytes);
        for x in samples {
            raw.extend_from_slice(&(x >> shift).to_le_bytes()[..nbytes]);
        }
        self.md5.update(&raw);
        let block = self.profile.block * self.spec.channels as usize;
        let mut pos = 0;
        while pos < samples.len() {
            let n = (block - self.pending.len()).min(samples.len() - pos);
            self.pending.extend_from_slice(&samples[pos..pos + n]);
            pos += n;
            if self.pending.len() == block {
                self.write_block()?;
                self.pending.clear();
            }
        }
        Ok(())
    }
    fn write_block(&mut self) -> Result<()> {
        self.limits.check()?;
        let channels = self.spec.channels as usize;
        let n = self.pending.len() / channels;
        let depth = self.spec.bits_per_sample as u32;
        let shift = 32 - depth;
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for row in self.pending.chunks_exact(channels) {
            left.push(row[0] >> shift);
            if channels == 2 {
                right.push(row[1] >> shift);
            }
        }
        let l = Plan::new(&left, depth, &self.dot, &self.lpc, self.profile);
        let r = if channels == 2 {
            Some(Plan::new(&right, depth, &self.dot, &self.lpc, self.profile))
        } else {
            None
        };
        let mut mid = Vec::new();
        let mut side = Vec::new();
        let mut ms = None;
        // Uncorrelated stereo usually gains nothing from mid-side. A cheap
        // covariance check avoids planning two extra subframes in that case.
        let correlated = if channels == 2 && self.profile.level >= 3 {
            let (mut ll, mut rr, mut lr) = (0.0f64, 0.0f64, 0.0f64);
            for (&a, &b) in left.iter().zip(&right) {
                ll += (a as f64) * (a as f64);
                rr += (b as f64) * (b as f64);
                lr += (a as f64) * (b as f64);
            }
            lr * lr > 0.015625 * ll * rr
        } else {
            false
        };
        if correlated {
            mid.reserve(n);
            side.reserve(n);
            for (&a, &b) in left.iter().zip(&right) {
                mid.push((a + b) >> 1);
                side.push(a - b);
            }
            let m = Plan::new(&mid, depth, &self.dot, &self.lpc, self.profile);
            let s = Plan::new(&side, depth + 1, &self.dot, &self.lpc, self.profile);
            if m.cost + s.cost < l.cost + r.as_ref().unwrap().cost {
                ms = Some((m, s));
            }
        }
        let mut bw = BeWriter::new();
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
            m.write(&mut bw, &mid, depth);
            s.write(&mut bw, &side, depth + 1);
        } else {
            l.write(&mut bw, &left, depth);
            if let Some(r) = r {
                r.write(&mut bw, &right, depth);
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
        self.frames += n as u64;
        self.number += 1;
        Ok(())
    }
    fn finish(mut self) -> Result<File> {
        if !self.pending.is_empty() {
            self.write_block()?;
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
impl Plan {
    fn new(
        samples: &[i32],
        depth: u32,
        dot: &Dot64Kernel,
        lpc: &LpcKernel,
        profile: Profile,
    ) -> Self {
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
        let mut diff: Vec<i64> = samples.iter().map(|x| *x as i64).collect();
        for order in 0..=profile.fixed.min(samples.len() - 1) {
            if order > 0 {
                for i in (order..samples.len()).rev() {
                    diff[i] -= diff[i - 1];
                }
            }
            let residual: Vec<u32> = diff[order..]
                .iter()
                .map(|x| ((*x << 1) ^ (*x >> 63)) as u32)
                .collect();
            let sum = residual.iter().map(|x| *x as u64).sum::<u64>();
            let mean = sum / residual.len() as u64;
            let estimate = if mean == 0 {
                0
            } else {
                63 - mean.leading_zeros()
            };
            let mut cheapest = (u64::MAX, 0);
            for k in estimate.saturating_sub(1)..=(estimate + 1).min(30) {
                let cost = 8
                    + order as u64 * depth as u64
                    + 11
                    + residual
                        .iter()
                        .map(|v| (*v as u64 >> k) + 1 + k as u64)
                        .sum::<u64>();
                if cost < cheapest.0 {
                    cheapest = (cost, k);
                }
            }
            if cheapest.0 < best.cost {
                best = Self {
                    cost: cheapest.0,
                    mode: Mode::Fixed {
                        order,
                        k: cheapest.1,
                        residual,
                    },
                };
            }
        }
        // Welch-tapered autocorrelation and Levinson-Durbin, limited to order
        // eight. Integer residual costs decide; no lossy reconstruction.
        if samples.len() > 16 && profile.lpc > 0 {
            let n = samples.len();
            let windowed: Vec<f64> = samples
                .iter()
                .enumerate()
                .map(|(i, &x)| {
                    let d = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
                    x as f64 * (1.0 - d * d)
                })
                .collect();
            let mut r = [0.0f64; 9];
            for (lag, energy) in r.iter_mut().enumerate() {
                *energy = dot.apply(&windowed[lag..], &windowed[..n - lag]);
            }
            let mut a = [0.0f64; 8];
            let mut error = r[0];
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
                let Some(residual) = lpc.residual(samples, &coefficients, shift) else {
                    continue;
                };
                let mean = residual.iter().map(|x| *x as u64).sum::<u64>() / residual.len() as u64;
                let estimate = if mean == 0 {
                    0
                } else {
                    63 - mean.leading_zeros()
                };
                let overhead = 8 + order as u64 * depth as u64 + 4 + 5 + order as u64 * 12 + 11;
                let mut cheapest = (u64::MAX, 0);
                for k in estimate.saturating_sub(1)..=(estimate + 1).min(30) {
                    let cost = overhead
                        + residual
                            .iter()
                            .map(|v| (*v as u64 >> k) + 1 + k as u64)
                            .sum::<u64>();
                    if cost < cheapest.0 {
                        cheapest = (cost, k);
                    }
                }
                if cheapest.0 < best.cost {
                    best = Self {
                        cost: cheapest.0,
                        mode: Mode::Lpc {
                            coefficients,
                            shift,
                            k: cheapest.1,
                            residual,
                        },
                    };
                }
            }
        }
        best
    }
    fn write(self, bw: &mut BeWriter, samples: &[i32], depth: u32) {
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
                for v in residual {
                    bw.rice(v, k);
                }
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
                for v in residual {
                    bw.rice(v, k);
                }
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
