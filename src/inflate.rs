//! DEFLATE (RFC 1951) and zlib (RFC 1950) decompression for PNG image data.
//!
//! Output is passed to a sink in pieces as it is produced; only the 32 KiB
//! window that back-references may reach is kept, so memory does not grow
//! with the image. Huffman codes are decoded with a 12-bit lookup table and
//! a canonical (count/symbol) walk for longer codes.
use crate::{Error, Result};

const WINDOW: usize = 1 << 15;
const FAST_BITS: u32 = 12;
const MAX_BITS: usize = 15;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order of the code-length code lengths in a dynamic block header.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn invalid() -> Error {
    Error::Invalid("deflate data")
}

/// LSB-first bit reader over input split into consecutive parts (PNG IDAT
/// chunk bodies), without joining them.
struct Bits<'a> {
    parts: &'a [&'a [u8]],
    part: &'a [u8],
    index: usize,
    pos: usize,
    /// Bytes of the parts before `part`.
    before: usize,
    buf: u64,
    count: u32,
}
impl<'a> Bits<'a> {
    fn new(parts: &'a [&'a [u8]]) -> Self {
        Self {
            parts,
            part: parts.first().copied().unwrap_or(&[]),
            index: 0,
            pos: 0,
            before: 0,
            buf: 0,
            count: 0,
        }
    }
    #[inline]
    fn refill(&mut self) {
        if let Some(word) = self.part.get(self.pos..self.pos + 8) {
            // Bits above `count` are the following input bits, so OR-ing the
            // same bytes again later leaves them unchanged.
            self.buf |= u64::from_le_bytes(word.try_into().unwrap()) << self.count;
            let bytes = (63 - self.count) / 8;
            self.pos += bytes as usize;
            self.count += bytes * 8;
            return;
        }
        while self.count <= 56 {
            if self.pos == self.part.len() {
                if self.index + 1 >= self.parts.len() {
                    break;
                }
                self.before += self.part.len();
                self.index += 1;
                self.part = self.parts[self.index];
                self.pos = 0;
                continue;
            }
            self.buf |= (self.part[self.pos] as u64) << self.count;
            self.pos += 1;
            self.count += 8;
        }
    }
    #[inline]
    fn need(&mut self, n: u32) -> Result<()> {
        if self.count < n {
            self.refill();
            if self.count < n {
                return Err(Error::Invalid("deflate data truncated"));
            }
        }
        Ok(())
    }
    #[inline]
    fn take(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.count -= n;
        Ok(v)
    }
    /// Discard bits up to the next byte boundary.
    fn align(&mut self) {
        let drop = self.count % 8;
        self.buf >>= drop;
        self.count -= drop;
    }
    /// Input bytes consumed (after `align`).
    fn byte_position(&self) -> usize {
        self.before + self.pos - (self.count / 8) as usize
    }
}

/// Canonical Huffman code: a direct table for codes up to `FAST_BITS` and
/// per-length counts and sorted symbols for the rest.
struct Huffman {
    /// (symbol << 4) | length, or 0 when the code is longer than FAST_BITS
    /// or unused.
    fast: Box<[u16; 1 << FAST_BITS]>,
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
}
impl Huffman {
    /// Build from code lengths. `allow_incomplete` permits the incomplete
    /// distance codes described below.
    fn new(lengths: &[u8], allow_incomplete: bool) -> Result<Self> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left = 1i32;
        for &c in &counts[1..] {
            left = (left << 1) - c as i32;
            if left < 0 {
                return Err(Error::Invalid("deflate code over-subscribed"));
            }
        }
        let used: u16 = counts.iter().sum();
        // An incomplete code is accepted only for distances, and only when
        // empty or a single one-bit code (as fdeflate and zlib do).
        if left > 0 && !(allow_incomplete && (used == 0 || (used == 1 && counts[1] == 1))) {
            return Err(Error::Invalid("deflate code incomplete"));
        }
        let mut offsets = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut symbols = vec![0u16; used as usize];
        for (symbol, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = symbol as u16;
                offsets[l as usize] += 1;
            }
        }
        // Fill the fast table with bit-reversed canonical codes.
        let mut fast = Box::new([0u16; 1 << FAST_BITS]);
        let mut code = 0u32;
        let mut index = 0usize;
        for len in 1..=MAX_BITS as u32 {
            for _ in 0..counts[len as usize] {
                if len <= FAST_BITS {
                    let reversed = code.reverse_bits() >> (32 - len);
                    let entry = (symbols[index] << 4) | len as u16;
                    let mut slot = reversed as usize;
                    while slot < fast.len() {
                        fast[slot] = entry;
                        slot += 1 << len;
                    }
                }
                code += 1;
                index += 1;
            }
            code <<= 1;
        }
        Ok(Self {
            fast,
            counts,
            symbols,
        })
    }
    #[inline(always)]
    fn decode(&self, bits: &mut Bits) -> Result<u16> {
        if bits.count < MAX_BITS as u32 {
            bits.refill();
        }
        let entry = self.fast[(bits.buf & ((1 << FAST_BITS) - 1)) as usize];
        let len = (entry & 15) as u32;
        if len != 0 && len <= bits.count {
            bits.buf >>= len;
            bits.count -= len;
            return Ok(entry >> 4);
        }
        // Canonical decode one bit at a time.
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_BITS {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Error::Invalid("deflate code"))
    }
}

