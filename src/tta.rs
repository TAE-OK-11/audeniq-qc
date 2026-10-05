// SPDX-License-Identifier: LGPL-2.1-or-later
// TTA decoder. Originally a Rust translation of FFmpeg libavcodec/tta.c,
// ttadata.c, ttadsp.c (Copyright (c) 2006 Alex Beregszaszi; FFmpeg
// contributors); now an AUDENIQ design: entropy decoding, filtering and
// output run as separate passes over a frame, the filter state is local and
// shifted by value, and the bit reader refills with unaligned word loads.
// Strict CRCs, checked sizes/bit reads and sample-count validation remain.
use crate::{
    bits::{crc32, Lsb},
    AudioSpec, Error, Limits, Result,
};
use std::{fs::File, io::Read};

pub struct Tta {
    file: File,
    pub spec: AudioSpec,
    sizes: Vec<u32>,
    index: usize,
    frame_len: u32,
    packet: Vec<u8>,
    planes: [Vec<i32>; 2],
}
impl Tta {
    pub fn open(mut file: File, limits: &Limits) -> Result<Self> {
        let mut h = [0u8; 22];
        file.read_exact(&mut h)?;
        if &h[..4] != b"TTA1" || le16(&h[4..]) != 1 {
            return Err(Error::Unsupported("encrypted/nonstandard TTA"));
        }
        if crc32(&h[..18]) != le32(&h[18..]) {
            return Err(Error::Invalid("TTA header CRC"));
        }
        let spec = AudioSpec {
            container: "tta".into(),
            codec: "tta".into(),
            channels: le16(&h[6..]),
            bits_per_sample: le16(&h[8..]),
            sample_rate: le32(&h[10..]),
            frames: Some(le32(&h[14..]) as u64),
        };
        spec.validate()?;
        let frame_len = 256 * spec.sample_rate / 245;
        let frames = spec.frames.unwrap().div_ceil(frame_len as u64) as usize;
        if frames > limits.max_chunks || spec.frames.unwrap() > limits.max_frames {
            return Err(Error::Limit("TTA frame table"));
        }
        let mut table = vec![0u8; frames * 4 + 4];
        file.read_exact(&mut table)?;
        if crc32(&table[..frames * 4]) != le32(&table[frames * 4..]) {
            return Err(Error::Invalid("TTA seektable CRC"));
        }
        let sizes: Vec<_> = table[..frames * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| le32(b))
            .collect();
        let mut total = 22 + table.len() as u64;
        for size in &sizes {
            if *size < 4 || *size as usize > limits.max_packet_bytes {
                return Err(Error::Limit("TTA packet bytes"));
            }
            total += *size as u64;
        }
        if total > file.metadata()?.len() {
            return Err(Error::Invalid("truncated TTA frames"));
        }
        Ok(Self {
            file,
            spec,
            sizes,
            index: 0,
            frame_len,
            packet: Vec::new(),
            planes: [Vec::new(), Vec::new()],
        })
    }
    pub fn next(&mut self, out: &mut Vec<i32>, limits: &Limits) -> Result<()> {
        out.clear();
        if self.index == self.sizes.len() {
            return Ok(());
        }
        limits.check()?;
        let size = self.sizes[self.index] as usize;
        self.packet.resize(size, 0);
        self.file.read_exact(&mut self.packet)?;
        if crc32(&self.packet[..size - 4]) != le32(&self.packet[size - 4..]) {
            return Err(Error::Invalid("TTA frame CRC"));
        }
        let channels = self.spec.channels as usize;
        let depth = self.spec.bits_per_sample as u32;
        let frames = (self.spec.frames.unwrap() - self.index as u64 * self.frame_len as u64)
            .min(self.frame_len as u64) as usize;
        // 1. Entropy decoding of every residual (the adaptive Rice state does
        //    not depend on the filters), stopping at the first error.
        for plane in &mut self.planes[..channels] {
            plane.resize(frames, 0);
        }
        let (rows, failure) = decode_residuals(
            &self.packet[..size - 4],
            &mut self.planes[..channels],
            frames,
            limits,
        );
        // 2. Per channel, the adaptive filter and the fixed first-order
        //    predictor over the complete rows.
        let shift = if depth == 16 { 9 } else { 10 };
        for plane in &mut self.planes[..channels] {
            restore(&mut plane[..rows], shift);
        }
        // 3. Decorrelation, range check, alignment and interleaving. A row
        //    out of range is reported before a later decoding error, as the
        //    row-by-row decoder did.
        let min = -(1i32 << (depth - 1));
        let max = (1i32 << (depth - 1)) - 1;
        out.resize(rows * channels, 0);
        let mut bad = None;
        if channels == 2 {
            let [left, right] = &self.planes;
            for (i, ((row, &a), &b)) in out
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(&left[..rows])
                .zip(&right[..rows])
                .enumerate()
            {
                let r = b.wrapping_add(a / 2);
                let l = r.wrapping_sub(a);
                if bad.is_none() && (l < min || l > max || r < min || r > max) {
                    bad = Some(i);
                }
                *row = [l.wrapping_shl(32 - depth), r.wrapping_shl(32 - depth)];
            }
        } else {
            for (i, (o, &x)) in out.iter_mut().zip(&self.planes[0][..rows]).enumerate() {
                if bad.is_none() && (x < min || x > max) {
                    bad = Some(i);
                }
                *o = x.wrapping_shl(32 - depth);
            }
        }
        if bad.is_some() {
            return Err(Error::Invalid("TTA sample range"));
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.index += 1;
        Ok(())
    }
}

/// Adaptive Rice state of one channel.
#[derive(Clone, Copy)]
struct Rice {
    k0: u32,
    k1: u32,
    sum0: u32,
    sum1: u32,
}

/// Decode the residuals of `frames` rows (channels interleaved per row) into
/// `planes`. Returns the number of complete rows and the error that stopped
/// decoding, if any; errors are checked in the order of the format's
/// definition, value by value.
fn decode_residuals(
    data: &[u8],
    planes: &mut [Vec<i32>],
    frames: usize,
    limits: &Limits,
) -> (usize, Option<Error>) {
    #[cfg(target_arch = "x86_64")]
    if crate::kernels::bit_ops() {
        // SAFETY: LZCNT/BMI1/BMI2 were detected at runtime; the body is the
        // same safe Rust (variable shifts become SHLX/SHRX).
        return unsafe { decode_residuals_bit_ops(data, planes, frames, limits) };
    }
    decode_residuals_body(data, planes, frames, limits)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "lzcnt,bmi1,bmi2")]
