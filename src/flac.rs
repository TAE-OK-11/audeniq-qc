// SPDX-License-Identifier: LGPL-2.1-or-later
// FLAC fixed predictors, signed Rice mapping and frame writer adapted from
// FFmpeg libavcodec/flacenc.c / lpc.c, Copyright (c) 2006 Justin Ruggles.
// Redesign: bounded block work buffers; constant/fixed/verbatim choice;
// exact Rice costs near an estimated parameter; adaptive independent/mid-side;
// streaming MD5/PCM SHA256, verified output and atomic no-clobber publication.
// Own planning kernels (bounded u32/i32 lanes, whole-block LPC residuals),
// branch-free Rice writer and positional frame verification; own Rice cost
// model and exact stereo-assignment trials at levels 6-9. Own model search:
// LPC orders up to the FLAC subset limits (12, or 32 above 48 kHz, with
// 16384-frame blocks there), several apodization windows, models ranked by
// residuals at gathered sample positions, minimal and searched coefficient
// precision.
#[cfg(feature = "reference-codecs")]
use crate::audio::pcm_sha256;
use crate::{
    audio::AudioReader,
    bits::{crc16, crc8, BeWriter},
    kernels::{Backend, Dot64Kernel, LpcKernel, RiceKernel},
    AudioSpec, Error, Limits, Result,
};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Apodization applied to a block before its autocorrelation. Each window
/// gives its own Levinson-Durbin model set; integer residual costs decide.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Window {
    /// 1 - x^2 over [-1, 1].
    Welch,
    /// Flat top with raised-cosine tapers over `percent`% of the block.
    Tukey(u8),
    /// Tukey(50) over segment `index` of `parts` equal segments, zero elsewhere:
    /// a model of one part of a block whose signal changes inside it.
    Partial(u8, u8),
    /// Tukey(50) over the block with segment `index` of `parts` tapered out.
    Punchout(u8, u8),
}

/// Encoder settings of one compression level. Every level writes FLAC
/// subset streams with fixed block sizes.
#[derive(Clone, Copy)]
struct Profile {
    level: u8,
    /// Block frames up to 48 kHz and above it (the subset allows 4608 and
    /// 16384). High-order predictors at high rates gain from longer blocks.
    block: usize,
    block_high: usize,
    fixed: usize,
    /// Largest LPC order up to 48 kHz and above it (the subset allows 12
    /// and 32).
    lpc: usize,
    lpc_high: usize,
    windows: &'static [Window],
    /// LPC models costed exactly besides the highest order of the first
    /// window: 0 for none, `usize::MAX` for all, otherwise the best ones by
    /// sampled estimate.
    exact: usize,
    /// Coefficient precision of the planned models; lower precisions down
    /// to `precision_low` are also costed for the winning model.
    precision: u32,
    precision_low: u32,
    /// Largest Rice partition order.
    partition: u32,
    /// Stereo assignments planned exactly, best estimated first.
    stereo: usize,
}
const WELCH: &[Window] = &[Window::Welch];
/// Level 9: whole-block, half-block and third-block models, and whole-block
/// models with each third left out.
const TUKEY3: &[Window] = &[
    Window::Welch,
    Window::Tukey(50),
    Window::Partial(2, 0),
    Window::Partial(2, 1),
    Window::Partial(3, 0),
    Window::Partial(3, 1),
    Window::Partial(3, 2),
    Window::Punchout(3, 0),
    Window::Punchout(3, 1),
    Window::Punchout(3, 2),
];
impl Profile {
    fn new(level: u8) -> Result<Self> {
        let base = Self {
            level,
            block: 4096,
            block_high: 4096,
            fixed: 4,
            lpc: 0,
            lpc_high: 0,
            windows: WELCH,
            exact: 0,
            precision: 13,
            precision_low: 13,
            partition: 6,
            stereo: 1,
        };
        // Levels 0-3 cost about the same CPU (decoding, hashing and frame
        // verification dominate; the fused fixed sums cost one pass for any
        // order from 2). Each of levels 0-8 stays within the CPU of the
        // previous preset of the same number; see
        // docs/verified-pipeline/round13-encoder.txt.
        Ok(match level {
            0 => Self {
                block: 1024,
                block_high: 1024,
                fixed: 1,
                partition: 3,
                ..base
            },
            1 => Self {
                block: 2048,
                block_high: 2048,
                fixed: 2,
                partition: 3,
                ..base
            },
            2 => Self {
                partition: 3,
                ..base
            },
            3 => base,
            4 => Self {
                lpc: 8,
                lpc_high: 8,
                ..base
            },
            5 => Self {
                block_high: 16384,
                lpc: 12,
                lpc_high: 16,
                ..base
            },
            6 => Self {
                block_high: 16384,
                lpc: 12,
                lpc_high: 16,
                exact: 1,
                stereo: 2,
                ..base
            },
            7 => Self {
                block_high: 16384,
                lpc: 12,
                lpc_high: 32,
                exact: 2,
                precision_low: 12,
                stereo: 4,
                ..base
            },
            8 => Self {
                block_high: 16384,
                lpc: 12,
                lpc_high: 32,
                exact: 3,
                precision: 15,
                precision_low: 12,
                stereo: 4,
                ..base
            },
            9 => Self {
                block_high: 16384,
                lpc: 12,
                lpc_high: 32,
                windows: TUKEY3,
                exact: 8,
                precision: 15,
                precision_low: 11,
                partition: 8,
                stereo: 4,
                ..base
            },
            _ => return Err(Error::Invalid("compression level range 0..9")),
        })
    }
}
pub struct Conversion {
    pub spec: AudioSpec,
    pub frames: u64,
    pub pcm_sha256: String,
    pub output_bytes: u64,
    pub encoder: &'static str,
    pub compression_level: Option<u8>,
    pub analysis: Option<crate::meter::Analysis>,
}
crate::json_struct!(Conversion {
    spec,
    frames,
    pcm_sha256,
    output_bytes,
    encoder,
    compression_level,
    analysis: skip_none,
});

#[derive(Clone, Copy, Default)]
pub struct ConvertOptions {
    pub compression_level: Option<u8>,
    pub analyze: bool,
    pub fingerprint: bool,
}
// One normalizer per command; stack storage avoids an unnecessary heap box.
#[allow(clippy::large_enum_variant)]
enum Normalizer {
    Copy(File, FrameLog),
    Encode(Encoder),
}
/// Running record of every audio-frame byte written after the 42-byte
/// `fLaC` + STREAMINFO header. The published file is re-read and must match
/// this length and IEEE CRC32 exactly; each recorded frame was already decoded
/// and compared sample-for-sample with its source PCM before being written.
#[derive(Default)]
struct FrameLog {
    crc: crate::crc32::Crc32,
    bytes: u64,
}
impl FrameLog {
    fn add(&mut self, frame: &[u8]) {
        self.crc.update(frame);
        self.bytes += frame.len() as u64;
    }
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
        f.write_all(&flac_header(&info))?;
        Normalizer::Copy(f, FrameLog::default())
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
    // Source SHA-256 and, when encoding, the STREAMINFO MD5 of the same
    // PCM in one fused pass (see `pcm_hash`).
    let encoding = matches!(writer, Normalizer::Encode(_));
    let mut hash = crate::pcm_hash::PcmHash::new(spec.bits_per_sample, encoding);
    while reader.next_hashed(&mut samples, backend, &mut hash)? {
        if let Some(analyzer) = &mut analyzer {
            let _profile = crate::profile::scope(crate::profile::Stage::Qc);
            analyzer.push(&samples);
        }
        match &mut writer {
            Normalizer::Encode(encoder) => encoder.push(&samples)?,
            Normalizer::Copy(file, log) => {
                // The borrowed frame is exactly the CRC-checked bytes that the
                // source decoder just reconstructed into `samples`.
                let frame = reader
                    .flac_frame()
                    .ok_or(Error::Invalid("missing FLAC frame"))?;
                log.add(frame);
                file.write_all(frame)?
            }
        }
    }
    let frames = reader.decoded_frames();
    let (source_sha, pcm_md5) = hash.finish_with(reader.verified_md5())?;
    let source_hash = crate::hex(&source_sha);
    let (f, header, log) = match writer {
        Normalizer::Encode(encoder) => {
            encoder.finish(pcm_md5.ok_or(Error::Invalid("missing PCM MD5"))?)?
        }
        Normalizer::Copy(file, log) => (file, flac_header(&copied.unwrap()), log),
    };
    f.sync_all()?;
    let output_bytes = f.metadata()?.len();
    if output_bytes > limits.max_file_bytes {
        return Err(Error::Limit("FLAC output bytes"));
    }
    drop(f);
    let out = {
        let _profile = crate::profile::scope(crate::profile::Stage::OutputVerify);
        verify_output(&temp.0, &header, &log, &source_hash, &limits, backend)?
    };
    if out.frames != Some(frames)
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
            "adaptive-lpc-rice-v2"
        },
        compression_level: if copied.is_some() {
            None
        } else {
            Some(profile.level)
        },
        analysis,
    })
}

