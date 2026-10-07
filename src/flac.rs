// SPDX-License-Identifier: LGPL-2.1-or-later
// FLAC fixed predictors, signed Rice mapping and frame writer adapted from
// FFmpeg libavcodec/flacenc.c / lpc.c, Copyright (c) 2006 Justin Ruggles.
// Redesign: bounded 4096-frame work buffers; constant/fixed/verbatim choice;
// exact Rice costs near an estimated parameter; adaptive independent/mid-side;
// streaming MD5/PCM SHA256, verified output and atomic no-clobber publication.
// Own planning kernels (bounded u32/i32 lanes, whole-block LPC residuals),
// branch-free Rice writer and positional frame verification; own Rice cost
// model and exact stereo-assignment trials at levels 7-8; LPC orders up to
// 32, chosen from the Levinson error with one exact residual.
#[cfg(feature = "reference-codecs")]
use crate::audio::pcm_sha256;
use crate::{
    audio::AudioReader,
    bits::{crc16, crc8, BeWriter},
    kernels::{AutocorrKernel, Backend, LpcKernel, RiceKernel, AUTOCORR_PAD},
    AudioSpec, Error, Limits, Result,
};
use std::{
    fs::{File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Which LPC orders are costed exactly after the Levinson recursion.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Search {
    /// Only the order with the lowest estimated size.
    Estimate,
    /// The estimated order and the highest order.
    EstimateAndHighest,
    /// Orders within the span of the estimated one, and the highest. On
    /// the test corpus a span of 2 kept 72% of the gain of costing all 32
    /// orders exactly, for 30% of its CPU.
    Around(usize),
}
/// Fixed predictors of orders 0..=4 are evaluated (in one fused pass) at
/// every level; levels differ in LPC order, partition search and the
/// number of exactly planned stereo assignments.
#[derive(Clone, Copy)]
struct Profile {
    level: u8,
    block: usize,
    lpc: usize,
    partition: u32,
    search: Search,
    stereo_trials: usize,
}
impl Profile {
    fn new(level: u8) -> Result<Self> {
        use Search::*;
        let (lpc, partition, search, stereo_trials) = match level {
            0 => (0, 3, Estimate, 1),
            1 => (0, 6, Estimate, 1),
            2 => (4, 6, Estimate, 1),
            3 => (8, 6, Estimate, 1),
            4 => (12, 6, Estimate, 1),
            5 => (16, 6, Estimate, 1),
            6 => (32, 8, Estimate, 1),
            7 => (32, 8, EstimateAndHighest, 2),
            8 => (32, 8, Around(2), 4),
            _ => return Err(Error::Invalid("compression level range 0..8")),
        };
        Ok(Self {
            level,
            block: 4096,
            lpc,
            partition,
            search,
            stereo_trials,
        })
    }
    /// Keep every level inside the FLAC streamable subset, which hardware
    /// decoders and FFmpeg's default encoder rely on: up to 48 kHz, LPC
    /// orders up to 12 and blocks up to 4608 frames; above, orders up to 32
    /// and blocks up to 16384. Above 48 kHz, levels 6-8 use blocks of 8192
    /// frames (the duration of 4096 at 44.1-48 kHz): 0.10-0.16% smaller
    /// high-resolution files for about 250 KiB more peak RSS, which level 5
    /// does not spend; 16384 saved little more for twice the buffers.
    fn for_rate(self, sample_rate: u32) -> Self {
        if sample_rate <= 48000 {
            Self {
                lpc: self.lpc.min(12),
                ..self
            }
        } else if self.level >= 6 {
            Self {
                block: 8192,
                // Orders to 32 make each exact costing dearer: +-1 around
                // the estimate kept the high-resolution files within
                // 0.001% of +-2 for 20% less CPU (below the order-8 level 8
                // of round 12).
                search: match self.search {
                    Search::Around(_) => Search::Around(1),
                    search => search,
                },
                ..self
            }
        } else {
            self
        }
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
            "adaptive-lpc32-rice-v2"
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
    fixed_sums: FixedSumsFn,
    autocorr: AutocorrKernel,
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
        profile: Profile,
    ) -> Result<Self> {
        file.write_all(b"fLaC\x80\x00\x00\x22")?;
        file.write_all(&[0u8; 34])?;
        let profile = profile.for_rate(spec.sample_rate);
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
            fixed_sums: fixed_sums_kernel(backend),
            autocorr: AutocorrKernel::new(backend),
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
            fixed_sums: self.fixed_sums,
            autocorr: &self.autocorr,
            lpc: &self.lpc,
            rice: &self.rice,
            profile: self.profile,
        };
        // FLAC channel assignment: 1 = L/R, 8 = left/side, 9 = side/right,
        // 10 = mid/side; side carries one extra bit. An order-4 Levinson
        // estimate of each channel ranks the four pairs (see
        // `stereo_estimate`). Levels up to 6 plan only the best-ranked pair;
        // level 7 plans the channels of the two best and level 8 all four
        // channels, and the smallest exact pair wins.
        let stereo = crate::profile::scope(crate::profile::Stage::EncoderStereo);
        // Indices into channel_buffers: left, right, mid, side.
        const PAIRS: [(u64, [usize; 2]); 4] = [(1, [0, 1]), (8, [0, 3]), (9, [3, 1]), (10, [2, 3])];
        // Whole-block lags 0..=STEREO_ESTIMATE_ORDER of each channel, when
        // the stereo estimate computed them.
        let mut priors = [[0.0f64; STEREO_ESTIMATE_ORDER + 1]; 4];
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
            let planner = &mut self.planner;
            let [l, r, m, d] = if self.profile.lpc > 0 && n > 16 {
                // LPC levels: the first lags of the whole block, which
                // planning the chosen channels reuses (see `Plan::new`).
                let lags = STEREO_ESTIMATE_ORDER.min(n - 1);
                let mut index = 0;
                [&*left, &*right, &*mid, &*side].map(|x| {
                    let prior = &mut priors[index];
                    index += 1;
                    let window = std::mem::take(&mut planner.window);
                    planner.window =
                        planner.autocorrelate(x, window, &self.autocorr, &mut prior[..=lags]);
                    levinson_estimate(&prior[..=lags], n, depth)
                })
            } else {
                [&*left, &*right, &*mid, &*side]
                    .map(|x| stereo_estimate(x, depth, &self.autocorr, planner))
            };
            let costs = [l + r, l + d, d + r, m + d];
            let mut ranked = [0, 1, 2, 3];
            // Stable: equal estimates keep the assignment order.
            ranked.sort_by(|&a, &b| costs[a].total_cmp(&costs[b]));
            Some(ranked)
        };
        stereo.end();
        let channel_depth = |index: usize| depth + u32::from(index == 3);
        let reuse = channels == 2 && self.profile.lpc > 0 && n > 16;
        let prior = |index: usize| reuse.then_some(priors[index]);
        let trials = if ranked.is_some() {
            self.profile.stereo_trials
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
                            plans[index] = Some(Plan::new(
                                samples,
                                channel_depth(index),
                                &ctx,
                                planner,
                                prior(index),
                            ));
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
                None => Plan::new(samples, channel_depth(index), &ctx, planner, prior(index)),
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
    fixed_sums: FixedSumsFn,
    autocorr: &'a AutocorrKernel,
    lpc: &'a LpcKernel,
    rice: &'a RiceKernel,
    profile: Profile,
}

/// Quantized LPC coefficient width. On the test corpus 13 bits were within
/// 0.003% of the best fixed width for every format (15, FFmpeg's default,
/// was 0.03% larger at orders 12-16), and they keep 16-bit prediction sums
/// in i32 lanes for frame verification up to order 16.
const LPC_PRECISION: u32 = 13;
const LPC_MAX: f64 = ((1 << (LPC_PRECISION - 1)) - 1) as f64;
/// Highest LPC order FLAC allows; levels 6-8 search up to it.
const MAX_LPC_ORDER: usize = 32;

/// Rice partition orders above this are never searched. With 4096-frame
/// blocks the finest partitions hold 16 residuals.
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
    // Branch-free form of the cases: count == 0 (then sum == 0) and
    // sum < count give k = 0, and k = 0 needs no separate formula
    // (count + sum). floor(log2(floor(sum / count))) is the largest k with
    // count 2^k <= sum, found from the bit lengths without a division.
    let raw = (63 - (sum | 1).leading_zeros()) as i32 - (63 - (count | 1).leading_zeros()) as i32;
    let mut k = raw.max(0) as u32;
    k -= u32::from(raw > 0 && count << k > sum);
    let k = k.min(30);
    let quotients = (2 * sum).saturating_sub(count * ((1 << k) - 1)) >> (k + 1);
    (k, count * (k as u64 + 1) + quotients)
}

/// Largest searched partition order for an n-sample block: partitions must
/// tile the block exactly and each must exceed the predictor order (<= 8).
fn max_partition_order(n: usize, limit: u32) -> u32 {
    // Level 5 (the default) stops at order 6: on the test corpus orders 7
    // and 8 changed 4 of 55 files by 292 bytes in total (1 ppm) while they
    // multiply the partition search (and its sums) by four.
    let mut order = n.trailing_zeros().min(limit);
    while order > 0 && n >> order < 16 {
        order -= 1;
    }
    order
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

/// `fixed_residual(x, order, order, x.len(), ..)` stored into `out`
/// (`x.len() - order` values). Writing through zipped slices instead of a
/// push per value lets the loops vectorize; the arithmetic is the same.
fn fixed_residual_into(x: &[i32], order: usize, out: &mut [u32]) {
    let n = x.len();
    assert_eq!(out.len(), n - order);
    match order {
        0 => out.iter_mut().zip(x).for_each(|(o, &a)| *o = fold(a)),
        1 => out
            .iter_mut()
            .zip(&x[1..])
            .zip(&x[..n - 1])
            .for_each(|((o, &a), &b)| *o = fold(a - b)),
        2 => out
            .iter_mut()
            .zip(&x[2..])
            .zip(&x[1..n - 1])
            .zip(&x[..n - 2])
            .for_each(|(((o, &a), &b), &c)| *o = fold(a - 2 * b + c)),
        3 => out
            .iter_mut()
            .zip(&x[3..])
            .zip(&x[2..n - 1])
            .zip(&x[1..n - 2])
            .zip(&x[..n - 3])
            .for_each(|((((o, &a), &b), &c), &d)| *o = fold(a - 3 * b + 3 * c - d)),
        _ => out
            .iter_mut()
            .zip(&x[4..])
            .zip(&x[3..n - 1])
            .zip(&x[2..n - 2])
            .zip(&x[1..n - 3])
            .zip(&x[..n - 4])
            .for_each(|(((((o, &a), &b), &c), &d), &e)| *o = fold(a - 4 * b + 6 * c - 4 * d + e)),
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
type FixedSumsFn = fn(&[i32], u32, usize, usize, &mut [[u64; PARTITIONS]]);

/// [`fixed_partition_sums`] for the backend: an AVX2 build (eight u32 lanes
/// instead of the baseline's four) on x86; NEON is the AArch64 baseline.
fn fixed_sums_kernel(backend: Backend) -> FixedSumsFn {
    #[cfg(target_arch = "x86_64")]
    if backend == Backend::Avx2 {
        // SAFETY: AVX2 is available when the backend is; the body is safe.
        return |x, depth, size, parts, sums| unsafe {
            fixed_partition_sums_avx2(x, depth, size, parts, sums)
        };
    }
    let _ = backend;
    fixed_partition_sums
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fixed_partition_sums_avx2(
    x: &[i32],
    depth: u32,
    size: usize,
    parts: usize,
    sums: &mut [[u64; PARTITIONS]],
) {
    fixed_partition_sums(x, depth, size, parts, sums)
}

#[inline(always)]
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

/// Order of the Levinson estimate that ranks stereo assignments. On the
/// test corpus orders 2/3/4/8/16 saved 0.09/0.11/0.15/0.16/0.17% at level 5
/// against second-order fixed residual sums (exactly planning the two best
/// pairs saves 0.23% for about 35% more encoder time). Estimating on the
/// central half of the block kept 89% of the order-4 gain at half the cost.
const STEREO_ESTIMATE_ORDER: usize = 4;

/// Estimated bits of one channel for ranking stereo assignments: the
/// smallest Levinson estimate up to `STEREO_ESTIMATE_ORDER` (the per-order
/// formula of LPC planning, on the absolute windowed error so channels
/// compare). Only the ranking matters; the chosen channels are planned
/// exactly. `depth` is the left/right depth; mid and side are estimated
/// alike (side's extra warm-up bit is immaterial for ranking).
fn stereo_estimate(x: &[i32], depth: u32, kernel: &AutocorrKernel, planner: &mut Planner) -> f64 {
    let x = if x.len() >= 64 {
        &x[x.len() / 4..x.len() / 4 + x.len() / 2]
    } else {
        x
    };
    let n = x.len();
    if n < 2 {
        return 0.0;
    }
    let max_order = STEREO_ESTIMATE_ORDER.min(n - 1);
    let mut r = [0.0f64; STEREO_ESTIMATE_ORDER + 1];
    let window = std::mem::take(&mut planner.stereo_window);
    planner.stereo_window = planner.autocorrelate(x, window, kernel, &mut r[..=max_order]);
    levinson_estimate(&r[..=max_order], n, depth)
}

/// The estimate of [`stereo_estimate`] from autocorrelation lags
/// `0..=order` of an `n`-sample window.
fn levinson_estimate(r: &[f64], n: usize, depth: u32) -> f64 {
    let max_order = r.len() - 1;
    // Silence and near-silence have no meaningful logarithm; below about
    // 0.01 per sample every channel costs the same.
    let floor = 1e-2 * n as f64;
    let mut best = 0.5 * n as f64 * r[0].max(floor).log2();
    let mut a = [0.0f64; STEREO_ESTIMATE_ORDER];
    let mut error = r[0];
    for index in 0..max_order.min(STEREO_ESTIMATE_ORDER) {
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
        let estimate = 0.5 * (n - order) as f64 * error.max(floor).log2()
            + (order as u32 * (LPC_PRECISION + depth)) as f64;
        best = best.min(estimate);
    }
    best
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
        coefficients: [i32; MAX_LPC_ORDER],
        order: usize,
        shift: u32,
        rice: Box<Rice>,
        /// Folded residuals stored while the model was costed.
        residual: Vec<u32>,
    },
}
#[derive(Clone)]
struct Plan {
    cost: u64,
    depth: u32,
    wasted: u32,
    mode: Mode,
}
#[derive(Default)]
struct Planner {
    window: Vec<f64>,
    /// Window of the central half block for the stereo estimate.
    stereo_window: Vec<f64>,
    windowed: Vec<f64>,
    residuals: Vec<Vec<u32>>,
    sums: Vec<u64>,
    fixed_sums: Vec<[u64; PARTITIONS]>,
}
impl Planner {
    /// Welch-windowed autocorrelation of `x` for lags `0..r.len()`, with
    /// `window` (recomputed when its length differs) handed back to the
    /// caller's cache.
    fn autocorrelate(
        &mut self,
        x: &[i32],
        window: Vec<f64>,
        kernel: &AutocorrKernel,
        r: &mut [f64],
    ) -> Vec<f64> {
        self.autocorrelate_from(x, window, kernel, 0, r)
    }
    /// [`Self::autocorrelate`] for lags `first..first + r.len()`.
    fn autocorrelate_from(
        &mut self,
        x: &[i32],
        mut window: Vec<f64>,
        kernel: &AutocorrKernel,
        first: usize,
        r: &mut [f64],
    ) -> Vec<f64> {
        let n = x.len();
        if window.len() != n {
            window = (0..n)
                .map(|i| {
                    let d = 2.0 * i as f64 / (n - 1) as f64 - 1.0;
                    1.0 - d * d
                })
                .collect();
        }
        let windowed = &mut self.windowed;
        // Zero padding around the block; the block itself is overwritten.
        windowed.resize(2 * AUTOCORR_PAD + n, 0.0);
        windowed[AUTOCORR_PAD + n..].fill(0.0);
        kernel.window(x, &window, &mut windowed[AUTOCORR_PAD..AUTOCORR_PAD + n]);
        kernel.apply_from(windowed, n, first, r);
        window
    }
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
}
/// Quantize predictor coefficients to `LPC_PRECISION` signed bits with the
/// largest shift that fits them; None when no non-negative shift does.
fn quantize(a: &[f64]) -> Option<([i32; MAX_LPC_ORDER], u32)> {
    let largest = a.iter().fold(0.0f64, |v, x| v.max(x.abs()));
    if largest == 0.0 || largest > LPC_MAX {
        return None;
    }
    let shift = (LPC_MAX / largest).log2().floor().clamp(0.0, 15.0) as u32;
    let mut coefficients = [0i32; MAX_LPC_ORDER];
    // Error-feedback rounding: each coefficient's rounding error is carried
    // into the next (noise-shaped quantization; plain rounding made the
    // corpus 0.25% larger).
    let mut error = 0.0f64;
    for (c, x) in coefficients.iter_mut().zip(a) {
        error += x * (1u32 << shift) as f64;
        let q = error.round().clamp(-LPC_MAX, LPC_MAX);
        *c = q as i32;
        error -= q;
    }
    Some((coefficients, shift))
}
impl Plan {
    /// Plan one subframe. Common trailing zero bits ("wasted bits") are
    /// removed in place first; `write` must receive the same, shifted slice.
    /// `prior`: autocorrelation lags `0..=STEREO_ESTIMATE_ORDER` of the
    /// unshifted channel over the whole block, if already computed.
    fn new(
        samples: &mut [i32],
        depth: u32,
        ctx: &Context<'_>,
        planner: &mut Planner,
        prior: Option<[f64; STEREO_ESTIMATE_ORDER + 1]>,
    ) -> Self {
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
        let fused = size > 4;
        if fused {
            (ctx.fixed_sums)(samples, depth, size, parts, &mut planner.fixed_sums);
        }
        for order in 0..=4.min(n - 1) {
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
        // Welch-tapered autocorrelation and Levinson-Durbin. Integer
        // residual costs decide; no lossy reconstruction.
        fixed_stage.end();
        // The warm-up samples must stay inside the first partition: orders
        // from the finest partition size up search coarser partitions only.
        let max_order = profile.lpc.min(n - 1);
        if n > 16 && max_order > 0 {
            let autocorr = crate::profile::scope(crate::profile::Stage::EncoderAutocorr);
            let mut r = [0.0f64; MAX_LPC_ORDER + 1];
            let mut first = 0;
            if let Some(prior) = prior {
                // Removing wasted bits divides every windowed sample by
                // 2^wasted, so every lag by 4^wasted, exactly in f64.
                let scale = 0.25f64.powi(wasted as i32);
                first = (STEREO_ESTIMATE_ORDER + 1).min(max_order + 1);
                for (r, &p) in r[..first].iter_mut().zip(&prior) {
                    *r = p * scale;
                }
            }
            if first <= max_order {
                let window = std::mem::take(&mut planner.window);
                planner.window = planner.autocorrelate_from(
                    samples,
                    window,
                    ctx.autocorr,
                    first,
                    &mut r[first..=max_order],
                );
            }
            autocorr.end();
            let levinson = crate::profile::scope(crate::profile::Stage::EncoderLevinson);
            // Levinson-Durbin keeps the predictor of every order. A Laplacian
            // residual with prediction error e costs about log2(e) / 2 bits
            // per sample plus a constant, so each order is estimated at
            // (n - order) log2(e) / 2 plus its warm-up samples and
            // coefficients, and only the cheapest estimate is costed exactly
            // (levels 7-8 cost more orders). On the test corpus this
            // compressed as well as exactly costing the highest order and
            // the best of every order's 128-point sampled cost (the former
            // levels 4-5), with one exact residual instead of two.
            let mut a = [0.0f64; MAX_LPC_ORDER];
            let mut models = [[0.0f64; MAX_LPC_ORDER]; MAX_LPC_ORDER + 1];
            let mut estimated = (0, f64::INFINITY);
            let mut error = r[0];
            let mut computed = 0;
            for index in 0..max_order {
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
                models[order] = a;
                computed = order;
                let estimate = (n - order) as f64 * 0.5 * (error / r[0]).max(1e-30).log2()
                    + (order as u32 * (LPC_PRECISION + depth)) as f64;
                // First minimum: ties keep the lower order.
                if estimate < estimated.1 {
                    estimated = (order, estimate);
                }
            }
            levinson.end();
            let _cost = crate::profile::scope(crate::profile::Stage::EncoderLpcCost);
            // Highest order first; iterating downwards, a lower order also
            // replaces an equal-cost LPC model (lowest order among equal
            // costs; LPC must beat fixed/verbatim strictly). The best model
            // keeps its residual for the writer.
            for order in (1..=computed).rev() {
                let costed = match profile.search {
                    Search::Estimate => order == estimated.0,
                    Search::EstimateAndHighest => order == estimated.0 || order == computed,
                    Search::Around(span) => {
                        order.abs_diff(estimated.0) <= span || order == computed
                    }
                };
                if !costed {
                    continue;
                }
                let Some((coefficients, shift)) = quantize(&models[order][..order]) else {
                    continue;
                };
                let mut finest = finest;
                while finest > 0 && n >> finest <= order {
                    finest -= 1;
                }
                if n >> finest <= order {
                    continue;
                }
                let mut residual = planner.residual();
                if !ctx.lpc.partition_sums_into(
                    samples,
                    &coefficients[..order],
                    shift,
                    n >> finest,
                    &mut planner.sums[..1 << finest],
                    &mut residual,
                ) {
                    planner.recycle(residual);
                    continue;
                }
                let rice = choose_rice(&planner.sums, n, order, finest);
                let cost = header
                    + order as u64 * depth as u64
                    + 4
                    + 5
                    + order as u64 * LPC_PRECISION as u64
                    + rice.bits;
                let lpc_best = matches!(best.mode, Mode::Lpc { .. });
                if cost < best.cost || (lpc_best && cost == best.cost) {
                    let replaced = std::mem::replace(
                        &mut best,
                        Self {
                            cost,
                            depth,
                            wasted,
                            mode: Mode::Lpc {
                                coefficients,
                                order,
                                shift,
                                rice: Box::new(rice),
                                residual,
                            },
                        },
                    );
                    replaced.release(planner);
                } else {
                    planner.recycle(residual);
                }
            }
        }
        best
    }
    /// Return a plan's stored residual buffer without writing it.
    fn release(self, planner: &mut Planner) {
        if let Mode::Lpc { residual, .. } = self.mode {
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
                residual.resize(samples.len() - order, 0);
                fixed_residual_into(samples, order, &mut residual);
                write_residual(bw, &residual, samples.len(), order, &rice, ctx.rice);
                planner.recycle(residual);
            }
            Mode::Lpc {
                coefficients,
                order,
                shift,
                rice,
                residual,
            } => {
                header(bw, 32 + order as u64 - 1);
                for x in &samples[..order] {
                    bw.put(depth, *x as u64);
                }
                bw.put(4, (LPC_PRECISION - 1) as u64);
                bw.put(5, shift as u64);
                for &c in &coefficients[..order] {
                    bw.put(LPC_PRECISION, c as u64);
                }
                // Stored while costing; every value was range-checked.
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

    /// Every level stays inside the FLAC streamable subset at every rate.
    #[test]
    fn profiles_stay_in_the_streamable_subset() {
        const { assert!(LPC_PRECISION <= 15) };
        for level in 0..=8 {
            for rate in [8000, 44100, 48000, 48001, 96000, 192000] {
                let p = Profile::new(level).unwrap().for_rate(rate);
                if rate <= 48000 {
                    assert!(p.lpc <= 12 && p.block <= 4608, "level {level} rate {rate}");
                } else {
                    assert!(p.lpc <= 32 && p.block <= 16384, "level {level} rate {rate}");
                }
                assert!(p.partition <= 8);
            }
        }
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
    fn residual_check_verification_accepts_exactly_what_decoding_reproduces() {
        let dir = std::env::temp_dir().join(format!("audeniq-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut seed = 1729u64;
        let mut scratch = Default::default();
        let mut checked = 0;
        let mut decodable_mismatches = 0;
        for channels in [1u16, 2] {
            for depth in [16u16, 24] {
                for level in [0u8, 3, 5, 8] {
                    let spec = AudioSpec {
                        container: "wav".into(),
                        codec: "pcm".into(),
                        sample_rate: 48000,
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
                    for n in [profile.block, 17, 1] {
                        for samples in blocks(channels as usize, depth as u32, n, &mut seed) {
                            encoder.write_block(&samples).unwrap();
                            let frame = encoder.frame_buffer.clone();
                            let block = profile.block;
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
                                if crate::flac_decode::decode(
                                    &bad,
                                    &spec,
                                    block,
                                    &mut planes,
                                    &mut out,
                                )
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
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(checked > 10_000);
        assert!(decodable_mismatches > 1_000, "{decodable_mismatches}");
    }
}
