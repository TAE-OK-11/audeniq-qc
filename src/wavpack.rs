// SPDX-License-Identifier: LGPL-2.1-or-later
// WavPack integer lossless decoder. Originally a Rust port of FFmpeg
// libavcodec/wavpack.c and wavpack.h (Copyright (c) 2006,2011 Konstantin
// Shishkov; (c) 2020 David Bryant); now an AUDENIQ design: entropy decoding
// with its state in registers, decorrelation and output as separate passes
// over a block, branch-free weight steps, and the shared word-refill bit
// reader. No hybrid/float/DSD/correction/multichannel code; checked reads,
// strict sample CRCs, all-block format checks, reusable buffers, bounded work.
use crate::{bits::Lsb, AudioSpec, Error, Limits, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

const RATES: [u32; 16] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    192000, 0,
];
include!("wavpack_table.rs");
fn le16(b: &[u8]) -> i16 {
    i16::from_le_bytes(b[..2].try_into().unwrap())
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn exp2(v: i16) -> Result<i32> {
    let v = v as i32;
    let n = v.unsigned_abs();
    let shift = n >> 8;
    if shift > 31 {
        return Err(Error::Invalid("WavPack exponent"));
    }
    let x = (EXP2[(n & 255) as usize] as u32) | 256;
    let x = if shift > 9 {
        x.wrapping_shl(shift - 9)
    } else {
        x >> (9 - shift)
    } as i32;
    Ok(if v < 0 { x.wrapping_neg() } else { x })
}
pub struct Wavpack {
    file: File,
    pub spec: AudioSpec,
    position: u64,
    frames: u64,
    packet: Vec<u8>,
    blocks: usize,
    planes: [Vec<i32>; 2],
}
impl Wavpack {
    pub fn open(mut file: File, limits: &Limits) -> Result<Self> {
        let mut h = [0u8; 32];
        file.read_exact(&mut h)?;
        validate_header(&h)?;
        let flags = le32(&h[24..]);
        let size = le32(&h[4..]) as usize + 8;
        if size > limits.max_packet_bytes || size < 32 {
            return Err(Error::Limit("WavPack block"));
        }
        let mut data = vec![0; size - 32];
        file.read_exact(&mut data)?;
        let c = Context::parse(&data, flags)?;
        let rate = RATES[((flags >> 23) & 15) as usize];
        let rate = if rate == 0 {
            c.rate.ok_or(Error::Invalid("WavPack custom rate"))?
        } else {
            rate
        };
        let depth = (((flags & 3) + 1) * 8)
            .checked_sub((flags >> 13) & 31)
            .ok_or(Error::Invalid("WavPack bit depth"))? as u16;
        let total = le32(&h[12..]);
        if total == u32::MAX {
            return Err(Error::Unsupported("unknown WavPack sample count"));
        }
        let spec = AudioSpec {
            container: "wv".into(),
            codec: "wavpack".into(),
            sample_rate: rate,
            channels: if flags & 4 != 0 { 1 } else { 2 },
            bits_per_sample: depth,
            frames: Some(total as u64),
        };
        spec.validate()?;
        file.rewind()?;
        Ok(Self {
            file,
            spec,
            position: 0,
            frames: 0,
            packet: Vec::new(),
            blocks: 0,
            planes: [Vec::new(), Vec::new()],
        })
    }
    pub fn next(&mut self, out: &mut Vec<i32>, limits: &Limits) -> Result<()> {
        out.clear();
        let len = self.file.metadata()?.len();
        if self.frames == self.spec.frames.unwrap() {
            // Recognized trailing tags only; do not scan arbitrary bytes for blocks.
            if self.position < len {
                self.validate_tail(len, limits)?;
                self.position = len;
            }
            return Ok(());
        }
        if len - self.position < 32 {
            return Err(Error::Invalid("truncated WavPack header"));
        }
        limits.check()?;
        self.blocks += 1;
        if self.blocks > limits.max_chunks {
            return Err(Error::Limit("WavPack blocks"));
        }
        let mut h = [0u8; 32];
        self.file.read_exact(&mut h)?;
        validate_header(&h)?;
        let flags = le32(&h[24..]);
        let size = le32(&h[4..]) as u64 + 8;
        if size < 32 || size > len - self.position || size as usize > limits.max_packet_bytes {
            return Err(Error::Invalid("WavPack block length"));
        }
        let declared = le32(&h[12..]);
        if le32(&h[16..]) as u64 != self.frames
            || (declared != 0
                && declared != u32::MAX
                && declared as u64 != self.spec.frames.unwrap())
        {
            return Err(Error::Invalid("WavPack index/count"));
        }
        let n = le32(&h[20..]) as usize;
        if n == 0 || n > 150_000 {
            return Err(Error::Limit("WavPack block samples"));
        }
        self.packet.resize(size as usize - 32, 0);
        self.file.read_exact(&mut self.packet)?;
        let mut c = Context::parse(&self.packet, flags)?;
        let rate = RATES[((flags >> 23) & 15) as usize];
        let rate = if rate == 0 { c.rate.unwrap_or(0) } else { rate };
        let depth = (((flags & 3) + 1) * 8)
            .checked_sub((flags >> 13) & 31)
            .ok_or(Error::Invalid("WavPack bit depth"))? as u16;
        if rate != self.spec.sample_rate
            || depth != self.spec.bits_per_sample
            || (if flags & 4 != 0 { 1 } else { 2 }) != self.spec.channels
        {
            return Err(Error::Invalid("WavPack format change"));
        }
        let stereo = c.stereo;
        let false_stereo = flags & 0x40000000 != 0;
        let joint = flags & 16 != 0;
        let mut bits = Lsb::new(c.data.ok_or(Error::Invalid("WavPack missing samples"))?);
        let [left, right] = &mut self.planes;
        left.resize(n, 0);
        right.resize(n, 0);
        // 1. Entropy decoding of every sample (the entropy state does not
        //    depend on the decorrelation), stopping at the first error.
        let mut words = c.words;
        let (rows, failure) = if stereo {
            decode_words::<true>(&mut words, &mut bits, left, right, n, limits)
        } else {
            decode_words::<false>(&mut words, &mut bits, left, right, n, limits)
        };
        // 2. The decorrelation terms, sample by sample: consecutive terms'
        //    serial chains overlap.
        let (left, right) = (&mut left[..rows], &mut right[..rows]);
        decorrelate(&mut c.decorr, left, right, stereo);
        // 3. Joint stereo, sample CRC, extra bits, shifts and interleaving,
        //    sample by sample in the format's order (an extra-bits error
        //    before the decoding error is reported first).
        let mut extra = c.extra.map(Lsb::new);
        let mut crc = !0u32;
        let mut extra_crc = !0u32;
        let channels = self.spec.channels as usize;
        out.reserve(rows * channels);
        if c.extra_bits == 0 {
            // No extra bits: nothing can fail, so no per-sample checks.
            for (&l, &r) in left.iter().zip(right.iter()) {
                let (mut l, mut r) = (l, r);
                if joint && stereo {
                    r = r.wrapping_sub(l >> 1);
                    l = l.wrapping_add(r);
                }
                crc = crc.wrapping_mul(3).wrapping_add(l as u32);
                let l = c.shifted(l as u32);
                if stereo {
                    crc = crc.wrapping_mul(3).wrapping_add(r as u32);
                    out.extend_from_slice(&[l, c.shifted(r as u32)]);
                } else if false_stereo {
                    out.extend_from_slice(&[l, l]);
                } else {
                    out.push(l);
                }
            }
        }
        for i in 0..if c.extra_bits == 0 { 0 } else { rows } {
            let (mut l, mut r) = (left[i], right[i]);
            if joint && stereo {
                r = r.wrapping_sub(l >> 1);
                l = l.wrapping_add(r);
            }
            crc = crc.wrapping_mul(3).wrapping_add(l as u32);
            if stereo {
                crc = crc.wrapping_mul(3).wrapping_add(r as u32);
            }
            let l = c.restore(l, &mut extra, &mut extra_crc)?;
            out.push(l);
            if stereo {
                out.push(c.restore(r, &mut extra, &mut extra_crc)?);
            } else if false_stereo {
                out.push(l);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if crc != le32(&h[28..]) || c.extra_crc.is_some_and(|x| x != extra_crc) {
            return Err(Error::Invalid("WavPack sample CRC"));
        }
        self.frames += n as u64;
        self.position += size;
        Ok(())
    }
    fn validate_tail(&mut self, len: u64, limits: &Limits) -> Result<()> {
        let mut end = len;
        if end - self.position >= 128 {
            self.file.seek(SeekFrom::Start(end - 128))?;
            let mut h = [0u8; 3];
            self.file.read_exact(&mut h)?;
            if &h == b"TAG" {
                end -= 128;
            }
        }
        if end - self.position >= 32 {
            self.file.seek(SeekFrom::Start(end - 32))?;
            let mut h = [0u8; 32];
            self.file.read_exact(&mut h)?;
            if &h[..8] == b"APETAGEX" {
                let size = le32(&h[12..]) as u64;
                if size < 32 || size > end - self.position || size > limits.max_packet_bytes as u64
                {
                    return Err(Error::Invalid("APE tag size"));
                }
                end -= size;
                if end - self.position >= 32 {
                    self.file.seek(SeekFrom::Start(end - 32))?;
                    let mut h = [0u8; 8];
                    self.file.read_exact(&mut h)?;
                    if &h == b"APETAGEX" {
                        end -= 32;
                    }
                }
            }
        }
        if end != self.position {
            return Err(Error::Invalid("unrecognized WavPack tail / hidden blocks"));
        }
        // An APE size must not conceal additional hybrid/audio blocks. Match
        // AUDENIQ's conservative tail policy using bounded streaming storage.
        self.file.seek(SeekFrom::Start(end))?;
        let mut remaining = len - end;
        let mut buf = [0; 8192];
        let mut window = [0; 4];
        let mut used = 0usize;
        while remaining != 0 {
            limits.check()?;
            let n = (remaining as usize).min(buf.len());
            self.file.read_exact(&mut buf[..n])?;
            for &b in &buf[..n] {
                window.rotate_left(1);
                window[3] = b;
                used += 1;
                if used >= 4 && &window == b"wvpk" {
                    return Err(Error::Invalid("WavPack block concealed by tags"));
                }
            }
            remaining -= n as u64;
        }
        Ok(())
    }
}
fn validate_header(h: &[u8; 32]) -> Result<()> {
    let version = u16::from_le_bytes(h[8..10].try_into().unwrap());
    let flags = le32(&h[24..]);
    if &h[..4] != b"wvpk" || !(0x402..=0x410).contains(&version) {
        return Err(Error::Invalid("WavPack header"));
    }
    if flags & (8 | 0x80 | 0x80000000) != 0 {
        return Err(Error::Unsupported("hybrid/float/DSD WavPack"));
    }
    if flags & 0x1800 != 0x1800 {
        return Err(Error::Unsupported("multiblock WavPack"));
    }
    Ok(())
}
#[derive(Default)]
struct Decorr {
    term: i32,
    delta: i32,
    wa: i32,
    wb: i32,
    a: [i32; 8],
    b: [i32; 8],
}
struct Context<'a> {
    stereo: bool,
    decorr: Vec<Decorr>,
    words: Words,
    data: Option<&'a [u8]>,
    extra: Option<&'a [u8]>,
    extra_crc: Option<u32>,
    extra_bits: u32,
    and: u32,
    or: u32,
    shift: u32,
    post_shift: u32,
    rate: Option<u32>,
}
impl<'a> Context<'a> {
    fn parse(packet: &'a [u8], flags: u32) -> Result<Self> {
        let stereo = flags & (4 | 0x40000000) == 0;
        let mut c = Self {
            stereo,
            decorr: Vec::new(),
            words: Words::default(),
            data: None,
            extra: None,
            extra_crc: None,
            extra_bits: 0,
            and: 0,
            or: 0,
            shift: 0,
            post_shift: 32 - ((flags & 3) + 1) * 8 + ((flags >> 13) & 31),
            rate: None,
        };
        if c.post_shift > 31 {
            return Err(Error::Invalid("WavPack shift"));
        }
        let mut pos = 0;
        let mut seen = 0u32;
        while pos < packet.len() {
            if packet.len() - pos < 2 {
                return Err(Error::Invalid("WavPack metadata"));
            }
            let id = packet[pos];
            let mut size = packet[pos + 1] as usize;
            pos += 2;
            if id & 0x80 != 0 {
                if packet.len() - pos < 2 {
                    return Err(Error::Invalid("WavPack metadata size"));
                }
                size |=
                    (u16::from_le_bytes(packet[pos..pos + 2].try_into().unwrap()) as usize) << 8;
                pos += 2;
            }
            size *= 2;
            let actual = size
                .checked_sub(if id & 0x40 != 0 { 1 } else { 0 })
                .ok_or(Error::Invalid("WavPack odd size"))?;
            if size > packet.len() - pos {
                return Err(Error::Invalid("WavPack metadata length"));
            }
            let d = &packet[pos..pos + actual];
            pos += size;
            let id = id & 0x3f;
            if id < 32 {
                let mask = 1u32 << id;
                if seen & mask != 0 {
                    return Err(Error::Invalid("duplicate WavPack metadata"));
                }
                seen |= mask;
            }
            match id {
                2 => {
                    if d.len() > 16 {
                        return Err(Error::Invalid("WavPack terms"));
                    }
                    for b in d.iter().rev() {
                        let t = (b & 31) as i32 - 5;
                        if !(matches!(t,-3..=-1|1..=8|17|18)) || (!stereo && t < 0) {
                            return Err(Error::Invalid("WavPack decorrelation term"));
                        }
                        c.decorr.push(Decorr {
                            term: t,
                            delta: (b >> 5) as i32,
                            ..Default::default()
                        });
                    }
                }
                3 => {
                    if seen & (1 << 2) == 0 || !d.len().is_multiple_of(if stereo { 2 } else { 1 }) {
                        return Err(Error::Invalid("WavPack weights"));
                    }
                    let mut i = 0;
                    for t in c.decorr.iter_mut().rev() {
                        if i == d.len() {
                            break;
                        }
                        t.wa = weight(d[i]);
                        i += 1;
                        if stereo {
                            t.wb = weight(d[i]);
                            i += 1;
                        }
                    }
                    if i != d.len() {
                        return Err(Error::Invalid("too many weights"));
                    }
                }
                4 => {
                    if seen & (1 << 2) == 0 {
                        return Err(Error::Invalid("WavPack samples order"));
                    }
                    let mut i = 0;
                    for t in c.decorr.iter_mut().rev() {
                        if i == d.len() {
                            break;
                        }
                        if t.term > 8 {
                            t.a[0] = get_exp(d, &mut i)?;
                            t.a[1] = get_exp(d, &mut i)?;
                            if stereo {
                                t.b[0] = get_exp(d, &mut i)?;
                                t.b[1] = get_exp(d, &mut i)?;
                            }
                        } else if t.term < 0 {
                            t.a[0] = get_exp(d, &mut i)?;
                            t.b[0] = get_exp(d, &mut i)?;
                        } else {
                            for j in 0..t.term as usize {
                                t.a[j] = get_exp(d, &mut i)?;
                                if stereo {
                                    t.b[j] = get_exp(d, &mut i)?;
                                }
                            }
                        }
                    }
                    if i != d.len() {
                        return Err(Error::Invalid("WavPack decorrelation samples"));
                    }
                }
                5 => {
                    if d.len() != 6 * if stereo { 2 } else { 1 } {
                        return Err(Error::Invalid("WavPack entropy"));
                    }
                    let mut i = 0;
                    for ch in 0..if stereo { 2 } else { 1 } {
                        for j in 0..3 {
                            c.words.median[ch][j] = get_exp(d, &mut i)?;
                            if c.words.median[ch][j] < 0 {
                                return Err(Error::Invalid("WavPack negative median"));
                            }
                        }
                    }
                }
                9 => {
                    if d.len() != 4 || d[0] > 30 || d[1..].iter().any(|x| *x > 31) {
                        return Err(Error::Invalid("WavPack integer info"));
                    }
                    c.extra_bits = d[0] as u32;
                    c.shift = d[1] as u32;
                    if d[2] != 0 {
                        c.and = 1;
                        c.or = 1;
                        c.shift = d[2] as u32;
                    }
                    if d[3] != 0 {
                        c.and = 1;
                        c.or = 0;
                        c.shift = d[3] as u32;
                    }
                }
                10 => c.data = Some(d),
                12 => {
                    if d.len() <= 4 {
                        return Err(Error::Invalid("WavPack extra bits"));
                    }
                    c.extra_crc = Some(le32(d));
                    c.extra = Some(&d[4..]);
                }
                0x27 => {
                    if d.len() != 3 {
                        return Err(Error::Invalid("WavPack rate metadata"));
                    }
                    c.rate = Some(d[0] as u32 | ((d[1] as u32) << 8) | ((d[2] as u32) << 16));
                }
                0 | 1 | 13 => (),
                _ if id & 0x20 != 0 => (),
                _ => return Err(Error::Unsupported("WavPack required metadata")),
            }
        }
        if seen & 0x43c != 0x43c {
            return Err(Error::Invalid("missing WavPack metadata"));
        }
        if c.extra_bits != 0 && c.extra.is_none() {
            return Err(Error::Invalid("missing extra samples"));
        }
        Ok(c)
    }
    fn restore(&self, v: i32, extra: &mut Option<Lsb<'_>>, crc: &mut u32) -> Result<i32> {
        let mut v = v as u32;
        if self.extra_bits != 0 {
            v = v.wrapping_shl(self.extra_bits)
                | extra
                    .as_mut()
                    .ok_or(Error::Invalid("missing WavPack extra bits"))?
                    .read(self.extra_bits)?;
            *crc = crc
                .wrapping_mul(9)
                .wrapping_add((v & 0xffff) * 3)
                .wrapping_add(v >> 16);
        }
        Ok(self.shifted(v))
    }
    /// The integer-info shifts and the alignment to 32 bits.
    #[inline(always)]
    fn shifted(&self, v: u32) -> i32 {
        let bit = (v & self.and) | self.or;
        let v = v
            .wrapping_add(bit)
            .wrapping_shl(self.shift)
            .wrapping_sub(bit);
        v.wrapping_shl(self.post_shift) as i32
    }
}

/// Entropy decoder state of one block (both channels' medians and the shared
/// run flags). Copied into locals for the decoding loop so that it stays in
/// registers.
#[derive(Clone, Copy, Default)]
struct Words {
    median: [[i32; 3]; 2],
    zero: bool,
    one: bool,
    zeroes: u32,
}
impl Words {
    #[inline(always)]
    fn value(&mut self, bits: &mut Lsb<'_>, ch: usize) -> Result<i32> {
        if self.median[0][0] < 2 && self.median[1][0] < 2 && !self.zero && !self.one {
            if self.zeroes != 0 {
                self.zeroes -= 1;
                if self.zeroes != 0 {
                    return Ok(0);
                }
            } else {
                let n = bits.ones(33)?;
                let t = if n >= 2 {
                    if n >= 32 {
                        return Err(Error::Invalid("WavPack zero run"));
                    }
                    bits.read(n - 1)? | (1 << (n - 1))
                } else {
                    n
                };
                self.zeroes = t;
                if t != 0 {
                    self.median = [[0; 3]; 2];
                    return Ok(0);
                }
            }
        }
        let t = if self.zero {
            self.zero = false;
            0
        } else {
            let mut t = bits.ones(33)?;
            if t == 16 {
                let n = bits.ones(33)?;
                t += if n < 2 {
                    n
                } else {
                    if n >= 32 {
                        return Err(Error::Invalid("WavPack unary"));
                    }
                    bits.read(n - 1)? | (1 << (n - 1))
                };
            }
            let old = self.one;
            self.one = t & 1 != 0;
            self.zero = !self.one;
            (t >> 1) + u32::from(old)
        };
        let med = &mut self.median[ch];
        let m = med.map(|x| (x as u32 >> 4) + 1);
        // Code t selects a base and a range from the three medians:
        // t = 0: [0, m0); 1: [m0, m0 + m1); t >= 2: m0 + m1 + (t - 2) m2
        // and range m2. Selected without branches (t is unpredictable).
        let base = match t {
            0 => 0,
            1 => m[0],
            _ => m[0]
                .wrapping_add(m[1])
                .wrapping_add(m[2].wrapping_mul(t.wrapping_sub(2))),
        };
        let add = [m[0], m[1], m[2]][(t as usize).min(2)] - 1;
        // Medians below t grow, the one at t shrinks, those above stay.
        let mut overflow = false;
        // Medians are never negative (a negative one is an error), so while
        // x + 128 cannot overflow the divisions are shifts of non-negative
        // values; near i32::MAX the wrapping signed form is kept.
        let small = med.iter().all(|&x| x <= i32::MAX - 128);
        for (j, x) in med.iter_mut().enumerate() {
            let div = 128i32 >> j;
            let (grown, shrunk) = if small {
                let shift = 7 - j as u32;
                (
                    x.wrapping_add(((*x + div) >> shift) * 5),
                    x.wrapping_sub(((*x + div - 2) >> shift) * 2),
                )
            } else {
                (
                    x.wrapping_add((x.wrapping_add(div) / div).wrapping_mul(5)),
                    x.wrapping_sub((x.wrapping_add(div - 2) / div).wrapping_mul(2)),
                )
            };
            let j = j as u32;
            *x = if t > j {
                grown
            } else if t == j {
                shrunk
            } else {
                *x
            };
            overflow |= *x < 0;
        }
        if overflow {
            return Err(Error::Invalid("WavPack median overflow"));
        }
        // add + 1 values: p bits, and one more for the upper ones (an
        // almost-binary code), then the sign.
        if bits.available < 57 {
            bits.refill();
        }
        let p = 31 - (add | 1).leading_zeros();
        let p = if add == 0 { 0 } else { p };
        let e = (1u64 << (p + 1)) - add as u64 - 1;
        if add != 0 && p + 2 <= bits.available {
            let x = (bits.cache & ((1u64 << p) - 1)) as u32;
            let long = x as u64 >= e;
            let extra = ((bits.cache >> p) & 1) as u32;
            let used = p + long as u32;
            let tail = if long {
                x.wrapping_mul(2).wrapping_sub(e as u32).wrapping_add(extra)
            } else {
                x
            };
            let sign = (bits.cache >> used) & 1;
            bits.cache >>= used + 1;
            bits.available -= used + 1;
            let v = base.wrapping_add(tail) as i32;
            return Ok(if sign != 0 { !v } else { v });
        }
        let tail = if add == 0 {
            0
        } else {
            let x = bits.read(p)?;
            if x as u64 >= e {
                x.wrapping_mul(2)
                    .wrapping_sub(e as u32)
                    .wrapping_add(bits.read(1)?)
            } else {
                x
            }
        };
        let v = base.wrapping_add(tail) as i32;
        Ok(if bits.read(1)? != 0 { !v } else { v })
    }
}
fn get_exp(d: &[u8], i: &mut usize) -> Result<i32> {
    if d.len() - *i < 2 {
        return Err(Error::Invalid("WavPack history"));
    }
    let v = exp2(le16(&d[*i..]))?;
    *i += 2;
    Ok(v)
}
fn weight(v: u8) -> i32 {
    let w = (v as i8 as i32) * 8;
    if w > 0 {
        w + ((w + 64) >> 7)
    } else {
        w
    }
}
fn apply(w: i32, v: i32) -> i32 {
    ((w as i64 * v as i64 + 512) >> 10) as i32
}
/// The weight after one sample: stepped by delta towards agreement of the
/// prediction's and the input's signs, unless either is zero. Branch-free:
/// the sign comparison is unpredictable. (Clipping, for the cross-channel
/// terms, is applied by the caller; a weight left unchanged is within the
/// clip range already, so clipping it is a no-op.)
#[inline(always)]
fn update(w: i32, delta: i32, pred: i32, res: i32) -> i32 {
    let negative = (pred ^ res) >> 31;
    let step = (delta ^ negative).wrapping_sub(negative);
    let active = -(((pred != 0) & (res != 0)) as i32);
    w.wrapping_add(step & active)
}

/// Entropy-decode `n` samples (both channels of a row in turn when
/// `STEREO`); returns the complete rows and the error that stopped decoding.
fn decode_words<const STEREO: bool>(
    words: &mut Words,
    bits: &mut Lsb<'_>,
    left: &mut [i32],
    right: &mut [i32],
    n: usize,
    limits: &Limits,
) -> (usize, Option<Error>) {
    #[cfg(target_arch = "x86_64")]
    if crate::kernels::bit_ops() {
        // SAFETY: LZCNT/BMI1/BMI2 were detected at runtime; the body is the
        // same safe Rust (variable shifts become SHLX/SHRX).
        return unsafe { decode_words_bit_ops::<STEREO>(words, bits, left, right, n, limits) };
    }
    decode_words_body::<STEREO>(words, bits, left, right, n, limits)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "lzcnt,bmi1,bmi2")]
unsafe fn decode_words_bit_ops<const STEREO: bool>(
    words: &mut Words,
    bits: &mut Lsb<'_>,
    left: &mut [i32],
    right: &mut [i32],
    n: usize,
    limits: &Limits,
) -> (usize, Option<Error>) {
    decode_words_body::<STEREO>(words, bits, left, right, n, limits)
}

#[inline(always)]
fn decode_words_body<const STEREO: bool>(
    words: &mut Words,
    bits: &mut Lsb<'_>,
    left: &mut [i32],
    right: &mut [i32],
    n: usize,
    limits: &Limits,
) -> (usize, Option<Error>) {
    let mut w = *words;
    for (i, (l, r)) in left[..n].iter_mut().zip(&mut right[..n]).enumerate() {
        if i % 4096 == 0 {
            if let Err(e) = limits.check() {
                return (i, Some(e));
            }
        }
        match w.value(bits, 0) {
            Ok(v) => *l = v,
            Err(e) => return (i, Some(e)),
        }
        if STEREO {
            match w.value(bits, 1) {
                Ok(v) => *r = v,
                Err(e) => return (i, Some(e)),
            }
        }
    }
    *words = w;
    (n, None)
}

/// The decorrelation terms over a block, in place (`left`/`right` hold the
/// entropy-decoded values and receive the output). Positive terms predict
/// from the channel's own output `term` samples back (1..8, from an
/// eight-entry history) or extrapolate from the last two outputs (17, 18);
/// negative terms (stereo only) cross the channels. The arithmetic is the
/// format's; the weight steps are branch-free (see [`update`]).
fn decorrelate(terms: &mut [Decorr], left: &mut [i32], right: &mut [i32], stereo: bool) {
    let mut pos = 0;
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        let (mut x, mut y) = (*l, *r);
        for d in terms.iter_mut() {
            let t = d.term;
            if t > 0 {
                let (a, b, j) = if t > 8 {
                    let a = if t == 17 {
                        d.a[0].wrapping_mul(2).wrapping_sub(d.a[1])
                    } else {
                        d.a[0].wrapping_mul(3).wrapping_sub(d.a[1]) >> 1
                    };
                    let b = if t == 17 {
                        d.b[0].wrapping_mul(2).wrapping_sub(d.b[1])
                    } else {
                        d.b[0].wrapping_mul(3).wrapping_sub(d.b[1]) >> 1
                    };
                    d.a[1] = d.a[0];
                    d.b[1] = d.b[0];
                    (a, b, 0)
                } else {
                    (d.a[pos], d.b[pos], (pos + t as usize) & 7)
                };
                let xx = x.wrapping_add(apply(d.wa, a));
                d.wa = update(d.wa, d.delta, a, x);
                let yy = if stereo {
                    let yy = y.wrapping_add(apply(d.wb, b));
                    d.wb = update(d.wb, d.delta, b, y);
                    yy
                } else {
                    0
                };
                x = xx;
                y = yy;
                d.a[j] = x;
                d.b[j] = y;
            } else if t == -1 {
                let xx = x.wrapping_add(apply(d.wa, d.a[0]));
                d.wa = update(d.wa, d.delta, d.a[0], x).clamp(-1024, 1024);
                x = xx;
                let yy = y.wrapping_add(apply(d.wb, xx));
                d.wb = update(d.wb, d.delta, xx, y).clamp(-1024, 1024);
                y = yy;
                d.a[0] = y;
            } else {
                let yy = y.wrapping_add(apply(d.wb, d.b[0]));
                d.wb = update(d.wb, d.delta, d.b[0], y).clamp(-1024, 1024);
                y = yy;
                let pred = if t == -3 {
                    let old = d.a[0];
                    d.a[0] = y;
                    old
                } else {
                    y
                };
                let xx = x.wrapping_add(apply(d.wa, pred));
                d.wa = update(d.wa, d.delta, pred, x).clamp(-1024, 1024);
                x = xx;
                d.b[0] = x;
            }
        }
        pos = (pos + 1) & 7;
        (*l, *r) = (x, y);
    }
}
