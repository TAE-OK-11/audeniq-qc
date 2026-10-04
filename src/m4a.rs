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
    let _profile = crate::profile::scope(crate::profile::Stage::M4aOpen);
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
    let stts = table(one(&stbl, b"stts")?, 8, limits)?;
    let mut timed_packets = 0;
    let mut total = 0u64;
    for b in stts.as_chunks::<8>().0.iter() {
        let n = be32(b) as usize;
        let frames = be32(&b[4..]);
        if n == 0 || n > count - timed_packets || frames == 0 || frames > be32(&config) {
            return Err(Error::Invalid("MP4 sample timing"));
        }
        total += n as u64 * frames as u64;
        timed_packets += n;
    }
    if timed_packets != count || total != duration {
        return Err(Error::Invalid("MP4 timing count"));
    }
    let (offsets, offset_width) = if stbl.iter().any(|a| &a.kind == b"co64") {
        if stbl.iter().any(|a| &a.kind == b"stco") {
            return Err(Error::Invalid("MP4 duplicate chunk offsets"));
        }
        (table(one(&stbl, b"co64")?, 8, limits)?, 8)
    } else {
        (table(one(&stbl, b"stco")?, 4, limits)?, 4)
    };
    let maps = table(one(&stbl, b"stsc")?, 12, limits)?.as_chunks::<12>().0;
    if maps.is_empty()
        || be32(&maps[0]) != 1
        || maps.windows(2).any(|w| be32(&w[0]) >= be32(&w[1]))
        || maps.iter().any(|b| {
            be32(b) as usize > offsets.len() / offset_width
                || be32(&b[4..]) == 0
                || be32(&b[8..]) != 1
        })
    {
        return Err(Error::Invalid("MP4 sample-to-chunk map"));
    }
    let mut packets = Vec::with_capacity(count);
    let mut map = 0;
    let mut previous_end = 0;
    let mut timing = stts.as_chunks::<8>().0.iter();
    let mut timing_left = 0;
    let mut packet_frames = 0;
    for (i, raw_offset) in offsets.chunks_exact(offset_width).enumerate() {
        let offset = if offset_width == 8 {
            be64(raw_offset)
        } else {
            be32(raw_offset) as u64
        };
        limits.check()?;
        if map + 1 < maps.len() && be32(&maps[map + 1]) as usize == i + 1 {
            map += 1;
        }
        let n = be32(&maps[map][4..]) as usize;
        if n > count - packets.len() || offset < previous_end {
            return Err(Error::Invalid("MP4 overlapping/sample count"));
        }
        let mut offset = offset;
        for _ in 0..n {
            let ix = packets.len();
            let size = if common == 0 {
                be32(&stsz[12 + ix * 4..]) as usize
            } else {
                common
            };
            if size == 0 || size > limits.max_packet_bytes {
                return Err(Error::Limit("MP4 packet bytes"));
            }
            if timing_left == 0 {
                let row = timing.next().ok_or(Error::Invalid("MP4 timing count"))?;
                timing_left = be32(row);
                packet_frames = be32(&row[4..]);
            }
            timing_left -= 1;
            let end = offset
                .checked_add(size as u64)
                .ok_or(Error::Invalid("MP4 offset overflow"))?;
            if !media.iter().any(|&(a, b)| offset >= a && end <= b) {
                return Err(Error::Invalid("MP4 packet outside mdat"));
            }
            packets.push(Packet {
                offset,
                size,
                frames: packet_frames,
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
                        let children = atoms(tag.body)?;
                        let key = match &tag.kind {
                            b"\xa9too" => "encoder",
                            b"\xa9enc" => "encoded_by",
                            b"\xa9cmt" => "comment",
                            b"desc" | b"ldes" => "description",
                            b"----" => {
                                let name = children.iter().find(|a| &a.kind == b"name");
                                let Some(name) = name else { continue };
                                if name.body.len() < 4 || name.body.len() > 68 {
                                    return Err(Error::Invalid("MP4 freeform tag name"));
                                }
                                match &name.body[4..] {
                                    b"encoder" => "encoder",
                                    b"encoded_by" => "encoded_by",
                                    b"software" => "software",
                                    b"writing_library" => "writing_library",
                                    b"creator_tool" => "creator_tool",
                                    b"comment" => "comment",
                                    b"description" => "description",
                                    _ => continue,
                                }
                            }
                            _ => continue,
                        };
                        for data in children {
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
    #[cfg(not(feature = "reference-codecs"))]
    #[test]
    fn borrowed_tables_preserve_chunk_maps_timing_runs_and_wide_offsets() {
        fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            out.extend(kind);
            out.extend(body);
            out
        }
        fn table(kind: &[u8; 4], entries: &[u32], unit: usize) -> Vec<u8> {
            let mut body = vec![0; 4];
            body.extend(((entries.len() * 4 / unit) as u32).to_be_bytes());
            for value in entries {
                body.extend(value.to_be_bytes());
            }
            atom(kind, &body)
        }
        for wide in [false, true] {
            for common in [false, true] {
                let sizes: [u32; 5] = if common { [3; 5] } else { [3, 4, 5, 6, 7] };
                let mut config = [0u8; 24];
                config[..4].copy_from_slice(&4096u32.to_be_bytes());
                config[5] = 16;
                config[9] = 2;
                config[20..].copy_from_slice(&48000u32.to_be_bytes());
                let mut cookie = vec![0; 4];
                cookie.extend(config);
                let mut desc = vec![0; 28];
                desc[16..18].copy_from_slice(&2u16.to_be_bytes());
                desc[18..20].copy_from_slice(&16u16.to_be_bytes());
                desc.extend(atom(b"alac", &cookie));
                let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
                stsd.extend(atom(b"alac", &desc));
                let mut stsz = vec![0; 4];
                stsz.extend(if common { 3u32 } else { 0u32 }.to_be_bytes());
                stsz.extend(5u32.to_be_bytes());
                if !common {
                    for size in sizes {
                        stsz.extend(size.to_be_bytes());
                    }
                }
                let offsets = [
                    8u32,
                    8 + sizes[0] + sizes[1],
                    8 + sizes[..4].iter().sum::<u32>(),
                ];
                let mut stbl = atom(b"stsd", &stsd);
                stbl.extend(atom(b"stsz", &stsz));
                stbl.extend(table(b"stts", &[2, 4096, 3, 1024], 8));
                stbl.extend(table(b"stsc", &[1, 2, 1, 3, 1, 1], 12));
                if wide {
                    let values: Vec<u32> = offsets.into_iter().flat_map(|o| [0, o]).collect();
                    stbl.extend(table(b"co64", &values, 8));
                } else {
                    stbl.extend(table(b"stco", &offsets, 4));
                }
                let mut mdhd = vec![0; 24];
                mdhd[12..16].copy_from_slice(&48000u32.to_be_bytes());
                mdhd[16..20].copy_from_slice(&11264u32.to_be_bytes());
                let mut hdlr = vec![0; 12];
                hdlr[8..].copy_from_slice(b"soun");
                let mut mdia = atom(b"mdhd", &mdhd);
                mdia.extend(atom(b"hdlr", &hdlr));
                mdia.extend(atom(b"minf", &atom(b"stbl", &stbl)));
                let mut data = atom(b"mdat", &vec![0; sizes.iter().sum::<u32>() as usize]);
                data.extend(atom(b"moov", &atom(b"trak", &atom(b"mdia", &mdia))));
                let path = std::env::temp_dir().join(format!(
                    "audeniq-m4a-tables-{}-{wide}-{common}.m4a",
                    std::process::id()
                ));
                std::fs::write(&path, &data).unwrap();
                let mut file = File::open(&path).unwrap();
                let track = open(&mut file, &Limits::default()).unwrap();
                std::fs::remove_file(&path).unwrap();
                assert_eq!(track.config, config);
                assert_eq!(track.spec.frames, Some(11264));
                assert_eq!(track.packets.len(), 5);
                let mut offset = 8;
                for (i, packet) in track.packets.iter().enumerate() {
                    assert_eq!(packet.offset, offset);
                    assert_eq!(packet.size, sizes[i] as usize);
                    assert_eq!(packet.frames, if i < 2 { 4096 } else { 1024 });
                    offset += sizes[i] as u64;
                }
                // A timing run with an extra packet must fail before building
                // an inconsistent borrowed-table cursor/index.
                let pos = data.windows(4).position(|b| b == b"stts").unwrap() + 12;
                data[pos..pos + 4].copy_from_slice(&3u32.to_be_bytes());
                std::fs::write(&path, &data).unwrap();
                let mut file = File::open(&path).unwrap();
                assert!(open(&mut file, &Limits::default()).is_err());
                std::fs::remove_file(&path).unwrap();
            }
        }
    }
    #[test]
    fn atom_headers_lengths_and_duplicate_tables_are_checked() {
        for len in 1..8 {
            assert!(atoms(&vec![0; len]).is_err());
        }
        for size in [1u32, 2, 7, 99] {
            let mut b = vec![0; 8];
            b[..4].copy_from_slice(&size.to_be_bytes());
            assert!(atoms(&b).is_err());
        }
        let mut b = vec![0; 16];
        b[..4].copy_from_slice(&1u32.to_be_bytes());
        b[8..].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(atoms(&b).is_err());
        let b = b"\0\0\0\x08stco\0\0\0\x08stco";
        let list = atoms(b).unwrap();
        assert_eq!(list.len(), 2);
        #[cfg(not(feature = "reference-codecs"))]
        assert!(one(&list, b"stco").is_err());
    }
}