fn flac_header(info: &[u8; 34]) -> [u8; 42] {
    let mut header = [0u8; 42];
    header[..8].copy_from_slice(b"fLaC\x80\x00\x00\x22");
    header[8..].copy_from_slice(info);
    header
}

/// Native builds verify every encoded frame in memory before it is written
/// (see `Encoder::write_block`), so the published file only needs to be shown
/// byte-identical to those verified frames: STREAMINFO is parsed/validated
/// again, the 42-byte header must match and the frame region must have the
/// recorded length and CRC32. This replaces a second full decode, packing,
/// MD5 and SHA-256 pass over the output.
#[cfg(not(feature = "reference-codecs"))]
fn verify_output(
    path: &Path,
    header: &[u8; 42],
    log: &FrameLog,
    _source_hash: &str,
    limits: &Limits,
    _backend: Backend,
) -> Result<AudioSpec> {
    use std::io::Read;
    let spec = crate::flac_decode::Decoder::open(File::open(path)?, limits)?
        .spec
        .clone();
    let mut file = File::open(path)?;
    let mut head = [0u8; 42];
    file.read_exact(&mut head)?;
    let mut crc = crate::crc32::Crc32::new();
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        limits.check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        crc.update(&buffer[..n]);
        bytes += n as u64;
    }
    if head != *header || bytes != log.bytes || crc.finalize() != log.crc.finalize() {
        return Err(Error::Invalid("lossless round-trip verification"));
    }
    Ok(spec)
}

/// The optional Symphonia comparison build keeps the original independent
/// whole-file redecode and PCM SHA-256 comparison.
#[cfg(feature = "reference-codecs")]
fn verify_output(
    path: &Path,
    _header: &[u8; 42],
    _log: &FrameLog,
    source_hash: &str,
    limits: &Limits,
    backend: Backend,
) -> Result<AudioSpec> {
    let (mut out, out_hash, out_frames) = pcm_sha256(path, limits.clone(), backend)?;
    if out_hash != source_hash {
        return Err(Error::Invalid("lossless round-trip verification"));
    }
    out.frames = Some(out_frames);
    Ok(out)
}

