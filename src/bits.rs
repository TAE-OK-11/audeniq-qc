use crate::{Error, Result};

/// LSB-first bit reader (TTA, WavPack): a 64-bit cache refilled with one unaligned 8-byte
/// load while eight bytes remain, byte by byte near the end.
pub(crate) struct Lsb<'a> {
    data: &'a [u8],
    next: usize,
    pub(crate) cache: u64,
    pub(crate) available: u32,
}
impl<'a> Lsb<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            next: 0,
            cache: 0,
            available: 0,
        }
    }
    #[inline(always)]
    pub(crate) fn refill(&mut self) {
        if let Some(word) = self.data.get(self.next..self.next + 8) {
            self.cache |= u64::from_le_bytes(word.try_into().unwrap()) << self.available;
            let bytes = (63 - self.available) >> 3;
            self.next += bytes as usize;
            self.available += bytes * 8;
        } else {
            while self.available <= 56 && self.next < self.data.len() {
                self.cache |= (self.data[self.next] as u64) << self.available;
                self.next += 1;
                self.available += 8;
            }
        }
    }
    /// `n` (at most 32) bits.
    #[inline(always)]
    pub(crate) fn read(&mut self, n: u32) -> Result<u32> {
        if self.available < n {
            self.refill();
            if self.available < n {
                return Err(Error::Invalid("truncated bitstream"));
            }
        }
        let value = (self.cache & ((1u64 << n) - 1)) as u32;
        self.cache >>= n;
        self.available -= n;
        Ok(value)
    }
    /// A run of one bits ended by a zero (consumed); fewer than `max` ones.
    #[inline(always)]
    pub(crate) fn ones(&mut self, max: u32) -> Result<u32> {
        let mut n = 0;
        loop {
            if self.available < 57 {
                self.refill();
                if self.available == 0 {
                    return Err(Error::Invalid("truncated bitstream"));
                }
            }
            let ones = self.cache.trailing_ones().min(self.available);
            if ones != 0 && ones >= max.saturating_sub(n) {
                return Err(Error::Invalid("unbounded unary code"));
            }
            n += ones;
            if ones < self.available {
                // The byte-wise refill can fill all 64 bits.
                self.cache = self.cache.checked_shr(ones + 1).unwrap_or(0);
                self.available -= ones + 1;
                return Ok(n);
            }
            self.cache = 0;
            self.available = 0;
        }
    }
}

