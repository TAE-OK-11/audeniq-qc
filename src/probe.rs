//! Purpose-specific FFprobe replacement; audio JSON and JPEG/PNG cover checks.
use crate::{audio::AudioReader, Error, Limits, Result};
use crate::{json, json::Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
type Tags = BTreeMap<String, String>;

/// The shared probe boundary used by AUDENIQ for audio and cover images.
pub fn media(path: &Path, limits: Limits) -> Result<Value> {
    limits.check()?;
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(Error::Unsupported("regular local files only"));
    }
    if meta.len() > limits.max_file_bytes {
        return Err(Error::Limit("input bytes"));
    }
    let mut h = [0; 12];
    File::open(path)?.read_exact(&mut h)?;
    if h.starts_with(b"\x89PNG\r\n\x1a\n") || h.starts_with(b"\xff\xd8\xff") {
        cover(path, limits)
    } else {
        audio(path, limits)
    }
}

pub fn audio(path: &Path, limits: Limits) -> Result<Value> {
    let r = AudioReader::open(path, limits.clone())?;
    let s = r.spec;
    let tags = tags(path, &limits)?;
    Ok(
        json!({"engine":crate::ENGINE_VERSION,"fully_decoded":false,"streams":[{"codec_type":"audio","codec_name":s.codec,"sample_rate":s.sample_rate.to_string(),"channels":s.channels,"bits_per_sample":s.bits_per_sample,"bits_per_raw_sample":s.bits_per_sample.to_string(),"disposition":{"attached_pic":0}}],"format":{"format_name":s.container,"duration":s.frames.map(|n|(n as f64/s.sample_rate as f64).to_string()),"tags":tags}}),
    )
}
pub fn cover(path: &Path, limits: Limits) -> Result<Value> {
    limits.check()?;
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(Error::Unsupported("regular local files only"));
    }
    if meta.len() > (64 * 1024 * 1024).min(limits.max_file_bytes) {
        return Err(Error::Limit("cover bytes"));
    }
    let data = std::fs::read(path)?;
    let jpeg = match cover_format(path, &data) {
        None => return Err(Error::Unsupported("cover format")),
        Some(CoverFormat::Jpeg) => true,
        Some(CoverFormat::Png) => false,
        Some(CoverFormat::Other) => return Err(Error::Unsupported("JPEG/PNG covers only")),
    };
    let (width, height) = if jpeg {
        crate::jpeg::validate(&data)?
    } else {
        crate::png::validate(&data)?
    };
    limits.check()?;
    Ok(
        json!({"engine":crate::ENGINE_VERSION,"fully_decoded":true,"streams":[{"codec_type":"video","codec_name":if jpeg{"mjpeg"}else{"png"},"width":width,"height":height}],"format":{"format_name":if jpeg{"jpeg_pipe"}else{"png_pipe"}}}),
    )
}

