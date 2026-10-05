//! FLAC frame CRC-16 (polynomial x^16 + x^15 + x^2 + 1, MSB first, initial
//! value 0, no final inversion).
//!
//! Inputs of 64 bytes or more are folded with carry-less multiplication, as
//! the IEEE CRC-32 in `crc32.rs`, but in the non-reflected bit order: each
//! 16-byte block is byte-reversed so that bit i of the 128-bit lane is the
//! coefficient of x^i, and a lane is moved forward by D bits by multiplying
//! its halves by x^(D+64) mod P and x^D mod P. Four lanes run in parallel and
//! are then combined. The final 128-bit residue has the same remainder modulo
//! P as the message before it, so the CRC of its 16 big-endian bytes followed
//! by the remaining tail bytes is the CRC of the whole input; that last part
//! goes through the slicing-by-8 table. The fold constants are computed at
//! compile time from the polynomial.

const POLY: u32 = 0x1_8005;

const fn byte_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut b = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            b = (b << 1) ^ if b & 0x8000 != 0 { POLY as u16 } else { 0 };
            j += 1;
        }
        t[i] = b;
        i += 1;
    }
    t
}

const fn slices() -> [[u16; 256]; 8] {
    let mut tables = [[0; 256]; 8];
    tables[0] = byte_table();
    let mut slice = 1;
    while slice < 8 {
        let mut i = 0;
        while i < 256 {
            let previous = tables[slice - 1][i];
            tables[slice][i] = (previous << 8) ^ tables[0][(previous >> 8) as usize];
            i += 1;
        }
        slice += 1;
    }
    tables
}
static TABLE: [[u16; 256]; 8] = slices();

/// Advance `crc` over `data` with slicing-by-8.
fn table_update(mut crc: u16, data: &[u8]) -> u16 {
    let t = &TABLE;
    let (blocks, tail) = data.as_chunks::<8>();
    for b in blocks {
        crc = t[7][((crc >> 8) as u8 ^ b[0]) as usize]
            ^ t[6][(crc as u8 ^ b[1]) as usize]
            ^ t[5][b[2] as usize]
            ^ t[4][b[3] as usize]
            ^ t[3][b[4] as usize]
            ^ t[2][b[5] as usize]
            ^ t[1][b[6] as usize]
            ^ t[0][b[7] as usize];
    }
    for &b in tail {
        crc = (crc << 8) ^ t[0][((crc >> 8) as u8 ^ b) as usize];
    }
    crc
}

/// x^n mod P.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const fn x_pow_mod(n: u32) -> u64 {
    let mut r = 1u32;
    let mut i = 0;
    while i < n {
        r <<= 1;
        if r & 0x1_0000 != 0 {
            r ^= POLY;
        }
        i += 1;
    }
    r as u64
}

/// (x^D mod P, x^(D+64) mod P): multipliers for the low and high halves of a
/// lane moved forward by D bits.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const fn keys(d: u32) -> (u64, u64) {
    (x_pow_mod(d), x_pow_mod(d + 64))
}
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const D128: (u64, u64) = keys(128);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const D256: (u64, u64) = keys(256);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const D384: (u64, u64) = keys(384);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const D512: (u64, u64) = keys(512);

/// Inputs shorter than this go through the table.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const MIN_FOLD: usize = 64;

