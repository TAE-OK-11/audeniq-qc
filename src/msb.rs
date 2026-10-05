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
    /// The whole input.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    /// Continue reading at bit `pos` (at most the input length).
    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() * 8 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        self.cache = 0;
        self.available = 0;
        self.loaded = pos / 8;
        self.pos = pos & !7;
        self.get((pos & 7) as u32)?;
        Ok(())
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
    /// Decode a complete FLAC Rice value from one cache word in the common
    /// case. Bounds/range checks and a bounded cross-word fallback remain.
    #[inline]
    pub fn rice_signed(&mut self, k: u32) -> Result<i32> {
        if k > 31 {
            return Err(Error::Invalid("FLAC Rice width"));
        }
        if self.available < k + 1 {
            self.fill();
        }
        let q = self.cache.leading_zeros();
        let consumed = q + 1 + k;
        let value = if consumed <= self.available {
            let tail = if k == 0 {
                0
            } else {
                (self.cache >> (64 - consumed)) & ((1u64 << k) - 1)
            };
            self.skip(consumed);
            ((q as u64) << k) | tail
        } else {
            let cap = (u32::MAX >> k).saturating_add(1);
            let q = self.unary(false, cap)?;
            if q == cap {
                return Err(Error::Invalid("FLAC residual range"));
            }
            ((q as u64) << k) | self.get(k)? as u64
        };
        if value > u32::MAX as u64 {
            return Err(Error::Invalid("FLAC residual range"));
        }
        Ok(((value >> 1) as i32) ^ -((value & 1) as i32))
    }
    /// Decode a complete FLAC Rice partition. While at least eight unread
    /// bytes remain, refill with one unaligned big-endian load so that every
    /// code of up to 56 bits decodes from the cache without per-value refill
    /// or bounds branches. Bits beyond `available` are then the true following
    /// stream bits, so a later byte-wise fill ORs identical values. Long
    /// quotients and the final bytes use the checked per-value path.
    // Always inlined so the caller's target features (LZCNT/BMI2) apply.
    #[inline(always)]
    pub fn rice_run(&mut self, k: u32, dst: &mut [i32]) -> Result<()> {
        if k > 31 {
            return Err(Error::Invalid("FLAC Rice width"));
        }
        let data = self.data;
        let mut cache = self.cache;
        let mut available = self.available;
        let mut loaded = self.loaded;
        let mut i = 0;
        let mut range = 0u64;
        'refill: while data.len() - loaded >= 8 {
            // available <= 63 here; whole bytes already in the cache add none.
            let word = u64::from_be_bytes(data[loaded..loaded + 8].try_into().unwrap());
            cache |= word >> available;
            let bytes = (63 - available) >> 3;
            loaded += bytes as usize;
            available += bytes * 8;
            // available is now 56..=63: decode every code that fits.
            loop {
                let Some(out) = dst.get_mut(i) else {
                    break 'refill;
                };
                let q = cache.leading_zeros();
                let consumed = q + 1 + k;
                if consumed > available {
                    if available >= 56 {
                        break 'refill; // long quotient: checked path
                    }
                    continue 'refill;
                }
                // q + 1 <= available <= 63, so neither shift reaches 64.
                let rest = cache << (q + 1);
                let tail = (rest >> 1) >> (63 - k);
                let value = ((q as u64) << k) | tail;
                range |= value >> 32;
                *out = ((value >> 1) as i32) ^ -((value & 1) as i32);
                cache = rest << k;
                available -= consumed;
                i += 1;
            }
        }
        self.cache = cache;
        self.available = available;
        self.loaded = loaded;
        self.pos = loaded * 8 - available as usize;
        if range != 0 {
            return Err(Error::Invalid("FLAC residual range"));
        }
        for v in &mut dst[i..] {
            *v = self.rice_signed(k)?;
        }
        Ok(())
    }
    /// Top the cache up to 56..=63 bits with one unaligned load when eight
    /// unread bytes remain (see `rice_run`); otherwise leave it to `fill`.
    #[inline(always)]
    pub fn refill(&mut self) {
        if self.data.len() - self.loaded >= 8 {
            let word =
                u64::from_be_bytes(self.data[self.loaded..self.loaded + 8].try_into().unwrap());
            self.cache |= word >> self.available;
            let bytes = (63 - self.available) >> 3;
            self.loaded += bytes as usize;
            self.available += bytes * 8;
        }
    }
    #[inline(always)]
    pub fn alac_scalar(&mut self, k: u32, bits: u32) -> Result<u32> {
        if k == 0 || k > 31 || bits > 32 {
            return Err(Error::Invalid("ALAC Rice width"));
        }
        if self.available < 9 {
            self.fill();
        }
        let q = (!self.cache).leading_zeros().min(9);
        let prefix = if q == 9 { 9 } else { q + 1 };
        let tail_bits = if q == 9 {
            bits
        } else if k == 1 {
            0
        } else {
            k
        };
        let required = prefix + tail_bits;
        if required <= self.available {
            let tail = if tail_bits == 0 {
                0
            } else {
                (self.cache >> (64 - required)) & ((1u64 << tail_bits) - 1)
            };
            let (consumed, value) = if q == 9 {
                (required, tail)
            } else if k == 1 {
                (prefix, q as u64)
            } else {
                (
                    prefix + if tail > 1 { k } else { k - 1 },
                    ((q as u64) << k) - q as u64 + tail.saturating_sub(1),
                )
            };
            if value > u32::MAX as u64 {
                return Err(Error::Invalid("ALAC scalar range"));
            }
            self.skip(consumed);
            return Ok(value as u32);
        }
        let q = self.unary(true, 9)?;
        if q == 9 {
            return self.get(bits);
        }
        if k == 1 {
            return Ok(q);
        }
        let tail = self.peek(k)?;
        let value = ((q as u64) << k) - q as u64 + (tail as u64).saturating_sub(1);
        self.get(if tail > 1 { k } else { k - 1 })?;
        u32::try_from(value).map_err(|_| Error::Invalid("ALAC scalar range"))
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
    fn fused_rice_reads_match_bitwise_reference_across_word_boundaries() {
        for offset in 0..64 {
            for k in 0..=31 {
                for q in [0u32, 1, 8, 63, 130] {
                    let value = (q as u64) << k;
                    if value > u32::MAX as u64 {
                        continue;
                    }
                    let mut w = crate::bits::BeWriter::new();
                    w.put(offset, 0);
                    w.unary(q);
                    w.put(k, 0);
                    w.align();
                    let mut b = Bits::new(&w.bytes);
                    for _ in 0..offset {
                        b.get(1).unwrap();
                    }
                    let expected = ((value >> 1) as i32) ^ -((value & 1) as i32);
                    assert_eq!(b.rice_signed(k).unwrap(), expected);
                    assert_eq!(b.pos, offset as usize + q as usize + 1 + k as usize);
                }
            }
        }
        for offset in 0..64 {
            for k in 1..=24 {
                for q in 0..=9 {
                    for tail in [0u64, 1, 2, 7] {
                        if k == 1 && tail != 0 {
                            continue;
                        }
                        let mut w = crate::bits::BeWriter::new();
                        w.put(offset, 0);
                        w.put(q, (1u64 << q) - 1);
                        let expected = if q == 9 {
                            w.put(24, tail);
                            tail
                        } else {
                            w.put(1, 0);
                            if k == 1 {
                                q as u64
                            } else {
                                let tail = tail & ((1 << k) - 1);
                                w.put(k, tail);
                                ((q as u64) << k) - q as u64 + tail.saturating_sub(1)
                            }
                        };
                        w.put(32, 0);
                        w.align();
                        let mut b = Bits::new(&w.bytes);
                        for _ in 0..offset {
                            b.get(1).unwrap();
                        }
                        assert_eq!(b.alac_scalar(k, 24).unwrap(), expected as u32);
                    }
                }
            }
        }
    }
    #[test]
    fn rice_runs_match_per_value_reads_and_reject_wide_values() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            seed = seed.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            z ^ (z >> 31)
        };
        for k in [0u32, 1, 4, 11, 14, 20, 30, 31] {
            for offset in [0u32, 3, 7, 13] {
                for count in [0usize, 1, 2, 9, 64, 300] {
                    let mut w = crate::bits::BeWriter::new();
                    w.put(offset, 5);
                    let mut values = Vec::new();
                    for i in 0..count {
                        // Mostly short codes, plus long quotients that need
                        // the checked fallback in the middle of a run.
                        let q = if i % 37 == 5 { 70 } else { next() % 6 };
                        let value = (q << k) | (next() & ((1u64 << k) - 1));
                        if value > u32::MAX as u64 {
                            continue;
                        }
                        w.unary(q as u32);
                        w.put(k, value);
                        values.push(((value >> 1) as i32) ^ -((value & 1) as i32));
                    }
                    w.put(13, 0x1abc);
                    w.align();
                    let mut fast = Bits::new(&w.bytes);
                    let mut slow = Bits::new(&w.bytes);
                    fast.get(offset).unwrap();
                    slow.get(offset).unwrap();
                    let mut out = vec![0; values.len()];
                    fast.rice_run(k, &mut out).unwrap();
                    for &v in &values {
                        assert_eq!(slow.rice_signed(k).unwrap(), v);
                    }
                    assert_eq!(out, values);
                    assert_eq!(fast.pos, slow.pos);
                    assert_eq!(fast.get(13).unwrap(), 0x1abc);
                    // Truncation inside the run must fail, never read past it.
                    let cut = slow.pos.saturating_sub(1) / 8;
                    if !values.is_empty() && cut * 8 >= offset as usize {
                        let mut b = Bits::new(&w.bytes[..cut]);
                        b.get(offset).unwrap();
                        assert!(b.rice_run(k, &mut out).is_err());
                    }
                }
            }
        }
        // Values above u32 are corrupt even when the code fits one word.
        let mut w = crate::bits::BeWriter::new();
        w.unary(3);
        w.put(31, 0);
        w.put(64, 0);
        let mut b = Bits::new(&w.bytes);
        assert!(b.rice_run(31, &mut [0]).is_err());
    }
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
