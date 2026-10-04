// SPDX-License-Identifier: LGPL-2.1-or-later
// Rust translation of FFmpeg libavcodec/tta.c, ttadata.c, ttadsp.c.
// Copyright (c) 2006 Alex Beregszaszi; FFmpeg contributors.
// Changes: strict CRCs, checked sizes/bit reads, reusable block storage,
// explicit wrapping arithmetic and complete sample-count validation.
use crate::{
    bits::{crc32, LeBits},
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
        let sizes: Vec<_> = table[..frames * 4].chunks_exact(4).map(le32).collect();
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
        let depth = self.spec.bits_per_sample;
        let frames = (self.spec.frames.unwrap() - self.index as u64 * self.frame_len as u64)
            .min(self.frame_len as u64) as usize;
        let mut states = vec![Channel::new(if depth == 16 { 9 } else { 10 }); channels];
        let mut bits = LeBits::new(&self.packet[..size - 4]);
        out.reserve(frames * channels);
        for i in 0..frames {
            if i % 4096 == 0 {
                limits.check()?;
            }
            let mut row = [0i32; 2];
            for (ch, state) in states.iter_mut().enumerate() {
                let unary = bits.unary_ones(1 << 24)?;
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
                let signed = 1i32.wrapping_add(((value >> 1) as i32) ^ ((value & 1) as i32 - 1));
                let filtered = state.filter(signed);
                let pred = (((state.pred as i64) << 5) - state.pred as i64) >> 5;
                let sample = filtered.wrapping_add(pred as i32);
                state.pred = sample;
                row[ch] = sample;
            }
            if channels == 2 {
                row[1] = row[1].wrapping_add(row[0] / 2);
                row[0] = row[1].wrapping_sub(row[0]);
            }
            let min = -(1i32 << (depth - 1));
            let max = (1i32 << (depth - 1)) - 1;
            for &x in &row[..channels] {
                if x < min || x > max {
                    return Err(Error::Invalid("TTA sample range"));
                }
                out.push(x.wrapping_shl((32 - depth) as u32));
            }
        }
        self.index += 1;
        Ok(())
    }
}
fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn threshold(k: u32) -> u32 {
    if k + 4 >= 31 {
        0x80000000
    } else {
        1 << (k + 4)
    }
}
fn adapt(k: &mut u32, sum: &mut u32, v: u32) {
    *sum = sum.wrapping_add(v.wrapping_sub(*sum >> 4));
    if *k > 0 && *sum < threshold(*k) {
        *k -= 1;
    } else if *sum > threshold(*k + 1) {
        *k += 1;
    }
}
#[derive(Clone)]
struct Channel {
    qm: [i32; 8],
    dx: [i32; 8],
    dl: [i32; 8],
    error: i32,
    shift: u32,
    pred: i32,
    k0: u32,
    k1: u32,
    sum0: u32,
    sum1: u32,
}
impl Channel {
    fn new(shift: u32) -> Self {
        Self {
            qm: [0; 8],
            dx: [0; 8],
            dl: [0; 8],
            error: 0,
            shift,
            pred: 0,
            k0: 10,
            k1: 10,
            sum0: 1 << 14,
            sum1: 1 << 14,
        }
    }
    fn filter(&mut self, input: i32) -> i32 {
        match self.error.cmp(&0) {
            std::cmp::Ordering::Less => {
                for i in 0..8 {
                    self.qm[i] = self.qm[i].wrapping_sub(self.dx[i]);
                }
            }
            std::cmp::Ordering::Greater => {
                for i in 0..8 {
                    self.qm[i] = self.qm[i].wrapping_add(self.dx[i]);
                }
            }
            std::cmp::Ordering::Equal => (),
        }
        let mut sum = 1i32 << (self.shift - 1);
        for i in 0..8 {
            sum = sum.wrapping_add(self.dl[i].wrapping_mul(self.qm[i]));
        }
        self.dx.copy_within(1..5, 0);
        self.dl.copy_within(1..5, 0);
        self.dx[4] = (self.dl[4] >> 30) | 1;
        self.dx[5] = ((self.dl[5] >> 30) | 2) & !1;
        self.dx[6] = ((self.dl[6] >> 30) | 2) & !1;
        self.dx[7] = ((self.dl[7] >> 30) | 4) & !3;
        self.error = input;
        let x = input.wrapping_add(sum >> self.shift);
        self.dl[4] = self.dl[5].wrapping_neg();
        self.dl[5] = self.dl[6].wrapping_neg();
        self.dl[6] = x.wrapping_sub(self.dl[7]);
        self.dl[7] = x;
        self.dl[5] = self.dl[5].wrapping_add(self.dl[6]);
        self.dl[4] = self.dl[4].wrapping_add(self.dl[5]);
        x
    }
}