struct Encoder {
    file: File,
    spec: AudioSpec,
    pending: Vec<i32>,
    frames: u64,
    number: u64,
    limits: Limits,
    bytes: u64,
    min_frame: usize,
    max_frame: usize,
    dot: Dot64Kernel,
    lpc: LpcKernel,
    rice: RiceKernel,
    #[cfg(not(feature = "reference-codecs"))]
    backend: Backend,
    profile: Profile,
    frame_buffer: Vec<u8>,
    channel_buffers: [Vec<i32>; 4],
    planner: Planner,
    log: FrameLog,
    #[cfg(not(feature = "reference-codecs"))]
    verify: crate::flac_decode::VerifyScratch,
}
impl Encoder {
    fn new(
        mut file: File,
        spec: AudioSpec,
        limits: Limits,
        backend: Backend,
        mut profile: Profile,
    ) -> Result<Self> {
        if spec.sample_rate > 48000 {
            profile.block = profile.block_high;
            profile.lpc = profile.lpc_high;
        }
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
            limits,
            bytes: 42,
            min_frame: usize::MAX,
            max_frame: 0,
            dot: Dot64Kernel::new(backend),
            lpc: LpcKernel::new(backend),
            rice: RiceKernel::new(backend),
            #[cfg(not(feature = "reference-codecs"))]
            backend,
            profile,
            frame_buffer: Vec::with_capacity(raw_capacity + 128),
            channel_buffers: std::array::from_fn(|_| Vec::new()),
            planner: Planner {
                fixed_sums: vec![[0; PARTITIONS]; 5],
                ..Default::default()
            },
            log: FrameLog::default(),
            #[cfg(not(feature = "reference-codecs"))]
            verify: Default::default(),
        })
    }
    fn push(&mut self, samples: &[i32]) -> Result<()> {
        let _profile = crate::profile::scope(crate::profile::Stage::Encoder);
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
        if channels == 2 {
            left.resize(n, 0);
            right.resize(n, 0);
            for ((row, l), r) in samples
                .as_chunks::<2>()
                .0
                .iter()
                .zip(left.iter_mut())
                .zip(right.iter_mut())
            {
                *l = row[0] >> shift;
                *r = row[1] >> shift;
            }
        } else {
            left.clear();
            left.extend(samples.iter().map(|&x| x >> shift));
        }
        let ctx = Context {
            dot: &self.dot,
            lpc: &self.lpc,
            rice: &self.rice,
            profile: self.profile,
        };
        // FLAC channel assignment: 1 = L/R, 8 = left/side, 9 = side/right,
        // 10 = mid/side; side carries one extra bit. Second-order fixed
        // residual sums rank the four pairs. Levels up to 6 plan only the
        // best-ranked pair; level 7 plans the channels of the two best and
        // levels 8-9 all four channels, and the smallest exact pair wins.
        let stereo = crate::profile::scope(crate::profile::Stage::EncoderStereo);
        // Indices into channel_buffers: left, right, mid, side.
        const PAIRS: [(u64, [usize; 2]); 4] = [(1, [0, 1]), (8, [0, 3]), (9, [3, 1]), (10, [2, 3])];
        let ranked = if channels == 1 {
            None
        } else {
            mid.resize(n, 0);
            side.resize(n, 0);
            for (((&a, &b), m), d) in left
                .iter()
                .zip(right.iter())
                .zip(mid.iter_mut())
                .zip(side.iter_mut())
            {
                *m = (a + b) >> 1;
                *d = a - b;
            }
            let estimate = |sum: u64| {
                if n < 3 {
                    return 0;
                }
                rice_estimate(sum, n as u64 - 2).1
            };
            let sums = second_order_sums([left, right, mid, side], depth + 1);
            let (l, r, m, d) = (
                estimate(sums[0]),
                estimate(sums[1]),
                estimate(sums[2]),
                estimate(sums[3]),
            );
            let costs = [l + r, l + d, d + r, m + d];
            let mut ranked = [0, 1, 2, 3];
            // Stable: equal estimates keep the assignment order.
            ranked.sort_by_key(|&i| costs[i]);
            Some(ranked)
        };
        stereo.end();
        let channel_depth = |index: usize| depth + u32::from(index == 3);
        let trials = if ranked.is_some() {
            self.profile.stereo
        } else {
            0
        };
        let planner = &mut self.planner;
        let mut plans: [Option<Plan>; 4] = Default::default();
        let (assignment, channel_indices) = match ranked {
            None => (0u64, [Some(0), None]),
            Some(ranked) if trials == 1 => {
                let (assignment, pair) = PAIRS[ranked[0]];
                (assignment, pair.map(Some))
            }
            Some(ranked) => {
                for &rank in &ranked[..trials] {
                    for index in PAIRS[rank].1 {
                        if plans[index].is_none() {
                            let samples = &mut self.channel_buffers[index];
                            plans[index] =
                                Some(Plan::new(samples, channel_depth(index), &ctx, planner));
                        }
                    }
                }
                let cost = |rank: usize| {
                    let [a, b] = PAIRS[rank].1;
                    plans[a].as_ref().unwrap().cost + plans[b].as_ref().unwrap().cost
                };
                // First minimum: ties keep the better-ranked estimate.
                let best = ranked[..trials]
                    .iter()
                    .copied()
                    .min_by_key(|&rank| cost(rank))
                    .unwrap();
                let (assignment, pair) = PAIRS[best];
                for (index, plan) in plans.iter_mut().enumerate() {
                    if !pair.contains(&index) {
                        if let Some(plan) = plan.take() {
                            plan.release(planner);
                        }
                    }
                }
                (assignment, pair.map(Some))
            }
        };
        let mut bw = BeWriter::reuse(std::mem::take(&mut self.frame_buffer));
        bw.put(16, 0xfff8);
        bw.put(4, 7);
        bw.put(4, 0);
        bw.put(4, assignment);
        bw.put(3, if depth == 16 { 4 } else { 6 });
        bw.put(1, 0);
        utf8(&mut bw, self.number);
        bw.put(16, (n - 1) as u64);
        let crc = crc8(&bw.bytes);
        bw.put(8, crc as u64);
        // The header does not depend on subframe contents, so below level 7
        // each subframe is planned and written in turn: only one planned
        // model (and its stored residual) is alive at a time.
        for index in channel_indices.into_iter().flatten() {
            let samples = &mut self.channel_buffers[index];
            let plan = match plans[index].take() {
                Some(plan) => plan,
                None => Plan::new(samples, channel_depth(index), &ctx, planner),
            };
            plan.write(&mut bw, samples, &ctx, planner)?;
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
        // Parse the complete frame (header CRC8, subframes, frame CRC16) and
        // require that it decodes to the exact source block before any byte
        // is written; see `flac_decode::verify` for the equivalence argument.
        #[cfg(not(feature = "reference-codecs"))]
        {
            let _profile = crate::profile::scope(crate::profile::Stage::FrameVerify);
            let verified = crate::flac_decode::verify(
                &bw.bytes,
                &self.spec,
                self.profile.block,
                samples,
                &mut self.verify,
                self.backend,
            );
            if !verified.is_ok_and(|(length, header)| {
                length == bw.bytes.len()
                    && header.samples == n
                    && header.number == self.number
                    && !header.variable
            }) {
                return Err(Error::Invalid("lossless frame verification"));
            }
        }
        self.log.add(&bw.bytes);
        self.file.write_all(&bw.bytes)?;
        self.frame_buffer = bw.bytes;
        self.frames += n as u64;
        self.number += 1;
        Ok(())
    }
    fn finish(mut self, md5: [u8; 16]) -> Result<(File, [u8; 42], FrameLog)> {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.write_block(&pending)?;
        }
        if self.frames == 0 || self.frames > self.limits.max_frames {
            return Err(Error::Invalid("empty/oversized FLAC output"));
        }
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
        let header = flac_header(b.bytes.as_slice().try_into().unwrap());
        Ok((self.file, header, self.log))
    }
}

/// Planning kernels and profile shared by every subframe of one encoder.
struct Context<'a> {
    dot: &'a Dot64Kernel,
    lpc: &'a LpcKernel,
    rice: &'a RiceKernel,
    profile: Profile,
}

/// Largest LPC order of any profile (the FLAC format's limit).
const MAX_LPC: usize = crate::kernels::MAX_LPC_ORDER;

/// Rice partition orders above this are never searched (the FLAC subset
/// limit). With 4096-frame blocks the finest partitions hold 16 residuals.
const MAX_PARTITION_ORDER: u32 = 8;
const PARTITIONS: usize = 1 << MAX_PARTITION_ORDER;

/// Partitioned Rice parameters for one residual block. `bits` counts the
/// coding method, partition order, parameters and residual codes.
#[derive(Clone, Copy)]
struct Rice {
    order: u32,
    params: [u8; PARTITIONS],
    bits: u64,
}

/// Estimated Rice parameter and bits for a partition of `count` folded
/// residuals summing to `sum`. With parameter k a value u costs
/// k + 1 + floor(u / 2^k) bits; if the k low bits of the values are spread
/// evenly, floor(u / 2^k) averages u / 2^k - (1 - 2^-k) / 2, so the
/// quotients total about (2 sum - count (2^k - 1)) / 2^(k+1). The parameter
/// is floor(log2(mean)), which this model ranks best in about 95% of
/// partitions (evaluating its neighbours too saved 0.0006% of bytes for 2%
/// more conversion CPU); k = 0 is exact. The writer still picks each
/// partition's exact best parameter.
fn rice_estimate(sum: u64, count: u64) -> (u32, u64) {
    if count == 0 {
        return (0, 0);
    }
    if sum < count {
        return (0, count + sum);
    }
    // floor(log2(floor(sum / count))) is the largest k with count 2^k <= sum;
    // found from the bit lengths without a division.
    let mut k = (63 - sum.leading_zeros()) - (63 - count.leading_zeros());
    if count << k > sum {
        k -= 1;
    }
    let k = k.min(30);
    if k == 0 {
        return (0, count + sum);
    }
    let quotients = (2 * sum).saturating_sub(count * ((1 << k) - 1)) >> (k + 1);
    (k, count * (k as u64 + 1) + quotients)
}

/// Largest searched partition order for an n-sample block: partitions must
/// tile the block exactly and hold at least 16 samples. Level 5 (the
/// default) stops at order 6: on the test corpus orders 7 and 8 changed 4 of
/// 55 files by 292 bytes in total (1 ppm) while they multiply the partition
/// search (and its sums) by four.
fn max_partition_order(n: usize, limit: u32) -> u32 {
    let mut order = n.trailing_zeros().min(limit);
    while order > 0 && n >> order < 16 {
        order -= 1;
    }
    order
}

/// Largest partition order not above `finest` whose first partition holds
/// more samples than the `predictor` warm-up, or None if even one partition
/// cannot.
fn predictor_partition_order(n: usize, finest: u32, predictor: usize) -> Option<u32> {
    (0..=finest).rev().find(|&order| n >> order > predictor)
}

