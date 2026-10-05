//! PNG cover validation: accepts exactly the files the previous decoder
//! stack (image 0.24.9 with png 0.17.16, EXPAND, 8192-pixel and 128 MiB
//! limits) decoded, without storing pixels.
//!
//! Decoding there succeeds when the chunks before the image data are valid,
//! one run of IDAT chunks (or fdAT chunks after an APNG fcTL) holds a complete
//! zlib stream whose first bytes are every row of the (sub)frame with a valid
//! filter type, the run's chunk CRCs match, and the next chunk header exists.
//! Nothing after that header is read. Unfiltering cannot fail and palette
//! indices are not range-checked, so only the filter bytes are inspected and
//! no image buffer is kept: memory is the 32 KiB inflate window. Files that
//! made png 0.17 panic (indexed images with a PLTE length that is not a
//! multiple of three or longer than 768) are rejected.
use crate::{inflate, Error, Result};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
/// The previous decoder's allocation budget (png::Limits and image's
/// max_alloc were both 128 MiB).
const BUDGET: usize = 128 * 1024 * 1024;
/// Initial capacity of png 0.17's chunk buffer (not charged to the budget).
const CHUNK_BUFFER: usize = 32 * 1024;
pub(crate) const MAX_DIMENSION: u32 = 8192;

fn invalid() -> Error {
    Error::Invalid("cover decode")
}

struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlaced: bool,
}
impl Header {
    fn samples(&self) -> usize {
        match self.color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }
    /// Bytes of one filtered row of `width` pixels, without the filter byte.
    fn row_bytes(&self, width: u32) -> usize {
        let samples = width as usize * self.samples();
        match self.depth {
            16 => samples * 2,
            8 => samples,
            d => samples.div_ceil(8 / d as usize),
        }
    }
    /// Bytes per pixel after png's EXPAND transformation.
    fn output_bpp(&self, trns: bool) -> usize {
        let wide = if self.depth == 16 { 2 } else { 1 };
        match (self.color, trns) {
            (0, false) => wide,
            (0, true) | (4, _) => 2 * wide,
            (2, false) => 3 * wide,
            (2, true) | (6, _) => 4 * wide,
            (3, false) => 3,
            _ => 4,
        }
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn parse_header(data: &[u8]) -> Result<Header> {
    if data.len() < 13 {
        return Err(invalid());
    }
    let (width, height, depth, color) = (be32(data), be32(&data[4..]), data[8], data[9]);
    if width == 0 || height == 0 || !matches!(depth, 1 | 2 | 4 | 8 | 16) {
        return Err(invalid());
    }
    let valid = match color {
        0 => true,
        2 | 4 | 6 => depth >= 8,
        3 => depth <= 8,
        _ => false,
    };
    if !valid || data[10] != 0 || data[11] != 0 || data[12] > 1 {
        return Err(invalid());
    }
    Ok(Header {
        width,
        height,
        depth,
        color,
        interlaced: data[12] == 1,
    })
}

/// zlib stream with its Adler-32 checked, as png used for iCCP profiles:
/// the decompressed length, or None when invalid or longer than `limit`.
fn iccp_length(data: &[u8], limit: usize) -> Option<usize> {
    if data.len() < 2 || !zlib_header_ok(data[0], data[1]) {
        return None;
    }
    let mut adler = inflate::Adler32::new();
    let mut total = 0usize;
    let used = inflate::inflate(&[&data[2..]], &mut |piece| {
        total += piece.len();
        if total > limit {
            return Err(invalid());
        }
        adler.update(piece);
        Ok(())
    })
    .ok()?;
    let check = data.get(2 + used..2 + used + 4)?;
    (be32(check) == adler.finish()).then_some(total)
}

fn zlib_header_ok(cmf: u8, flg: u8) -> bool {
    cmf & 15 == 8
        && cmf >> 4 <= 7
        && flg & 0x20 == 0
        && (((cmf as u16) << 8) | flg as u16).is_multiple_of(31)
}

/// Expected filtered rows: checks each row's filter byte as output arrives.
struct Rows {
    /// (row length including the filter byte, rows) per non-empty pass.
    passes: Vec<(usize, u64)>,
    pass: usize,
    row: u64,
    offset: usize,
}
impl Rows {
    fn new(header: &Header, width: u32, height: u32) -> Self {
        let mut passes = Vec::new();
        let adam7 = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        if header.interlaced {
            for (x, y, dx, dy) in adam7 {
                let w = (width as u64).saturating_sub(x).div_ceil(dx) as u32;
                let h = (height as u64).saturating_sub(y).div_ceil(dy);
                if w > 0 && h > 0 {
                    passes.push((1 + header.row_bytes(w), h));
                }
            }
        } else {
            passes.push((1 + header.row_bytes(width), height as u64));
        }
        Self {
            passes,
            pass: 0,
            row: 0,
            offset: 0,
        }
    }
    fn done(&self) -> bool {
        self.pass == self.passes.len()
    }
    fn feed(&mut self, mut piece: &[u8]) -> Result<()> {
        while !piece.is_empty() && !self.done() {
            let (len, rows) = self.passes[self.pass];
            if self.offset == 0 && piece[0] > 4 {
                return Err(invalid());
            }
            let take = (len - self.offset).min(piece.len());
            piece = &piece[take..];
            self.offset += take;
            if self.offset == len {
                self.offset = 0;
                self.row += 1;
                if self.row == rows {
                    self.row = 0;
                    self.pass += 1;
                }
            }
        }
        Ok(())
    }
}

/// Validate a PNG file; returns the IHDR width and height.
pub(crate) fn validate(data: &[u8]) -> Result<(u32, u32)> {
    if !data.starts_with(SIGNATURE) {
        return Err(invalid());
    }
    let mut pos = 8;
    let mut header: Option<Header> = None;
    let mut budget = BUDGET;
    let mut capacity = CHUNK_BUFFER;
    let mut palette: Option<usize> = None;
    let mut trns = false;
    let (mut srgb, mut gama, mut chrm, mut phys, mut sbit, mut iccp) =
        (false, false, false, false, false, false);
    let mut sequence: Option<u32> = None;
    let mut frames: Option<u32> = None;
    let mut subframe: Option<(u32, u32)> = None;
    // Chunk framing: (length, type, data); the data may be cut short by EOF,
    // in which case the decoder failed.
    let chunk_at = |pos: usize| -> Result<(usize, [u8; 4], &[u8], bool)> {
        let head = data.get(pos..pos + 8).ok_or_else(invalid)?;
        let length = be32(head) as usize;
        let kind: [u8; 4] = head[4..8].try_into().unwrap();
        let body = data
            .get(pos + 8..pos.saturating_add(8).saturating_add(length))
            .ok_or_else(invalid)?;
        let crc = data
            .get(pos + 8 + length..pos + 12 + length)
            .ok_or_else(invalid)?;
        let mut c = crate::crc32::Crc32::new();
        c.update(&kind);
        c.update(body);
        Ok((length, kind, body, c.finalize() == be32(crc)))
    };
    loop {
        let (length, kind, body, crc_ok) = chunk_at(pos)?;
        let critical = kind[0] & 32 == 0;
        if header.is_none() && &kind != b"IHDR" {
            return Err(invalid());
        }
        if &kind == b"IDAT" || &kind == b"fdAT" {
            if &kind == b"fdAT" && sequence.is_none() {
                return Err(invalid());
            }
            break;
        }
        if &kind == b"IEND" {
            return Err(invalid());
        }
        // Non-image chunks are buffered whole; growth is charged.
        while length > capacity {
            let reserve = budget.saturating_sub(capacity).min(capacity);
            if reserve == 0 {
                return Err(invalid());
            }
            budget -= reserve;
            capacity += reserve;
        }
        let short = |n: usize| {
            if body.len() < n {
                Err(invalid())
            } else {
                Ok(())
            }
        };
        // png 0.17 parses a chunk once its last data byte arrives, so empty
        // chunks are never parsed (an empty PLTE is no palette at all).
        let kind = if length == 0 { [0; 4] } else { kind };
        match &kind {
            b"IHDR" => {
                if header.is_some() {
                    return Err(invalid());
                }
                let h = parse_header(body)?;
                if h.width > MAX_DIMENSION || h.height > MAX_DIMENSION {
                    return Err(Error::Invalid("cover decode"));
                }
                header = Some(h);
            }
            b"PLTE" => {
                if palette.is_some() || budget < length {
                    return Err(invalid());
                }
                budget -= length;
                palette = Some(length);
            }
            b"tRNS" => {
                let h = header.as_ref().unwrap();
                if trns || budget < length {
                    return Err(invalid());
                }
                budget -= length;
                match h.color {
                    0 => short(2)?,
                    2 => short(6)?,
                    3 if palette.is_none() => return Err(invalid()),
                    3 => (),
                    _ => return Err(invalid()),
                }
                trns = true;
            }
            b"sBIT" => {
                let h = header.as_ref().unwrap();
                if palette.is_none() && !sbit && budget >= length {
                    budget -= length;
                    let (expected, max) = match h.color {
                        0 => (1, h.depth),
                        2 => (3, h.depth),
                        3 => (3, 8),
                        4 => (2, h.depth),
                        _ => (4, h.depth),
                    };
                    sbit = length == expected && body.iter().all(|&s| s >= 1 && s <= max);
                }
            }
            b"pHYs" => {
                if phys {
                    return Err(invalid());
                }
                short(9)?;
                if body[8] > 1 {
                    return Err(invalid());
                }
                phys = true;
            }
            b"cHRM" => {
                if chrm {
                    return Err(invalid());
                }
                short(32)?;
                chrm = true;
            }
            b"gAMA" => {
                if gama {
                    return Err(invalid());
                }
                short(4)?;
                gama = true;
            }
            b"sRGB" => {
                if srgb {
                    return Err(invalid());
                }
                short(1)?;
                if body[0] > 3 {
                    return Err(invalid());
                }
                srgb = true;
            }
            b"acTL" => {
                short(8)?;
                frames = Some(be32(body));
            }
            b"fcTL" => {
                short(4)?;
                let number = be32(body);
                if number != sequence.map_or(0, |s| s.wrapping_add(1)) {
                    return Err(invalid());
                }
                sequence = Some(number);
                short(26)?;
                let (w, h, x, y) = (
                    be32(&body[4..]),
                    be32(&body[8..]),
                    be32(&body[12..]),
                    be32(&body[16..]),
                );
                if body[24] > 2 || body[25] > 1 {
                    return Err(invalid());
                }
                let image = header.as_ref().unwrap();
                if w == 0
                    || h == 0
                    || image.width.checked_sub(x).is_none_or(|room| w > room)
                    || image.height.checked_sub(y).is_none_or(|room| h > room)
                {
                    return Err(invalid());
                }
                subframe = Some((w, h));
            }
            b"iCCP" if !iccp => {
                iccp = true;
                // Name of 1..=79 bytes, NUL, compression method 0, zlib.
                if let Some(nul) = body.iter().take(80).position(|&b| b == 0) {
                    if nul > 0 && body.get(nul + 1) == Some(&0) {
                        if let Some(profile) = iccp_length(&body[nul + 2..], budget) {
                            budget -= profile;
                        }
                    }
                }
            }
            _ => (),
        }
        // Ancillary chunks with a bad CRC were skipped after being parsed.
        if critical && !crc_ok {
            return Err(invalid());
        }
        pos += 12 + length;
    }
    let header = header.ok_or_else(invalid)?;
    if frames == Some(0) && subframe.is_some() {
        return Err(invalid());
    }
    let (width, height) = subframe.unwrap_or((header.width, header.height));
    let bpp = header.output_bpp(trns);
    // image's max_alloc on the whole output; png's budget on one row.
    if header.width as u64 * header.height as u64 * bpp as u64 > BUDGET as u64
        || budget < width as usize * bpp
    {
        return Err(invalid());
    }
    if header.color == 3 {
        // Missing PLTE was an error; a malformed one a panic.
        match palette {
            Some(len) if len % 3 == 0 && len <= 768 => (),
            _ => return Err(invalid()),
        }
    }
    // The image data run: consecutive chunks of the first one's type.
    let run_kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
    let mut bodies: Vec<&[u8]> = Vec::new();
    let mut expected_sequence = sequence.map(|s| s.wrapping_add(1));
    loop {
        // The run ends at the next chunk's type; nothing after it is read.
        let next = data.get(pos..pos + 8).ok_or_else(invalid)?;
        if next[4..8] != run_kind {
            break;
        }
        let (length, kind, body, crc_ok) = chunk_at(pos)?;
        let body = if &kind == b"fdAT" {
            if length < 4 || Some(be32(body)) != expected_sequence {
                return Err(invalid());
            }
            expected_sequence = expected_sequence.map(|s| s.wrapping_add(1));
            &body[4..]
        } else {
            body
        };
        if !crc_ok {
            return Err(invalid());
        }
        bodies.push(body);
        pos += 12 + length;
    }
    // The zlib header (whose two bytes may lie in different chunks).
    let total: usize = bodies.iter().map(|b| b.len()).sum();
    let mut header_bytes = bodies.iter().flat_map(|b| b.iter()).take(2);
    let (Some(&cmf), Some(&flg)) = (header_bytes.next(), header_bytes.next()) else {
        return Err(invalid());
    };
    if !zlib_header_ok(cmf, flg) {
        return Err(invalid());
    }
    let mut skip = 2;
    let deflate: Vec<&[u8]> = bodies
        .iter()
        .filter_map(|b| {
            let cut = skip.min(b.len());
            skip -= cut;
            (cut < b.len()).then(|| &b[cut..])
        })
        .collect();
    let mut rows = Rows::new(&header, width, height);
    let used = inflate::inflate(&deflate, &mut |piece| rows.feed(piece))?;
    // The Adler-32 is not checked, but must be present.
    if !rows.done() || total < 2 + used + 4 {
        return Err(invalid());
    }
    Ok((header.width, header.height))
}