enum CoverFormat {
    Jpeg,
    Png,
    Other,
}
/// The format the previous image library chose: the first matching magic
/// number among the formats it knew, else the file extension.
fn cover_format(path: &Path, data: &[u8]) -> Option<CoverFormat> {
    const MAGIC: [&[u8]; 23] = [
        b"\x89PNG\r\n\x1a\n",
        &[0xff, 0xd8, 0xff],
        b"GIF89a",
        b"GIF87a",
        b"RIFF",
        b"MM\x00*",
        b"II*\x00",
        b"DDS ",
        b"BM",
        &[0, 0, 1, 0],
        b"#?RADIANCE",
        b"P1",
        b"P2",
        b"P3",
        b"P4",
        b"P5",
        b"P6",
        b"P7",
        b"farbfeld",
        b"\0\0\0 ftypavif",
        b"\0\0\0\x1cftypavif",
        &[0x76, 0x2f, 0x31, 0x01],
        b"qoif",
    ];
    let head = &data[..data.len().min(16)];
    if let Some(i) = MAGIC.iter().position(|m| head.starts_with(m)) {
        return Some(match i {
            0 => CoverFormat::Png,
            1 => CoverFormat::Jpeg,
            _ => CoverFormat::Other,
        });
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => CoverFormat::Jpeg,
        "png" => CoverFormat::Png,
        "avif" | "gif" | "webp" | "tif" | "tiff" | "tga" | "dds" | "bmp" | "ico" | "hdr"
        | "exr" | "pbm" | "pam" | "ppm" | "pgm" | "ff" | "farbfeld" | "qoi" => CoverFormat::Other,
        _ => return None,
    })
}
fn insert(tags: &mut Tags, key: &str, value: &[u8]) -> Result<()> {
    if value.len() > 65536 || tags.len() >= 128 {
        return Err(Error::Limit("tag bytes/count"));
    }
    let key = key.to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "encoder"
            | "encoded_by"
            | "software"
            | "writing_library"
            | "creator_tool"
            | "comment"
            | "description"
    ) {
        let text = String::from_utf8_lossy(value)
            .trim_end_matches('\0')
            .to_owned();
        tags.insert(key, text);
    }
    Ok(())
}
pub fn tags(path: &Path, limits: &Limits) -> Result<Tags> {
    limits.check()?;
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if !f.metadata()?.is_file() {
        return Err(Error::Unsupported("regular local files only"));
    }
    if len > limits.max_file_bytes {
        return Err(Error::Limit("input bytes"));
    }
    let mut h = [0u8; 12];
    f.read_exact(&mut h)?;
    let mut tags = Tags::new();
    if &h[..4] == b"fLaC" {
        f.seek(SeekFrom::Start(4))?;
        for _ in 0..4096 {
            limits.check()?;
            let mut h = [0u8; 4];
            f.read_exact(&mut h)?;
            let n = (h[1] as u64) << 16 | (h[2] as u64) << 8 | h[3] as u64;
            if n > limits.max_packet_bytes as u64 {
                return Err(Error::Limit("FLAC metadata"));
            }
            if h[0] & 127 == 4 {
                let mut b = vec![0; n as usize];
                f.read_exact(&mut b)?;
                vorbis(&b, &mut tags)?;
            } else {
                f.seek(SeekFrom::Current(n as i64))?;
            }
            if h[0] & 128 != 0 {
                return Ok(tags);
            }
        }
        return Err(Error::Limit("FLAC metadata blocks"));
    }
    if &h[..4] == b"RIFF" || &h[..4] == b"RF64" || &h[..4] == b"BW64" {
        let mut pos = 12u64;
        for _ in 0..4096 {
            limits.check()?;
            if pos + 8 > len {
                break;
            }
            f.seek(SeekFrom::Start(pos))?;
            let mut h = [0u8; 8];
            f.read_exact(&mut h)?;
            let n = u32::from_le_bytes(h[4..].try_into().unwrap()) as u64;
            if n > len - pos - 8 {
                break;
            }
            if &h[..4] == b"LIST" && n <= limits.max_packet_bytes as u64 {
                let mut b = vec![0; n as usize];
                f.read_exact(&mut b)?;
                if b.starts_with(b"INFO") {
                    let mut i = 4;
                    while i + 8 <= b.len() {
                        let size = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
                        let body = i + 8;
                        if size > b.len() - body {
                            return Err(Error::Invalid("RIFF tag"));
                        }
                        let key = match &b[i..i + 4] {
                            b"ISFT" => "software",
                            b"ICMT" => "comment",
                            _ => "",
                        };
                        insert(&mut tags, key, &b[body..body + size])?;
                        i = body + size + (size & 1);
                    }
                }
            }
            pos += 8 + n + (n & 1);
        }
        return Ok(tags);
    }
    if &h[..4] == b"wvpk" || &h[..4] == b"TTA1" {
        let mut end = len;
        if end >= 128 {
            f.seek(SeekFrom::Start(end - 128))?;
            let mut b = [0; 3];
            f.read_exact(&mut b)?;
            if &b == b"TAG" {
                end -= 128;
            }
        }
        if end < 32 {
            return Ok(tags);
        }
        f.seek(SeekFrom::Start(end - 32))?;
        let mut footer = [0; 32];
        f.read_exact(&mut footer)?;
        if &footer[..8] != b"APETAGEX" {
            return Ok(tags);
        }
        let size = u32::from_le_bytes(footer[12..16].try_into().unwrap()) as usize;
        let count = u32::from_le_bytes(footer[16..20].try_into().unwrap());
        if size < 32 || size as u64 > end || size > limits.max_packet_bytes || count > 128 {
            return Err(Error::Limit("APE tags"));
        }
        f.seek(SeekFrom::Start(end - size as u64))?;
        let mut b = vec![0; size - 32];
        f.read_exact(&mut b)?;
        let mut i = 0;
        for _ in 0..count {
            limits.check()?;
            if b.len() - i < 8 {
                return Err(Error::Invalid("APE entry"));
            }
            let n = u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
            let flags = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap());
            i += 8;
            let z = b[i..]
                .iter()
                .position(|x| *x == 0)
                .ok_or(Error::Invalid("APE key"))?;
            let key = String::from_utf8_lossy(&b[i..i + z]).into_owned();
            i += z + 1;
            if n > b.len() - i {
                return Err(Error::Invalid("APE value"));
            }
            if flags & 6 == 0 {
                insert(&mut tags, &key, &b[i..i + n])?;
            }
            i += n;
        }
        return Ok(tags);
    }
    if &h[..4] == b"FORM" && matches!(&h[8..12], b"AIFF" | b"AIFC") {
        let mut pos = 12u64;
        for _ in 0..4096 {
            limits.check()?;
            if pos + 8 > len {
                break;
            }
            f.seek(SeekFrom::Start(pos))?;
            let mut header = [0; 8];
            f.read_exact(&mut header)?;
            let n = u32::from_be_bytes(header[4..].try_into().unwrap()) as u64;
            if n > len - pos - 8 {
                return Err(Error::Invalid("AIFF tag chunk"));
            }
            if &header[..4] == b"ANNO" {
                if n > 65536 {
                    return Err(Error::Limit("AIFF annotation"));
                }
                let mut b = vec![0; n as usize];
                f.read_exact(&mut b)?;
                insert(&mut tags, "comment", &b)?;
            }
            pos += 8 + n + (n & 1);
        }
        return Ok(tags);
    }
    if &h[4..8] == b"ftyp" {
        for (key, value) in crate::m4a::tags(&mut f, limits)? {
            insert(&mut tags, &key, &value)?;
        }
        return Ok(tags);
    }
    Ok(tags)
}
fn vorbis(b: &[u8], tags: &mut Tags) -> Result<()> {
    let mut i = 0;
    let vendor = take_le(b, &mut i)?;
    insert(tags, "encoder", vendor)?;
    if b.len() - i < 4 {
        return Err(Error::Invalid("Vorbis comment count"));
    }
    let count = u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    i += 4;
    if count > 128 {
        return Err(Error::Limit("Vorbis comments"));
    }
    for _ in 0..count {
        let v = take_le(b, &mut i)?;
        if let Some(p) = v.iter().position(|x| *x == b'=') {
            let raw = String::from_utf8_lossy(&v[..p]);
            let key = if raw.eq_ignore_ascii_case("description") {
                "comment"
            } else {
                &raw
            };
            insert(tags, key, &v[p + 1..])?;
        }
    }
    Ok(())
}
fn take_le<'a>(b: &'a [u8], i: &mut usize) -> Result<&'a [u8]> {
    if b.len() - *i < 4 {
        return Err(Error::Invalid("metadata length"));
    }
    let n = u32::from_le_bytes(b[*i..*i + 4].try_into().unwrap()) as usize;
    *i += 4;
    if n > b.len() - *i {
        return Err(Error::Invalid("metadata value"));
    }
    let out = &b[*i..*i + n];
    *i += n;
    Ok(out)
}