/// Choose the partition order from per-finest-partition residual sums.
/// `sums[i]` covers residuals of partition i at `finest`, excluding the
/// `predictor` warm-up samples, which are not coded as residuals.
fn choose_rice(sums: &[u64], n: usize, predictor: usize, finest: u32) -> Rice {
    let mut level = [0u64; PARTITIONS];
    level[..1 << finest].copy_from_slice(&sums[..1 << finest]);
    let mut best = Rice {
        order: 0,
        params: [0; PARTITIONS],
        bits: u64::MAX,
    };
    let mut params = [0u8; PARTITIONS];
    for order in (0..=finest).rev() {
        let parts = 1usize << order;
        if order < finest {
            for i in 0..parts {
                level[i] = level[2 * i] + level[2 * i + 1];
            }
        }
        let size = (n >> order) as u64;
        let mut bits = 2 + 4;
        let mut wide = false;
        for i in 0..parts {
            let count = size - if i == 0 { predictor as u64 } else { 0 };
            let (k, b) = rice_estimate(level[i], count);
            params[i] = k as u8;
            wide |= k > 14;
            bits += b;
        }
        bits += parts as u64 * if wide { 5 } else { 4 };
        if bits < best.bits {
            best.bits = bits;
            best.order = order;
            best.params[..parts].copy_from_slice(&params[..parts]);
        }
    }
    best
}

#[inline]
fn fold(r: i32) -> u32 {
    ((r << 1) ^ (r >> 31)) as u32
}

/// Fixed-predictor residual of `order` for every sample from `start`.
/// Samples have at most 25 significant bits, so order-4 residuals (at most
/// 16x the sample range) fit i32 exactly.
fn fixed_residual(x: &[i32], order: usize, start: usize, end: usize, mut f: impl FnMut(u32)) {
    match order {
        0 => x[start..end].iter().for_each(|&a| f(fold(a))),
        1 => x[start..end]
            .iter()
            .zip(&x[start - 1..end - 1])
            .for_each(|(&a, &b)| f(fold(a - b))),
        2 => x[start..end]
            .iter()
            .zip(&x[start - 1..end - 1])
            .zip(&x[start - 2..end - 2])
            .for_each(|((&a, &b), &c)| f(fold(a - 2 * b + c))),
        3 => x[start..end]
            .iter()
            .zip(&x[start - 1..end - 1])
            .zip(&x[start - 2..end - 2])
            .zip(&x[start - 3..end - 3])
            .for_each(|(((&a, &b), &c), &d)| f(fold(a - 3 * b + 3 * c - d))),
        _ => x[start..end]
            .iter()
            .zip(&x[start - 1..end - 1])
            .zip(&x[start - 2..end - 2])
            .zip(&x[start - 3..end - 3])
            .zip(&x[start - 4..end - 4])
            .for_each(|((((&a, &b), &c), &d), &e)| f(fold(a - 4 * b + 6 * c - 4 * d + e))),
    }
}

/// Sum of folded fixed residuals over one partition, written as zipped
/// slices without per-element bounds checks so the baseline ISA vectorizes
/// it (SSE2/AVX2/NEON).
fn fixed_sum(x: &[i32], order: usize, start: usize, end: usize) -> u64 {
    let mut total = 0u64;
    fixed_residual(x, order, start, end, |r| total += r as u64);
    total
}

/// `sums[order][i] == fixed_sum(x, order, max(i * size, order), (i + 1) * size)`
/// for every order 0..=4, from one traversal instead of five. Residuals are
/// formed as repeated differences of the same samples, which equal the
/// direct fixed-predictor formulas as integers (no intermediate exceeds the
/// order-4 residual bound), so every sum and planning decision is unchanged.
fn fixed_partition_sums(
    x: &[i32],
    depth: u32,
    size: usize,
    parts: usize,
    sums: &mut [[u64; PARTITIONS]],
) {
    let narrow = (size as u64) << (depth + 4) <= u32::MAX as u64;
    for i in 0..parts {
        let (lo, hi) = (i * size, (i + 1) * size);
        let mut acc = [0u64; 5];
        // Order k has residuals only from sample k; before sample 4 the
        // orders are summed separately (first partition only).
        let start = lo.max(4).min(hi);
        for (order, acc) in acc.iter_mut().enumerate() {
            if lo.max(order) < start {
                *acc = fixed_sum(x, order, lo.max(order), start);
            }
        }
        if narrow {
            // Each folded order-k residual is below 2^(depth + k), so a
            // partition's sums fit u32: eight lanes per vector, not four.
            let mut lanes = [0u32; 5];
            let [a0, a1, a2, a3, a4] = &mut lanes;
            for ((((&a, &b), &c), &d), &e) in x[start..hi]
                .iter()
                .zip(&x[start - 1..hi - 1])
                .zip(&x[start - 2..hi - 2])
                .zip(&x[start - 3..hi - 3])
                .zip(&x[start - 4..hi - 4])
            {
                let (d1, d1b, d1c, d1d) = (a - b, b - c, c - d, d - e);
                let (d2, d2b, d2c) = (d1 - d1b, d1b - d1c, d1c - d1d);
                let (d3, d3b) = (d2 - d2b, d2b - d2c);
                *a0 += fold(a);
                *a1 += fold(d1);
                *a2 += fold(d2);
                *a3 += fold(d3);
                *a4 += fold(d3 - d3b);
            }
            for (sums, (total, lane)) in sums.iter_mut().zip(acc.iter().zip(lanes)) {
                sums[i] = total + lane as u64;
            }
            continue;
        }
        let [a0, a1, a2, a3, a4] = &mut acc;
        for ((((&a, &b), &c), &d), &e) in x[start..hi]
            .iter()
            .zip(&x[start - 1..hi - 1])
            .zip(&x[start - 2..hi - 2])
            .zip(&x[start - 3..hi - 3])
            .zip(&x[start - 4..hi - 4])
        {
            let (d1, d1b, d1c, d1d) = (a - b, b - c, c - d, d - e);
            let (d2, d2b, d2c) = (d1 - d1b, d1b - d1c, d1c - d1d);
            let (d3, d3b) = (d2 - d2b, d2b - d2c);
            *a0 += fold(a) as u64;
            *a1 += fold(d1) as u64;
            *a2 += fold(d2) as u64;
            *a3 += fold(d3) as u64;
            *a4 += fold(d3 - d3b) as u64;
        }
        for (sums, total) in sums.iter_mut().zip(acc) {
            sums[i] = total;
        }
    }
}

/// `fixed_sum(x, 2, 2, x.len())` for four equally long channels of at most
/// `depth` bits, in one pass. Each folded second-order residual is below
/// 2^(depth + 2), so runs of 2^(30 - depth) samples sum in u32 lanes.
fn second_order_sums(x: [&[i32]; 4], depth: u32) -> [u64; 4] {
    let n = x[0].len();
    let mut totals = [0u64; 4];
    if n < 3 {
        return totals;
    }
    if depth > 22 {
        for (total, x) in totals.iter_mut().zip(x) {
            *total = fixed_sum(x, 2, 2, n);
        }
        return totals;
    }
    let run = 1usize << (30 - depth);
    let mut start = 2;
    while start < n {
        let end = n.min(start + run);
        for (total, x) in totals.iter_mut().zip(x) {
            let mut sum = 0u32;
            for ((&a, &b), &c) in x[start..end]
                .iter()
                .zip(&x[start - 1..end - 1])
                .zip(&x[start - 2..end - 2])
            {
                sum += fold(a - 2 * b + c);
            }
            *total += sum as u64;
        }
        start = end;
    }
    totals
}