/// CRC-16 of `data`.
pub fn crc16(data: &[u8]) -> u16 {
    #[cfg(target_arch = "x86_64")]
    if data.len() >= MIN_FOLD
        && std::is_x86_feature_detected!("pclmulqdq")
        && std::is_x86_feature_detected!("ssse3")
    {
        // SAFETY: both instruction sets were detected at runtime.
        return unsafe { x86::crc16(data) };
    }
    #[cfg(target_arch = "aarch64")]
    if data.len() >= MIN_FOLD && std::arch::is_aarch64_feature_detected!("pmull") {
        // SAFETY: PMULL (and NEON) were detected at runtime.
        return unsafe { arm::crc16(data) };
    }
    table_update(0, data)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{table_update, D128, D256, D384, D512};
    use std::arch::x86_64::*;

    #[inline(always)]
    unsafe fn keys(k: (u64, u64)) -> __m128i {
        _mm_set_epi64x(k.1 as i64, k.0 as i64)
    }
    #[inline(always)]
    unsafe fn reverse() -> __m128i {
        _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15)
    }
    /// The 16 bytes at the start of `data` as a polynomial (first bit is the
    /// coefficient of x^127).
    #[inline(always)]
    unsafe fn load(data: &[u8], reverse: __m128i) -> __m128i {
        _mm_shuffle_epi8(_mm_loadu_si128(data.as_ptr().cast()), reverse)
    }
    /// a * x^D + b for the distance D of `k`.
    #[inline(always)]
    unsafe fn fold(a: __m128i, b: __m128i, k: __m128i) -> __m128i {
        _mm_xor_si128(
            _mm_xor_si128(
                _mm_clmulepi64_si128(a, k, 0x00),
                _mm_clmulepi64_si128(a, k, 0x11),
            ),
            b,
        )
    }

    #[target_feature(enable = "pclmulqdq,ssse3")]
    pub(super) unsafe fn crc16(mut data: &[u8]) -> u16 {
        let r = reverse();
        let mut x0 = load(data, r);
        let mut x1 = load(&data[16..], r);
        let mut x2 = load(&data[32..], r);
        let mut x3 = load(&data[48..], r);
        data = &data[64..];
        let k512 = keys(D512);
        while data.len() >= 64 {
            x0 = fold(x0, load(data, r), k512);
            x1 = fold(x1, load(&data[16..], r), k512);
            x2 = fold(x2, load(&data[32..], r), k512);
            x3 = fold(x3, load(&data[48..], r), k512);
            data = &data[64..];
        }
        let mut x = _mm_xor_si128(
            fold(x0, x3, keys(D384)),
            fold(x1, fold(x2, _mm_setzero_si128(), keys(D128)), keys(D256)),
        );
        let k128 = keys(D128);
        while data.len() >= 16 {
            x = fold(x, load(data, r), k128);
            data = &data[16..];
        }
        let mut residue = [0u8; 16];
        _mm_storeu_si128(residue.as_mut_ptr().cast(), _mm_shuffle_epi8(x, r));
        table_update(table_update(0, &residue), data)
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::{table_update, D128, D256, D384, D512};
    use std::arch::aarch64::*;

    /// The 16 bytes at the start of `data` as a polynomial (first bit is the
    /// coefficient of x^127).
    #[inline(always)]
    unsafe fn load(data: &[u8]) -> uint8x16_t {
        let v = vrev64q_u8(vld1q_u8(data.as_ptr()));
        vextq_u8::<8>(v, v)
    }
    /// a * x^D + b for the distance D of `k`.
    #[inline(always)]
    unsafe fn fold(a: uint8x16_t, b: uint8x16_t, k: (u64, u64)) -> uint8x16_t {
        let a = vreinterpretq_p64_u8(a);
        let lo = vmull_p64(vgetq_lane_p64::<0>(a), k.0);
        let hi = vmull_p64(vgetq_lane_p64::<1>(a), k.1);
        veorq_u8(
            veorq_u8(vreinterpretq_u8_p128(lo), vreinterpretq_u8_p128(hi)),
            b,
        )
    }

    #[target_feature(enable = "neon,aes")]
    pub(super) unsafe fn crc16(mut data: &[u8]) -> u16 {
        let mut x0 = load(data);
        let mut x1 = load(&data[16..]);
        let mut x2 = load(&data[32..]);
        let mut x3 = load(&data[48..]);
        data = &data[64..];
        while data.len() >= 64 {
            x0 = fold(x0, load(data), D512);
            x1 = fold(x1, load(&data[16..]), D512);
            x2 = fold(x2, load(&data[32..]), D512);
            x3 = fold(x3, load(&data[48..]), D512);
            data = &data[64..];
        }
        let mut x = veorq_u8(
            fold(x0, x3, D384),
            fold(x1, fold(x2, vdupq_n_u8(0), D128), D256),
        );
        while data.len() >= 16 {
            x = fold(x, load(data), D128);
            data = &data[16..];
        }
        let v = vrev64q_u8(x);
        let mut residue = [0u8; 16];
        vst1q_u8(residue.as_mut_ptr(), vextq_u8::<8>(v, v));
        table_update(table_update(0, &residue), data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-at-a-time definition, independent of the tables and folding.
    fn bitwise(data: &[u8]) -> u16 {
        let mut crc = 0u16;
        for &b in data {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                crc = (crc << 1) ^ if crc & 0x8000 != 0 { 0x8005 } else { 0 };
            }
        }
        crc
    }

    #[test]
    fn every_length_and_offset_matches_independent_code() {
        assert_eq!(crc16(b"123456789"), 0xfee8);
        let mut seed = 0x9e37_79b9u32;
        let data: Vec<u8> = (0..20000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        for offset in 0..4 {
            for len in (0..1200).chain([2047, 2048, 4096, 4607, 8191, 8192, 19995]) {
                let part = &data[offset..offset + len];
                let expected = bitwise(part);
                assert_eq!(crc16(part), expected, "{offset} {len}");
                assert_eq!(table_update(0, part), expected, "table {offset} {len}");
            }
        }
        // All-ones and all-zeros blocks exercise every fold constant bit.
        for fill in [0u8, 0xff] {
            for len in [64, 80, 128, 1000] {
                let part = vec![fill; len];
                assert_eq!(crc16(&part), bitwise(&part));
            }
        }
    }
}
