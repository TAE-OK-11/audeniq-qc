//! Experimental: 8 PMULL accumulators (128 bytes per iteration) with EOR3.
use std::arch::aarch64::*;
const D128: (u64, u64) = (0x1_7519_97d0, 0x0_ccaa_009e);
const D256: (u64, u64) = (0x0_f1da_05aa, 0x1_5a54_6366);
const D384: (u64, u64) = (0x0_3db1_ecdc, 0x1_7435_9406);
const D512: (u64, u64) = (0x1_5444_2bd4, 0x1_c6e4_1596);
const D1024: (u64, u64) = (0x1_e88e_f372, 0x1_4a7f_e880);
#[inline(always)]
unsafe fn load(d: &[u8]) -> uint8x16_t { vld1q_u8(d.as_ptr()) }
#[inline(always)]
unsafe fn fold(a: uint8x16_t, b: uint8x16_t, k: (u64, u64)) -> uint8x16_t {
    let a = vreinterpretq_p64_u8(a);
    let lo = vreinterpretq_u8_p128(vmull_p64(vgetq_lane_p64::<0>(a), k.0));
    let hi = vreinterpretq_u8_p128(vmull_p64(vgetq_lane_p64::<1>(a), k.1));
    veor3q_u8(lo, hi, b)
}
#[target_feature(enable = "crc")]
unsafe fn crc_bytes(mut c: u32, data: &[u8]) -> u32 {
    let (w, t) = data.as_chunks::<8>();
    for w in w { c = __crc32d(c, u64::from_le_bytes(*w)); }
    for &b in t { c = __crc32b(c, b); }
    c
}
#[target_feature(enable = "neon,aes,crc,sha3")]
pub unsafe fn crc32(data: &[u8]) -> u32 {
    let mut data = data;
    let c = !0u32;
    if data.len() < 256 { return !crc_bytes(c, data); }
    let seed = vreinterpretq_u8_u32(vsetq_lane_u32::<0>(c, vdupq_n_u32(0)));
    let mut x = [vdupq_n_u8(0); 8];
    for i in 0..8 { x[i] = load(&data[16 * i..]); }
    x[0] = veorq_u8(x[0], seed);
    data = &data[128..];
    while data.len() >= 128 {
        for i in 0..8 { x[i] = fold(x[i], load(&data[16 * i..]), D1024); }
        data = &data[128..];
    }
    let y0 = fold(x[0], x[4], D512);
    let y1 = fold(x[1], x[5], D512);
    let y2 = fold(x[2], x[6], D512);
    let y3 = fold(x[3], x[7], D512);
    let mut v = veorq_u8(fold(y0, y3, D384), fold(y1, fold(y2, vdupq_n_u8(0), D128), D256));
    while data.len() >= 16 { v = fold(v, load(data), D128); data = &data[16..]; }
    let v = vreinterpretq_u64_u8(v);
    !crc_bytes(__crc32d(__crc32d(0, vgetq_lane_u64::<0>(v)), vgetq_lane_u64::<1>(v)), data)
}