#[derive(Clone)]
enum Mode {
    Constant,
    Verbatim,
    Fixed {
        order: usize,
        rice: Box<Rice>,
    },
    Lpc {
        model: Box<Model>,
        rice: Box<Rice>,
        /// Folded residuals stored while the model was costed, if they were.
        residual: Option<Vec<u32>>,
    },
}
#[derive(Clone)]
struct Plan {
    cost: u64,
    depth: u32,
    wasted: u32,
    mode: Mode,
}
/// Quantized LPC predictor: `precision` is the signed width written for
/// every coefficient (the smallest that holds them all).
#[derive(Clone, Copy)]
struct Model {
    coefficients: [i32; MAX_LPC],
    order: usize,
    shift: u32,
    precision: u32,
}
impl Model {
    /// Quantize `a` with coefficients of at most `precision` signed bits,
    /// or None if the largest coefficient cannot be represented.
    fn quantize(a: &[f64], precision: u32) -> Option<Self> {
        let limit = ((1 << (precision - 1)) - 1) as f64;
        let largest = a.iter().fold(0.0f64, |v, x| v.max(x.abs()));
        if largest == 0.0 || largest > limit {
            return None;
        }
        let shift = (limit / largest).log2().floor().clamp(0.0, 15.0) as u32;
        let mut coefficients = [0i32; MAX_LPC];
        // Error-feedback rounding: each coefficient's rounding error is
        // carried into the next (noise-shaped quantization; plain rounding
        // made the corpus 0.25% larger).
        let mut error = 0.0f64;
        for (c, x) in coefficients.iter_mut().zip(a) {
            error += x * (1u32 << shift) as f64;
            let q = error.round().clamp(-limit, limit);
            *c = q as i32;
            error -= q;
        }
        let precision = coefficients[..a.len()]
            .iter()
            .map(|&c| 33 - (c ^ (c >> 31)).leading_zeros())
            .max()
            .unwrap_or(1);
        Some(Self {
            coefficients,
            order: a.len(),
            shift,
            precision,
        })
    }
    fn coefficients(&self) -> &[i32] {
        &self.coefficients[..self.order]
    }
    /// Subframe bits other than the residual and the subframe header.
    fn overhead(&self, depth: u32) -> u64 {
        self.order as u64 * (depth + self.precision) as u64 + 4 + 5
    }
}
struct LpcCandidate {
    model: Model,
    /// Unquantized coefficients, for the precision search.
    real: [f64; MAX_LPC],
    estimate: u64,
}
#[derive(Default)]
struct Planner {
    /// Apodization windows for blocks of `window_frames` samples, with the
    /// range outside which each is zero.
    windows: Vec<(Vec<f64>, usize, usize)>,
    window_frames: usize,
    windowed: Vec<f64>,
    candidates: Vec<LpcCandidate>,
    ranking: Vec<usize>,
    probe: Probe,
    residuals: Vec<Vec<u32>>,
    sums: Vec<u64>,
    fixed_sums: Vec<[u64; PARTITIONS]>,
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
        debug_assert!(self.residuals.len() < 6);
        self.residuals.push(residual);
    }
    fn prepare_windows(&mut self, n: usize, windows: &[Window]) {
        if self.window_frames == n && self.windows.len() == windows.len() {
            return;
        }
        self.window_frames = n;
        self.windows = windows.iter().map(|&w| window(w, n)).collect();
    }
}

/// Tukey window over `out` (tapering `percent`% of it), multiplied in.
fn tukey(out: &mut [f64], percent: u8) {
    let len = out.len();
    let taper = (len * percent as usize / 200).max(1);
    for i in 0..taper.min(len / 2) {
        let w = 0.5 - 0.5 * (std::f64::consts::PI * (i as f64 + 0.5) / taper as f64).cos();
        out[i] *= w;
        out[len - 1 - i] *= w;
    }
}

/// Window values for an n-sample block and the range [lo, hi) outside which
/// they are zero.
fn window(kind: Window, n: usize) -> (Vec<f64>, usize, usize) {
    let mut w = vec![1.0f64; n];
    let segment = |parts: u8, index: u8| {
        let (parts, index) = (parts as usize, index as usize);
        (n * index / parts, n * (index + 1) / parts)
    };
    match kind {
        Window::Welch => {
            for (i, w) in w.iter_mut().enumerate() {
                let d = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
                *w = 1.0 - d * d;
            }
        }
        Window::Tukey(percent) => tukey(&mut w, percent),
        Window::Partial(parts, index) => {
            let (lo, hi) = segment(parts, index);
            w[..lo].fill(0.0);
            w[hi..].fill(0.0);
            tukey(&mut w[lo..hi], 50);
            return (w, lo, hi);
        }
        Window::Punchout(parts, index) => {
            let (lo, hi) = segment(parts, index);
            let mut hole = vec![1.0f64; hi - lo];
            tukey(&mut hole, 50);
            for (w, h) in w[lo..hi].iter_mut().zip(hole) {
                *w = 1.0 - h;
            }
            tukey(&mut w, 50);
        }
    }
    (w, 0, n)
}

/// Residuals at evenly spaced sample positions rank prediction models
/// without computing whole blocks. This changes compression choices only;
/// reconstruction still uses checked exact residuals for every sample and
/// verifies output PCM. The positions and their histories are gathered once
/// per subframe, column by column, so that every model's sampled residuals
/// are formed with one vectorized multiply-add per coefficient.
struct Probe {
    count: usize,
    /// Residuals coded per model of order k: `n - k`.
    frames: usize,
    current: [i32; PROBES],
    /// `history[j][t]` is the sample j + 1 positions before point t.
    history: Vec<[i32; PROBES]>,
}
const PROBES: usize = crate::kernels::PROBE_POINTS;
impl Default for Probe {
    fn default() -> Self {
        Self {
            count: 0,
            frames: 0,
            current: [0; PROBES],
            history: Vec::new(),
        }
    }
}
impl Probe {
    /// Gather up to PROBES positions from `orders` (so that every order up
    /// to it has a full history) to the end of the block.
    fn gather(&mut self, samples: &[i32], orders: usize) {
        let n = samples.len();
        self.frames = n;
        self.count = PROBES.min(n - orders);
        self.history.resize(orders, [0; PROBES]);
        // Position orders + floor(point * span / steps), stepped without a
        // division per point.
        let span = n - orders - 1;
        let steps = (self.count - 1).max(1);
        let (whole, part) = (span / steps, span % steps);
        let (mut i, mut remainder) = (orders, 0);
        for t in 0..self.count {
            self.current[t] = samples[i];
            for (j, column) in self.history.iter_mut().enumerate() {
                column[t] = samples[i - 1 - j];
            }
            i += whole;
            remainder += part;
            if remainder >= steps {
                remainder -= steps;
                i += 1;
            }
        }
    }
    /// Estimated subframe bits of `model` (u64::MAX if a sampled residual
    /// is outside i32).
    fn cost(&self, model: &Model, depth: u32, kernel: &LpcKernel) -> u64 {
        let count = self.count;
        let mut prediction = [0i64; PROBES];
        kernel.columns(model.coefficients(), &self.history, &mut prediction);
        let mut residual = [0u32; PROBES];
        let mut out = 0u64;
        let mut total = 0u64;
        for ((r, &p), &x) in residual
            .iter_mut()
            .zip(&prediction)
            .zip(&self.current[..count])
        {
            let delta = x as i64 - (p >> model.shift);
            out |= (delta.wrapping_add(1 << 31) as u64) >> 32;
            *r = ((delta << 1) ^ (delta >> 63)) as u32;
            total += *r as u64;
        }
        if out != 0 {
            return u64::MAX;
        }
        let residual = &residual[..count];
        let mean = total / count as u64;
        let estimate = if mean == 0 {
            0
        } else {
            63 - mean.leading_zeros()
        };
        let coded = (self.frames - model.order) as u64;
        let overhead = 8 + model.overhead(depth) + 11;
        (estimate.saturating_sub(1)..=(estimate + 1).min(30))
            .map(|k| {
                let bits = residual
                    .iter()
                    .map(|&r| (r as u64 >> k) + 1 + k as u64)
                    .sum::<u64>();
                overhead + bits * coded / count as u64
            })
            .min()
            .unwrap_or(u64::MAX)
    }
}