unsafe fn decode_residuals_bit_ops(
    data: &[u8],
    planes: &mut [Vec<i32>],
    frames: usize,
    limits: &Limits,
) -> (usize, Option<Error>) {
    decode_residuals_body(data, planes, frames, limits)
}

#[inline(always)]
fn decode_residuals_body(
    data: &[u8],
    planes: &mut [Vec<i32>],
    frames: usize,
    limits: &Limits,
) -> (usize, Option<Error>) {
    let mut bits = Lsb::new(data);
    let start = Rice {
        k0: 10,
        k1: 10,
        sum0: 1 << 14,
        sum1: 1 << 14,
    };
    // The channel count is a constant in each loop, so both channels'
    // adaptive states stay in registers.
    if let [left, right] = planes {
        let (mut a, mut b) = (start, start);
        for (i, (l, r)) in left[..frames]
            .iter_mut()
            .zip(&mut right[..frames])
            .enumerate()
        {
            if i % 4096 == 0 {
                if let Err(e) = limits.check() {
                    return (i, Some(e));
                }
            }
            match residual(&mut bits, &mut a) {
                Ok(v) => *l = v,
                Err(e) => return (i, Some(e)),
            }
            match residual(&mut bits, &mut b) {
                Ok(v) => *r = v,
                Err(e) => return (i, Some(e)),
            }
        }
    } else {
        let mut a = start;
        for (i, x) in planes[0][..frames].iter_mut().enumerate() {
            if i % 4096 == 0 {
                if let Err(e) = limits.check() {
                    return (i, Some(e));
                }
            }
            match residual(&mut bits, &mut a) {
                Ok(v) => *x = v,
                Err(e) => return (i, Some(e)),
            }
        }
    }
    (frames, None)
}

/// One residual: a unary run of ones (n), then k bits, k from the high
/// state when n > 0. The common case decodes from one refilled cache word
/// and adapts both states without branches; anything else (a run or code
/// reaching past the cache, a parameter out of range) takes the checked
/// path, which reports errors in the format's order.
#[inline(always)]
fn residual(bits: &mut Lsb<'_>, state: &mut Rice) -> Result<i32> {
    if bits.available < 57 {
        bits.refill();
    }
    let n = bits.cache.trailing_ones();
    let high = n != 0;
    let k = if high { state.k1 } else { state.k0 };
    let q = n.wrapping_sub(high as u32);
    if n + 1 + k > bits.available || k > 31 || q > i32::MAX as u32 >> k || state.k0 > 31 {
        return residual_checked(bits, state);
    }
    let rest = bits.cache >> (n + 1);
    let mut value = (q << k).wrapping_add((rest & ((1u64 << k) - 1)) as u32);
    bits.cache = rest >> k;
    bits.available -= n + 1 + k;
    let (k1, sum1) = adapted(state.k1, state.sum1, value);
    if high {
        (state.k1, state.sum1) = (k1, sum1);
    }
    value = value.wrapping_add((high as u32) << state.k0);
    (state.k0, state.sum0) = adapted(state.k0, state.sum0, value);
    Ok(1i32.wrapping_add(((value >> 1) as i32) ^ ((value & 1) as i32 - 1)))
}

/// [`adapt`] as a value, without branches.
#[inline(always)]
fn adapted(k: u32, sum: u32, v: u32) -> (u32, u32) {
    let sum = sum.wrapping_add(v.wrapping_sub(sum >> 4));
    let down = (k > 0) & (sum < threshold(k));
    let up = !down & (sum > threshold(k + 1));
    (k - down as u32 + up as u32, sum)
}

