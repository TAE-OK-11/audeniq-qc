//! ALAC Rice/adaptive LPC Rust port, reworked for reusable in-place buffers.
//! Derived from FFmpeg libavcodec/alac.c and alacdsp.c (LGPL-2.1-or-later),
//! Copyright (c) 2005 David Hammerton. See THIRD_PARTY.md for the pinned source.
use crate::{msb::Bits, Error, Result};
pub(crate) struct Decoder {
    config: [u8; 24],
    pcm: [Vec<i32>; 2],
    extra: [Vec<u32>; 2],
}

#[inline]
fn extend(v: i32, bits: u32) -> i32 {
    v.wrapping_shl(32 - bits) >> (32 - bits)
}
#[inline]
fn scalar(b: &mut Bits<'_>, k: u32, bits: u32) -> Result<u32> {
    b.alac_scalar(k, bits)
}
fn rice(
    b: &mut Bits<'_>,
    out: &mut [i32],
    bits: u32,
    initial: u32,
    mult: u32,
    limit: u32,
) -> Result<()> {
    let _profile = crate::profile::scope(crate::profile::Stage::AlacRice);
    let mut history = initial;
    let mut modifier = 0;
    let mut i = 0;
    while i < out.len() {
        let k = (31 - ((history >> 9) + 3).leading_zeros()).min(limit);
        let x = scalar(b, k, bits)?
            .checked_add(modifier)
            .ok_or(Error::Invalid("ALAC residual overflow"))?;
        modifier = 0;
        out[i] = ((x >> 1) as i32) ^ -((x & 1) as i32);
        if x > 0xffff {
            history = 0xffff;
        } else {
            history = history
                .wrapping_add(x.wrapping_mul(mult))
                .wrapping_sub((history.wrapping_mul(mult)) >> 9);
        }
        i += 1;
        if history < 128 && i < out.len() {
            let log = if history == 0 {
                0
            } else {
                31 - history.leading_zeros()
            };
            let k = (7 - log + ((history + 16) >> 6)).min(limit);
            let n = scalar(b, k, 16)? as usize;
            if n > out.len() - i {
                return Err(Error::Invalid("ALAC zero-run length"));
            }
            out[i..i + n].fill(0);
            i += n;
            if n <= 0xffff {
                modifier = 1;
            }
            history = 0;
        }
    }
    Ok(())
}
fn predict(p: &mut [i32], bits: u32, coeff: &mut [i16], quant: u32) {
    let _profile = crate::profile::scope(crate::profile::Stage::AlacPredict);
    let order = coeff.len();
    crate::profile::alac_order(order, p.len());
    if order == 0 || p.len() < 2 {
        return;
    }
    if order == 31 {
        for i in 1..p.len() {
            p[i] = extend(p[i - 1].wrapping_add(p[i]), bits);
        }
        return;
    }
    for i in 1..=order.min(p.len() - 1) {
        p[i] = extend(p[i - 1].wrapping_add(p[i]), bits);
    }
    match order {
        4 => predict_order::<4>(p, bits, coeff, quant),
        6 => predict_order::<6>(p, bits, coeff, quant),
        8 => predict_order::<8>(p, bits, coeff, quant),
        _ => predict_order::<0>(p, bits, coeff, quant),
    }
}
impl Decoder {
    pub fn new(config: [u8; 24]) -> Self {
        Self {
            config,
            pcm: std::array::from_fn(|_| Vec::new()),
            extra: std::array::from_fn(|_| Vec::new()),
        }
    }
    pub fn decode(&mut self, packet: &[u8], frames: u32, out: &mut Vec<i32>) -> Result<()> {
        let mut b = Bits::new(packet);
        let channels = self.config[9] as usize;
        let width = self.config[5] as u32;
        let maximum = u32::from_be_bytes(self.config[..4].try_into().unwrap());
        let mut channel = 0;
        let mut count = 0;
        let mut stereo_output = false;
        loop {
            let element = b.get(3)?;
            if element == 7 {
                break;
            }
            let n = match element {
                0 | 3 => 1,
                1 => 2,
                _ => return Err(Error::Unsupported("ALAC syntax element")),
            };
            if channel + n > channels {
                return Err(Error::Invalid("ALAC element channels"));
            }
            b.get(4)?;
            if b.get(12)? != 0 {
                return Err(Error::Invalid("ALAC reserved header"));
            }
            let sized = b.get(1)? != 0;
            let mut extra = b.get(2)? * 8;
            let compressed = b.get(1)? == 0;
            let samples = if sized { b.get(32)? } else { maximum };
            if samples == 0
                || samples > maximum
                || samples != frames
                || (count != 0 && count != samples)
            {
                return Err(Error::Invalid("ALAC sample count"));
            }
            count = samples;
            let count = samples as usize;
            for ch in channel..channel + n {
                self.pcm[ch].resize(count, 0);
            }
            let mut shift = 0;
            let mut weight = 0;
            if compressed {
                let bits = width
                    .checked_sub(extra)
                    .ok_or(Error::Invalid("ALAC extra bits"))?
                    + n as u32
                    - 1;
                if bits == 0 || bits > 32 || self.config[8] == 0 {
                    return Err(Error::Invalid("ALAC coded width/Rice limit"));
                }
                shift = b.get(8)?;
                weight = b.get(8)?;
                if n == 2 && weight != 0 && shift > 31 {
                    return Err(Error::Invalid("ALAC stereo shift"));
                }
                let mut coeff = [[0i16; 32]; 2];
                let mut orders = [0usize; 2];
                let mut modes = [0; 2];
                let mut quant = [0; 2];
                let mut mult = [0; 2];
                for ch in 0..n {
                    modes[ch] = b.get(4)?;
                    quant[ch] = b.get(4)?;
                    mult[ch] = b.get(3)? * self.config[6] as u32 / 4;
                    orders[ch] = b.get(5)? as usize;
                    if !matches!(modes[ch], 0 | 15)
                        || quant[ch] == 0
                        || orders[ch] >= maximum as usize
                    {
                        return Err(Error::Invalid("ALAC predictor configuration"));
                    }
                    for i in (0..orders[ch]).rev() {
                        coeff[ch][i] = b.signed(16)? as i16;
                    }
                }
                if extra != 0 {
                    for ch in channel..channel + n {
                        self.extra[ch].resize(count, 0);
                    }
                    for i in 0..count {
                        for ch in channel..channel + n {
                            self.extra[ch][i] = b.get(extra)?;
                        }
                    }
                }
                for ch in 0..n {
                    let p = &mut self.pcm[channel + ch];
                    rice(
                        &mut b,
                        p,
                        bits,
                        self.config[7] as u32,
                        mult[ch],
                        self.config[8] as u32,
                    )?;
                    if modes[ch] == 15 {
                        predict(p, bits, &mut [0; 31], 0);
                    }
                    predict(p, bits, &mut coeff[ch][..orders[ch]], quant[ch]);
                }
            } else {
                for i in 0..count {
                    for ch in channel..channel + n {
                        self.pcm[ch][i] = b.signed(width)?;
                    }
                }
                extra = 0;
            }
            let _profile = crate::profile::scope(crate::profile::Stage::AlacOutput);
            if n == 2 {
                out.resize(count * 2, 0);
                finish_stereo(&self.pcm, &self.extra, width, extra, weight, shift, out);
                stereo_output = true;
            } else {
                for i in 0..count {
                    let value = if extra != 0 {
                        self.pcm[channel][i].wrapping_shl(extra) | (self.extra[channel][i] as i32)
                    } else {
                        self.pcm[channel][i]
                    };
                    self.pcm[channel][i] = value.wrapping_shl(32 - width);
                }
            }
            channel += n;
        }
        if channel != channels || count == 0 || b.left() > 7 {
            return Err(Error::Invalid("ALAC end/trailing data"));
        }
        b.align_zero()?;
        if channels == 1 {
            std::mem::swap(out, &mut self.pcm[0]);
            crate::profile::count(crate::profile::Counter::MonoBufferSwaps, 1);
        } else if !stereo_output {
            out.resize(count as usize * 2, 0);
            crate::kernels::interleave_i32(&self.pcm[0], &self.pcm[1], out);
        }
        Ok(())
    }
}

