#!/usr/bin/env python3
"""Generate src/hash_fused.rs: MD5 steps interleaved with SHA-NI round groups.

One MD5 block is a serial chain of 64 scalar steps; one SHA-256 block is a
serial chain of 16 four-round groups on the SHA unit. Emitting them
interleaved in program order lets an out-of-order core run both chains at
once; separate loops only overlap at block boundaries. PCM hashing feeds
SHA-256 four bytes per sample and MD5 two (16-bit) or three (24-bit) bytes per
sample, so one kernel pairs 1 MD5 block with 2 SHA blocks and the other 3 MD5
blocks with 4 SHA blocks.
"""
import sys

MD5_SHIFTS = [[7, 12, 17, 22], [5, 9, 14, 20], [4, 11, 16, 23], [6, 10, 15, 21]]


def md5_index(t):
    r = t // 16
    i = t % 16
    return [i, (5 * i + 1) % 16, (3 * i + 5) % 16, (7 * i) % 16][r]


def md5_step(blk, t):
    names = ["a", "b", "c", "d"]
    rot = (-t) % 4
    a, b, c, d = (names[(rot + k) % 4] for k in range(4))
    fn = "fgh i"[t // 16]  # placeholder
    func = ["ff", "gg", "hh", "ii"][t // 16]
    s = MD5_SHIFTS[t // 16][t % 4]
    return f"        {func}!({a}, {b}, {c}, {d}, m{blk}[{md5_index(t)}], {t}, {s});"


def sha_group(blk, g):
    w = [f"w{blk}_{k}" for k in range(4)]
    lines = []
    if g == 0:
        lines.append(f"        let (abef_saved{blk}, cdgh_saved{blk}) = (abef, cdgh);")
        for k in range(4):
            lines.append(f"        let mut {w[k]} = load(&s[{blk}], {k});")
    cur = w[g % 4]
    lines.append(f"        rounds!(abef, cdgh, {cur}, {g});")
    if 3 <= g <= 14:
        nxt, prev = w[(g + 1) % 4], w[(g + 3) % 4]
        lines.append(f"        extend!({nxt}, {cur}, {prev});")
    if 1 <= g <= 12:
        tgt = w[(g + 3) % 4]
        lines.append(f"        {tgt} = _mm_sha256msg1_epu32({tgt}, {cur});")
    if g == 15:
        lines.append(f"        abef = _mm_add_epi32(abef, abef_saved{blk});")
        lines.append(f"        cdgh = _mm_add_epi32(cdgh, cdgh_saved{blk});")
    return lines


def kernel(name, md5_blocks, sha_blocks):
    steps = md5_blocks * 64
    groups = sha_blocks * 16
    out = []
    out.append(f"/// {md5_blocks} MD5 block(s) interleaved with {sha_blocks} SHA-256 block(s) per unit.")
    out.append("#[target_feature(enable = \"sha,sse2,ssse3,sse4.1\")]")
    out.append(f"pub(crate) unsafe fn {name}(")
    out.append("    md5: &mut [u32; 4],")
    out.append("    md5_blocks: &[[u8; 64]],")
    out.append("    sha: &mut [u32; 8],")
    out.append("    sha_blocks: &[[u8; 64]],")
    out.append(") {")
    rhs = "sha_blocks.len()" if md5_blocks == 1 else f"sha_blocks.len() * {md5_blocks}"
    out.append(f"    assert_eq!(md5_blocks.len() * {sha_blocks}, {rhs});")
    out.append("    let byte_swap = _mm_set_epi64x(0x0c0d0e0f08090a0b, 0x0405060700010203);")
    out.append("    // SAFETY (all SIMD loads/stores): pointers come from live arrays of")
    out.append("    // at least 16 bytes; loads and stores are unaligned.")
    out.append("    let load = |block: &[u8; 64], i: usize| {")
    out.append("        _mm_shuffle_epi8(_mm_loadu_si128(block.as_ptr().add(16 * i).cast()), byte_swap)")
    out.append("    };")
    out.append("    let dcba = _mm_loadu_si128(sha.as_ptr().cast());")
    out.append("    let hgfe = _mm_loadu_si128(sha.as_ptr().add(4).cast());")
    out.append("    let cdab = _mm_shuffle_epi32(dcba, 0xb1);")
    out.append("    let efgh = _mm_shuffle_epi32(hgfe, 0x1b);")
    out.append("    let mut abef = _mm_alignr_epi8(cdab, efgh, 8);")
    out.append("    let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xf0);")
    out.append("    let [mut a, mut b, mut c, mut d] = *md5;")
    out.append(f"    for (m, s) in md5_blocks")
    out.append(f"        .as_chunks::<{md5_blocks}>()")
    out.append("        .0")
    out.append("        .iter()")
    out.append(f"        .zip(sha_blocks.as_chunks::<{sha_blocks}>().0.iter())")
    out.append("    {")
    for blk in range(md5_blocks):
        out.append(f"        let m{blk} = words(&m[{blk}]);")
    out.append("        let saved = (a, b, c, d);")
    # Interleave: spread SHA groups evenly over MD5 steps.
    emitted = 0
    for t in range(steps):
        blk = t // 64
        if t % 64 == 0 and blk > 0:
            out.append("        let saved_inner = (a, b, c, d);")
            out.append("        let _ = saved_inner;")
        out.append(md5_step(blk, t % 64))
        if t % 64 == 63:
            out.append("        a = a.wrapping_add(saved.0);" if blk == 0 else "        a = a.wrapping_add(saved_blk.0);")
            out.append("        b = b.wrapping_add(saved.1);" if blk == 0 else "        b = b.wrapping_add(saved_blk.1);")
            out.append("        c = c.wrapping_add(saved.2);" if blk == 0 else "        c = c.wrapping_add(saved_blk.2);")
            out.append("        d = d.wrapping_add(saved.3);" if blk == 0 else "        d = d.wrapping_add(saved_blk.3);")
            if blk + 1 < md5_blocks:
                out.append("        let saved_blk = (a, b, c, d);")
        target = (t + 1) * groups // steps
        while emitted < target:
            out.extend(sha_group(emitted // 16, emitted % 16))
            emitted += 1
    out.append("    }")
    out.append("    *md5 = [a, b, c, d];")
    out.append("    let feba = _mm_shuffle_epi32(abef, 0x1b);")
    out.append("    let dchg = _mm_shuffle_epi32(cdgh, 0xb1);")
    out.append("    _mm_storeu_si128(sha.as_mut_ptr().cast(), _mm_blend_epi16(feba, dchg, 0xf0));")
    out.append("    _mm_storeu_si128(sha.as_mut_ptr().add(4).cast(), _mm_alignr_epi8(dchg, feba, 8));")
    out.append("}")
    text = "\n".join(out)
    # The first MD5 block in a unit uses `saved`; later ones `saved_blk`.
    return text.replace("        let saved_inner = (a, b, c, d);\n        let _ = saved_inner;\n", "")


HEADER = '''// @generated by tools/scripts/gen_fused_hash.py; do not edit by hand.
//! MD5 and SHA-256 (SHA extensions) blocks with their serial chains
//! interleaved in program order, so one out-of-order core runs both at
//! once. Each step and round group is the same arithmetic as `md5::compress`
//! and `sha256::compress_sha_ni`; the tests compare the results with them.
#![cfg(target_arch = "x86_64")]
use crate::md5::K as MD5_K;
use crate::sha256::K as SHA_K;
use std::arch::x86_64::*;

#[inline(always)]
fn words(block: &[u8; 64]) -> [u32; 16] {
    let mut m = [0u32; 16];
    for (word, bytes) in m.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    m
}

// MD5 steps exactly as in `md5::compress` (see that module for the round
// function forms).
macro_rules! step {
    ($a:ident, $b:ident, $f:expr, $k:expr, $m:expr, $s:expr) => {
        $a = $b.wrapping_add($a.wrapping_add($k).wrapping_add($m).wrapping_add($f).rotate_left($s));
    };
}
macro_rules! ff {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        step!($a, $b, $d ^ ($b & ($c ^ $d)), std::hint::black_box(&MD5_K)[$i], $m, $s)
    };
}
macro_rules! gg {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        $a = $b.wrapping_add(
            $a.wrapping_add(std::hint::black_box(&MD5_K)[$i])
                .wrapping_add($m)
                .wrapping_add($c & !$d)
                .wrapping_add($b & $d)
                .rotate_left($s),
        );
    };
}
macro_rules! hh {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        step!($a, $b, $b ^ ($c ^ $d), std::hint::black_box(&MD5_K)[$i], $m, $s)
    };
}
macro_rules! ii {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        step!($a, $b, $c ^ ($b | !$d), std::hint::black_box(&MD5_K)[$i], $m, $s)
    };
}
macro_rules! rounds {
    ($abef:ident, $cdgh:ident, $m:expr, $i:expr) => {{
        let message = _mm_add_epi32($m, _mm_loadu_si128(SHA_K.as_ptr().add(4 * $i).cast()));
        $cdgh = _mm_sha256rnds2_epu32($cdgh, $abef, message);
        $abef = _mm_sha256rnds2_epu32($abef, $cdgh, _mm_shuffle_epi32(message, 0x0e));
    }};
}
macro_rules! extend {
    ($next:ident, $cur:ident, $prev:ident) => {
        $next = _mm_sha256msg2_epu32(_mm_add_epi32($next, _mm_alignr_epi8($cur, $prev, 4)), $cur);
    };
}

'''
src = HEADER + kernel("md5_1_sha_2", 1, 2) + "\n\n" + kernel("md5_3_sha_4", 3, 4) + "\n"
sys.stdout.write(src)
