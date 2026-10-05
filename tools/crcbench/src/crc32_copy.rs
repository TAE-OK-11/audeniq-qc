//! IEEE 802.3 CRC-32 (reflected polynomial 0xEDB88320, initial and final
//! value inverted), as stored in TTA headers and used for the frame-copy
//! round-trip check.
//!
//! Long inputs are folded with carry-less multiplication (Gopal et al.,
//! "Fast CRC computation for generic polynomials using PCLMULQDQ", Intel
//! 2009): 128-bit lanes of the message are multiplied forward by constants
//! x^(D±32) mod P and added to the data D bits later, which keeps the
//! remainder unchanged. With AVX-512 VPCLMULQDQ, sixteen lanes are folded at
//! once; with PCLMULQDQ (x86) or PMULL (AArch64), four. At the end the lanes
//! are combined in a tree (independent folds by their distances to the last
//! lane) rather than one after another, and the final 128-bit residue, which
//! has the same remainder as everything before it, is run through the table
//! (or the AArch64 CRC32 instructions) together with the last few bytes.
//! Short inputs use slicing-by-8 tables. The CPU features are checked once
//! per hasher; every engine gives the same value, which the tests compare
//! with independent code for all lengths, offsets and split points.

const fn tables() -> [[u32; 256]; 8] {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xedb8_8320
            } else {
                c >> 1
            };
            bit += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut slice = 1;
    while slice < 8 {
        let mut i = 0;
        while i < 256 {
            let previous = t[slice - 1][i];
            t[slice][i] = (previous >> 8) ^ t[0][(previous & 0xff) as usize];
            i += 1;
        }
        slice += 1;
    }
    t
}
static TABLE: [[u32; 256]; 8] = tables();