// CPE reconstruction, extra-bit append, canonical alignment and interleaving
// share one traversal. Predictor planes are not rewritten just to be copied.
fn finish_stereo(
    pcm: &[Vec<i32>; 2],
    tail: &[Vec<u32>; 2],
    width: u32,
    extra: u32,
    weight: u32,
    shift: u32,
    out: &mut [i32],
) {
    for (i, (row, (&a, &d))) in out
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(pcm[0].iter().zip(&pcm[1]))
        .enumerate()
    {
        let (mut left, mut right) = (a, d);
        if weight != 0 {
            right = left.wrapping_sub(right.wrapping_mul(weight as i32) >> shift);
            left = right.wrapping_add(d);
        }
        if extra != 0 {
            left = left.wrapping_shl(extra) | tail[0][i] as i32;
            right = right.wrapping_shl(extra) | tail[1][i] as i32;
        }
        row[0] = left.wrapping_shl(32 - width);
        row[1] = right.wrapping_shl(32 - width);
    }
}

#[inline]
fn predict_order<const N: usize>(p: &mut [i32], bits: u32, coeff: &mut [i16], quant: u32) {
    let order = if N == 0 { coeff.len() } else { N };
    for i in order + 1..p.len() {
        let start = i - order - 1;
        let base = p[start];
        let error = p[i];
        let mut sum = 0i32;
        for (j, &sample) in p[start + 1..i].iter().enumerate() {
            sum = sum.wrapping_add(sample.wrapping_sub(base).wrapping_mul(coeff[j] as i32));
        }
        let pred = ((sum as i64 + (1i64 << (quant - 1))) >> quant) as i32;
        p[i] = extend(pred.wrapping_add(base).wrapping_add(error), bits);
        let sign = error.signum();
        let mut remaining = error;
        if sign > 0 {
            for j in 0..order {
                if remaining <= 0 {
                    break;
                }
                let delta = base.wrapping_sub(p[start + 1 + j]);
                let direction = delta.signum();
                coeff[j] = coeff[j].wrapping_sub(direction as i16);
                remaining = remaining.wrapping_sub(
                    (delta.wrapping_mul(direction) >> quant).wrapping_mul((j + 1) as i32),
                );
            }
        } else if sign < 0 {
            for j in 0..order {
                if remaining >= 0 {
                    break;
                }
                let delta = base.wrapping_sub(p[start + 1 + j]);
                let direction = -delta.signum();
                coeff[j] = coeff[j].wrapping_sub(direction as i16);
                remaining = remaining.wrapping_sub(
                    (delta.wrapping_mul(direction) >> quant).wrapping_mul((j + 1) as i32),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_predictor_matches_scalar_pcm_and_coefficient_state() {
        for order in [4, 6, 8] {
            for bits in [1, 8, 16, 17, 24, 25] {
                for quant in 1..=15 {
                    for n in [
                        0,
                        1,
                        order,
                        order + 1,
                        order + 2,
                        order + 7,
                        order + 63,
                        order + 511,
                    ] {
                        for pattern in 0..4 {
                            let mut seed = 1729u32;
                            let mut input: Vec<i32> = (0..n)
                                .map(|i| {
                                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                                    match pattern {
                                        0 => 0,
                                        1 => [i32::MIN, i32::MAX, -1, 0, 1][i % 5],
                                        2 => seed as i32,
                                        _ => extend(seed as i32, bits),
                                    }
                                })
                                .collect();
                            if pattern == 1 && !input.is_empty() {
                                input[0] = 0;
                            }
                            let initial: Vec<i16> = (0..order)
                                .map(|i| {
                                    [i16::MIN, i16::MAX, 0, 1, -1, 1023, -1024][(i + pattern) % 7]
                                })
                                .collect();
                            let mut expected = input.clone();
                            let mut expected_coeff = initial.clone();
                            if expected.len() > 1 {
                                for i in 1..=order.min(expected.len() - 1) {
                                    expected[i] =
                                        extend(expected[i - 1].wrapping_add(expected[i]), bits);
                                }
                                predict_order::<0>(&mut expected, bits, &mut expected_coeff, quant);
                            }
                            let mut actual = input;
                            let mut actual_coeff = initial;
                            predict(&mut actual, bits, &mut actual_coeff, quant);
                            assert_eq!(
                                actual, expected,
                                "order={order} bits={bits} quant={quant} n={n} pattern={pattern}"
                            );
                            assert_eq!(actual_coeff, expected_coeff, "coefficients order={order} bits={bits} quant={quant} n={n} pattern={pattern}");
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn fused_output_matches_staged_wrapping_reconstruction() {
        for n in [1, 3, 4, 7, 16, 65] {
            let pcm = std::array::from_fn(|ch| {
                (0..n)
                    .map(|i| {
                        (i as i32).wrapping_mul(123456789).wrapping_add(if ch == 0 {
                            i32::MIN
                        } else {
                            i32::MAX
                        })
                    })
                    .collect::<Vec<_>>()
            });
            let tail = std::array::from_fn(|ch| {
                (0..n)
                    .map(|i| (i * 73 + ch * 19) as u32 & 65535)
                    .collect::<Vec<_>>()
            });
            for width in [16, 24] {
                for extra in [0, 8, 16] {
                    for weight in [0, 1, 255] {
                        for shift in [0, 1, 31] {
                            let mut expected = pcm.clone();
                            if weight != 0 {
                                for i in 0..n {
                                    let right = pcm[0][i].wrapping_sub(
                                        pcm[1][i].wrapping_mul(weight as i32) >> shift,
                                    );
                                    expected[0][i] = right.wrapping_add(pcm[1][i]);
                                    expected[1][i] = right;
                                }
                            }
                            for ch in 0..2 {
                                for i in 0..n {
                                    if extra != 0 {
                                        expected[ch][i] = expected[ch][i].wrapping_shl(extra)
                                            | tail[ch][i] as i32;
                                    }
                                    expected[ch][i] = expected[ch][i].wrapping_shl(32 - width);
                                }
                            }
                            let mut out = vec![123; n * 2];
                            finish_stereo(&pcm, &tail, width, extra, weight, shift, &mut out);
                            for i in 0..n {
                                assert_eq!(
                                    &out[i * 2..i * 2 + 2],
                                    &[expected[0][i], expected[1][i]]
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn escaped_packets_extremes_and_every_truncation() {
        for width in [16u32, 24] {
            for channels in [1, 2] {
                let count = 9;
                let mut config = [0u8; 24];
                config[..4].copy_from_slice(&(count as u32).to_be_bytes());
                config[5] = width as u8;
                config[9] = channels;
                let mut w = crate::bits::BeWriter::new();
                w.put(3, if channels == 2 { 1 } else { 0 });
                w.put(4, 0);
                w.put(12, 0);
                w.put(1, 1);
                w.put(2, 0);
                w.put(1, 1);
                w.put(32, count as u64);
                let values = [-(1i32 << (width - 1)), -1, 0, 1, (1 << (width - 1)) - 1];
                let mut expected = Vec::new();
                for i in 0..count * channels as usize {
                    let value = values[i % values.len()];
                    w.put(width, value as u32 as u64);
                    expected.push(value.wrapping_shl(32 - width));
                }
                w.put(3, 7);
                w.align();
                let packet = w.bytes;
                let mut out = Vec::new();
                let mut d = Decoder::new(config);
                d.decode(&packet, count as u32, &mut out).unwrap();
                assert_eq!(out, expected);
                d.decode(&packet, count as u32, &mut out).unwrap();
                assert_eq!(out, expected);
                for n in 0..packet.len() {
                    assert!(d.decode(&packet[..n], count as u32, &mut out).is_err());
                }
                let mut tail = packet.clone();
                tail.push(0);
                assert!(d.decode(&tail, count as u32, &mut out).is_err());
            }
        }
    }
    #[test]
    fn malformed_packets_and_configs_never_panic() {
        let mut state = 1729u64;
        let mut packet = vec![0; 96];
        for _ in 0..4096 {
            for v in &mut packet {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *v = state as u8;
            }
            let mut config = [0; 24];
            config[..4].copy_from_slice(&32u32.to_be_bytes());
            config[5] = 24;
            config[6] = 40;
            config[7] = 10;
            config[8] = 14;
            config[9] = 2;
            let mut d = Decoder::new(config);
            let mut out = Vec::new();
            let _ = d.decode(&packet, 32, &mut out);
        }
    }
}
