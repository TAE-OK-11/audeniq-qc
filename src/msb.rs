//! Bounded MSB reader. Cached words and CLZ unary scans, without input padding.
use crate::{Error, Result};
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pub pos: usize,
    cache: u64,
    available: u32,
    loaded: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            cache: 0,
            available: 0,
            loaded: 0,
        }
    }
    pub fn left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }
    #[inline]
    fn fill(&mut self) {
        let count = ((64 - self.available) / 8).min((self.data.len() - self.loaded) as u32);
        if count == 0 {
            return;
        }
        let word = if self.data.len() - self.loaded >= 8 {
            u64::from_be_bytes(self.data[self.loaded..self.loaded + 8].try_into().unwrap())
        } else {
            let mut bytes = [0u8; 8];
            let remaining = self.data.len() - self.loaded;
            bytes[..remaining].copy_from_slice(&self.data[self.loaded..]);
            u64::from_be_bytes(bytes)
        };
        self.cache |= (word & (u64::MAX << (64 - count * 8))) >> self.available;
        self.loaded += count as usize;
        self.available += count * 8;
    }
    #[inline]
    pub fn get(&mut self, n: u32) -> Result<u32> {
        if n > 32 {
            return Err(Error::Invalid("bit width"));
        }
        if n == 0 {
            return Ok(0);
        }
        if self.left() < n as usize {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        if self.available < n {
            self.fill();
        }
        let x = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.available -= n;
        self.pos += n as usize;
        Ok(x)
    }
    #[inline]
    pub fn signed(&mut self, n: u32) -> Result<i32> {
        if n == 0 {
            return Ok(0);
        }
        let x = self.get(n)?;
        Ok(((x << (32 - n)) as i32) >> (32 - n))
    }
    /// Consume up to limit equal bits; consume the terminator only if found.
    #[inline]
    pub fn unary(&mut self, ones: bool, limit: u32) -> Result<u32> {
        let mut count = 0;
        loop {
            if count == limit {
                return Ok(count);
            }
            if self.available == 0 {
                self.fill();
            }
            if self.available == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            let scan = if ones { !self.cache } else { self.cache };
            let n = scan.leading_zeros().min(self.available).min(limit - count);
            self.skip(n);
            count += n;
            if count == limit {
                return Ok(count);
            }
            if self.available != 0 {
                self.skip(1);
                return Ok(count);
            }
        }
    }
    #[inline]
    fn skip(&mut self, n: u32) {
        self.cache = self.cache.checked_shl(n).unwrap_or(0);
        self.available -= n;
        self.pos += n as usize;
    }
    #[inline]
    pub fn peek(&mut self, n: u32) -> Result<u32> {
        if n > 32 || self.left() < n as usize {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        if n == 0 {
            return Ok(0);
        }
        if self.available < n {
            self.fill();
        }
        Ok((self.cache >> (64 - n)) as u32)
    }
    pub fn align_zero(&mut self) -> Result<()> {
        let n = (8 - self.pos % 8) % 8;
        if self.get(n as u32)? != 0 {
            return Err(Error::Invalid("nonzero bit padding"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_reads_match_independent_bits_at_all_boundaries() {
        let data: Vec<u8> = (0..259).map(|i| (i * 73 + 19) as u8).collect();
        for offset in 0..64 {
            for width in 0..=32 {
                let mut b = Bits::new(&data);
                for _ in 0..offset {
                    b.get(1).unwrap();
                }
                for _ in 0..16 {
                    let mut expected = 0u32;
                    for i in b.pos..b.pos + width as usize {
                        expected = (expected << 1) | ((data[i / 8] >> (7 - i % 8)) & 1) as u32;
                    }
                    assert_eq!(b.peek(width).unwrap(), expected);
                    assert_eq!(b.get(width).unwrap(), expected);
                }
            }
        }
        for len in 0..=9 {
            let mut b = Bits::new(&data[..len]);
            for _ in 0..len * 8 {
                b.get(1).unwrap();
            }
            assert!(b.get(1).is_err());
            assert!(b.peek(1).is_err());
            assert!(b.unary(false, 1).is_err());
        }
    }
    #[test]
    fn unary_scans_bound_runs_and_cross_cache_words() {
        for ones in [false, true] {
            for len in [0, 1, 8, 9, 63, 64, 65, 127, 130] {
                let mut data = vec![if ones { 255 } else { 0 }; len / 8 + 1];
                if ones {
                    data[len / 8] &= !(1 << (7 - len % 8));
                } else {
                    data[len / 8] |= 1 << (7 - len % 8);
                }
                let mut b = Bits::new(&data);
                assert_eq!(b.unary(ones, 200).unwrap(), len as u32);
                assert_eq!(b.pos, len + 1);
                let mut b = Bits::new(&data);
                let cap = (len / 2) as u32;
                assert_eq!(b.unary(ones, cap).unwrap(), cap);
                assert_eq!(b.pos, cap as usize);
            }
        }
    }
}
