use crate::{Error, Result};

/// Bounds-checked LSB-first bit reader for FFmpeg-derived TTA/WavPack paths.
pub struct LeBits<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> LeBits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if n > 32 || self.pos + n as usize > self.data.len() * 8 {
            return Err(Error::Invalid("truncated bitstream"));
        }
        let mut value = 0u32;
        // At most five bytes; never perform unchecked or padded overreads.
        let mut remaining = n;
        let mut shift = 0;
        while remaining != 0 {
            let offset = self.pos & 7;
            let take = remaining.min(8 - offset as u32);
            value |= (((self.data[self.pos >> 3] >> offset) as u32) & ((1 << take) - 1)) << shift;
            self.pos += take as usize;
            shift += take;
            remaining -= take;
        }
        Ok(value)
    }
    pub fn unary_ones(&mut self, max: u32) -> Result<u32> {
        let mut n = 0;
        loop {
            if self.read(1)? == 0 {
                return Ok(n);
            }
            n += 1;
            if n >= max {
                return Err(Error::Invalid("unbounded unary code"));
            }
        }
    }
}

pub struct BeWriter {
    pub bytes: Vec<u8>,
    used: u8,
}
impl BeWriter {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            used: 0,
        }
    }
    pub fn put(&mut self, n: u32, value: u64) {
        let mut left = n;
        while left != 0 {
            if self.used == 0 {
                self.bytes.push(0);
            }
            let take = left.min((8 - self.used) as u32);
            let end = self.bytes.len() - 1;
            self.bytes[end] |= (((value >> (left - take)) & ((1 << take) - 1)) as u8)
                << (8 - self.used - take as u8);
            self.used = (self.used + take as u8) & 7;
            left -= take;
        }
    }
    pub fn unary(&mut self, zeros: u32) {
        // Aligned bulk zero fill avoids a loop per residual quotient bit.
        let mut left = zeros;
        if self.used != 0 {
            let n = left.min((8 - self.used) as u32);
            self.put(n, 0);
            left -= n;
        }
        self.bytes.resize(self.bytes.len() + (left / 8) as usize, 0);
        self.put(left % 8, 0);
        self.put(1, 1);
    }
    pub fn align(&mut self) {
        if self.used != 0 {
            self.put((8 - self.used) as u32, 0);
        }
    }
}

const fn crc_tables() -> ([u8; 256], [u16; 256]) {
    let mut c8 = [0; 256];
    let mut c16 = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut a = i as u8;
        let mut b = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            a = (a << 1) ^ if a & 0x80 != 0 { 7 } else { 0 };
            b = (b << 1) ^ if b & 0x8000 != 0 { 0x8005 } else { 0 };
            j += 1;
        }
        c8[i] = a;
        c16[i] = b;
        i += 1;
    }
    (c8, c16)
}
const CRC: ([u8; 256], [u16; 256]) = crc_tables();

pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for b in data {
        crc = CRC.0[(crc ^ b) as usize];
    }
    crc
}
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for b in data {
        crc = (crc << 8) ^ CRC.1[((crc >> 8) as u8 ^ b) as usize];
    }
    crc
}
pub fn crc32(data: &[u8]) -> u32 {
    // IEEE CRC32, not the incompatible x86 SSE4.2 CRC32C polynomial.
    // crc32fast selects PCLMULQDQ on x86 or CRC instructions on AArch64.
    crc32fast::hash(data)
}