/// Levinson-Durbin recursion on autocorrelation `r`, calling `found` with
/// the predictor of every order up to `r.len() - 1` until it fails.
fn levinson(r: &[f64], mut found: impl FnMut(&[f64])) {
    let mut a = [0.0f64; MAX_LPC];
    let mut error = r[0];
    for index in 0..r.len() - 1 {
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
        found(&a[..index + 1]);
    }
}

impl Plan {
    /// Plan one subframe. Common trailing zero bits ("wasted bits") are
    /// removed in place first; `write` must receive the same, shifted slice.
    fn new(samples: &mut [i32], depth: u32, ctx: &Context<'_>, planner: &mut Planner) -> Self {
        let _profile = crate::profile::scope(crate::profile::Stage::EncoderPlan);
        let profile = ctx.profile;
        if samples.iter().all(|x| *x == samples[0]) {
            return Self {
                cost: 8 + depth as u64,
                depth,
                wasted: 0,
                mode: Mode::Constant,
            };
        }
        let bits = samples.iter().fold(0i32, |a, &x| a | x);
        let wasted = bits.trailing_zeros().min(depth - 1);
        if wasted > 0 {
            samples.iter_mut().for_each(|x| *x >>= wasted);
        }
        let samples = &*samples;
        let depth = depth - wasted;
        let header = 8 + wasted as u64;
        let n = samples.len();
        let mut best = Self {
            cost: header + n as u64 * depth as u64,
            depth,
            wasted,
            mode: Mode::Verbatim,
        };
        let finest = max_partition_order(n, profile.partition);
        let size = n >> finest;
        let parts = 1usize << finest;
        planner.sums.resize(parts, 0);
        let fixed_stage = crate::profile::scope(crate::profile::Stage::EncoderFixed);
        let fused = profile.fixed >= 2 && size > 4;
        if fused {
            fixed_partition_sums(samples, depth, size, parts, &mut planner.fixed_sums);
        }
        for order in 0..=profile.fixed.min(n - 1) {
            if size <= order {
                break;
            }
            if fused {
                planner
                    .sums
                    .copy_from_slice(&planner.fixed_sums[order][..parts]);
            } else {
                for (i, sum) in planner.sums.iter_mut().enumerate() {
                    let start = (i * size).max(order);
                    *sum = fixed_sum(samples, order, start, (i + 1) * size);
                }
            }
            let rice = choose_rice(&planner.sums, n, order, finest);
            let cost = header + order as u64 * depth as u64 + rice.bits;
            if cost < best.cost {
                best = Self {
                    cost,
                    depth,
                    wasted,
                    mode: Mode::Fixed {
                        order,
                        rice: Box::new(rice),
                    },
                };
            }
        }
        fixed_stage.end();
        if n > 16 && profile.lpc > 0 {
            best = Self::plan_lpc(samples, header, best, finest, ctx, planner);
        }
        best
    }

    /// Autocorrelation of every apodized copy and Levinson-Durbin up to the
    /// profile's order give one model per window and order. The highest
    /// order of the first window (it wins most real-music subframes) and the
    /// `exact` best by sampled estimate are costed exactly; lower coefficient
    /// precisions are then tried on the winner. Integer residual costs
    /// decide; no lossy reconstruction.
    fn plan_lpc(
        samples: &[i32],
        header: u64,
        mut best: Self,
        finest: u32,
        ctx: &Context<'_>,
        planner: &mut Planner,
    ) -> Self {
        let profile = ctx.profile;
        let (n, depth, wasted) = (samples.len(), best.depth, best.wasted);
        let max_order = profile.lpc.min(n - 1);
        // Models are estimated from sampled residuals only when some, but
        // not all, are to be costed exactly besides the first highest.
        let sampled = profile.exact != 0 && profile.exact != usize::MAX;
        planner.prepare_windows(n, profile.windows);
        planner.candidates.clear();
        if sampled {
            planner.probe.gather(samples, max_order);
        }
        let mut r = [0.0f64; MAX_LPC + 1];
        let mut first_highest = None;
        for w in 0..planner.windows.len() {
            let autocorr = crate::profile::scope(crate::profile::Stage::EncoderAutocorr);
            let (window, lo, hi) = &planner.windows[w];
            let (lo, hi) = (*lo, *hi);
            let len = hi - lo;
            let windowed = &mut planner.windowed;
            windowed.resize(len, 0.0);
            for ((v, &x), &w) in windowed
                .iter_mut()
                .zip(&samples[lo..hi])
                .zip(&window[lo..hi])
            {
                *v = x as f64 * w;
            }
            let orders = max_order.min(len - 1);
            for (lag, energy) in r[..=orders].iter_mut().enumerate() {
                *energy = ctx.dot.apply(&windowed[lag..], &windowed[..len - lag]);
            }
            autocorr.end();
            let _levinson = crate::profile::scope(crate::profile::Stage::EncoderLevinson);
            let (candidates, probe) = (&mut planner.candidates, &planner.probe);
            levinson(&r[..=orders], |a| {
                let Some(model) = Model::quantize(a, profile.precision) else {
                    return;
                };
                let estimate = if sampled {
                    probe.cost(&model, depth, ctx.lpc)
                } else {
                    0
                };
                let mut real = [0.0; MAX_LPC];
                real[..a.len()].copy_from_slice(a);
                if w == 0 {
                    first_highest = Some(candidates.len());
                }
                candidates.push(LpcCandidate {
                    model,
                    real,
                    estimate,
                });
            });
        }
        let _cost = crate::profile::scope(crate::profile::Stage::EncoderLpcCost);
        // Exactly costed candidates: the first window's highest order and
        // the `exact` best others by sampled estimate (ties keep the lower
        // order and earlier window).
        let ranking = &mut planner.ranking;
        ranking.clear();
        if profile.exact != 0 {
            ranking.extend((0..planner.candidates.len()).filter(|&i| Some(i) != first_highest));
        }
        if sampled && ranking.len() > profile.exact {
            let candidates = &planner.candidates;
            ranking.sort_by_key(|&i| candidates[i].estimate);
            ranking.truncate(profile.exact);
        }
        ranking.extend(first_highest);
        let mut scratch = planner.residual();
        let mut winner = None;
        for k in (0..planner.ranking.len()).rev() {
            let index = planner.ranking[k];
            let model = planner.candidates[index].model;
            if Self::cost_lpc(
                samples,
                header,
                &mut best,
                model,
                finest,
                ctx,
                planner,
                &mut scratch,
            ) {
                winner = Some(index);
            }
        }
        // Lower coefficient precisions for the winning model: fewer header
        // bits against coarser prediction.
        if let Some(index) = winner {
            let candidate = &planner.candidates[index];
            let (real, order) = (candidate.real, candidate.model.order);
            for precision in (profile.precision_low..profile.precision).rev() {
                let Some(model) = Model::quantize(&real[..order], precision) else {
                    break;
                };
                Self::cost_lpc(
                    samples,
                    header,
                    &mut best,
                    model,
                    finest,
                    ctx,
                    planner,
                    &mut scratch,
                );
            }
        }
        planner.recycle(scratch);
        debug_assert!(best.depth == depth && best.wasted == wasted);
        best
    }

