//! AUDENIQ's single-track, non-fragmented ALAC reader. ISO BMFF sample tables
//! are interpreted directly; only the bounded moov tree and packet index live
//! in memory. Media payload stays in the file.
#[cfg(not(feature = "reference-codecs"))]
use crate::AudioSpec;
use crate::{Error, Limits, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
#[cfg(not(feature = "reference-codecs"))]
pub(crate) struct Packet {
    pub offset: u64,
    pub size: usize,
    pub frames: u32,
}
#[cfg(not(feature = "reference-codecs"))]
pub(crate) struct Track {
    pub spec: AudioSpec,
    pub config: [u8; 24],
    pub packets: Vec<Packet>,
}
#[derive(Clone, Copy)]
struct Atom<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}
fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}
fn atoms(mut b: &[u8]) -> Result<Vec<Atom<'_>>> {
    let mut result = Vec::new();
    while !b.is_empty() {
        if b.len() < 8 {
            return Err(Error::Invalid("MP4 atom header"));
        }
        if result.len() >= 4096 {
            return Err(Error::Limit("MP4 child atoms"));
        }
        let size = be32(b);
        let header = if size == 1 { 16 } else { 8 };
        if b.len() < header {
            return Err(Error::Invalid("MP4 extended atom"));
        }
        let size = if size == 0 {
            b.len() as u64
        } else if size == 1 {
            be64(&b[8..])
        } else {
            size as u64
        };
        if size < header as u64 || size > b.len() as u64 {
            return Err(Error::Invalid("MP4 atom length"));
        }
        let n = size as usize;
        result.push(Atom {
            kind: b[4..8].try_into().unwrap(),
            body: &b[header..n],
        });
        b = &b[n..];
    }
    Ok(result)
}
#[cfg(not(feature = "reference-codecs"))]
fn one<'a>(list: &[Atom<'a>], kind: &[u8; 4]) -> Result<&'a [u8]> {
    let mut found = list.iter().filter(|a| &a.kind == kind);
    let a = found.next().ok_or(Error::Invalid("missing MP4 table"))?;
    if found.next().is_some() {
        return Err(Error::Invalid("duplicate MP4 table"));
    }
    Ok(a.body)
}
type MediaRanges = Vec<(u64, u64)>;
fn moov(file: &mut File, limits: &Limits) -> Result<(Vec<u8>, MediaRanges)> {
    crate::mp4::preflight(file, limits)?;
    let length = file.metadata()?.len();
    let mut pos = 0;
    let mut tree = None;
    let mut media = Vec::new();
    while pos < length {
        file.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 16];
        file.read_exact(&mut h[..8])?;
        let raw = be32(&h);
        let header = if raw == 1 { 16 } else { 8 };
        let size = if raw == 0 {
            length - pos
        } else if raw == 1 {
            file.read_exact(&mut h[8..])?;
            be64(&h[8..])
        } else {
            raw as u64
        };
        if &h[4..8] == b"moov" {
            if tree.is_some() {
                return Err(Error::Invalid("duplicate MP4 moov"));
            }
            let mut b = vec![0; (size - header) as usize];
            file.read_exact(&mut b)?;
            tree = Some(b);
        } else if &h[4..8] == b"mdat" {
            media.push((pos + header, pos + size));
        }
        pos += size;
    }
    Ok((tree.ok_or(Error::Invalid("missing MP4 moov"))?, media))
}
#[cfg(not(feature = "reference-codecs"))]
fn table<'a>(b: &'a [u8], unit: usize, limits: &Limits) -> Result<&'a [u8]> {
    if b.len() < 8 || b[..4] != [0, 0, 0, 0] {
        return Err(Error::Invalid("MP4 table version"));
    }
    let n = be32(&b[4..]) as usize;
    if n > limits.max_chunks {
        return Err(Error::Limit("MP4 table entries"));
    }
    if n.checked_mul(unit) != Some(b.len() - 8) {
        return Err(Error::Invalid("MP4 table size"));
    }
    Ok(&b[8..])
}
#[cfg(not(feature = "reference-codecs"))]
pub(crate) fn open(file: &mut File, limits: &Limits) -> Result<Track> {
    let (tree, media) = moov(file, limits)?;
    let top = atoms(&tree)?;
    let track = one(&top, b"trak")?;
    let track = atoms(track)?;
    let mdia = atoms(one(&track, b"mdia")?)?;
    let handler = one(&mdia, b"hdlr")?;
    if handler.len() < 12 || &handler[8..12] != b"soun" {
        return Err(Error::Unsupported("ALAC audio track only"));
    }
    let minf = atoms(one(&mdia, b"minf")?)?;
    let stbl = atoms(one(&minf, b"stbl")?)?;
    let stsd = one(&stbl, b"stsd")?;
    if stsd.len() < 8 || stsd[..8] != [0, 0, 0, 0, 0, 0, 0, 1] {
        return Err(Error::Unsupported("single MP4 codec description"));
    }
    let descriptions = atoms(&stsd[8..])?;
    let desc = one(&descriptions, b"alac")?;
    if descriptions.len() != 1 || desc.len() < 28 || desc[8..10] != [0, 0] {
        return Err(Error::Unsupported("ALAC sample entry version"));
    }
    let configs = atoms(&desc[28..])?;
    let config = one(&configs, b"alac")?;
    if config.len() != 28 || config[..4] != [0, 0, 0, 0] {
        return Err(Error::Invalid("ALAC configuration"));
    }
    let config: [u8; 24] = config[4..].try_into().unwrap();
    if config[4] != 0 || be32(&config) == 0 || be32(&config) > 150_000 || config[8] > 31 {
        return Err(Error::Invalid("ALAC block configuration"));
    }
    let mut spec = AudioSpec {
        container: "mov".into(),
        codec: "alac".into(),
        sample_rate: be32(&config[20..]),
        channels: config[9] as u16,
        bits_per_sample: config[5] as u16,
        frames: None,
    };
    spec.validate()?;
    if u16::from_be_bytes(desc[16..18].try_into().unwrap()) != spec.channels
        || u16::from_be_bytes(desc[18..20].try_into().unwrap()) != spec.bits_per_sample
    {
        return Err(Error::Invalid("ALAC description mismatch"));
    }
    let mdhd = one(&mdia, b"mdhd")?;
    let (rate, duration) = match mdhd.first() {
        Some(0) if mdhd.len() >= 24 => (be32(&mdhd[12..]), be32(&mdhd[16..]) as u64),
        Some(1) if mdhd.len() >= 36 => (be32(&mdhd[20..]), be64(&mdhd[24..])),
        _ => return Err(Error::Invalid("MP4 media duration")),
    };
    if rate != spec.sample_rate || duration == 0 || duration > limits.max_frames {
        return Err(Error::Invalid("MP4 ALAC timescale/duration"));
    }
    let stsz = one(&stbl, b"stsz")?;
    if stsz.len() < 12 || stsz[..4] != [0, 0, 0, 0] {
        return Err(Error::Invalid("MP4 sizes"));
    }
    let common = be32(&stsz[4..]) as usize;
    let count = be32(&stsz[8..]) as usize;
    if count == 0 || count > limits.max_chunks || common > limits.max_packet_bytes {
        return Err(Error::Limit("MP4 sample count/size"));
    }
    if stsz.len() != 12 + if common == 0 { count * 4 } else { 0 } {
        return Err(Error::Invalid("MP4 size table"));
    }
    let sizes: Vec<usize> = (0..count)
        .map(|i| {
            if common == 0 {
                be32(&stsz[12 + i * 4..]) as usize
            } else {
                common
            }
        })
        .collect();
    if sizes.iter().any(|&n| n == 0 || n > limits.max_packet_bytes) {
        return Err(Error::Limit("MP4 packet bytes"));
    }
    let stts = table(one(&stbl, b"stts")?, 8, limits)?;
    let mut durations = Vec::with_capacity(count);
    let mut total = 0u64;
    for b in stts.as_chunks::<8>().0.iter() {
        let n = be32(b) as usize;
        let frames = be32(&b[4..]);
        if n == 0 || n > count - durations.len() || frames == 0 || frames > be32(&config) {
            return Err(Error::Invalid("MP4 sample timing"));
        }
        total += n as u64 * frames as u64;
        durations.resize(durations.len() + n, frames);
    }
    if durations.len() != count || total != duration {
        return Err(Error::Invalid("MP4 timing count"));
    }
    let offsets: Vec<u64> = if stbl.iter().any(|a| &a.kind == b"co64") {
        if stbl.iter().any(|a| &a.kind == b"stco") {
            return Err(Error::Invalid("MP4 duplicate chunk offsets"));
        }
        table(one(&stbl, b"co64")?, 8, limits)?
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| be64(b))
            .collect()
    } else {
        table(one(&stbl, b"stco")?, 4, limits)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| be32(b) as u64)
            .collect()
    };
    let maps: Vec<(u32, u32)> = table(one(&stbl, b"stsc")?, 12, limits)?
        .as_chunks::<12>()
        .0
        .iter()
        .map(|b| (be32(b), be32(&b[4..])))
        .collect();
    let raw_map = table(one(&stbl, b"stsc")?, 12, limits)?;
    if maps.is_empty()
        || maps[0].0 != 1
        || maps.windows(2).any(|w| w[0].0 >= w[1].0)
        || maps
            .iter()
            .any(|&(a, b)| a as usize > offsets.len() || b == 0)
        || raw_map
            .as_chunks::<12>()
            .0
            .iter()
            .any(|b| be32(&b[8..]) != 1)
    {
        return Err(Error::Invalid("MP4 sample-to-chunk map"));
    }
    let mut packets = Vec::with_capacity(count);
    let mut map = 0;
    let mut previous_end = 0;
    for (i, &offset) in offsets.iter().enumerate() {
        limits.check()?;
        if map + 1 < maps.len() && maps[map + 1].0 as usize == i + 1 {
            map += 1;
        }
        let n = maps[map].1 as usize;
        if n > count - packets.len() || offset < previous_end {
            return Err(Error::Invalid("MP4 overlapping/sample count"));
        }
        let mut offset = offset;
        for _ in 0..n {
            let ix = packets.len();
            let size = sizes[ix];
            let end = offset
                .checked_add(size as u64)
                .ok_or(Error::Invalid("MP4 offset overflow"))?;
            if !media.iter().any(|&(a, b)| offset >= a && end <= b) {
                return Err(Error::Invalid("MP4 packet outside mdat"));
            }
            packets.push(Packet {
                offset,
                size,
                frames: durations[ix],
            });
            offset = end;
        }
        previous_end = offset;
    }
    if packets.len() != count {
        return Err(Error::Invalid("MP4 packet count"));
    }
    spec.frames = Some(total);
    Ok(Track {
        spec,
        config,
        packets,
    })
}
/// Values needed for QC provenance/transcode screening. Unknown/binary tags are
/// left out, just as probe's bounded tag allowlist does for other containers.
pub(crate) fn tags(file: &mut File, limits: &Limits) -> Result<Vec<(String, Vec<u8>)>> {
    let (tree, _) = moov(file, limits)?;
    let mut result = Vec::new();
    fn walk(b: &[u8], depth: u32, result: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
        if depth > 16 {
            return Err(Error::Limit("MP4 tag nesting"));
        }
        for a in atoms(b)? {
            match &a.kind {
                b"udta" => walk(a.body, depth + 1, result)?,
                b"meta" if a.body.len() >= 4 => walk(&a.body[4..], depth + 1, result)?,
                b"ilst" => {
                    for tag in atoms(a.body)? {
                        let key = match &tag.kind {
                            b"\xa9too" => "encoder",
                            b"\xa9enc" => "encoded_by",
                            b"\xa9cmt" => "comment",
                            b"desc" | b"ldes" => "description",
                            _ => continue,
                        };
                        for data in atoms(tag.body)? {
                            if &data.kind == b"data" && data.body.len() >= 8 && be32(data.body) == 1
                            {
                                if data.body.len() > 65544 || result.len() >= 128 {
                                    return Err(Error::Limit("MP4 tag bytes/count"));
                                }
                                result.push((key.into(), data.body[8..].to_vec()));
                            }
                        }
                    }
                }
                _ => (),
            }
        }
        Ok(())
    }
    walk(&tree, 0, &mut result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atom_headers_lengths_and_duplicate_tables_are_checked() {
        for len in 1..8 {assert!(atoms(&vec![0;len]).is_err());}
        for size in [1u32,2,7,99] {let mut b=vec![0;8];b[..4].copy_from_slice(&size.to_be_bytes());assert!(atoms(&b).is_err());}
        let mut b=vec![0;16];b[..4].copy_from_slice(&1u32.to_be_bytes());b[8..].copy_from_slice(&u64::MAX.to_be_bytes());assert!(atoms(&b).is_err());
        let b=b"\0\0\0\x08stco\0\0\0\x08stco";let list=atoms(b).unwrap();assert_eq!(list.len(),2);
        #[cfg(not(feature="reference-codecs"))] assert!(one(&list,b"stco").is_err());
    }
}