/// Output not yet passed to the sink, after the last 32 KiB already passed
/// (which back-references may still reach).
struct Output<'s> {
    buf: Vec<u8>,
    /// Bytes of `buf` already passed to the sink.
    sent: usize,
    sink: &'s mut dyn FnMut(&[u8]) -> Result<()>,
}
impl Output<'_> {
    #[inline]
    fn push(&mut self, b: u8) {
        self.buf.push(b);
    }
    /// `buf` always holds at least the last 32 KiB once that much was
    /// produced, so a distance beyond it reaches before the stream start.
    #[inline]
    fn copy(&mut self, distance: usize, length: usize) -> Result<()> {
        if distance > self.buf.len() {
            return Err(Error::Invalid("deflate distance"));
        }
        let start = self.buf.len() - distance;
        if distance >= length {
            self.buf.extend_from_within(start..start + length);
        } else {
            for i in 0..length {
                let b = self.buf[start + i];
                self.buf.push(b);
            }
        }
        Ok(())
    }
    fn flush_if(&mut self, at_least: usize) -> Result<()> {
        if self.buf.len() - self.sent >= at_least {
            (self.sink)(&self.buf[self.sent..])?;
            self.sent = self.buf.len();
            if self.buf.len() > 4 * WINDOW {
                let drop = self.buf.len() - WINDOW;
                self.buf.drain(..drop);
                self.sent = WINDOW;
            }
        }
        Ok(())
    }
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [0u8; 288];
    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..].fill(8);
    (
        Huffman::new(&lengths, false).expect("fixed literal code"),
        // All 32 five-bit codes; symbols 30 and 31 are rejected when decoded.
        Huffman::new(&[5u8; 32], false).expect("fixed distance code"),
    )
}

fn dynamic_tables(bits: &mut Bits) -> Result<(Huffman, Huffman)> {
    let hlit = bits.take(5)? as usize + 257;
    let hdist = bits.take(5)? as usize + 1;
    let hclen = bits.take(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(Error::Invalid("deflate table sizes"));
    }
    let mut clen = [0u8; 19];
    for &i in &CLEN_ORDER[..hclen] {
        clen[i] = bits.take(3)? as u8;
    }
    let clen_code = Huffman::new(&clen, false)?;
    let mut lengths = [0u8; 286 + 30];
    let mut i = 0;
    while i < hlit + hdist {
        let symbol = clen_code.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                if i == 0 {
                    return Err(Error::Invalid("deflate repeat without length"));
                }
                (lengths[i - 1], 3 + bits.take(2)? as usize)
            }
            17 => (0, 3 + bits.take(3)? as usize),
            _ => (0, 11 + bits.take(7)? as usize),
        };
        if i + repeat > hlit + hdist {
            return Err(Error::Invalid("deflate code lengths overflow"));
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        return Err(Error::Invalid("deflate block without end code"));
    }
    Ok((
        Huffman::new(&lengths[..hlit], false)?,
        Huffman::new(&lengths[hlit..hlit + hdist], true)?,
    ))
}

