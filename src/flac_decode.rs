//! Bounded native FLAC decoder. Rice/fixed/LPC and channel reconstruction
//! ported/reworked from FFmpeg flac.c, flacdec.c, flacdsp.c (LGPL-2.1-or-later).
//! Copyright (c) 2003 Alex Beregszaszi; (c) 2012 Mans Rullgard.
//! See THIRD_PARTY.md.
use crate::{msb::Bits, AudioSpec, Error, Limits, Result};
use md5::{Digest, Md5};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};
pub(crate) struct Decoder {
    file: File,
    pub spec: AudioSpec,
    pub info: [u8; 34],
    buffer: Vec<u8>,
    start: usize,
    eof: bool,
    planes: [Vec<i32>; 2],
    decoded: u64,
    frame: u64,
    strategy: Option<bool>,
    md5: Option<Md5>,
    packed: Vec<u8>,
}

fn invalid<T>(s: &'static str) -> Result<T> {
    Err(Error::Invalid(s))
}
fn checked(v: i64, bits: u32) -> Result<i32> {
    if v < -(1i64 << (bits - 1)) || v >= 1i64 << (bits - 1) {
        return invalid("FLAC reconstructed sample range");
    }
    Ok(v as i32)
}
fn residual(b: &mut Bits<'_>, p: &mut [i32], order: usize) -> Result<()> {
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
            for v in dst {
                *v = b.rice_signed(k)?;
            }
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
                for i in order..p.len() {
                    let prediction = match order {
                        0 => 0,
                        1 => p[i - 1] as i64,
                        2 => 2 * p[i - 1] as i64 - p[i - 2] as i64,
                        3 => 3 * p[i - 1] as i64 - 3 * p[i - 2] as i64 + p[i - 3] as i64,
                        4 => {
                            4 * p[i - 1] as i64 - 6 * p[i - 2] as i64 + 4 * p[i - 3] as i64
                                - p[i - 4] as i64
                        }
                        _ => unreachable!(),
                    };
                    p[i] = checked(prediction + p[i] as i64, width)?;
                }
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
struct Header {
    samples: usize,
    number: u64,
    variable: bool,
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
fn decode(
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
    out.resize(h.samples * spec.channels as usize, 0);
    let shift = 32 - spec.bits_per_sample;
    if spec.channels == 1 {
        for (dst, &v) in out.iter_mut().zip(&planes[0]) {
            *dst = v.wrapping_shl(shift as u32);
        }
    } else if h.mode < 8 {
        for plane in planes.iter_mut() {
            for v in plane {
                *v = v.wrapping_shl(shift as u32);
            }
        }
        crate::kernels::interleave_i32(&planes[0], &planes[1], out);
    } else {
        // Each subframe is at most 25 signed bits; stereo reconstruction fits
        // i32 even before validation. Accumulate range failures and check once
        // so SIMD/vectorization is possible without weakening integrity.
        let bound = 1i32 << (spec.bits_per_sample - 1);
        let mut bad = false;
        for ((dst, &a), &d) in out
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&planes[0])
            .zip(&planes[1])
        {
            let (left, right) = match h.mode {
                8 => (a, a - d),
                9 => (a + d, d),
                10 => {
                    let mid = (a << 1) | (d & 1);
                    ((mid + d) >> 1, (mid - d) >> 1)
                }
                _ => (a, d),
            };
            bad |= left < -bound || left >= bound || right < -bound || right >= bound;
            dst[0] = left.wrapping_shl(shift as u32);
            dst[1] = right.wrapping_shl(shift as u32);
        }
        if bad {
            return invalid("FLAC reconstructed stereo range");
        }
    }
    Ok((length + 2, h))
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
            start: 0,
            eof: false,
            planes: std::array::from_fn(|_| Vec::new()),
            decoded: 0,
            frame: 0,
            strategy: None,
            md5,
            packed: Vec::new(),
        })
    }
    fn fill(&mut self, limit: usize) -> Result<()> {
        self.buffer.copy_within(self.start.., 0);
        self.buffer.truncate(self.buffer.len() - self.start);
        self.start = 0;
        let old = self.buffer.len();
        let target = (old + 65536).min(limit);
        if target == old {
            return Err(Error::Limit("FLAC packet bytes"));
        }
        self.buffer.resize(target, 0);
        let n = self.file.read(&mut self.buffer[old..])?;
        self.buffer.truncate(old + n);
        if n == 0 {
            self.eof = true;
        }
        Ok(())
    }
    pub fn next(
        &mut self,
        out: &mut Vec<i32>,
        limits: &Limits,
        retain: bool,
    ) -> Result<Option<Box<[u8]>>> {
        out.clear();
        if self.decoded == self.spec.frames.unwrap() {
            if self.start != self.buffer.len() {
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
            }
            return Ok(None);
        }
        let maximum = u16::from_be_bytes(self.info[2..4].try_into().unwrap()) as usize;
        let declared_max =
            u32::from_be_bytes([0, self.info[7], self.info[8], self.info[9]]) as usize;
        if declared_max > limits.max_packet_bytes {
            return Err(Error::Limit("FLAC declared packet bytes"));
        }
        loop {
            limits.check()?;
            if !self.eof && self.buffer.len() - self.start < declared_max {
                self.fill(limits.max_packet_bytes)?;
                continue;
            }
            let data = &self.buffer[self.start..];
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
                    if let Some(md5) = &mut self.md5 {
                        crate::audio::compact_pcm(out, self.spec.bits_per_sample, &mut self.packed);
                        md5.update(&self.packed);
                    }
                    let raw = if retain { Some(data[..n].into()) } else { None };
                    self.start += n;
                    return Ok(raw);
                }
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && !self.eof => {
                    self.fill(limits.max_packet_bytes)?
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[inline]
fn restore_lpc<const N: usize>(p: &mut [i32], coeff: &[i32], shift: i32, bits: u32) -> Result<()> {
    let order = if N == 0 { coeff.len() } else { N };
    for i in order..p.len() {
        let window = &p[i - order..i];
        let mut sum = 0i64;
        for j in 0..order {
            sum += coeff[j] as i64 * window[order - j - 1] as i64;
        }
        let prediction = if shift >= 0 {
            sum >> shift
        } else {
            sum << -shift
        };
        p[i] = checked(prediction + p[i] as i64, bits)?;
    }
    Ok(())
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