#[cold]
fn residual_checked(bits: &mut Lsb<'_>, state: &mut Rice) -> Result<i32> {
    let unary = bits.ones(1 << 24)?;
    let high = unary != 0;
    let k = if high { state.k1 } else { state.k0 };
    if k > 31 {
        return Err(Error::Invalid("TTA Rice parameter"));
    }
    let q = if high { unary - 1 } else { unary };
    if q > i32::MAX as u32 >> k {
        return Err(Error::Invalid("TTA residual overflow"));
    }
    let mut value = (q << k).wrapping_add(bits.read(k)?);
    if high {
        adapt(&mut state.k1, &mut state.sum1, value);
        if state.k0 > 31 {
            return Err(Error::Invalid("TTA parameter"));
        }
        value = value.wrapping_add(1u32 << state.k0);
    }
    adapt(&mut state.k0, &mut state.sum0, value);
    Ok(1i32.wrapping_add(((value >> 1) as i32) ^ ((value & 1) as i32 - 1)))
}

/// The eight-tap sign-sign adaptive filter followed by the fixed predictor
/// x[n] + 31/32 x[n-1], over one channel's residuals in place.
///
/// Per sample the format steps the coefficients by the sign of the previous
/// residual, forms the sum of the delay line times the coefficients, adds
/// the shifted sum to the residual and pushes the result into the delay
/// line in place. Written out, the line after sample x is d1..d4,
/// x - d7 - d6 - d5, x - d7 - d6, x - d7, x (d1..d7 the line before it), so
/// the next sum is C + x * Q, where C and Q depend only on the line before
/// x, the next coefficients and the current residual (wrapping i32
/// arithmetic, so the same sum modulo 2^32). They are formed while x is
/// still being computed; the serial path from sample to sample is one
/// multiply, two adds and a shift.
#[inline(never)]
fn restore(values: &mut [i32], shift: u32) {
    let round = 1i32 << (shift - 1);
    // Coefficients already stepped for the current sample.
    let mut qm = [0i32; 8];
    let mut dx = [0i32; 8];
    // Delay line before the current sample's predecessor `x` was pushed.
    let mut dl = [0i32; 8];
    // sum = c + x * q for the current sample; the line starts all zero.
    let (mut c, mut q, mut x) = (round, 0i32, 0i32);
    let mut pred = 0i32;
    for v in values {
        let e = *v;
        let sum = c.wrapping_add(x.wrapping_mul(q));
        // The line before this sample: d = dl with x pushed.
        let [_, d1, d2, d3, d4, d5, d6, d7] = dl;
        let line = [
            d1,
            d2,
            d3,
            d4,
            x.wrapping_sub(d7).wrapping_sub(d6).wrapping_sub(d5),
            x.wrapping_sub(d7).wrapping_sub(d6),
            x.wrapping_sub(d7),
            x,
        ];
        let [_, l1, l2, l3, l4, l5, l6, l7] = line;
        let next_dx = [
            dx[1],
            dx[2],
            dx[3],
            dx[4],
            (l4 >> 30) | 1,
            ((l5 >> 30) | 2) & !1,
            ((l6 >> 30) | 2) & !1,
            ((l7 >> 30) | 4) & !3,
        ];
        let sign = e.signum();
        let mut next = qm;
        for i in 0..8 {
            next[i] = qm[i].wrapping_add(next_dx[i].wrapping_mul(sign));
        }
        // The next sample's sum is c + new_x * q.
        let [n0, n1, n2, n3, n4, n5, n6, n7] = next;
        let n456 = n4.wrapping_add(n5).wrapping_add(n6);
        let n45 = n4.wrapping_add(n5);
        c = round
            .wrapping_add(l1.wrapping_mul(n0))
            .wrapping_add(l2.wrapping_mul(n1))
            .wrapping_add(l3.wrapping_mul(n2))
            .wrapping_add(l4.wrapping_mul(n3))
            .wrapping_sub(l7.wrapping_mul(n456))
            .wrapping_sub(l6.wrapping_mul(n45))
            .wrapping_sub(l5.wrapping_mul(n4));
        q = n456.wrapping_add(n7);
        x = e.wrapping_add(sum >> shift);
        dl = line;
        dx = next_dx;
        qm = next;
        let sample = x.wrapping_add((((pred as i64) * 31) >> 5) as i32);
        pred = sample;
        *v = sample;
    }
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
#[inline(always)]
fn threshold(k: u32) -> u32 {
    1 << (k + 4).min(31)
}
fn adapt(k: &mut u32, sum: &mut u32, v: u32) {
    *sum = sum.wrapping_add(v.wrapping_sub(*sum >> 4));
    if *k > 0 && *sum < threshold(*k) {
        *k -= 1;
    } else if *sum > threshold(*k + 1) {
        *k += 1;
    }
}
