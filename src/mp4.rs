//! Reject hostile atom/table lengths before a dependency can allocate from them.
use crate::{Error, Limits, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
pub fn preflight(file: &mut File, limits: &Limits) -> Result<()> {
    let len = file.metadata()?.len();
    let mut budget = 4096;
    walk(file, 0, len, 0, &mut budget, limits)?;
    file.rewind()?;
    Ok(())
}
fn walk(
    f: &mut File,
    start: u64,
    end: u64,
    depth: u32,
    budget: &mut u32,
    limits: &Limits,
) -> Result<()> {
    if depth > 16 {
        return Err(Error::Limit("MP4 nesting"));
    }
    let mut pos = start;
    while pos < end {
        limits.check()?;
        if *budget == 0 {
            return Err(Error::Limit("MP4 atoms"));
        }
        *budget -= 1;
        if end - pos < 8 {
            return Err(Error::Invalid("MP4 atom header"));
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 16];
        f.read_exact(&mut h[..8])?;
        let raw = u32::from_be_bytes(h[..4].try_into().unwrap());
        let header = if raw == 1 { 16 } else { 8 };
        let size = if raw == 0 {
            end - pos
        } else if raw == 1 {
            if end - pos < 16 {
                return Err(Error::Invalid("MP4 extended atom"));
            }
            f.read_exact(&mut h[8..])?;
            u64::from_be_bytes(h[8..].try_into().unwrap())
        } else {
            raw as u64
        };
        if size < header || size > end - pos {
            return Err(Error::Invalid("MP4 atom length"));
        }
        let body = pos + header;
        let bytes = size - header;
        let kind = &h[4..8];
        match kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" => {
                if size > limits.max_packet_bytes as u64 {
                    return Err(Error::Limit("MP4 metadata tree"));
                }
                walk(f, body, pos + size, depth + 1, budget, limits)?;
            }
            b"meta" => {
                if bytes < 4 {
                    return Err(Error::Invalid("MP4 meta"));
                }
                walk(f, body + 4, pos + size, depth + 1, budget, limits)?;
            }
            b"stco" | b"co64" | b"stts" | b"ctts" | b"stsc" | b"stss" => {
                if bytes < 8 {
                    return Err(Error::Invalid("MP4 table header"));
                }
                let mut b = [0u8; 8];
                f.seek(SeekFrom::Start(body))?;
                f.read_exact(&mut b)?;
                let n = u32::from_be_bytes(b[4..8].try_into().unwrap()) as u64;
                let unit = match kind {
                    b"co64" | b"stts" | b"ctts" => 8,
                    b"stsc" => 12,
                    _ => 4,
                };
                if n > limits.max_chunks as u64 || n * unit > bytes - 8 {
                    return Err(Error::Limit("MP4 table entries"));
                }
            }
            b"stsz" => {
                if bytes < 12 {
                    return Err(Error::Invalid("MP4 sample table"));
                }
                let mut b = [0u8; 12];
                f.seek(SeekFrom::Start(body))?;
                f.read_exact(&mut b)?;
                let sz = u32::from_be_bytes(b[4..8].try_into().unwrap());
                let n = u32::from_be_bytes(b[8..12].try_into().unwrap()) as u64;
                if n > limits.max_chunks as u64
                    || (sz == 0 && n * 4 > bytes - 12)
                    || sz as usize > limits.max_packet_bytes
                {
                    return Err(Error::Limit("MP4 samples"));
                }
                if sz == 0 {
                    for _ in 0..n {
                        let mut b = [0u8; 4];
                        f.read_exact(&mut b)?;
                        if u32::from_be_bytes(b) as usize > limits.max_packet_bytes {
                            return Err(Error::Limit("MP4 packet bytes"));
                        }
                    }
                }
            }
            b"stsd" => {
                if bytes < 8 || bytes > limits.max_packet_bytes as u64 {
                    return Err(Error::Limit("MP4 sample description"));
                }
                f.seek(SeekFrom::Start(body + 4))?;
                let mut b = [0; 4];
                f.read_exact(&mut b)?;
                if u32::from_be_bytes(b) != 1 {
                    return Err(Error::Unsupported("multiple MP4 codec descriptions"));
                }
            }
            b"moof" | b"mvex" | b"stz2" => {
                return Err(Error::Unsupported("fragmented/compact MP4"))
            }
            b"mdat" | b"free" | b"skip" => (),
            _ if bytes > limits.max_packet_bytes as u64 => {
                return Err(Error::Limit("MP4 atom bytes"))
            }
            _ => (),
        }
        pos += size;
    }
    Ok(())
}