pub struct BeWriter {
    pub bytes: Vec<u8>,
    used: u8,
}
impl BeWriter {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            used: 0,
        }
    }
    pub fn reuse(mut bytes: Vec<u8>) -> Self {
        bytes.clear();
        Self { bytes, used: 0 }
    }
    pub fn put(&mut self, n: u32, value: u64) {
        // Most FLAC writes fit in one word. Append its bytes in one copy,
        // including the partial final byte needed by header CRC consumers.
        if n == 0 {
            return;
        }
        if n <= 56 {
            let previous = if self.used == 0 {
                0
            } else {
                self.bytes.pop().unwrap() as u64
            };
            let total = n + self.used as u32;
            let word = (previous << 56) | ((value & ((1u64 << n) - 1)) << (64 - total));
            self.bytes
                .extend_from_slice(&word.to_be_bytes()[..total.div_ceil(8) as usize]);
            self.used = (total & 7) as u8;
            return;
        }
        assert!(n <= 64);
        let mut left = n;
        while left != 0 {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let take = left.min((8 - self.used) as u32);
            let end = self.bytes.len() - 1;
            self.bytes[end] |= (((value >> (left - take)) & ((1 << take) - 1)) as u8)
                << (8 - self.used - take as u8);
            self.used = (self.used + take as u8) & 7;
            left -= take;
        }
    }
    /// One Rice code; the reference for [`Self::rice_block`] in tests.
    #[cfg(test)]
    pub fn rice(&mut self, value: u32, k: u32) {
        let zeros = value >> k;
        let n = zeros as u64 + k as u64 + 1;
        let suffix = (1u64 << k) | (value as u64 & ((1u64 << k) - 1));
        if n <= 56 {
            // Quotient zeros, terminator and remainder share one append.
            self.put(n as u32, suffix);
        } else {
            self.unary(zeros);
            self.put(k, value as u64);
        }
    }
    /// Append the Rice codes of a whole residual block. The exact bit count
    /// is summed first, so the output is sized (zero-filled) once; then each
    /// code is ORed into a left-aligned word that is stored as eight bytes
    /// unconditionally, and the write position advances by whole bytes. No
    /// branch depends on where codes cross bytes, and the zero quotient bits
    /// of a long code are already in the buffer, so they are skipped.
    pub fn rice_block(&mut self, residual: &[u32], k: u32) {
        assert!(k <= 30);
        let quotients: u64 = residual.iter().map(|&u| (u >> k) as u64).sum();
        let start = self.bytes.len() as u64 * 8 - ((8 - self.used as u64) & 7);
        let end = start + quotients + residual.len() as u64 * (k as u64 + 1);
        let length = end.div_ceil(8) as usize;
        let pos = (start / 8) as usize;
        let used = (start % 8) as u32;
        let word = if used == 0 {
            0
        } else {
            (self.bytes[pos] as u64) << 56
        };
        self.bytes.resize(length + 8, 0);
        #[cfg(target_arch = "x86_64")]
        if crate::kernels::bit_ops() {
            // SAFETY: LZCNT/BMI1/BMI2 were detected at runtime; the body is
            // the same safe Rust (variable shifts become SHLX/SHRX).
            unsafe { rice_codes_bit_ops(&mut self.bytes, residual, k, pos, used, word) };
        } else {
            rice_codes(&mut self.bytes, residual, k, pos, used, word);
        }
        #[cfg(not(target_arch = "x86_64"))]
        rice_codes(&mut self.bytes, residual, k, pos, used, word);
        self.bytes.truncate(length);
        self.used = (end % 8) as u8;
    }
    #[cfg(test)]
    pub fn unary(&mut self, zeros: u32) {
        // Aligned bulk zero fill avoids a loop per residual quotient bit.
        let mut left = zeros;
        if self.used != 0 {
            let n = left.min((8 - self.used) as u32);
            self.put(n, 0);
            left -= n;
        }
        self.bytes.resize(self.bytes.len() + (left / 8) as usize, 0);
        self.put(left % 8, 0);
        self.put(1, 1);
    }
    pub fn align(&mut self) {
        if self.used != 0 {
            self.put((8 - self.used) as u32, 0);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "lzcnt,bmi1,bmi2")]
unsafe fn rice_codes_bit_ops(
    out: &mut [u8],
    residual: &[u32],
    k: u32,
    pos: usize,
    used: u32,
    word: u64,
) {
    rice_codes(out, residual, k, pos, used, word)
}

/// The code loop of [`BeWriter::rice_block`]: `word` holds the `used` bits
/// already written into byte `pos`, left-aligned. Two codes are joined in a
/// register and stored together when they fit 56 bits (the common case).
#[inline(always)]
fn rice_codes(out: &mut [u8], residual: &[u32], k: u32, pos: usize, used: u32, word: u64) {
    let suffix = 1u64 << k;
    let mask = suffix - 1;
    let mut w = Words {
        out,
        pos,
        used,
        word,
    };
    // Four codes per store when they fit 56 bits (short codes, the common
    // case for 16-bit material), otherwise two at a time. With k above 12
    // four codes never fit (24-bit material), so pairs are used directly.
    let quad_k = if k <= 12 { residual.len() } else { 0 };
    let (quads, _) = residual[..quad_k].as_chunks::<4>();
    let tail = &residual[quads.len() * 4..];
    for &[a, b, c, d] in quads {
        let [a, b, c, d] = [a, b, c, d].map(u64::from);
        let len = |u: u64| (u >> k) + 1 + k as u64;
        let (la, lb, lc, ld) = (len(a), len(b), len(c), len(d));
        if la + lb + lc + ld <= 56 {
            let code = |u: u64| suffix | (u & mask);
            let joined = (((((code(a) << lb) | code(b)) << lc) | code(c)) << ld) | code(d);
            w.put(joined, la + lb + lc + ld);
        } else {
            w.pair_cold(a, b, k);
            w.pair_cold(c, d, k);
        }
    }
    let (pairs, rest) = tail.as_chunks::<2>();
    for &[a, b] in pairs {
        w.pair(a as u64, b as u64, k);
    }
    for &u in rest {
        let u = u as u64;
        w.code(u, suffix | (u & mask), (u >> k) + 1 + k as u64, k);
    }
}