/// Decompress one raw DEFLATE stream from the concatenation of `parts`,
/// passing output to `sink`. Returns the number of input bytes it occupied.
pub(crate) fn inflate(parts: &[&[u8]], sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<usize> {
    let mut bits = Bits::new(parts);
    let mut out = Output {
        buf: Vec::with_capacity(4 * WINDOW + 512),
        sent: 0,
        sink,
    };
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.align();
                let len = bits.take(16)?;
                let nlen = bits.take(16)?;
                if len != !nlen & 0xffff {
                    return Err(Error::Invalid("deflate stored length"));
                }
                for _ in 0..len {
                    let b = bits.take(8)? as u8;
                    out.push(b);
                }
            }
            kind @ (1 | 2) => {
                let (lit, dist) = if kind == 1 {
                    fixed_tables()
                } else {
                    dynamic_tables(&mut bits)?
                };
                loop {
                    // Literals whose codes are in the fast table, without
                    // the general decode: two per refill when they fit.
                    if bits.count < 2 * MAX_BITS as u32 {
                        bits.refill();
                    }
                    let entry = lit.fast[(bits.buf & ((1 << FAST_BITS) - 1)) as usize];
                    let len = (entry & 15) as u32;
                    if len != 0 && len <= bits.count && entry >> 4 < 256 {
                        bits.buf >>= len;
                        bits.count -= len;
                        out.push((entry >> 4) as u8);
                        let entry = lit.fast[(bits.buf & ((1 << FAST_BITS) - 1)) as usize];
                        let len = (entry & 15) as u32;
                        if len != 0 && len <= bits.count && entry >> 4 < 256 {
                            bits.buf >>= len;
                            bits.count -= len;
                            out.push((entry >> 4) as u8);
                        }
                        out.flush_if(WINDOW)?;
                        continue;
                    }
                    let symbol = lit.decode(&mut bits)? as usize;
                    if symbol < 256 {
                        out.push(symbol as u8);
                    } else if symbol == 256 {
                        break;
                    } else {
                        let s = symbol - 257;
                        if s >= 29 {
                            return Err(invalid());
                        }
                        let length =
                            LENGTH_BASE[s] as usize + bits.take(LENGTH_EXTRA[s] as u32)? as usize;
                        let d = dist.decode(&mut bits)? as usize;
                        if d >= 30 {
                            return Err(invalid());
                        }
                        let distance =
                            DIST_BASE[d] as usize + bits.take(DIST_EXTRA[d] as u32)? as usize;
                        out.copy(distance, length)?;
                    }
                    out.flush_if(WINDOW)?;
                }
            }
            _ => return Err(Error::Invalid("deflate block type")),
        }
        out.flush_if(1)?;
        if last {
            break;
        }
    }
    bits.align();
    Ok(bits.byte_position())
}

/// Adler-32 (RFC 1950) running checksum.
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}
impl Adler32 {
    pub(crate) fn new() -> Self {
        Self { a: 1, b: 0 }
    }
    pub(crate) fn update(&mut self, data: &[u8]) {
        // 5552 is the largest n with 255n(n+1)/2 + (n+1)(65520) < 2^32.
        for chunk in data.chunks(5552) {
            for &x in chunk {
                self.a += x as u32;
                self.b += self.a;
            }
            self.a %= 65521;
            self.b %= 65521;
        }
    }
    pub(crate) fn finish(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decompress(data: &[u8]) -> Result<(Vec<u8>, usize)> {
        let mut out = Vec::new();
        let used = inflate(&[data], &mut |piece| {
            out.extend_from_slice(piece);
            Ok(())
        })?;
        Ok((out, used))
    }

    #[test]
    fn stored_fixed_and_dynamic_blocks() {
        // Stored block "abc".
        assert_eq!(
            decompress(&[0x01, 3, 0, 0xfc, 0xff, b'a', b'b', b'c']).unwrap(),
            (b"abc".to_vec(), 8)
        );
        // Fixed-Huffman "a" (from zlib: 78 9c 4b 04 00 ...), raw part 4b 04 00.
        assert_eq!(decompress(&[0x4b, 0x04, 0x00]).unwrap().0, b"a");
        // Bad block type 3.
        assert!(decompress(&[0x07]).is_err());
        // Truncated.
        assert!(decompress(&[0x01, 3, 0, 0xfc, 0xff, b'a']).is_err());
        // Stored length mismatch.
        assert!(decompress(&[0x01, 3, 0, 0xfc, 0xfe, b'a', b'b', b'c']).is_err());
    }

    #[test]
    fn adler32_vectors() {
        let mut a = Adler32::new();
        a.update(b"Wikipedia");
        assert_eq!(a.finish(), 0x11e6_0398);
        let mut a = Adler32::new();
        a.update(&vec![0xffu8; 100_000]);
        let (mut s1, mut s2) = (1u32, 0u32);
        for _ in 0..100_000 {
            s1 = (s1 + 255) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        assert_eq!(a.finish(), (s2 << 16) | s1);
    }
}
