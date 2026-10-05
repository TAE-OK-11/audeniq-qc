//! Bounded native FLAC decoder. Rice/fixed/LPC and channel reconstruction
//! ported/reworked from FFmpeg flac.c, flacdec.c, flacdsp.c (LGPL-2.1-or-later).
//! Copyright (c) 2003 Alex Beregszaszi; (c) 2012 Mans Rullgard.
//! See THIRD_PARTY.md.
use crate::md5::Md5;
use crate::{kernels::Backend, msb::Bits, AudioSpec, Error, Limits, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
pub(crate) struct Decoder {
    file: File,
    pub spec: AudioSpec,
    pub info: [u8; 34],
    buffer: Vec<u8>,
    end: usize,
    start: usize,
    retained: Option<std::ops::Range<usize>>,
    eof: bool,
    planes: [Vec<i32>; 2],
    decoded: u64,
    frame: u64,
    strategy: Option<bool>,
    md5: Option<Md5>,
    md5_verified: bool,
    packed: Vec<u8>,
}

fn invalid<T>(s: &'static str) -> Result<T> {
    Err(Error::Invalid(s))
}
fn residual(b: &mut Bits<'_>, p: &mut [i32], order: usize) -> Result<()> {
    let _profile = crate::profile::scope(crate::profile::Stage::FlacResidual);
    #[cfg(target_arch = "x86_64")]
    if crate::kernels::bit_ops() {
        // SAFETY: LZCNT/BMI1/BMI2 were detected at runtime; the body is the
        // same bounds-checked safe Rust.
        return unsafe { residual_bit_ops(b, p, order) };
    }
    residual_body(b, p, order)
}

/// Rice decoding is a serial chain of leading-zero counts and variable
/// shifts. Baseline x86-64 lowers those to BSR plus fix-ups and 3-uop
/// `shl/shr cl`; LZCNT and BMI2 `shlx/shrx` shorten every step.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "lzcnt,bmi1,bmi2")]
unsafe fn residual_bit_ops(b: &mut Bits<'_>, p: &mut [i32], order: usize) -> Result<()> {
    residual_body(b, p, order)
}

#[inline(always)]
fn residual_body(b: &mut Bits<'_>, p: &mut [i32], order: usize) -> Result<()> {
    let method = b.get(2)?;
    let partition = b.get(4)?;
    let partitions = 1usize << partition;
    if method > 1 || !p.len().is_multiple_of(partitions) {
        return invalid("FLAC Rice partition");
    }
    let size = p.len() / partitions;
    if size < order {
        return invalid("FLAC residual predictor order");
    }
    let width = 4 + method;
    let escape = (1 << width) - 1;
    for i in 0..partitions {
        let k = b.get(width)?;
        let begin = if i == 0 { order } else { i * size };
        let dst = &mut p[begin..(i + 1) * size];
        if k == escape {
            let bits = b.get(5)?;
            for v in dst {
                *v = b.signed(bits)?;
            }
        } else {
            b.rice_run(k, dst)?;
        }
    }
    Ok(())
}
fn subframe(b: &mut Bits<'_>, p: &mut [i32], bits: u32) -> Result<()> {
    if b.get(1)? != 0 {
        return invalid("FLAC subframe padding");
    }
    let mode = b.get(6)?;
    let wasted = if b.get(1)? != 0 {
        1 + b.unary(false, bits)?
    } else {
        0
    };
    if wasted >= bits {
        return invalid("FLAC wasted bits");
    }
    let width = bits - wasted;
    match mode {
        0 => p.fill(b.signed(width)?),
        1 => {
            for v in p.iter_mut() {
                *v = b.signed(width)?;
            }
        }
        8..=12 | 32..=63 => {
            let order = if mode < 32 {
                (mode - 8) as usize
            } else {
                (mode - 31) as usize
            };
            if order > p.len() {
                return invalid("FLAC predictor order");
            }
            for v in &mut p[..order] {
                *v = b.signed(width)?;
            }
            let mut coeff = [0i32; 32];
            let mut shift = 0;
            if mode >= 32 {
                let precision = b.get(4)?;
                if precision == 15 {
                    return invalid("FLAC LPC precision");
                }
                shift = b.signed(5)?;
                for c in &mut coeff[..order] {
                    *c = b.signed(precision + 1)?;
                }
            }
            residual(b, p, order)?;
            if mode >= 32 {
                match order {
                    1 => restore_lpc::<1>(p, &coeff[..order], shift, width)?,
                    2 => restore_lpc::<2>(p, &coeff[..order], shift, width)?,
                    3 => restore_lpc::<3>(p, &coeff[..order], shift, width)?,
                    4 => restore_lpc::<4>(p, &coeff[..order], shift, width)?,
                    5 => restore_lpc::<5>(p, &coeff[..order], shift, width)?,
                    6 => restore_lpc::<6>(p, &coeff[..order], shift, width)?,
                    7 => restore_lpc::<7>(p, &coeff[..order], shift, width)?,
                    8 => restore_lpc::<8>(p, &coeff[..order], shift, width)?,
                    10 => restore_lpc::<10>(p, &coeff[..order], shift, width)?,
                    12 => restore_lpc::<12>(p, &coeff[..order], shift, width)?,
                    _ => restore_lpc::<0>(p, &coeff[..order], shift, width)?,
                }
            } else {
                restore_fixed(p, order, width)?;
            }
        }
        _ => return invalid("FLAC subframe mode"),
    }
    if wasted != 0 {
        for v in p {
            *v = v.wrapping_shl(wasted);
        }
    }
    Ok(())
}
pub(crate) struct Header {
    pub samples: usize,
    pub number: u64,
    pub variable: bool,
    mode: u32,
}
fn header(b: &mut Bits<'_>, spec: &AudioSpec, maximum: usize) -> Result<Header> {
    if b.get(15)? != 0x7ffc {
        return invalid("FLAC sync");
    }
    let variable = b.get(1)? != 0;
    let block = b.get(4)?;
    let rate = b.get(4)?;
    let mode = b.get(4)?;
    let bits = b.get(3)?;
    if b.get(1)? != 0 || mode > 10 || (if mode < 8 { mode + 1 } else { 2 }) != spec.channels as u32
    {
        return invalid("FLAC frame channels/reserved bit");
    }
    let width = [0, 8, 12, 0, 16, 20, 24, 32][bits as usize];
    if bits == 3 || (width != 0 && width != spec.bits_per_sample as u32) {
        return invalid("FLAC frame sample width");
    }
    let first = b.get(8)?;
    let n = (first as u8).leading_ones();
    let mut number = if n == 0 {
        first as u64
    } else {
        if !(2..=7).contains(&n) {
            return invalid("FLAC frame number encoding");
        }
        (first & ((1 << (7 - n)) - 1)) as u64
    };
    for _ in 1..n {
        let byte = b.get(8)?;
        if byte & 0xc0 != 0x80 {
            return invalid("FLAC frame number continuation");
        }
        number = (number << 6) | (byte & 63) as u64;
    }
    if n != 0 {
        let minimum = [0, 0, 0x80, 0x800, 0x10000, 0x200000, 0x4000000, 0x80000000][n as usize];
        if number < minimum {
            return invalid("FLAC overlong frame number");
        }
    }
    if (!variable && number >= 1 << 31) || number >= 1 << 36 {
        return invalid("FLAC frame number range");
    }
    let samples = match block {
        0 => return invalid("FLAC block code"),
        1 => 192,
        2..=5 => 576 << (block - 2),
        6 => b.get(8)? + 1,
        7 => b.get(16)? + 1,
        _ => 256 << (block - 8),
    } as usize;
    let sample_rate = match rate {
        0 => spec.sample_rate,
        1 => 88200,
        2 => 176400,
        3 => 192000,
        4 => 8000,
        5 => 16000,
        6 => 22050,
        7 => 24000,
        8 => 32000,
        9 => 44100,
        10 => 48000,
        11 => 96000,
        12 => b.get(8)? * 1000,
        13 => b.get(16)?,
        14 => b.get(16)? * 10,
        _ => return invalid("FLAC sample rate code"),
    };
    if sample_rate != spec.sample_rate || samples > maximum {
        return invalid("FLAC frame format/block size");
    }
    Ok(Header {
        samples,
        number,
        variable,
        mode,
    })
}
pub(crate) fn decode(
    data: &[u8],
    spec: &AudioSpec,
    maximum: usize,
    planes: &mut [Vec<i32>; 2],
    out: &mut Vec<i32>,
) -> Result<(usize, Header)> {
    let mut b = Bits::new(data);
    let h = header(&mut b, spec, maximum)?;
    let header_len = b.pos / 8;
    let crc = b.get(8)?;
    if crate::bits::crc8(&data[..header_len]) as u32 != crc {
        return invalid("FLAC header CRC");
    }
    for (ch, plane) in planes.iter_mut().enumerate().take(spec.channels as usize) {
        plane.resize(h.samples, 0);
        let bits = spec.bits_per_sample as u32
            + u32::from((ch == 0 && h.mode == 9) || (ch == 1 && matches!(h.mode, 8 | 10)));
        subframe(&mut b, plane, bits)?;
    }
    b.align_zero()?;
    let length = b.pos / 8;
    let crc = b.get(16)?;
    if crate::bits::crc16(&data[..length]) as u32 != crc {
        return invalid("FLAC frame CRC");
    }
    let _profile = crate::profile::scope(crate::profile::Stage::FlacOutput);
    let shift = 32 - spec.bits_per_sample;
    if spec.channels == 1 {
        for v in &mut planes[0] {
            *v = v.wrapping_shl(shift as u32);
        }
        std::mem::swap(out, &mut planes[0]);
        crate::profile::count(crate::profile::Counter::MonoBufferSwaps, 1);
    } else {
        out.resize(h.samples * 2, 0);
        if h.mode < 8 {
            crate::kernels::interleave_shift_i32(&planes[0], &planes[1], out, shift as u32);
            return Ok((length + 2, h));
        }
        // Each subframe is at most 25 signed bits; stereo reconstruction fits
        // i32 even before validation. Accumulate range failures and check once
        // so SIMD/vectorization is possible without weakening integrity.
        let bound = 1i32 << (spec.bits_per_sample - 1);
        let (a, d) = (&planes[0][..], &planes[1][..]);
        let bad = match h.mode {
            8 => decorrelate::<8>(a, d, out, bound, shift as u32),
            9 => decorrelate::<9>(a, d, out, bound, shift as u32),
            _ => decorrelate::<10>(a, d, out, bound, shift as u32),
        };
        if bad {
            return invalid("FLAC reconstructed stereo range");
        }
    }
    Ok((length + 2, h))
}

/// Scratch planes for [`verify`]: expected subframe signal and parsed residuals.
#[derive(Default)]
pub(crate) struct VerifyScratch {
    expected: [Vec<i32>; 2],
    residual: Vec<i32>,
}

/// Check that `data` begins with exactly one FLAC frame that [`decode`] would
/// accept and reconstruct to `expected` (interleaved, left-aligned PCM).
///
/// The bitstream is parsed exactly as `decode` parses it: header and CRC-8,
/// every subframe header, warm-up sample, coefficient, Rice partition and
/// escape, zero padding and CRC-16. Only the final reconstruction differs.
/// `decode` rebuilds sample `i` as `pred(decoded[..i]) + residual[i]`, a serial
/// recurrence. Here the expected subframe signal `x` is derived from the
/// source and every residual must equal `x[i] - pred(x[..i])` with the same
/// predictor arithmetic, and every warm-up/verbatim/constant value must equal
/// `x[i]`. By induction over `i`, the decoder's output equals `x` exactly
/// when all of these hold, so acceptance is identical to `decode` followed by
/// a sample-for-sample comparison, while the checks have no loop-carried
/// dependency. Stereo decorrelation is checked in the forward direction; each
/// FLAC channel mode is a bijection on in-range samples (see `channel`).
pub(crate) fn verify(
    data: &[u8],
    spec: &AudioSpec,
    maximum: usize,
    expected: &[i32],
    scratch: &mut VerifyScratch,
    backend: Backend,
) -> Result<(usize, Header)> {
    let mut b = Bits::new(data);
    let h = header(&mut b, spec, maximum)?;
    let header_len = b.pos / 8;
    let crc = b.get(8)?;
    if crate::bits::crc8(&data[..header_len]) as u32 != crc {
        return invalid("FLAC header CRC");
    }
    let channels = spec.channels as usize;
    if expected.len() != h.samples * channels {
        return invalid("FLAC verified block length");
    }
    // `decode` left-aligns with a wrapping shift, so the source must have zero
    // low bits; the planes are the right-aligned source channels.
    let shift = 32 - spec.bits_per_sample as u32;
    let low = (1i32 << shift) - 1;
    let mut bad = 0i32;
    let [first, second] = &mut scratch.expected;
    first.resize(h.samples, 0);
    if channels == 1 {
        for (e, &x) in first.iter_mut().zip(expected) {
            bad |= x & low;
            *e = x >> shift;
        }
    } else {
        second.resize(h.samples, 0);
        // Forward FLAC decorrelation of the source. `decode` inverts these
        // maps: left/side (a, a - d), side/right (a + d, d) and mid/side via
        // mid2 = (a << 1) | (d & 1). With L, R in range, L + R and L - R
        // have equal parity, so the inverse of each forward pair is (L, R);
        // and forward(inverse(a, d)) == (a, d), so no other in-range pair
        // decodes to (L, R).
        for ((row, a), d) in expected
            .as_chunks::<2>()
            .0
            .iter()
            .zip(first.iter_mut())
            .zip(second.iter_mut())
        {
            bad |= (row[0] | row[1]) & low;
            let (l, r) = (row[0] >> shift, row[1] >> shift);
            (*a, *d) = match h.mode {
                8 => (l, l - r),
                9 => (l - r, r),
                10 => ((l + r) >> 1, l - r),
                _ => (l, r),
            };
        }
    }
    if bad != 0 {
        return invalid("FLAC verified source alignment");
    }
    for ch in 0..channels {
        let bits = spec.bits_per_sample as u32
            + u32::from((ch == 0 && h.mode == 9) || (ch == 1 && matches!(h.mode, 8 | 10)));
        verify_subframe(
            &mut b,
            &mut scratch.expected[ch],
            &mut scratch.residual,
            bits,
            backend,
        )?;
    }
    b.align_zero()?;
    let length = b.pos / 8;
    let crc = b.get(16)?;
    if crate::bits::crc16(&data[..length]) as u32 != crc {
        return invalid("FLAC frame CRC");
    }
    Ok((length + 2, h))
}

/// `subframe` followed by a comparison with `x`; see [`verify`]. `x` holds
/// in-range samples of `bits` bits and is consumed (shifted) in place.
fn verify_subframe(
    b: &mut Bits<'_>,
    x: &mut [i32],
    r: &mut Vec<i32>,
    bits: u32,
    backend: Backend,
) -> Result<()> {
    if b.get(1)? != 0 {
        return invalid("FLAC subframe padding");
    }
    let mode = b.get(6)?;
    let wasted = if b.get(1)? != 0 {
        1 + b.unary(false, bits)?
    } else {
        0
    };
    if wasted >= bits {
        return invalid("FLAC wasted bits");
    }
    let width = bits - wasted;
    // `decode` outputs v << wasted for a `width`-bit v, so x must have zero
    // low bits and v must equal x >> wasted (which is then within `width`).
    if wasted != 0 {
        let low = (1i32 << wasted) - 1;
        let mut bad = 0;
        for v in x.iter_mut() {
            bad |= *v & low;
            *v >>= wasted;
        }
        if bad != 0 {
            return invalid("FLAC verified wasted bits");
        }
    }
    let x = &*x;
    let mismatch = || invalid("lossless frame verification");
    match mode {
        0 => {
            let value = b.signed(width)?;
            if x.iter().any(|&v| v != value) {
                return mismatch();
            }
        }
        1 => {
            for &v in x {
                if b.signed(width)? != v {
                    return mismatch();
                }
            }
        }
        8..=12 | 32..=63 => {
            let order = if mode < 32 {
                (mode - 8) as usize
            } else {
                (mode - 31) as usize
            };
            if order > x.len() {
                return invalid("FLAC predictor order");
            }
            for &v in &x[..order] {
                if b.signed(width)? != v {
                    return mismatch();
                }
            }
            let mut coeff = [0i32; 32];
            let mut shift = 0;
            if mode >= 32 {
                let precision = b.get(4)?;
                if precision == 15 {
                    return invalid("FLAC LPC precision");
                }
                shift = b.signed(5)?;
                for c in &mut coeff[..order] {
                    *c = b.signed(precision + 1)?;
                }
            }
            // The residuals that make the decoder reproduce x exactly; the
            // stream must hold exactly their codes.
            r.resize(x.len(), 0);
            let fits = if mode >= 32 {
                let c = &coeff[..order];
                match order {
                    1 => expected_lpc::<1>(x, c, shift, r, backend),
                    2 => expected_lpc::<2>(x, c, shift, r, backend),
                    3 => expected_lpc::<3>(x, c, shift, r, backend),
                    4 => expected_lpc::<4>(x, c, shift, r, backend),
                    5 => expected_lpc::<5>(x, c, shift, r, backend),
                    6 => expected_lpc::<6>(x, c, shift, r, backend),
                    7 => expected_lpc::<7>(x, c, shift, r, backend),
                    8 => expected_lpc::<8>(x, c, shift, r, backend),
                    _ => expected_lpc::<0>(x, c, shift, r, backend),
                }
            } else {
                expected_fixed(x, order, r, backend)
            };
            if !fits || !verify_residual(b, r, order)? {
                return mismatch();
            }
        }
        _ => return invalid("FLAC subframe mode"),
    }
    Ok(())
}

/// `e[i] = x[i] - prediction` for i >= order, the residual with which
/// `restore_fixed` turns the (by induction equal) history x[..i] into x[i]:
/// it forms a wrapping i32 prediction and adds the residual in i64, so that
/// residual exists exactly when this difference fits i32. Returns false
/// otherwise (no FLAC stream then reproduces x).
fn expected_fixed(x: &[i32], order: usize, e: &mut [i32], backend: Backend) -> bool {
    macro_rules! dispatch {
        ($n:literal) => {{
            #[cfg(target_arch = "x86_64")]
            if backend == Backend::Avx2 {
                // SAFETY: the backend was checked available by the encoder;
                // the generic body is bounds-checked safe Rust.
                return unsafe { expected_fixed_avx2::<$n>(x, e) };
            }
            expected_fixed_body::<$n>(x, e)
        }};
    }
    let _ = backend;
    match order {
        0 => {
            e.copy_from_slice(x);
            true
        }
        1 => dispatch!(1),
        2 => dispatch!(2),
        3 => dispatch!(3),
        4 => dispatch!(4),
        _ => unreachable!(),
    }
}

/// `h[N - k]` is x[i - k]; the expressions are those of `restore_fixed`.
#[inline(always)]
fn expected_fixed_body<const N: usize>(x: &[i32], e: &mut [i32]) -> bool {
    let mut out = 0u64;
    for (w, e) in x.windows(N + 1).zip(&mut e[N..]) {
        let h: &[i32; N] = w[..N].try_into().unwrap();
        let prediction = match N {
            1 => h[0],
            2 => h[1].wrapping_mul(2).wrapping_sub(h[0]),
            3 => h[2].wrapping_sub(h[1]).wrapping_mul(3).wrapping_add(h[0]),
            _ => h[3]
                .wrapping_add(h[1])
                .wrapping_mul(4)
                .wrapping_sub(h[2].wrapping_mul(6))
                .wrapping_sub(h[0]),
        };
        let v = w[N] as i64 - prediction as i64;
        out |= (v.wrapping_add(1 << 31) as u64) >> 32;
        *e = v as i32;
    }
    out == 0
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn expected_fixed_avx2<const N: usize>(x: &[i32], e: &mut [i32]) -> bool {
    expected_fixed_body::<N>(x, e)
}

/// [`expected_fixed`] for LPC: the residuals with which `restore_lpc`
/// reproduces x. `restore_lpc` always forms the exact sum (its i32
/// accumulator is used only when its bound proves exactness), shifts it as
/// here and adds the residual in i64; any exact accumulator therefore gives
/// the same residuals. The i32 accumulator here is chosen from the actual
/// sample magnitudes.
#[inline]
fn expected_lpc<const N: usize>(
    x: &[i32],
    coeff: &[i32],
    shift: i32,
    e: &mut [i32],
    backend: Backend,
) -> bool {
    if N == 0 {
        return expected_lpc_wide(x, coeff, shift, e);
    }
    let c: [i32; N] = coeff.try_into().unwrap();
    let largest = c.iter().map(|c| c.unsigned_abs()).max().unwrap_or(0) as u64;
    let peak = x.iter().fold(0u32, |a, &v| a.max(v.unsigned_abs())) as u64;
    if !(0..32).contains(&shift) || largest * N as u64 * peak >= 1 << 31 {
        return expected_lpc_wide(x, coeff, shift, e);
    }
    #[cfg(target_arch = "x86_64")]
    if backend == Backend::Avx2 {
        // SAFETY: the backend was checked available by the encoder; the
        // generic body is bounds-checked safe Rust.
        return unsafe { expected_lpc_avx2::<N>(x, &c, shift, e) };
    }
    let _ = backend;
    expected_lpc_body::<N>(x, &c, shift, e)
}

/// Exact i32 accumulation, valid when |c| * N * max|x| < 2^31.
#[inline(always)]
fn expected_lpc_body<const N: usize>(x: &[i32], c: &[i32; N], shift: i32, e: &mut [i32]) -> bool {
    let mut out = 0u64;
    // Window w holds x[i - N..=i].
    for (w, e) in x.windows(N + 1).zip(&mut e[N..]) {
        let mut sum = 0i32;
        for j in 0..N {
            sum = sum.wrapping_add(c[j].wrapping_mul(w[N - 1 - j]));
        }
        let v = w[N] as i64 - (sum >> shift) as i64;
        out |= (v.wrapping_add(1 << 31) as u64) >> 32;
        *e = v as i32;
    }
    out == 0
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn expected_lpc_avx2<const N: usize>(
    x: &[i32],
    c: &[i32; N],
    shift: i32,
    e: &mut [i32],
) -> bool {
    expected_lpc_body::<N>(x, c, shift, e)
}

/// Exact i64 accumulation for any order, coefficient size and shift.
fn expected_lpc_wide(x: &[i32], coeff: &[i32], shift: i32, e: &mut [i32]) -> bool {
    let order = coeff.len();
    let mut out = 0u64;
    for (w, e) in x.windows(order + 1).zip(&mut e[order..]) {
        let mut sum = 0i64;
        for j in 0..order {
            sum += coeff[j] as i64 * w[order - 1 - j] as i64;
        }
        let prediction = if shift >= 0 {
            sum >> shift.min(63)
        } else {
            sum.wrapping_shl(shift.unsigned_abs())
        };
        // restore_lpc computes prediction.wrapping_add(r) for an i32 r.
        let v = (w[order] as i64).wrapping_sub(prediction);
        out |= (v.wrapping_add(1 << 31) as u64) >> 32;
        *e = v as i32;
    }
    out == 0
}

/// Parse a residual section exactly as `residual` does (coding method,
/// partition order, parameters, escapes) and report whether it holds exactly
/// the residuals `e[order..]`. Rice codes are not decoded one after another:
/// each value's code has a known length, so its position follows from the
/// expected values, and the bits there are compared with the code (see
/// [`rice_codes_at`]). A decoder reading the same bits takes the same codes,
/// in the same places, by induction over the values.
fn verify_residual(b: &mut Bits<'_>, e: &[i32], order: usize) -> Result<bool> {
    let _profile = crate::profile::scope(crate::profile::Stage::FlacResidual);
    let method = b.get(2)?;
    let partition = b.get(4)?;
    let partitions = 1usize << partition;
    if method > 1 || !e.len().is_multiple_of(partitions) {
        return invalid("FLAC Rice partition");
    }
    let size = e.len() / partitions;
    if size < order {
        return invalid("FLAC residual predictor order");
    }
    let width = 4 + method;
    let escape = (1 << width) - 1;
    let mut ok = true;
    for i in 0..partitions {
        let k = b.get(width)?;
        let begin = if i == 0 { order } else { i * size };
        let values = &e[begin..(i + 1) * size];
        if k == escape {
            let bits = b.get(5)?;
            for &v in values {
                ok &= b.signed(bits)? == v;
            }
        } else {
            match rice_codes_at(b.data(), b.pos, k, values) {
                Some(end) => b.seek(end)?,
                None => return Ok(false),
            }
        }
    }
    Ok(ok)
}

/// Whether the bits of `data` from bit `pos` are exactly the FLAC Rice codes
/// with parameter `k` (the quotient in unary as zeros, a one, then the low
/// `k` bits) of the folded `values`, all within `data`. Returns the position
/// after the last code. Codes of up to 57 bits are compared with one 8-byte
/// load; longer ones and those in the last bytes bit by bit.
fn rice_codes_at(data: &[u8], pos: usize, k: u32, values: &[i32]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    if crate::kernels::bit_ops() {
        // SAFETY: LZCNT/BMI1/BMI2 were detected at runtime; the body is the
        // same bounds-checked safe Rust (variable shifts become SHLX/SHRX).
        return unsafe { rice_codes_bit_ops(data, pos, k, values) };
    }
    rice_codes_body(data, pos, k, values)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "lzcnt,bmi1,bmi2")]
unsafe fn rice_codes_bit_ops(data: &[u8], pos: usize, k: u32, values: &[i32]) -> Option<usize> {
    rice_codes_body(data, pos, k, values)
}

#[inline(always)]
fn rice_codes_body(data: &[u8], mut pos: usize, k: u32, values: &[i32]) -> Option<usize> {
    let suffix = 1u64 << k;
    let mask = suffix - 1;
    let mut diff = 0u64;
    let fold = |v: i32| ((v << 1) ^ (v >> 31)) as u32 as u64;
    // Compare `len` (at most 57) bits at `pos` with `code`, or bit by bit.
    let compare = |pos: usize, code: u64, len: u64, u: u64, diff: &mut u64| match data
        .get(pos >> 3..(pos >> 3) + 8)
    {
        Some(bytes) if len <= 57 => {
            let word = u64::from_be_bytes(bytes.try_into().unwrap()) << (pos & 7);
            *diff |= (word >> (64 - len)) ^ code;
            Some(())
        }
        _ => rice_code_bits(data, pos, k, u)?.then_some(()),
    };
    // Two consecutive codes are one bit string: compare them together
    // when they fit 57 bits.
    let (pairs, rest) = values.as_chunks::<2>();
    for &[a, b] in pairs {
        let (a, b) = (fold(a), fold(b));
        let (ca, la) = (suffix | (a & mask), (a >> k) + 1 + k as u64);
        let (cb, lb) = (suffix | (b & mask), (b >> k) + 1 + k as u64);
        let joined = data.get(pos >> 3..(pos >> 3) + 8);
        if let (Some(bytes), true) = (joined, la + lb <= 57) {
            let word = u64::from_be_bytes(bytes.try_into().unwrap()) << (pos & 7);
            diff |= (word >> (64 - (la + lb))) ^ ((ca << lb) | cb);
        } else {
            compare(pos, ca, la, a, &mut diff)?;
            compare(pos + la as usize, cb, lb, b, &mut diff)?;
        }
        pos += (la + lb) as usize;
    }
    for &v in rest {
        let u = fold(v);
        let len = (u >> k) + 1 + k as u64;
        compare(pos, suffix | (u & mask), len, u, &mut diff)?;
        pos += len as usize;
    }
    (diff == 0 && pos <= data.len() * 8).then_some(pos)
}

/// One Rice code compared bit by bit; None when it extends past `data`.
#[cold]
fn rice_code_bits(data: &[u8], pos: usize, k: u32, u: u64) -> Option<bool> {
    let bit = |p: usize| data.get(p / 8).map(|b| (b >> (7 - p % 8)) & 1);
    let q = (u >> k) as usize;
    for i in 0..q {
        if bit(pos + i)? != 0 {
            return Some(false);
        }
    }
    if bit(pos + q)? != 1 {
        return Some(false);
    }
    for i in 0..k as usize {
        if bit(pos + q + 1 + i)? as u64 != (u >> (k as usize - 1 - i)) & 1 {
            return Some(false);
        }
    }
    Some(true)
}

/// Undo left/side (8), side/right (9) or mid/side (10) decorrelation and
/// left-align. The mode is a constant so each loop is branch-free and
/// vectorizable however the caller is inlined; range failures are
/// accumulated. Returns true when any sample is out of range.
#[inline(always)]
fn decorrelate<const MODE: u32>(
    a: &[i32],
    d: &[i32],
    out: &mut [i32],
    bound: i32,
    shift: u32,
) -> bool {
    // -bound <= v < bound exactly when (v + bound) as u32 < 2 * bound, a
    // power of two (bound <= 2^24; v + bound wraps only for v >= 2^31 - bound,
    // which is out of range too). So OR-accumulating those values and testing
    // the high bits once keeps every lane 32-bit.
    let span = 2 * bound as u32;
    let mut bits = 0u32;
    for ((dst, &a), &d) in out.as_chunks_mut::<2>().0.iter_mut().zip(a).zip(d) {
        let (left, right) = match MODE {
            8 => (a, a - d),
            9 => (a + d, d),
            _ => {
                let mid = (a << 1) | (d & 1);
                ((mid + d) >> 1, (mid - d) >> 1)
            }
        };
        bits |= left.wrapping_add(bound) as u32 | right.wrapping_add(bound) as u32;
        dst[0] = left.wrapping_shl(shift);
        dst[1] = right.wrapping_shl(shift);
    }
    bits >= span
}

impl Decoder {
    pub fn open(mut file: File, limits: &Limits) -> Result<Self> {
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        if &magic != b"fLaC" {
            return invalid("FLAC signature");
        }
        let length = file.metadata()?.len();
        let mut info = None;
        for i in 0..limits.max_chunks {
            limits.check()?;
            let mut h = [0; 4];
            file.read_exact(&mut h)?;
            let kind = h[0] & 127;
            let size = u32::from_be_bytes([0, h[1], h[2], h[3]]) as u64;
            if size > limits.max_packet_bytes as u64
                || size > length.saturating_sub(file.stream_position()?)
            {
                return invalid("FLAC metadata size");
            }
            if i == 0 && kind != 0 {
                return invalid("FLAC STREAMINFO must be first");
            }
            if kind == 127 {
                return invalid("FLAC metadata type");
            }
            if kind == 0 {
                if info.is_some() || size != 34 {
                    return invalid("FLAC STREAMINFO size/count");
                }
                let mut b = [0; 34];
                file.read_exact(&mut b)?;
                info = Some(b);
            } else {
                file.seek(SeekFrom::Current(size as i64))?;
            }
            if h[0] & 128 != 0 {
                break;
            }
            if i + 1 == limits.max_chunks {
                return Err(Error::Limit("FLAC metadata blocks"));
            }
        }
        let info = info.ok_or(Error::Invalid("missing FLAC STREAMINFO"))?;
        let maximum = u16::from_be_bytes(info[2..4].try_into().unwrap());
        let minimum = u16::from_be_bytes(info[..2].try_into().unwrap());
        if minimum < 16 || maximum < minimum {
            return invalid("FLAC STREAMINFO block size");
        }
        let packed = u64::from_be_bytes(info[10..18].try_into().unwrap());
        let spec = AudioSpec {
            container: "flac".into(),
            codec: "flac".into(),
            sample_rate: (packed >> 44) as u32,
            channels: ((packed >> 41) & 7) as u16 + 1,
            bits_per_sample: ((packed >> 36) & 31) as u16 + 1,
            frames: Some(packed & 0xfffffffff),
        };
        spec.validate()?;
        if spec.frames == Some(0) {
            return Err(Error::Unsupported("FLAC declared sample count required"));
        }
        if spec.frames.unwrap() > limits.max_frames {
            return Err(Error::Limit("FLAC sample count"));
        }
        let md5 = if info[18..].iter().any(|&b| b != 0) {
            Some(Md5::new())
        } else {
            None
        };
        Ok(Self {
            file,
            spec,
            info,
            buffer: Vec::new(),
            end: 0,
            start: 0,
            retained: None,
            eof: false,
            planes: std::array::from_fn(|_| Vec::new()),
            decoded: 0,
            frame: 0,
            strategy: None,
            md5,
            md5_verified: false,
            packed: Vec::new(),
        })
    }
    fn fill(&mut self, limit: usize) -> Result<()> {
        // Keep the allocation (and its initialized bytes) across refills;
        // only the unread tail moves and only new bytes are read.
        self.buffer.copy_within(self.start..self.end, 0);
        self.end -= self.start;
        self.start = 0;
        let old = self.end;
        let target = (old + 65536).min(limit);
        if target == old {
            return Err(Error::Limit("FLAC packet bytes"));
        }
        if self.buffer.len() < target {
            self.buffer.resize(target, 0);
        }
        let n = self.file.read(&mut self.buffer[old..target])?;
        self.end = old + n;
        if n == 0 {
            self.eof = true;
        }
        Ok(())
    }
    /// Decode the next frame into `out` (empty at the verified end). With
    /// `sha`, the caller's canonical SHA-256 of `out` is computed here,
    /// fused with this decoder's STREAMINFO MD5 when that check is active.
    pub fn next(
        &mut self,
        out: &mut Vec<i32>,
        limits: &Limits,
        retain: bool,
        sha: Option<&mut crate::sha256::Sha256>,
    ) -> Result<()> {
        self.retained = None;
        if self.decoded == self.spec.frames.unwrap() {
            out.clear();
            if self.start != self.end {
                return invalid("FLAC trailing data");
            }
            let mut tail = [0];
            if self.file.read(&mut tail)? != 0 {
                return invalid("FLAC trailing data");
            }
            if let Some(md5) = self.md5.take() {
                if md5.finalize()[..] != self.info[18..] {
                    return invalid("FLAC MD5 mismatch");
                }
                self.md5_verified = true;
            }
            return Ok(());
        }
        let maximum = u16::from_be_bytes(self.info[2..4].try_into().unwrap()) as usize;
        let declared_max =
            u32::from_be_bytes([0, self.info[7], self.info[8], self.info[9]]) as usize;
        if declared_max > limits.max_packet_bytes {
            return Err(Error::Limit("FLAC declared packet bytes"));
        }
        loop {
            limits.check()?;
            if !self.eof && self.end - self.start < declared_max {
                self.fill(limits.max_packet_bytes)?;
                continue;
            }
            let data = &self.buffer[self.start..self.end];
            match decode(data, &self.spec, maximum, &mut self.planes, out) {
                Ok((n, h)) => {
                    if declared_max != 0 && n > declared_max {
                        return invalid("FLAC declared frame size");
                    }
                    if self.strategy.is_some_and(|s| s != h.variable)
                        || h.number != if h.variable { self.decoded } else { self.frame }
                    {
                        return invalid("FLAC frame sequence");
                    }
                    if h.samples as u64 > self.spec.frames.unwrap() - self.decoded {
                        return invalid("FLAC decoded sample count");
                    }
                    self.strategy = Some(h.variable);
                    self.decoded += h.samples as u64;
                    self.frame += 1;
                    let bits = self.spec.bits_per_sample;
                    match (&mut self.md5, sha) {
                        (Some(md5), sha) => {
                            let _profile = crate::profile::scope(crate::profile::Stage::FlacMd5);
                            crate::audio::compact_pcm(out, bits, &mut self.packed);
                            match sha {
                                Some(sha) => crate::pcm_hash::update_both(
                                    sha,
                                    &crate::audio::pcm_bytes(out),
                                    md5,
                                    &self.packed,
                                    bits,
                                ),
                                None => md5.update(&self.packed),
                            }
                        }
                        (None, Some(sha)) => sha.update(crate::audio::pcm_bytes(out)),
                        (None, None) => (),
                    }
                    self.retained = retain.then_some(self.start..self.start + n);
                    if retain {
                        crate::profile::count(crate::profile::Counter::FlacBorrowedBytes, n as u64);
                    }
                    self.start += n;
                    return Ok(());
                }
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && !self.eof => {
                    self.fill(limits.max_packet_bytes)?
                }
                Err(e) => return Err(e),
            }
        }
    }
    /// Whether the STREAMINFO MD5 is being checked (it is not all zero).
    pub fn md5_active(&self) -> bool {
        self.md5.is_some() || self.md5_verified
    }
    /// The STREAMINFO MD5 once the decoded PCM has been verified against it.
    pub fn verified_md5(&self) -> Option<[u8; 16]> {
        self.md5_verified
            .then(|| self.info[18..].try_into().unwrap())
    }
    pub fn flac_frame(&self) -> Option<&[u8]> {
        self.retained
            .as_ref()
            .map(|range| &self.buffer[range.clone()])
    }
}

/// Fixed predictors 0..4. Inputs are bounded to 25 bits once validated, so
/// predictions fit i32; the residual addition widens. Range failures are
/// accumulated and reported once per subframe instead of branching per sample.
fn restore_fixed(p: &mut [i32], order: usize, bits: u32) -> Result<()> {
    let _profile = crate::profile::scope(crate::profile::Stage::FlacPredict);
    let low = -(1i64 << (bits - 1));
    // One unsigned comparison checks low <= v < 2^(bits-1).
    let span = (1u64 << bits) - 1;
    let mut bad = false;
    for &v in &p[..order] {
        bad |= (v as i64).wrapping_sub(low) as u64 > span;
    }
    macro_rules! run {
        ($predict:expr) => {
            for i in order..p.len() {
                let v = $predict(&*p, i) as i64 + p[i] as i64;
                bad |= v.wrapping_sub(low) as u64 > span;
                p[i] = v as i32;
            }
        };
    }
    match order {
        0 => {
            for &v in p.iter() {
                bad |= (v as i64).wrapping_sub(low) as u64 > span;
            }
        }
        1 => run!(|p: &[i32], i: usize| p[i - 1]),
        2 => run!(|p: &[i32], i: usize| p[i - 1].wrapping_mul(2).wrapping_sub(p[i - 2])),
        3 => run!(|p: &[i32], i: usize| p[i - 1]
            .wrapping_sub(p[i - 2])
            .wrapping_mul(3)
            .wrapping_add(p[i - 3])),
        4 => run!(|p: &[i32], i: usize| p[i - 1]
            .wrapping_add(p[i - 3])
            .wrapping_mul(4)
            .wrapping_sub(p[i - 2].wrapping_mul(6))
            .wrapping_sub(p[i - 4])),
        _ => unreachable!(),
    }
    if bad {
        return invalid("FLAC reconstructed sample range");
    }
    Ok(())
}

/// LPC restoration. Products are widened to i64 (coefficients have at most 15
/// bits, samples 32), and range failures are accumulated per subframe. When
/// the stream's declared precision, width and order bound every partial sum
/// below 2^31 (FFmpeg's flacdsp 32-bit condition), an exact i32 accumulator
/// is used instead; any out-of-range sample still fails the subframe.
#[inline]
fn restore_lpc<const N: usize>(p: &mut [i32], coeff: &[i32], shift: i32, bits: u32) -> Result<()> {
    let _profile = crate::profile::scope(crate::profile::Stage::FlacPredict);
    let order = if N == 0 { coeff.len() } else { N };
    let low = -(1i64 << (bits - 1));
    // One unsigned comparison checks low <= v < 2^(bits-1).
    let span = (1u64 << bits) - 1;
    let mut bad = false;
    for &v in &p[..order] {
        bad |= (v as i64).wrapping_sub(low) as u64 > span;
    }
    if N != 0 && (0..32).contains(&shift) && p.len() >= 2 * N {
        let blocks = (p.len() - N) / N * N;
        bad |= lpc_unrolled::<N>(&mut p[..N + blocks], coeff, shift as u32, low, span);
        // The remaining samples continue from the restored ones below.
        return lpc_tail(p, coeff, shift, bits, N + blocks, bad);
    }
    lpc_tail(p, coeff, shift, bits, order, bad)
}

/// Samples from `start` restored one at a time from the stored window.
fn lpc_tail(
    p: &mut [i32],
    coeff: &[i32],
    shift: i32,
    bits: u32,
    start: usize,
    mut bad: bool,
) -> Result<()> {
    let order = coeff.len();
    let low = -(1i64 << (bits - 1));
    let span = (1u64 << bits) - 1;
    let largest = coeff.iter().map(|c| c.unsigned_abs()).max().unwrap_or(0) as u64;
    // Sum of |c| * |x| over the order, with |x| <= 2^(bits-1).
    let bound = largest * order as u64 * (1u64 << (bits - 1));
    if (0..32).contains(&shift) && bound < 1 << 31 {
        for i in start..p.len() {
            let window = &p[i - order..i];
            let mut sum = 0i32;
            for j in 0..order {
                sum = sum.wrapping_add(coeff[j].wrapping_mul(window[order - j - 1]));
            }
            let v = (sum >> shift) as i64 + p[i] as i64;
            bad |= v.wrapping_sub(low) as u64 > span;
            p[i] = v as i32;
        }
    } else {
        for i in start..p.len() {
            let window = &p[i - order..i];
            let mut sum = 0i64;
            for j in 0..order {
                sum += coeff[j] as i64 * window[order - j - 1] as i64;
            }
            let prediction = if shift >= 0 {
                sum >> shift.min(63)
            } else {
                sum.wrapping_shl(shift.unsigned_abs())
            };
            let v = prediction.wrapping_add(p[i] as i64);
            bad |= v.wrapping_sub(low) as u64 > span;
            p[i] = v as i32;
        }
    }
    if bad {
        return invalid("FLAC reconstructed sample range");
    }
    Ok(())
}

/// Fixed-order LPC restoration of `p[N..]`, whose length must be a multiple
/// of N, with the last N samples in a register ring and the loop unrolled N
/// times so that no sample moves between registers. All arithmetic is exact
/// in i64 while the samples are in range (|sum| < 2^51), and
/// (sum + r * 2^shift) >> shift == (sum >> shift) + r, so the residual is
/// added before the shift and the newest product last: the loop-carried path
/// is one multiply, two adds and the shift. Once a sample is out of range the
/// subframe fails, whatever is computed after it (wrapping operations keep
/// that well defined). Returns whether any sample was out of range.
#[inline(always)]
fn lpc_unrolled<const N: usize>(
    p: &mut [i32],
    coeff: &[i32],
    shift: u32,
    low: i64,
    span: u64,
) -> bool {
    let c: [i64; N] = std::array::from_fn(|j| coeff[j] as i64);
    // With head h, sample i - 1 - j is w[(h + j) % N]; h starts at 0.
    let mut w: [i64; N] = std::array::from_fn(|j| p[N - 1 - j] as i64);
    let mut seen = 0u64;
    let (blocks, _) = p[N..].as_chunks_mut::<N>();
    for block in blocks {
        for (u, v) in block.iter_mut().enumerate() {
            let h = (N - u) % N;
            let mut sum = (*v as i64) << shift;
            for j in (1..N).rev() {
                sum = sum.wrapping_add(c[j].wrapping_mul(w[(h + j) % N]));
            }
            let x = sum.wrapping_add(c[0].wrapping_mul(w[h])) >> shift;
            seen |= x.wrapping_sub(low) as u64;
            *v = x as i32;
            // The oldest sample's slot becomes the newest.
            w[(h + N - 1) % N] = x;
        }
    }
    // span is 2^bits - 1, so one OR collects every out-of-range sample.
    seen > span
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lpc_restores_exact_pcm_with_wide_products_shifts_and_short_tails() {
        fn run<const N: usize>() {
            for bits in [16u32, 24, 25] {
                for length in [N, N + 1, N + 7, N + 65, N + 1024] {
                    for shift in [-3, 0, 4, 15] {
                        for large in [false, true] {
                            let coeff: Vec<i32> = (0..N)
                                .map(|i| {
                                    if large {
                                        if i % 2 == 0 {
                                            16383
                                        } else {
                                            -16384
                                        }
                                    } else if i % 2 == 0 {
                                        1
                                    } else {
                                        -1
                                    }
                                })
                                .collect();
                            let expected: Vec<i32> = (0..length)
                                .map(|i| (((i * 73 + 19) % 257) as i32 - 128) * (1 << (bits - 10)))
                                .collect();
                            let mut residual = expected.clone();
                            let mut fits = true;
                            for i in N..length {
                                let mut sum = 0i64;
                                for j in 0..N {
                                    sum += coeff[j] as i64 * expected[i - j - 1] as i64;
                                }
                                let prediction = if shift >= 0 {
                                    sum >> shift
                                } else {
                                    sum << -shift
                                };
                                if let Ok(v) = i32::try_from(expected[i] as i64 - prediction) {
                                    residual[i] = v;
                                } else {
                                    fits = false;
                                    break;
                                }
                            }
                            if !fits {
                                continue;
                            }
                            let mut p = residual.clone();
                            restore_lpc::<N>(&mut p, &coeff, shift, bits).unwrap();
                            assert_eq!(p, expected);
                        }
                    }
                }
            }
            let mut p = vec![(1 << 24) - 1; N + 1];
            assert!(restore_lpc::<N>(&mut p, &vec![16383; N], -16, 25).is_err());
        }
        run::<4>();
        run::<8>();
    }
    #[test]
    fn decorrelation_matches_reference_values_and_range_verdicts() {
        let mut seed = 7u32;
        for bits in [16u32, 24] {
            let bound = 1i32 << (bits - 1);
            let edges = [
                -bound - 1,
                -bound,
                -1,
                0,
                1,
                bound - 1,
                bound,
                2 * bound,
                i32::MIN / 4,
                i32::MAX / 4,
            ];
            for mode in [8u32, 9, 10] {
                for case in 0..400 {
                    let n = 1 + case % 37;
                    let mut pick = || {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        if case % 3 == 0 {
                            edges[(seed >> 8) as usize % edges.len()]
                        } else {
                            (seed as i32) >> (32 - bits - 1)
                        }
                    };
                    let a: Vec<i32> = (0..n).map(|_| pick()).collect();
                    let d: Vec<i32> = (0..n).map(|_| pick()).collect();
                    let shift = 32 - bits;
                    let mut expected = vec![0; 2 * n];
                    let mut expected_bad = false;
                    for i in 0..n {
                        let (left, right) = match mode {
                            8 => (a[i], a[i].wrapping_sub(d[i])),
                            9 => (a[i].wrapping_add(d[i]), d[i]),
                            _ => {
                                let mid = (a[i] << 1) | (d[i] & 1);
                                (mid.wrapping_add(d[i]) >> 1, mid.wrapping_sub(d[i]) >> 1)
                            }
                        };
                        expected_bad |=
                            left < -bound || left >= bound || right < -bound || right >= bound;
                        expected[2 * i] = left.wrapping_shl(shift);
                        expected[2 * i + 1] = right.wrapping_shl(shift);
                    }
                    let mut out = vec![0; 2 * n];
                    let bad = match mode {
                        8 => decorrelate::<8>(&a, &d, &mut out, bound, shift),
                        9 => decorrelate::<9>(&a, &d, &mut out, bound, shift),
                        _ => decorrelate::<10>(&a, &d, &mut out, bound, shift),
                    };
                    assert_eq!(bad, expected_bad, "mode={mode} bits={bits} a={a:?} d={d:?}");
                    if !bad {
                        assert_eq!(out, expected);
                    }
                }
            }
        }
    }
    #[test]
    fn all_predictor_orders_escape_residuals_and_truncations() {
        let spec = AudioSpec {
            container: "flac".into(),
            codec: "flac".into(),
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: 24,
            frames: Some(64),
        };
        for mode in [0, 1, 8, 9, 10, 11, 12, 32, 35, 39, 47, 63] {
            let mut w = crate::bits::BeWriter::new();
            w.put(15, 0x7ffc);
            w.put(1, 0);
            w.put(4, 6);
            w.put(4, 10);
            w.put(4, 0);
            w.put(3, 6);
            w.put(1, 0);
            w.put(8, 0);
            w.put(8, 63);
            let crc = crate::bits::crc8(&w.bytes);
            w.put(8, crc as u64);
            w.put(1, 0);
            w.put(6, mode);
            w.put(1, 0);
            let expected = if mode == 0 {
                w.put(24, 7);
                vec![7i32; 64]
            } else if mode == 1 {
                for i in 0..64 {
                    w.put(24, (7 + i) as u64);
                }
                (7..71).collect()
            } else {
                let order = if mode < 32 {
                    (mode - 8) as usize
                } else {
                    (mode - 31) as usize
                };
                for i in 0..order {
                    w.put(24, (7 + i) as u64);
                }
                if mode >= 32 {
                    w.put(4, 14);
                    w.put(5, 0);
                    for i in 0..order {
                        w.put(15, u64::from(i == 0));
                    }
                }
                w.put(2, 0);
                w.put(4, 0);
                w.put(4, 15);
                w.put(5, 16);
                for i in order..64 {
                    w.put(
                        16,
                        if order == 0 {
                            (7 + i) as u64
                        } else if order == 1 || mode >= 32 {
                            1
                        } else {
                            0
                        },
                    );
                }
                (7..71).collect()
            };
            w.align();
            let crc = crate::bits::crc16(&w.bytes);
            w.put(16, crc as u64);
            let packet = w.bytes;
            let mut planes = std::array::from_fn(|_| Vec::new());
            let mut out = Vec::new();
            let (n, _) = decode(&packet, &spec, 64, &mut planes, &mut out).unwrap();
            assert_eq!(n, packet.len());
            assert_eq!(out, expected.iter().map(|v| v << 8).collect::<Vec<_>>());
            for n in 0..packet.len() {
                assert!(decode(&packet[..n], &spec, 64, &mut planes, &mut out).is_err());
            }
            let mut corrupt = packet.clone();
            let last = corrupt.len() - 1;
            corrupt[last] ^= 1;
            assert!(decode(&corrupt, &spec, 64, &mut planes, &mut out).is_err());
        }
    }
}