/// Output position of [`rice_codes`]; the buffer is zero-filled with eight
/// bytes of slack past the last code.
struct Words<'a> {
    out: &'a mut [u8],
    pos: usize,
    used: u32,
    word: u64,
}
impl Words<'_> {
    /// Append `len` (at most 56) bits; store the word and advance by the
    /// whole bytes completed.
    #[inline(always)]
    fn put(&mut self, bits: u64, len: u64) {
        self.word |= bits << (64 - self.used as u64 - len);
        self.used += len as u32;
        self.out[self.pos..self.pos + 8].copy_from_slice(&self.word.to_be_bytes());
        let advance = self.used >> 3;
        self.pos += advance as usize;
        self.word <<= advance * 8;
        self.used &= 7;
    }
    /// [`Self::pair`] out of line, for the rare quads that do not fit:
    /// keeps the quad loop small.
    #[cold]
    #[inline(never)]
    fn pair_cold(&mut self, a: u64, b: u64, k: u32) {
        self.pair(a, b, k)
    }
    /// The Rice codes of `a` and `b`: one store when they fit 56 bits.
    #[inline(always)]
    fn pair(&mut self, a: u64, b: u64, k: u32) {
        let suffix = 1u64 << k;
        let mask = suffix - 1;
        let (ca, la) = (suffix | (a & mask), (a >> k) + 1 + k as u64);
        let (cb, lb) = (suffix | (b & mask), (b >> k) + 1 + k as u64);
        if la + lb <= 56 {
            self.put((ca << lb) | cb, la + lb);
        } else {
            self.code(a, ca, la, k);
            self.code(b, cb, lb, k);
        }
    }
    /// The Rice code `code` of `len` bits for value `u`.
    #[inline(always)]
    fn code(&mut self, u: u64, code: u64, len: u64, k: u32) {
        if len <= 56 {
            self.put(code, len);
            return;
        }
        // Store what is pending, skip the zeros (already in the buffer),
        // then the one and the remainder (at most 31 bits).
        self.out[self.pos..self.pos + 8].copy_from_slice(&self.word.to_be_bytes());
        let zeros = self.used as u64 + (u >> k);
        if zeros >= 8 {
            self.pos += (zeros / 8) as usize;
            self.word = 0;
        }
        self.used = (zeros % 8) as u32;
        self.put(code, k as u64 + 1);
    }
}