    /// Cost `model` exactly; if it beats `best` (strictly, or an LPC model
    /// of equal cost and higher order) it replaces it, keeping its residual
    /// in place of `scratch`. Returns whether it did.
    #[allow(clippy::too_many_arguments)]
    fn cost_lpc(
        samples: &[i32],
        header: u64,
        best: &mut Self,
        model: Model,
        finest: u32,
        ctx: &Context<'_>,
        planner: &mut Planner,
        scratch: &mut Vec<u32>,
    ) -> bool {
        let n = samples.len();
        let Some(finest) = predictor_partition_order(n, finest, model.order) else {
            return false;
        };
        let parts = 1usize << finest;
        if !ctx.lpc.partition_sums_into(
            samples,
            model.coefficients(),
            model.shift,
            n >> finest,
            &mut planner.sums[..parts],
            scratch,
        ) {
            return false;
        }
        let rice = choose_rice(&planner.sums, n, model.order, finest);
        let cost = header + model.overhead(best.depth) + rice.bits;
        let replaces = match &best.mode {
            Mode::Lpc { model: held, .. } => {
                cost < best.cost || (cost == best.cost && model.order < held.order)
            }
            _ => cost < best.cost,
        };
        if !replaces {
            return false;
        }
        let residual = std::mem::replace(scratch, planner.residual());
        let replaced = std::mem::replace(
            &mut best.mode,
            Mode::Lpc {
                model: Box::new(model),
                rice: Box::new(rice),
                residual: Some(residual),
            },
        );
        best.cost = cost;
        if let Mode::Lpc {
            residual: Some(buffer),
            ..
        } = replaced
        {
            planner.recycle(buffer);
        }
        true
    }

    /// Return a plan's stored residual buffer without writing it.
    fn release(self, planner: &mut Planner) {
        if let Mode::Lpc {
            residual: Some(residual),
            ..
        } = self.mode
        {
            planner.recycle(residual);
        }
    }
    fn write(
        self,
        bw: &mut BeWriter,
        samples: &[i32],
        ctx: &Context<'_>,
        planner: &mut Planner,
    ) -> Result<()> {
        let depth = self.depth;
        let header = |bw: &mut BeWriter, kind: u64| {
            bw.put(8, (kind << 1) | u64::from(self.wasted != 0));
            if self.wasted != 0 {
                // Unary wasted-bit count minus one: zeros then a one.
                bw.put(self.wasted, 1);
            }
        };
        match self.mode {
            Mode::Constant => {
                bw.put(8, 0);
                bw.put(depth, samples[0] as u64);
            }
            Mode::Verbatim => {
                header(bw, 1);
                for x in samples {
                    bw.put(depth, *x as u64);
                }
            }
            Mode::Fixed { order, rice } => {
                header(bw, 8 + order as u64);
                for x in &samples[..order] {
                    bw.put(depth, *x as u64);
                }
                let mut residual = planner.residual();
                residual.clear();
                residual.reserve(samples.len() - order);
                fixed_residual(samples, order, order, samples.len(), |r| residual.push(r));
                write_residual(bw, &residual, samples.len(), order, &rice, ctx.rice);
                planner.recycle(residual);
            }
            Mode::Lpc {
                model,
                rice,
                residual,
            } => {
                let order = model.order;
                header(bw, 32 + order as u64 - 1);
                for x in &samples[..order] {
                    bw.put(depth, *x as u64);
                }
                bw.put(4, (model.precision - 1) as u64);
                bw.put(5, model.shift as u64);
                for &c in model.coefficients() {
                    bw.put(model.precision, c as u64);
                }
                let residual = match residual {
                    // Stored while costing; every value was range-checked.
                    Some(residual) => residual,
                    None => {
                        let mut residual = planner.residual();
                        // Planning proved every residual of this model fits i32.
                        if ctx
                            .lpc
                            .residual_into(
                                samples,
                                model.coefficients(),
                                model.shift,
                                &mut residual,
                            )
                            .is_none()
                        {
                            return Err(Error::Invalid("planned LPC residual range"));
                        }
                        residual
                    }
                };
                write_residual(bw, &residual, samples.len(), order, &rice, ctx.rice);
                planner.recycle(residual);
            }
        }
        Ok(())
    }
}

