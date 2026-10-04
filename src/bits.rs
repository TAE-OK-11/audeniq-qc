use crate::{Error, Result};

/// Bounds-checked LSB-first bit reader for FFmpeg-derived TTA/WavPack paths.
pub struct LeBits<'a> {
    data: &'a [u8],
    next: usize,
    cache: u64,
    available: u32,
}
impl<'a> LeBits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            next: 0,
            cache: 0,
            available: 0,
        }
    }
    fn refill(&mut self, required: u32) -> Result<()> {
        if self.available < required {
            let bytes = ((64 - self.available) / 8) as usize;
            let n = bytes.min(self.data.len() - self.next);
            let mut word = [0u8; 8];
            word[..n].copy_from_slice(&self.data[self.next..self.next + n]);
            self.cache |= u64::from_le_bytes(word) << self.available;
            self.available += n as u32 * 8;
            self.next += n;
        }
        if self.available < required {
            return Err(Error::Invalid("truncated bitstream"));
        }
        Ok(())
    }
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if n > 32 {
            return Err(Error::Invalid("truncated bitstream"));
        }
        self.refill(n)?;
        let value = (self.cache & ((1u64 << n) - 1)) as u32;
        self.cache >>= n;
        self.available -= n;
        Ok(value)
    }
    pub fn unary_ones(&mut self, max: u32) -> Result<u32> {
        let mut n = 0;
        loop {
            self.refill(1)?;
            let ones = self.cache.trailing_ones().min(self.available);
            if ones != 0 && ones >= max.saturating_sub(n) {
                return Err(Error::Invalid("unbounded unary code"));
            }
            n += ones;
            if ones < self.available {
                let consumed = ones + 1;
                self.cache = if consumed == 64 {
                    0
                } else {
                    self.cache >> consumed
                };
                self.available -= consumed;
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

const fn crc_tables() -> ([u8; 256], [u16; 256]) {
    let mut c8 = [0; 256];
    let mut c16 = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut a = i as u8;
        let mut b = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            a = (a << 1) ^ if a & 0x80 != 0 { 7 } else { 0 };
            b = (b << 1) ^ if b & 0x8000 != 0 { 0x8005 } else { 0 };
            j += 1;
        }
        c8[i] = a;
        c16[i] = b;
        i += 1;
    }
    (c8, c16)
}
const CRC: ([u8; 256], [u16; 256]) = crc_tables();

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for b in data {
        crc = CRC.0[(crc ^ b) as usize];
    }
    crc
}
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for b in data {
        crc = (crc << 8) ^ CRC.1[((crc >> 8) as u8 ^ b) as usize];
    }
    crc
}
pub fn crc32(data: &[u8]) -> u32 {
    // IEEE CRC32, not the incompatible x86 SSE4.2 CRC32C polynomial.
    // crc32fast selects PCLMULQDQ on x86 or CRC instructions on AArch64.
    crc32fast::hash(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_reader_matches_bit_reference_and_bounds() {
        for length in 0..=97 {
            let data: Vec<u8> = (0..length).map(|i| (i * 131 + 197) as u8).collect();
            let mut reader = LeBits::new(&data);
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
            assert_eq!(
                LeBits::new(&data).unary_ones(ones as u32 + 1).unwrap(),
                ones as u32
            );
            if ones > 0 {
                assert!(LeBits::new(&data).unary_ones(ones as u32).is_err());
            }
        }
        assert!(LeBits::new(&[255; 8]).unary_ones(256).is_err());
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
}