const fn crc8_table() -> [u8; 256] {
    let mut t = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut a = i as u8;
        let mut j = 0;
        while j < 8 {
            a = (a << 1) ^ if a & 0x80 != 0 { 7 } else { 0 };
            j += 1;
        }
        t[i] = a;
        i += 1;
    }
    t
}
const CRC8: [u8; 256] = crc8_table();

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for b in data {
        crc = CRC8[(crc ^ b) as usize];
    }
    crc
}
pub use crate::crc16::crc16;
pub use crate::crc32::crc32;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_reader_matches_bit_reference_and_bounds() {
        for length in 0..=97 {
            let data: Vec<u8> = (0..length).map(|i| (i * 131 + 197) as u8).collect();
            let mut reader = Lsb::new(&data);
            let mut position = 0;
            for n in (0..=32).cycle().take(150) {
                let expected = if position + n as usize <= length * 8 {
                    let mut value = 0u32;
                    for bit in 0..n {
                        value |= (((data[(position + bit as usize) / 8]
                            >> ((position + bit as usize) % 8))
                            & 1) as u32)
                            << bit;
                    }
                    Some(value)
                } else {
                    None
                };
                let actual = reader.read(n).ok();
                assert_eq!(actual, expected);
                if actual.is_none() {
                    break;
                }
                position += n as usize;
            }
        }
        for ones in 0..=129 {
            let mut data = vec![0u8; (ones + 1usize).div_ceil(8)];
            for i in 0..ones {
                data[i / 8] |= 1 << (i % 8);
            }
            assert_eq!(Lsb::new(&data).ones(ones as u32 + 1).unwrap(), ones as u32);
            if ones > 0 {
                assert!(Lsb::new(&data).ones(ones as u32).is_err());
            }
        }
        assert!(Lsb::new(&[255; 8]).ones(256).is_err());
    }
    #[test]
    fn word_writer_matches_bit_reference() {
        let mut writer = BeWriter::new();
        let mut reference = Vec::<bool>::new();
        for offset in 0..8 {
            for n in 0..=64 {
                let value = 0x8fedcba987654321u64.rotate_left(n);
                writer.put(offset, value);
                for i in (0..offset).rev() {
                    reference.push(value >> i & 1 != 0);
                }
                writer.put(n, value);
                for i in (0..n).rev() {
                    reference.push(value >> i & 1 != 0);
                }
            }
            for k in [0, 1, 15, 30] {
                for quotient in [0, 1, 7, 31, 56, 65] {
                    let value = ((quotient as u64) << k).min(u32::MAX as u64) as u32;
                    writer.rice(value, k);
                    reference.extend(std::iter::repeat_n(false, (value >> k) as usize));
                    reference.push(true);
                    for i in (0..k).rev() {
                        reference.push(value >> i & 1 != 0);
                    }
                }
            }
        }
        writer.align();
        let mut expected = vec![0u8; reference.len().div_ceil(8)];
        for (i, bit) in reference.into_iter().enumerate() {
            if bit {
                expected[i / 8] |= 1 << (7 - i % 8);
            }
        }
        assert_eq!(writer.bytes, expected);
    }
    #[test]
    fn batched_rice_and_sliced_crc_match_references() {
        for offset in 0..8 {
            for k in 0..=30 {
                let residual: Vec<u32> = [0, 1, 7, 31, 56, 63, 64, 65, 127, 259]
                    .into_iter()
                    .map(|q| {
                        (((q as u64) << k) | (0x89abcdefu64 & ((1 << k) - 1))).min(u32::MAX as u64)
                            as u32
                    })
                    .collect();
                let mut reference = BeWriter::new();
                let mut actual = BeWriter::new();
                reference.put(offset, 0x55);
                actual.put(offset, 0x55);
                for _ in 0..3 {
                    for &r in &residual {
                        reference.rice(r, k);
                    }
                    actual.rice_block(&residual, k);
                    reference.put(11, 0x765);
                    actual.put(11, 0x765);
                }
                reference.align();
                actual.align();
                assert_eq!(actual.bytes, reference.bytes, "offset={offset} k={k}");
            }
        }
        let data: Vec<u8> = (0..1025).map(|i| (i * 137 + 29) as u8).collect();
        for offset in 0..8 {
            for len in 0..=1024 - offset {
                let input = &data[offset..offset + len];
                let mut reference = 0u16;
                for &b in input {
                    reference ^= (b as u16) << 8;
                    for _ in 0..8 {
                        reference =
                            (reference << 1) ^ if reference & 0x8000 != 0 { 0x8005 } else { 0 };
                    }
                }
                assert_eq!(crc16(input), reference);
            }
        }
    }
}