/// Write the partitioned residual. Each estimated parameter is refined with
/// exact costs of its neighbours; this changes only compression, and the
/// complete frame is decoded and compared with its source before writing.
fn write_residual(
    bw: &mut BeWriter,
    residual: &[u32],
    n: usize,
    predictor: usize,
    rice: &Rice,
    kernel: &RiceKernel,
) {
    let parts = 1usize << rice.order;
    let size = n >> rice.order;
    let mut params = [0u8; PARTITIONS];
    let mut wide = false;
    let choice = crate::profile::scope(crate::profile::Stage::EncoderRiceChoice);
    for (i, param) in params[..parts].iter_mut().enumerate() {
        let start = (i * size).max(predictor) - predictor;
        let slice = &residual[start..(i + 1) * size - predictor];
        *param = if slice.is_empty() {
            rice.params[i]
        } else {
            kernel.choose(slice, 0).1 as u8
        };
        wide |= *param > 14;
    }
    choice.end();
    let _write = crate::profile::scope(crate::profile::Stage::EncoderRiceWrite);
    bw.put(2, u64::from(wide));
    bw.put(4, rice.order as u64);
    for (i, &param) in params[..parts].iter().enumerate() {
        let start = (i * size).max(predictor) - predictor;
        bw.put(if wide { 5 } else { 4 }, param as u64);
        bw.rice_block(&residual[start..(i + 1) * size - predictor], param as u32);
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

#[cfg(all(test, not(feature = "reference-codecs")))]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = *seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    #[test]
    fn rice_estimate_parameter_is_floor_log2_of_the_mean() {
        let mut seed = 7;
        for _ in 0..200_000 {
            let count = 1 + rng(&mut seed) % 5000;
            let sum = rng(&mut seed) >> (rng(&mut seed) % 64).max(18);
            let mean = sum / count;
            let k = if mean == 0 {
                0
            } else {
                (63 - mean.leading_zeros()).min(30)
            };
            let quotients = (2 * sum).saturating_sub(count * ((1 << k) - 1)) >> (k + 1);
            let bits = if k == 0 {
                count + sum
            } else {
                count * (k as u64 + 1) + quotients
            };
            assert_eq!(rice_estimate(sum, count), (k, bits), "{sum} {count}");
        }
    }

    /// Left-aligned interleaved source blocks covering every subframe type the
    /// encoder emits: constant, verbatim (noise), fixed and LPC (tones),
    /// wasted bits, extremes and all stereo assignments.
    fn blocks(channels: usize, depth: u32, n: usize, seed: &mut u64) -> Vec<Vec<i32>> {
        let shift = 32 - depth;
        let max = (1i64 << (depth - 1)) - 1;
        let mut out = Vec::new();
        for kind in 0..9 {
            let mut block = Vec::with_capacity(n * channels);
            for i in 0..n {
                let t = i as f64 / 48000.0;
                for ch in 0..channels {
                    let tone = (t * std::f64::consts::TAU * (440.0 + 97.0 * ch as f64)).sin();
                    let v: i64 = match kind {
                        0 => 1234,
                        1 => (rng(seed) as i64 >> 20) % max,
                        2 => (tone * 0.4 * max as f64) as i64,
                        3 => ((tone * 0.3 * max as f64) as i64 >> 4) << 4,
                        4 => {
                            if (i / 7) % 2 == 0 {
                                max
                            } else {
                                -max - 1
                            }
                        }
                        5 => (tone * 0.4 * max as f64) as i64 * if ch == 0 { 1 } else { -1 },
                        6 => (tone * 0.4 * max as f64) as i64 + (rng(seed) % 9) as i64,
                        7 => {
                            let left = (tone * 0.5 * max as f64) as i64;
                            if ch == 0 {
                                left
                            } else {
                                left / 2 + (rng(seed) % 64) as i64
                            }
                        }
                        _ => ((t * std::f64::consts::TAU * 3000.0).sin() * 0.9 * max as f64) as i64,
                    };
                    block.push((v.clamp(-max - 1, max) as i32) << shift);
                }
            }
            out.push(block);
        }
        out
    }

    fn decoded_matches(frame: &[u8], spec: &AudioSpec, block: usize, samples: &[i32]) -> bool {
        let mut planes = std::array::from_fn(|_| Vec::new());
        let mut out = Vec::new();
        crate::flac_decode::decode(frame, spec, block, &mut planes, &mut out)
            .is_ok_and(|(length, _)| length == frame.len())
            && out[..] == *samples
    }

    fn verified(
        frame: &[u8],
        spec: &AudioSpec,
        block: usize,
        samples: &[i32],
        scratch: &mut crate::flac_decode::VerifyScratch,
    ) -> bool {
        crate::flac_decode::verify(frame, spec, block, samples, scratch, Backend::detect())
            .is_ok_and(|(length, _)| length == frame.len())
    }

    #[test]
    fn fused_fixed_sums_equal_per_order_sums() {
        let mut seed = 99u64;
        for bits in [16u32, 17, 24, 25] {
            let max = (1i64 << (bits - 1)) - 1;
            for (size, parts) in [(5usize, 1usize), (18, 256), (16, 8), (4608, 1), (37, 3)] {
                let n = size * parts;
                for pattern in 0..3 {
                    let x: Vec<i32> = (0..n)
                        .map(|i| match pattern {
                            0 => (rng(&mut seed) as i64 % (max + 1)) as i32,
                            1 => (if i % 2 == 0 { max } else { -max - 1 }) as i32,
                            _ => ((i as f64 * 0.01).sin() * max as f64) as i32,
                        })
                        .collect();
                    let mut sums = vec![[0u64; PARTITIONS]; 5];
                    fixed_partition_sums(&x, bits, size, parts, &mut sums);
                    for (order, sums) in sums.iter().enumerate() {
                        for (i, &sum) in sums[..parts].iter().enumerate() {
                            let start = (i * size).max(order);
                            assert_eq!(sum, fixed_sum(&x, order, start, (i + 1) * size));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn second_order_sums_equal_fixed_sums() {
        let mut seed = 7u64;
        for depth in [8u32, 16, 17, 20, 22, 24, 25] {
            let max = (1i64 << (depth - 1)) - 1;
            for n in [0usize, 1, 2, 3, 9, 4608, 20000] {
                for pattern in 0..3 {
                    let channels: Vec<Vec<i32>> = (0..4)
                        .map(|c| {
                            (0..n)
                                .map(|i| match pattern {
                                    0 => (rng(&mut seed) as i64 % (max + 1)) as i32,
                                    1 => (if (i + c) % 2 == 0 { max } else { -max - 1 }) as i32,
                                    _ => ((i as f64 * 0.01).sin() * max as f64) as i32,
                                })
                                .collect()
                        })
                        .collect();
                    let x = [&channels[0][..], &channels[1], &channels[2], &channels[3]];
                    let expected = x.map(|x| if n < 3 { 0 } else { fixed_sum(x, 2, 2, n) });
                    assert_eq!(second_order_sums(x, depth), expected, "depth {depth} n {n}");
                }
            }
        }
    }

    #[test]
    fn residual_check_verification_accepts_exactly_what_decoding_reproduces() {
        let dir = std::env::temp_dir().join(format!("audeniq-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut seed = 1729u64;
        let mut scratch = Default::default();
        let mut checked = 0;
        let mut decodable_mismatches = 0;
        // Level 9 at 96 kHz covers 16384-frame blocks, every window and
        // LPC orders up to 32.
        for (channels, depth, level, sample_rate) in [1u16, 2]
            .into_iter()
            .flat_map(|c| [16u16, 24].map(|d| (c, d)))
            .flat_map(|(c, d)| {
                [(0u8, 48000), (3, 48000), (5, 48000), (8, 48000), (9, 96000)]
                    .map(|(l, r)| (c, d, l, r))
            })
        {
            let spec = AudioSpec {
                container: "wav".into(),
                codec: "pcm".into(),
                sample_rate,
                channels,
                bits_per_sample: depth,
                frames: None,
            };
            let profile = Profile::new(level).unwrap();
            let path = dir.join(format!("{channels}-{depth}-{level}.flac"));
            let _ = std::fs::remove_file(&path);
            let file = File::create(&path).unwrap();
            let mut encoder = Encoder::new(
                file,
                spec.clone(),
                Limits::default(),
                Backend::detect(),
                profile,
            )
            .unwrap();
            let block = encoder.profile.block;
            for n in [block, 17, 1] {
                for samples in blocks(channels as usize, depth as u32, n, &mut seed) {
                    encoder.write_block(&samples).unwrap();
                    let frame = encoder.frame_buffer.clone();
                    assert!(decoded_matches(&frame, &spec, block, &samples));
                    assert!(verified(&frame, &spec, block, &samples, &mut scratch));
                    // CRC-repaired bit flips: both must agree on
                    // acceptance (a flip that still decodes to the
                    // source is accepted by both).
                    for _ in 0..48 {
                        let mut bad = frame.clone();
                        let bit = rng(&mut seed) as usize % ((bad.len() - 2) * 8);
                        bad[bit / 8] ^= 0x80 >> (bit % 8);
                        let end = bad.len() - 2;
                        let crc = crate::bits::crc16(&bad[..end]);
                        bad[end..].copy_from_slice(&crc.to_be_bytes());
                        assert_eq!(
                            verified(&bad, &spec, block, &samples, &mut scratch),
                            decoded_matches(&bad, &spec, block, &samples),
                            "bit {bit} channels={channels} depth={depth} level={level}"
                        );
                        checked += 1;
                        // Count flips that parse and decode, but to
                        // different PCM: only the sample check sees them.
                        let mut planes = std::array::from_fn(|_| Vec::new());
                        let mut out = Vec::new();
                        if crate::flac_decode::decode(&bad, &spec, block, &mut planes, &mut out)
                            .is_ok()
                            && out[..] != *samples
                        {
                            decodable_mismatches += 1;
                        }
                    }
                    // Source perturbations: a different sample, or a
                    // set low bit below the declared depth.
                    for delta in [1i32 << (32 - depth), 1] {
                        let mut other = samples.clone();
                        let i = rng(&mut seed) as usize % other.len();
                        other[i] = other[i].wrapping_add(delta);
                        assert!(!verified(&frame, &spec, block, &other, &mut scratch));
                        assert!(!decoded_matches(&frame, &spec, block, &other));
                    }
                    for cut in [0, 1, frame.len() / 2, frame.len() - 1] {
                        assert!(!verified(
                            &frame[..cut],
                            &spec,
                            block,
                            &samples,
                            &mut scratch
                        ));
                    }
                }
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(checked > 10_000);
        assert!(decodable_mismatches > 1_000, "{decodable_mismatches}");
    }
}
