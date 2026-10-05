//! Verified WAV or canonical raw s32le export; input sample values are preserved.
use crate::sha256::Sha256;
use crate::{
    audio::{pcm_sha256, AudioReader},
    kernels::Backend,
    AudioSpec, Error, Limits, Result,
};
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Wav,
    S32le,
}
crate::json_enum!(Format { Wav => "wav", S32le => "s32le" });
pub struct Export {
    pub spec: AudioSpec,
    pub source_spec: AudioSpec,
    pub format: Format,
    pub frames: u64,
    pub pcm_sha256: String,
    pub output_bytes: u64,
}
crate::json_struct!(Export {
    spec,
    source_spec,
    format,
    frames,
    pcm_sha256,
    output_bytes
});
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn decode(
    src: &Path,
    dst: &Path,
    format: Format,
    limits: Limits,
    backend: Backend,
) -> Result<Export> {
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
    let mut file = BufWriter::with_capacity(65536, options.open(&temp.0)?);
    if format == Format::Wav {
        file.write_all(&[0u8; 44])?;
    }
    let mut output_bytes = if format == Format::Wav { 44u64 } else { 0 };
    let mut samples = Vec::new();
    let mut raw = Vec::new();
    let mut canonical = Vec::new();
    let mut hash = crate::pcm_hash::PcmHash::new(spec.bits_per_sample, false);
    while reader.next_hashed(&mut samples, backend, &mut hash)? {
        raw.clear();
        canonical.clear();
        for &x in &samples {
            if format != Format::Wav {
                canonical.extend_from_slice(&x.to_le_bytes());
            } else {
                raw.extend_from_slice(
                    &(x >> (32 - spec.bits_per_sample)).to_le_bytes()
                        [..(spec.bits_per_sample / 8) as usize],
                );
            }
        }
        let bytes = if format == Format::Wav {
            &raw
        } else {
            &canonical
        };
        output_bytes = output_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Limit("PCM output bytes"))?;
        if output_bytes > limits.max_file_bytes
            || (format == Format::Wav && output_bytes > u32::MAX as u64)
        {
            return Err(Error::Limit("PCM output bytes / RIFF size"));
        }
        file.write_all(bytes)?;
    }
    let frames = reader.decoded_frames();
    let source_hash = crate::hex(&hash.finish().0);
    if format == Format::Wav {
        let data_bytes = (output_bytes - 44) as u32;
        if !data_bytes.is_multiple_of(2) {
            file.write_all(&[0])?;
            output_bytes += 1;
        }
        if output_bytes > limits.max_file_bytes || output_bytes > u32::MAX as u64 {
            return Err(Error::Limit("WAV output bytes"));
        }
        file.seek(SeekFrom::Start(0))?;
        let align = spec.channels * (spec.bits_per_sample / 8);
        file.write_all(b"RIFF")?;
        file.write_all(&((output_bytes - 8) as u32).to_le_bytes())?;
        file.write_all(b"WAVEfmt \x10\x00\x00\x00\x01\x00")?;
        file.write_all(&spec.channels.to_le_bytes())?;
        file.write_all(&spec.sample_rate.to_le_bytes())?;
        file.write_all(&(spec.sample_rate * align as u32).to_le_bytes())?;
        file.write_all(&align.to_le_bytes())?;
        file.write_all(&spec.bits_per_sample.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_bytes.to_le_bytes())?;
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    drop(file);
    limits.check()?;
    let output_spec = if format == Format::Wav {
        let (out, hash, count) = pcm_sha256(&temp.0, limits, backend)?;
        if hash != source_hash
            || count != frames
            || out.sample_rate != spec.sample_rate
            || out.channels != spec.channels
            || out.bits_per_sample != spec.bits_per_sample
        {
            return Err(Error::Invalid("WAV round-trip verification"));
        }
        out
    } else {
        let mut file = File::open(&temp.0)?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            limits.check()?;
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if crate::hex(&hash.finalize()) != source_hash
            || output_bytes != frames * spec.channels as u64 * 4
        {
            return Err(Error::Invalid("raw PCM round-trip verification"));
        }
        AudioSpec {
            container: "raw".into(),
            codec: "pcm_s32le".into(),
            bits_per_sample: 32,
            frames: Some(frames),
            ..spec.clone()
        }
    };
    std::fs::hard_link(&temp.0, dst)?;
    Ok(Export {
        spec: output_spec,
        source_spec: spec,
        format,
        frames,
        pcm_sha256: source_hash,
        output_bytes,
    })
}
