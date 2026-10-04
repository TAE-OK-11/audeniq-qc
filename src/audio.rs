use crate::{kernels::Backend, AudioSpec, Error, Limits, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{Decoder, DecoderOptions, CODEC_TYPE_ALAC, CODEC_TYPE_FLAC},
    formats::{FormatOptions, FormatReader},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

enum Source {
    Pcm(Pcm),
    Compressed(Compressed),
    Tta(crate::tta::Tta),
    Wavpack(crate::wavpack::Wavpack),
}
pub struct AudioReader {
    pub spec: AudioSpec,
    source: Source,
    limits: Limits,
    decoded: u64,
}
impl AudioReader {
    pub fn open(path: &Path, limits: Limits) -> Result<Self> {
        limits.check()?;
        let mut f = File::open(path)?;
        let meta = f.metadata()?;
        if !meta.is_file() {
            return Err(Error::Unsupported("regular local files only"));
        }
        if meta.len() > limits.max_file_bytes {
            return Err(Error::Limit("input bytes"));
        }
        let mut magic = [0u8; 12];
        f.read_exact(&mut magic)?;
        f.rewind()?;
        let (spec, source) =
            if &magic[..4] == b"RIFF" || &magic[..4] == b"RF64" || &magic[..4] == b"BW64" {
                let p = Pcm::wav(f, &limits)?;
                (p.spec.clone(), Source::Pcm(p))
            } else if &magic[..4] == b"FORM" {
                let p = Pcm::aiff(f, &limits)?;
                (p.spec.clone(), Source::Pcm(p))
            } else if &magic[..4] == b"TTA1" {
                let p = crate::tta::Tta::open(f, &limits)?;
                (p.spec.clone(), Source::Tta(p))
            } else if &magic[..4] == b"wvpk" {
                let p = crate::wavpack::Wavpack::open(f, &limits)?;
                (p.spec.clone(), Source::Wavpack(p))
            } else if &magic[..4] == b"fLaC" || &magic[4..8] == b"ftyp" {
                let p = Compressed::open(f, &limits, &magic[..4] == b"fLaC")?;
                (p.spec.clone(), Source::Compressed(p))
            } else {
                return Err(Error::Unsupported(
                    "accepted containers: WAV/RF64/BW64, AIFF/AIFC, FLAC, ALAC M4A, TTA, WavPack",
                ));
            };
        spec.validate()?;
        if spec.frames.is_some_and(|n| n > limits.max_frames) {
            return Err(Error::Limit("declared sample frames"));
        }
        Ok(Self {
            spec,
            source,
            limits,
            decoded: 0,
        })
    }
    /// Interleaved signed left-aligned s32, matching FFmpeg pcm_s32le hashing.
    /// Reuses caller storage; empty means EOF. Never returns partial PASS.
    pub fn next(&mut self, samples: &mut Vec<i32>, backend: Backend) -> Result<bool> {
        self.limits.check()?;
        match &mut self.source {
            Source::Pcm(p) => p.next(samples, backend)?,
            Source::Compressed(p) => p.next(samples, &self.limits)?,
            Source::Tta(p) => p.next(samples, &self.limits)?,
            Source::Wavpack(p) => p.next(samples, &self.limits)?,
        }
        if samples.len() % self.spec.channels as usize != 0 {
            return Err(Error::Invalid("partial sample frame"));
        }
        self.decoded += (samples.len() / self.spec.channels as usize) as u64;
        if self.decoded > self.limits.max_frames {
            return Err(Error::Limit("decoded frames"));
        }
        if let Some(n) = self.spec.frames {
            if self.decoded > n || (samples.is_empty() && self.decoded != n) {
                return Err(Error::Invalid("decoded count differs from container"));
            }
        }
        if samples.is_empty() && self.decoded == 0 {
            return Err(Error::Invalid("empty audio"));
        }
        Ok(!samples.is_empty())
    }
    pub fn decoded_frames(&self) -> u64 {
        self.decoded
    }
}

struct Pcm {
    file: File,
    spec: AudioSpec,
    left: u64,
    big_endian: bool,
    bytes: Vec<u8>,
}
fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn u32be(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
impl Pcm {
    fn wav(mut f: File, limits: &Limits) -> Result<Self> {
        let len = f.metadata()?.len();
        let mut h = [0u8; 12];
        f.read_exact(&mut h)?;
        if &h[8..] != b"WAVE" {
            return Err(Error::Invalid("WAV signature"));
        }
        let rf = &h[..4] != b"RIFF";
        let mut end = if rf { len } else { u32le(&h[4..]) as u64 + 8 };
        if end > len || end < 12 {
            return Err(Error::Invalid("truncated RIFF"));
        }
        let mut fmt = None;
        let mut data = None;
        let mut ds64 = None;
        let mut pos = 12;
        for _ in 0..limits.max_chunks {
            limits.check()?;
            if pos == end {
                break;
            }
            if pos + 8 > end {
                return Err(Error::Invalid("trailing RIFF chunk"));
            }
            f.seek(SeekFrom::Start(pos))?;
            let mut ch = [0u8; 8];
            f.read_exact(&mut ch)?;
            let mut size = u32le(&ch[4..]) as u64;
            let body = pos + 8;
            if size == u32::MAX as u64 {
                if &ch[..4] != b"data" {
                    return Err(Error::Unsupported("RF64 ds64 table chunks"));
                }
                size = ds64.ok_or(Error::Invalid("missing ds64"))?;
            }
            if size > end - body {
                return Err(Error::Invalid("truncated WAV chunk"));
            }
            match &ch[..4] {
                b"ds64" => {
                    if !rf || size < 28 || ds64.is_some() {
                        return Err(Error::Invalid("ds64"));
                    }
                    let mut d = [0u8; 28];
                    f.read_exact(&mut d)?;
                    end = u64::from_le_bytes(d[..8].try_into().unwrap())
                        .checked_add(8)
                        .ok_or(Error::Invalid("RF64 size overflow"))?;
                    if end > len || end < body + size {
                        return Err(Error::Invalid("RF64 length"));
                    }
                    ds64 = Some(u64::from_le_bytes(d[8..16].try_into().unwrap()));
                    if u32le(&d[24..]) != 0 {
                        return Err(Error::Unsupported("RF64 size table"));
                    }
                }
                b"fmt " => {
                    if fmt.is_some() || size < 16 {
                        return Err(Error::Invalid("WAV fmt"));
                    }
                    let mut d = [0u8; 40];
                    let n = (size as usize).min(40);
                    f.read_exact(&mut d[..n])?;
                    let tag = u16le(&d);
                    let channels = u16le(&d[2..]);
                    let rate = u32le(&d[4..]);
                    let depth = u16le(&d[14..]);
                    if tag == 0xfffe {
                        if size < 40
                            || u16le(&d[16..]) < 22
                            || u16le(&d[18..]) != depth
                            || d[24..40]
                                != [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]
                        {
                            return Err(Error::Unsupported("non-integer/extensible WAV"));
                        }
                        let mask = u32le(&d[20..]);
                        if mask != 0 && mask != if channels == 1 { 4 } else { 3 } {
                            return Err(Error::Unsupported("channel mask"));
                        }
                    } else if tag != 1 {
                        return Err(Error::Unsupported("only integer PCM WAV"));
                    }
                    let s = AudioSpec {
                        container: "wav".into(),
                        codec: format!("pcm_s{depth}le"),
                        sample_rate: rate,
                        channels,
                        bits_per_sample: depth,
                        frames: None,
                    };
                    s.validate()?;
                    let align = channels as u32 * (depth / 8) as u32;
                    if u16le(&d[12..]) as u32 != align || u32le(&d[8..]) != rate * align {
                        return Err(Error::Invalid("inconsistent WAV alignment/rate"));
                    }
                    fmt = Some(s);
                }
                b"data" => {
                    if data.is_some() {
                        return Err(Error::Unsupported("multiple WAV data chunks"));
                    }
                    data = Some((body, size));
                }
                _ => (),
            }
            pos = body + size + (size & 1);
            if pos > end {
                return Err(Error::Invalid("missing chunk pad"));
            }
        }
        if pos != end || (rf && ds64.is_none()) {
            return Err(Error::Limit("chunk walk / missing ds64"));
        }
        let mut spec = fmt.ok_or(Error::Invalid("missing fmt"))?;
        let (start, size) = data.ok_or(Error::Invalid("missing audio data"))?;
        let align = spec.channels as u64 * (spec.bits_per_sample / 8) as u64;
        if size == 0 || size % align != 0 {
            return Err(Error::Invalid("partial/empty PCM frame"));
        }
        spec.frames = Some(size / align);
        f.seek(SeekFrom::Start(start))?;
        Ok(Self {
            file: f,
            spec,
            left: size,
            big_endian: false,
            bytes: vec![0; 8192 * align as usize],
        })
    }
    fn aiff(mut f: File, limits: &Limits) -> Result<Self> {
        let len = f.metadata()?.len();
        let mut h = [0u8; 12];
        f.read_exact(&mut h)?;
        let aifc = &h[8..] == b"AIFC";
        if !aifc && &h[8..] != b"AIFF" {
            return Err(Error::Invalid("AIFF signature"));
        }
        let end = u32be(&h[4..]) as u64 + 8;
        if end > len || end < 12 {
            return Err(Error::Invalid("truncated AIFF"));
        }
        let mut pos = 12;
        let mut fmt = None;
        let mut data = None;
        let mut little = false;
        for _ in 0..limits.max_chunks {
            limits.check()?;
            if pos == end {
                break;
            }
            if pos + 8 > end {
                return Err(Error::Invalid("AIFF chunk"));
            }
            f.seek(SeekFrom::Start(pos))?;
            let mut ch = [0u8; 8];
            f.read_exact(&mut ch)?;
            let size = u32be(&ch[4..]) as u64;
            let body = pos + 8;
            if size > end - body {
                return Err(Error::Invalid("truncated AIFF chunk"));
            }
            match &ch[..4] {
                b"COMM" => {
                    if fmt.is_some() || size < if aifc { 22 } else { 18 } {
                        return Err(Error::Invalid("AIFF COMM"));
                    }
                    let mut d = [0u8; 22];
                    f.read_exact(&mut d[..if aifc { 22 } else { 18 }])?;
                    let exponent = u16::from_be_bytes(d[8..10].try_into().unwrap());
                    let mantissa = u64::from_be_bytes(d[10..18].try_into().unwrap());
                    if exponent & 0x8000 != 0 || exponent == 0x7fff || mantissa & (1 << 63) == 0 {
                        return Err(Error::Invalid("AIFF rate"));
                    }
                    let rate = (mantissa as f64) * 2f64.powi(exponent as i32 - 16383 - 63);
                    if !rate.is_finite() || rate.fract() != 0.0 || rate > 192000.0 {
                        return Err(Error::Invalid("AIFF sample rate"));
                    }
                    if aifc {
                        match &d[18..22] {
                            b"NONE" | b"twos" => (),
                            b"sowt" => little = true,
                            _ => return Err(Error::Unsupported("compressed AIFF")),
                        }
                    }
                    let depth = u16::from_be_bytes(d[6..8].try_into().unwrap());
                    let s = AudioSpec {
                        container: "aiff".into(),
                        codec: format!("pcm_s{depth}{}", if little { "le" } else { "be" }),
                        sample_rate: rate as u32,
                        channels: u16::from_be_bytes(d[..2].try_into().unwrap()),
                        bits_per_sample: depth,
                        frames: Some(u32be(&d[2..]) as u64),
                    };
                    s.validate()?;
                    fmt = Some(s);
                }
                b"SSND" => {
                    if data.is_some() || size < 8 {
                        return Err(Error::Invalid("AIFF SSND"));
                    }
                    let mut d = [0u8; 8];
                    f.read_exact(&mut d)?;
                    let offset = u32be(&d) as u64;
                    if offset > size - 8 {
                        return Err(Error::Invalid("AIFF offset"));
                    }
                    data = Some((body + 8 + offset, size - 8 - offset));
                }
                _ => (),
            }
            pos = body + size + (size & 1);
            if pos > end {
                return Err(Error::Invalid("AIFF pad"));
            }
        }
        if pos != end {
            return Err(Error::Limit("AIFF chunk count"));
        }
        let spec = fmt.ok_or(Error::Invalid("AIFF missing COMM"))?;
        let (start, size) = data.ok_or(Error::Invalid("AIFF missing SSND"))?;
        if size != spec.frames.unwrap() * spec.channels as u64 * (spec.bits_per_sample / 8) as u64 {
            return Err(Error::Invalid("AIFF count mismatch"));
        }
        f.seek(SeekFrom::Start(start))?;
        let align = spec.channels as usize * (spec.bits_per_sample / 8) as usize;
        Ok(Self {
            file: f,
            spec,
            left: size,
            big_endian: !little,
            bytes: vec![0; 8192 * align],
        })
    }
    fn next(&mut self, out: &mut Vec<i32>, backend: Backend) -> Result<()> {
        let n = self.left.min(self.bytes.len() as u64) as usize;
        out.resize(n / (self.spec.bits_per_sample / 8) as usize, 0);
        self.file.read_exact(&mut self.bytes[..n])?;
        self.left -= n as u64;
        if !self.big_endian {
            crate::kernels::pcm_le(&self.bytes[..n], self.spec.bits_per_sample, out, backend);
        } else if self.spec.bits_per_sample == 16 {
            for (s, b) in out.iter_mut().zip(self.bytes[..n].chunks_exact(2)) {
                *s = (i16::from_be_bytes([b[0], b[1]]) as i32) * 65536;
            }
        } else {
            for (s, b) in out.iter_mut().zip(self.bytes[..n].chunks_exact(3)) {
                *s = i32::from_be_bytes([b[0], b[1], b[2], 0]);
            }
        }
        Ok(())
    }
}

struct Compressed {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    spec: AudioSpec,
    track: u32,
    buffer: Option<SampleBuffer<i32>>,
}
fn decode_err(e: symphonia::core::errors::Error) -> Error {
    match e {
        symphonia::core::errors::Error::IoError(e) => Error::Io(e),
        _ => Error::Invalid("compressed codec/container decode"),
    }
}
impl Compressed {
    fn open(mut f: File, limits: &Limits, is_flac: bool) -> Result<Self> {
        if !is_flac {
            crate::mp4::preflight(&mut f, limits)?;
        }
        let mss = MediaSourceStream::new(Box::new(f), Default::default());
        let probed = symphonia::default::get_probe()
            .format(
                &Hint::new(),
                mss,
                &FormatOptions {
                    enable_gapless: false,
                    ..Default::default()
                },
                &MetadataOptions {
                    limit_metadata_bytes: symphonia::core::meta::Limit::Maximum(
                        limits.max_packet_bytes,
                    ),
                    limit_visual_bytes: symphonia::core::meta::Limit::Maximum(
                        limits.max_packet_bytes,
                    ),
                },
            )
            .map_err(decode_err)?;
        let format = probed.format;
        let audio: Vec<_> = format
            .tracks()
            .iter()
            .filter(|t| t.codec_params.sample_rate.is_some())
            .collect();
        if audio.len() != 1 {
            return Err(Error::Unsupported("exactly one audio track required"));
        }
        let t = audio[0];
        let p = &t.codec_params;
        if p.codec
            != if is_flac {
                CODEC_TYPE_FLAC
            } else {
                CODEC_TYPE_ALAC
            }
        {
            return Err(Error::Unsupported("lossless ALAC/FLAC only"));
        }
        // Container readers must not silently select an audio stream from a movie.
        if format.tracks().len() != 1 {
            return Err(Error::Unsupported("additional tracks"));
        }
        let bits = p
            .bits_per_sample
            .or(p.bits_per_coded_sample)
            .or_else(|| {
                p.extra_data.as_ref().and_then(|d| {
                    if p.codec == CODEC_TYPE_ALAC {
                        d.get(5).map(|v| *v as u32)
                    } else {
                        None
                    }
                })
            })
            .ok_or(Error::Invalid("missing bit depth"))?;
        let channels = p
            .channels
            .map(|c| c.count() as u16)
            .or_else(|| {
                p.extra_data.as_deref().and_then(|d| {
                    if p.codec == CODEC_TYPE_ALAC {
                        d.get(9).map(|c| *c as u16)
                    } else {
                        None
                    }
                })
            })
            .ok_or(Error::Invalid("channels"))?;
        let rate = if p.codec == CODEC_TYPE_ALAC {
            p.extra_data
                .as_deref()
                .and_then(|d| d.get(20..24))
                .map(|d| u32::from_be_bytes(d.try_into().unwrap()))
                .ok_or(Error::Invalid("ALAC sample rate"))?
        } else {
            p.sample_rate.ok_or(Error::Invalid("sample rate"))?
        };
        let spec = AudioSpec {
            container: if is_flac { "flac" } else { "mov" }.into(),
            codec: if is_flac { "flac" } else { "alac" }.into(),
            sample_rate: rate,
            channels,
            bits_per_sample: bits as u16,
            frames: p.n_frames,
        };
        spec.validate()?;
        if p.codec == CODEC_TYPE_ALAC {
            let d = p
                .extra_data
                .as_deref()
                .ok_or(Error::Invalid("ALAC config"))?;
            if !matches!(d.len(), 24 | 48) {
                return Err(Error::Invalid("ALAC config length"));
            }
            let frames = u32::from_be_bytes(d[..4].try_into().unwrap());
            if frames == 0
                || frames > 150_000
                || d[9] != spec.channels as u8
                || d[5] != spec.bits_per_sample as u8
            {
                return Err(Error::Limit("ALAC block configuration"));
            }
        }
        let decoder = symphonia::default::get_codecs()
            .make(p, &DecoderOptions { verify: true })
            .map_err(decode_err)?;
        let track = t.id;
        Ok(Self {
            format,
            decoder,
            spec,
            track,
            buffer: None,
        })
    }
    fn next(&mut self, out: &mut Vec<i32>, limits: &Limits) -> Result<()> {
        out.clear();
        let packet = match self.format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                if self.decoder.finalize().verify_ok == Some(false) {
                    return Err(Error::Invalid("FLAC MD5 mismatch"));
                }
                return Ok(());
            }
            Err(e) => return Err(decode_err(e)),
        };
        if packet.track_id() != self.track
            || packet.data.len() > limits.max_packet_bytes
            || packet.dur() > 150_000
        {
            return Err(Error::Limit("compressed packet"));
        }
        let decoded = self.decoder.decode(&packet).map_err(decode_err)?;
        if decoded.spec().rate != self.spec.sample_rate
            || decoded.spec().channels.count() != self.spec.channels as usize
            || decoded.capacity() > 150_000
        {
            return Err(Error::Invalid("midstream format change / oversized block"));
        }
        if self.buffer.as_ref().map_or(true, |b| {
            b.capacity() < decoded.capacity() * self.spec.channels as usize
        }) {
            self.buffer = Some(SampleBuffer::<i32>::new(
                decoded.capacity() as u64,
                *decoded.spec(),
            ));
        }
        let b = self.buffer.as_mut().unwrap();
        b.copy_interleaved_ref(decoded);
        out.extend_from_slice(b.samples());
        Ok(())
    }
}

pub fn pcm_sha256(
    path: &Path,
    limits: Limits,
    backend: Backend,
) -> Result<(AudioSpec, String, u64)> {
    use sha2::{Digest, Sha256};
    let mut r = AudioReader::open(path, limits)?;
    let mut s = Vec::new();
    let mut h = Sha256::new();
    let mut bytes = Vec::new();
    while r.next(&mut s, backend)? {
        bytes.clear();
        for x in &s {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        h.update(&bytes);
    }
    let n = r.decoded_frames();
    Ok((r.spec, crate::hex(&h.finalize()), n))
}