/// Advance the (inverted) register `c` over `data` with slicing-by-8.
fn table_update(mut c: u32, data: &[u8]) -> u32 {
    let t = &TABLE;
    let (blocks, tail) = data.as_chunks::<8>();
    for b in blocks {
        let v = c ^ u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        c = t[7][(v & 0xff) as usize]
            ^ t[6][((v >> 8) & 0xff) as usize]
            ^ t[5][((v >> 16) & 0xff) as usize]
            ^ t[4][(v >> 24) as usize]
            ^ t[3][b[4] as usize]
            ^ t[2][b[5] as usize]
            ^ t[1][b[6] as usize]
            ^ t[0][b[7] as usize];
    }
    for &b in tail {
        c = t[0][((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c
}

/// Fold constants (x^(D+32) mod P, x^(D-32) mod P), bit-reflected and
/// shifted left by one, for a fold forward by D bits.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod k {
    pub const D128: (u64, u64) = (0x1_7519_97d0, 0x0_ccaa_009e);
    pub const D256: (u64, u64) = (0x0_f1da_05aa, 0x1_5a54_6366);
    pub const D384: (u64, u64) = (0x0_3db1_ecdc, 0x1_7435_9406);
    pub const D512: (u64, u64) = (0x1_5444_2bd4, 0x1_c6e4_1596);
    /// x^64 mod P, the reflected polynomial and floor(x^64 / P), for the
    /// final Barrett reduction.
    #[cfg(target_arch = "x86_64")]
    pub const D64: i64 = 0x1_63cd_6124;
    #[cfg(target_arch = "x86_64")]
    pub const P: u64 = 0x1_db71_0641;
    #[cfg(target_arch = "x86_64")]
    pub const MU: u64 = 0x1_f701_1641;
    #[cfg(target_arch = "x86_64")]
    pub const D768: (u64, u64) = (0x0_df06_8dc2, 0x1_8cb4_4e58);
    #[cfg(target_arch = "x86_64")]
    pub const D1024: (u64, u64) = (0x1_e88e_f372, 0x1_4a7f_e880);
    #[cfg(target_arch = "x86_64")]
    pub const D1536: (u64, u64) = (0x1_821d_8bc0, 0x1_2e95_8ac4);
    #[cfg(target_arch = "x86_64")]
    pub const D2048: (u64, u64) = (0x1_1542_778a, 0x1_322d_1430);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Engine {
    Portable,
    #[cfg(target_arch = "x86_64")]
    Pclmul,
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "x86_64")]
    Avx512,
    #[cfg(target_arch = "aarch64")]
    Pmull,
}

impl Engine {
    pub(crate) fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("pclmulqdq") && std::is_x86_feature_detected!("sse4.1") {
            if std::is_x86_feature_detected!("vpclmulqdq") && std::is_x86_feature_detected!("avx2")
            {
                if std::is_x86_feature_detected!("avx512f") {
                    return Self::Avx512;
                }
                return Self::Avx2;
            }
            return Self::Pclmul;
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("pmull")
            && std::arch::is_aarch64_feature_detected!("crc")
        {
            return Self::Pmull;
        }
        Self::Portable
    }
}

/// Running CRC-32.
#[derive(Clone)]
pub(crate) struct Crc32 {
    /// Inverted register.
    c: u32,
    engine: Engine,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub(crate) fn new() -> Self {
        Self::with_engine(Engine::detect())
    }
    pub(crate) fn with_engine(engine: Engine) -> Self {
        Self { c: !0, engine }
    }
    pub(crate) fn update(&mut self, data: &[u8]) {
        // SAFETY: each engine is selected only when `Engine::detect` found
        // the instructions it uses.
        self.c = unsafe {
            match self.engine {
                Engine::Portable => table_update(self.c, data),
                #[cfg(target_arch = "x86_64")]
                Engine::Pclmul => x86::update_pclmul(self.c, data),
                #[cfg(target_arch = "x86_64")]
                Engine::Avx2 => x86::update_avx2(self.c, data),
                #[cfg(target_arch = "x86_64")]
                Engine::Avx512 => x86::update_avx512(self.c, data),
                #[cfg(target_arch = "aarch64")]
                Engine::Pmull => arm::update_pmull(self.c, data),
            }
        };
    }
    pub(crate) fn finalize(&self) -> u32 {
        !self.c
    }
}

/// CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finalize()
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{k, table_update};
    use std::arch::x86_64::*;

    /// Inputs shorter than this go through the table.
    const MIN_FOLD: usize = 64;
    /// Inputs at least this long use four 512-bit accumulators.
    const MIN_WIDE: usize = 512;

    #[inline(always)]
    unsafe fn keys(k: (u64, u64)) -> __m128i {
        _mm_set_epi64x(k.1 as i64, k.0 as i64)
    }
    #[inline(always)]
    unsafe fn load(data: &[u8]) -> __m128i {
        _mm_loadu_si128(data.as_ptr().cast())
    }
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

    /// Fold the remaining whole 16-byte blocks into `x`, then run the
    /// residue and the last bytes through the table.
    #[inline(always)]
    unsafe fn finish(mut x: __m128i, mut data: &[u8]) -> u32 {
        let k128 = keys(k::D128);
        while data.len() >= 16 {
            x = fold(x, load(data), k128);
            data = &data[16..];
        }
        // 128 -> 64 -> 32 bits, then Barrett reduction modulo P.
        let x = _mm_xor_si128(_mm_clmulepi64_si128(x, k128, 0x10), _mm_srli_si128(x, 8));
        let low32 = _mm_set_epi32(0, 0, 0, -1);
        let x = _mm_xor_si128(
            _mm_clmulepi64_si128(_mm_and_si128(x, low32), _mm_set_epi64x(0, k::D64), 0x00),
            _mm_srli_si128(x, 4),
        );
        let pu = _mm_set_epi64x(k::MU as i64, k::P as i64);
        let t1 = _mm_clmulepi64_si128(_mm_and_si128(x, low32), pu, 0x10);
        let t2 = _mm_clmulepi64_si128(_mm_and_si128(t1, low32), pu, 0x00);
        let c = _mm_extract_epi32::<1>(_mm_xor_si128(x, t2)) as u32;
        table_update(c, data)
    }

    #[target_feature(enable = "pclmulqdq,sse4.1")]
    pub(super) unsafe fn update_pclmul(c: u32, mut data: &[u8]) -> u32 {
        if data.len() < MIN_FOLD {
            return table_update(c, data);
        }
        let seed = _mm_cvtsi32_si128(c as i32);
        if data.len() < 2 * MIN_FOLD {
            return finish(_mm_xor_si128(load(data), seed), &data[16..]);
        }
        let mut x0 = _mm_xor_si128(load(data), seed);
        let mut x1 = load(&data[16..]);
        let mut x2 = load(&data[32..]);
        let mut x3 = load(&data[48..]);
        data = &data[64..];
        let k512 = keys(k::D512);
        while data.len() >= 64 {
            x0 = fold(x0, load(data), k512);
            x1 = fold(x1, load(&data[16..]), k512);
            x2 = fold(x2, load(&data[32..]), k512);
            x3 = fold(x3, load(&data[48..]), k512);
            data = &data[64..];
        }
        let x = _mm_xor_si128(
            fold(x0, x3, keys(k::D384)),
            fold(
                x1,
                fold(x2, _mm_setzero_si128(), keys(k::D128)),
                keys(k::D256),
            ),
        );
        finish(x, data)
    }

    #[inline(always)]
    unsafe fn keys256(k: (u64, u64)) -> __m256i {
        _mm256_broadcastsi128_si256(keys(k))
    }
    #[inline(always)]
    unsafe fn load256(data: &[u8]) -> __m256i {
        _mm256_loadu_si256(data.as_ptr().cast())
    }
    #[inline(always)]
    unsafe fn fold256(a: __m256i, b: __m256i, k: __m256i) -> __m256i {
        _mm256_xor_si256(
            _mm256_xor_si256(
                _mm256_clmulepi64_epi128(a, k, 0x00),
                _mm256_clmulepi64_epi128(a, k, 0x11),
            ),
            b,
        )
    }

    #[target_feature(enable = "pclmulqdq,sse4.1,avx2,vpclmulqdq")]
    pub(super) unsafe fn update_avx2(c: u32, mut data: &[u8]) -> u32 {
        if data.len() < MIN_WIDE / 2 {
            return update_pclmul(c, data);
        }
        let seed = _mm256_castsi128_si256(_mm_cvtsi32_si128(c as i32));
        let mut v0 = _mm256_xor_si256(load256(data), seed);
        let mut v1 = load256(&data[32..]);
        let mut v2 = load256(&data[64..]);
        let mut v3 = load256(&data[96..]);
        data = &data[128..];
        let k1024 = keys256(k::D1024);
        while data.len() >= 128 {
            v0 = fold256(v0, load256(data), k1024);
            v1 = fold256(v1, load256(&data[32..]), k1024);
            v2 = fold256(v2, load256(&data[64..]), k1024);
            v3 = fold256(v3, load256(&data[96..]), k1024);
            data = &data[128..];
        }
        let k256 = keys256(k::D256);
        let a = fold256(v0, v3, keys256(k::D768));
        let b = fold256(v1, _mm256_setzero_si256(), keys256(k::D512));
        let mut v = fold256(v2, _mm256_xor_si256(a, b), k256);
        while data.len() >= 32 {
            v = fold256(v, load256(data), k256);
            data = &data[32..];
        }
        let x = fold(
            _mm256_castsi256_si128(v),
            _mm256_extracti128_si256::<1>(v),
            keys(k::D128),
        );
        finish(x, data)
    }

    #[inline(always)]
    unsafe fn keys512(k: (u64, u64)) -> __m512i {
        _mm512_broadcast_i32x4(keys(k))
    }
    #[inline(always)]
    unsafe fn load512(data: &[u8]) -> __m512i {
        _mm512_loadu_si512(data.as_ptr().cast())
    }
    /// clmul(a.lo, k.lo) ^ clmul(a.hi, k.hi) ^ b in each 128-bit lane.
    #[inline(always)]
    unsafe fn fold512(a: __m512i, b: __m512i, k: __m512i) -> __m512i {
        _mm512_ternarylogic_epi64::<0x96>(
            _mm512_clmulepi64_epi128(a, k, 0x00),
            _mm512_clmulepi64_epi128(a, k, 0x11),
            b,
        )
    }

    #[target_feature(enable = "pclmulqdq,sse4.1,avx2,avx512f,vpclmulqdq")]
    pub(super) unsafe fn update_avx512(c: u32, mut data: &[u8]) -> u32 {
        if data.len() < 128 {
            return update_pclmul(c, data);
        }
        let seed = _mm512_castsi128_si512(_mm_cvtsi32_si128(c as i32));
        let k512 = keys512(k::D512);
        let mut v = if data.len() >= MIN_WIDE {
            let mut v0 = _mm512_xor_si512(load512(data), seed);
            let mut v1 = load512(&data[64..]);
            let mut v2 = load512(&data[128..]);
            let mut v3 = load512(&data[192..]);
            data = &data[256..];
            let k2048 = keys512(k::D2048);
            while data.len() >= 256 {
                v0 = fold512(v0, load512(data), k2048);
                v1 = fold512(v1, load512(&data[64..]), k2048);
                v2 = fold512(v2, load512(&data[128..]), k2048);
                v3 = fold512(v3, load512(&data[192..]), k2048);
                data = &data[256..];
            }
            let a = fold512(v0, v3, keys512(k::D1536));
            let b = fold512(v1, _mm512_setzero_si512(), keys512(k::D1024));
            fold512(v2, _mm512_xor_si512(a, b), k512)
        } else {
            let v = _mm512_xor_si512(load512(data), seed);
            data = &data[64..];
            v
        };
        while data.len() >= 64 {
            v = fold512(v, load512(data), k512);
            data = &data[64..];
        }
        // Lanes 0..2 forward by 384, 256 and 128 bits onto lane 3.
        let lane_keys = _mm512_set_epi64(
            0,
            0,
            k::D128.1 as i64,
            k::D128.0 as i64,
            k::D256.1 as i64,
            k::D256.0 as i64,
            k::D384.1 as i64,
            k::D384.0 as i64,
        );
        let last = _mm512_maskz_mov_epi64(0xc0, v);
        let t = fold512(v, last, lane_keys);
        let t = _mm256_xor_si256(_mm512_castsi512_si256(t), _mm512_extracti64x4_epi64::<1>(t));
        let x = _mm_xor_si128(_mm256_castsi256_si128(t), _mm256_extracti128_si256::<1>(t));
        finish(x, data)
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::k;
    use std::arch::aarch64::*;

    const MIN_FOLD: usize = 64;

    #[inline(always)]
    unsafe fn load(data: &[u8]) -> uint8x16_t {
        vld1q_u8(data.as_ptr())
    }
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

    #[target_feature(enable = "crc")]
    unsafe fn crc_bytes(mut c: u32, data: &[u8]) -> u32 {
        let (words, tail) = data.as_chunks::<8>();
        for w in words {
            c = __crc32d(c, u64::from_le_bytes(*w));
        }
        for &b in tail {
            c = __crc32b(c, b);
        }
        c
    }

    #[target_feature(enable = "neon,aes,crc")]
    pub(super) unsafe fn update_pmull(c: u32, mut data: &[u8]) -> u32 {
        if data.len() < MIN_FOLD {
            return crc_bytes(c, data);
        }
        let seed = vreinterpretq_u8_u32(vsetq_lane_u32::<0>(c, vdupq_n_u32(0)));
        let mut x0 = veorq_u8(load(data), seed);
        let mut x1 = load(&data[16..]);
        let mut x2 = load(&data[32..]);
        let mut x3 = load(&data[48..]);
        data = &data[64..];
        while data.len() >= 64 {
            x0 = fold(x0, load(data), k::D512);
            x1 = fold(x1, load(&data[16..]), k::D512);
            x2 = fold(x2, load(&data[32..]), k::D512);
            x3 = fold(x3, load(&data[48..]), k::D512);
            data = &data[64..];
        }
        let mut x = veorq_u8(
            fold(x0, x3, k::D384),
            fold(x1, fold(x2, vdupq_n_u8(0), k::D128), k::D256),
        );
        while data.len() >= 16 {
            x = fold(x, load(data), k::D128);
            data = &data[16..];
        }
        let x = vreinterpretq_u64_u8(x);
        let c = __crc32d(__crc32d(0, vgetq_lane_u64::<0>(x)), vgetq_lane_u64::<1>(x));
        crc_bytes(c, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engines() -> Vec<Engine> {
        let mut engines = vec![Engine::Portable];
        let detected = Engine::detect();
        #[cfg(target_arch = "x86_64")]
        if detected != Engine::Portable {
            engines.push(Engine::Pclmul);
        }
        #[cfg(target_arch = "x86_64")]
        if detected == Engine::Avx512 {
            engines.push(Engine::Avx2);
        }
        if !engines.contains(&detected) {
            engines.push(detected);
        }
        engines
    }

    /// Bit-at-a-time definition, independent of the tables and folding.
    fn bitwise(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xedb8_8320
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    #[test]
    fn check_values() {
        for engine in engines() {
            for (data, expected) in [
                (&b""[..], 0),
                (b"a", 0xe8b7_be43),
                (b"123456789", 0xcbf4_3926),
                (b"The quick brown fox jumps over the lazy dog", 0x414f_a339),
            ] {
                let mut crc = Crc32::with_engine(engine);
                crc.update(data);
                assert_eq!(crc.finalize(), expected, "{engine:?}");
            }
        }
    }

    #[test]
    fn every_length_offset_and_split_matches_independent_code() {
        let mut seed = 0x9e37_79b9u32;
        let data: Vec<u8> = (0..9000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        for engine in engines() {
            for offset in 0..4 {
                for len in (0..1200).chain([2047, 2048, 4096, 4607, 8191, 8192, 8995]) {
                    let part = &data[offset..offset + len];
                    let expected = bitwise(part);
                    let mut crc = Crc32::with_engine(engine);
                    crc.update(part);
                    assert_eq!(crc.finalize(), expected, "{engine:?} {offset} {len}");
                    assert_eq!(crc32fast_reference::hash(part), expected);
                    let split = (len * 7 / 13).min(len);
                    let mut crc = Crc32::with_engine(engine);
                    crc.update(&part[..split]);
                    crc.update(&part[split..]);
                    assert_eq!(crc.finalize(), expected, "{engine:?} {offset} {len} split");
                }
            }
            for len in [300, 1100] {
                let expected = bitwise(&data[..len]);
                for split in 0..=len {
                    let mut crc = Crc32::with_engine(engine);
                    crc.update(&data[..split]);
                    crc.update(&data[split..len]);
                    assert_eq!(crc.finalize(), expected, "{engine:?} {len} at {split}");
                }
            }
        }
    }
}
