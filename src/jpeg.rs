//! JPEG cover validation: accepts exactly the files the previous decoder
//! stack (image 0.24.9 with jpeg-decoder 0.3.2, 8192-pixel and 128 MiB
//! limits) decoded, and reports the frame size.
//!
//! Every check that could fail there is reproduced: marker and segment
//! syntax, frame, table and scan parameters, Huffman decoding of every
//! block with the same 64-bit bit reader (whose look-ahead decides where
//! markers are found and pads with zero bits after one), restart markers,
//! progressive refinement (which needs the coefficients) and the
//! component/colour-transform requirements at EOI. Dequantization, IDCT,
//! upsampling and colour conversion cannot fail and are not performed.
//! Lossless frames that made image panic (precision other than 8, except
//! one 9..16-bit component) are rejected.
use crate::{Error, Result};

fn invalid() -> Error {
    Error::Invalid("cover decode")
}

/// Image's `max_alloc` for the decoded output.
const MAX_ALLOC: u64 = 128 * 1024 * 1024;

struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}
impl Bytes<'_> {
    #[inline]
    fn u8(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(invalid)?;
        self.pos += 1;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        if self.data.len() - self.pos < n {
            return Err(invalid());
        }
        self.pos += n;
        Ok(())
    }
    /// Segment length minus its own two bytes.
    fn length(&mut self) -> Result<usize> {
        let length = self.u16()? as usize;
        if length < 2 {
            return Err(invalid());
        }
        Ok(length - 2)
    }
    /// Skip to the next marker (any bytes up to 0xFF, fill bytes, FF 00).
    fn marker(&mut self) -> Result<u8> {
        loop {
            while self.u8()? != 0xff {}
            let mut byte = self.u8()?;
            while byte == 0xff {
                byte = self.u8()?;
            }
            if byte != 0 {
                return Ok(byte);
            }
        }
    }
}

struct Table {
    values: Vec<u8>,
    delta: [i32; 16],
    maxcode: [i32; 16],
    /// (value, code length) for codes of up to 8 bits.
    lut: [(u8, u8); 256],
    /// AC tables: (coefficient, run << 4 | code + magnitude bits) when both
    /// fit in 8 bits.
    ac: Option<Box<[(i16, u8); 256]>>,
}
impl Table {
    fn new(counts: &[u8; 16], values: &[u8], ac: bool) -> Result<Self> {
        let sizes: Vec<u8> = (0..16)
            .flat_map(|i| std::iter::repeat_n(i as u8 + 1, counts[i] as usize))
            .collect();
        let mut codes = vec![0u16; sizes.len()];
        let mut size = sizes[0];
        let mut code = 0u32;
        for (i, &s) in sizes.iter().enumerate() {
            while size < s {
                code <<= 1;
                size += 1;
            }
            if code >= 1 << s {
                return Err(invalid());
            }
            codes[i] = code as u16;
            code += 1;
        }
        let mut delta = [0i32; 16];
        let mut maxcode = [-1i32; 16];
        let mut j = 0;
        for i in 0..16 {
            if counts[i] != 0 {
                delta[i] = j as i32 - codes[j] as i32;
                j += counts[i] as usize;
                maxcode[i] = codes[j - 1] as i32;
            }
        }
        let mut lut = [(0u8, 0u8); 256];
        for (i, &s) in sizes.iter().enumerate().filter(|&(_, &s)| s <= 8) {
            let start = (codes[i] as usize) << (8 - s);
            lut[start..start + (1 << (8 - s))].fill((values[i], s));
        }
        let ac = ac.then(|| {
            let mut table = Box::new([(0i16, 0u8); 256]);
            for (i, &(value, s)) in lut.iter().enumerate() {
                let run = value >> 4;
                let magnitude = value & 15;
                if magnitude > 0 && s + magnitude <= 8 {
                    let raw = (((i << s) & 255) >> (8 - magnitude)) as u16;
                    table[i] = (extend(raw, magnitude), (run << 4) | (s + magnitude));
                }
            }
            table
        });
        Ok(Self {
            values: values.to_vec(),
            delta,
            maxcode,
            lut,
            ac,
        })
    }
}

fn extend(value: u16, count: u8) -> i16 {
    if value < 1 << (count - 1) {
        (value as i16)
            .wrapping_add((-1i16).wrapping_shl(count as u32))
            .wrapping_add(1)
    } else {
        value as i16
    }
}

/// jpeg-decoder's entropy bit reader: a left-aligned 64-bit buffer refilled
/// while it holds at most 56 bits; after a marker it supplies zero bytes.
struct Bits {
    bits: u64,
    count: u8,
    marker: Option<u8>,
}
impl Bits {
    fn new() -> Self {
        Self {
            bits: 0,
            count: 0,
            marker: None,
        }
    }
    fn fill(&mut self, r: &mut Bytes) -> Result<()> {
        while self.count <= 56 {
            let byte = if self.marker.is_some() { 0 } else { r.u8()? };
            if byte == 0xff {
                let mut next = r.u8()?;
                if next != 0 {
                    while next == 0xff {
                        next = r.u8()?;
                    }
                    if next == 0 {
                        return Err(invalid());
                    }
                    self.marker = Some(next);
                    continue;
                }
            }
            self.bits |= (byte as u64) << (56 - self.count);
            self.count += 8;
        }
        Ok(())
    }
    #[inline]
    fn peek(&self, n: u8) -> u16 {
        ((self.bits >> (64 - n as u32)) & ((1 << n) - 1)) as u16
    }
    #[inline]
    fn consume(&mut self, n: u8) {
        self.bits <<= n;
        self.count -= n;
    }
    fn decode(&mut self, r: &mut Bytes, t: &Table) -> Result<u8> {
        if self.count < 16 {
            self.fill(r)?;
        }
        let (value, size) = t.lut[self.peek(8) as usize];
        if size > 0 {
            self.consume(size);
            return Ok(value);
        }
        let bits = self.peek(16);
        for i in 8..16 {
            let code = (bits >> (15 - i)) as i32;
            if code <= t.maxcode[i] {
                self.consume(i as u8 + 1);
                let index = usize::try_from(code + t.delta[i]).map_err(|_| invalid())?;
                return t.values.get(index).copied().ok_or_else(invalid);
            }
        }
        Err(invalid())
    }
    fn fast_ac(&mut self, r: &mut Bytes, t: &Table) -> Result<Option<(i16, u8)>> {
        let Some(ac) = &t.ac else { return Ok(None) };
        if self.count < 8 {
            self.fill(r)?;
        }
        let (value, run_size) = ac[self.peek(8) as usize];
        if run_size == 0 {
            return Ok(None);
        }
        self.consume(run_size & 15);
        Ok(Some((value, run_size >> 4)))
    }
    fn get(&mut self, r: &mut Bytes, n: u8) -> Result<u16> {
        if self.count < n {
            self.fill(r)?;
        }
        let v = self.peek(n);
        self.consume(n);
        Ok(v)
    }
    fn receive_extend(&mut self, r: &mut Bytes, n: u8) -> Result<i16> {
        Ok(extend(self.get(r, n)?, n))
    }
    fn take_marker(&mut self, r: &mut Bytes) -> Result<Option<u8>> {
        self.fill(r)?;
        Ok(self.marker.take())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Process {
    Sequential,
    Progressive,
    Lossless,
}

struct Component {
    id: u8,
    h: u8,
    v: u8,
    tq: usize,
    block_w: usize,
    block_h: usize,
}

struct Frame {
    baseline: bool,
    process: Process,
    precision: u8,
    width: u16,
    height: u16,
    mcu_w: usize,
    mcu_h: usize,
    components: Vec<Component>,
}

struct Scan {
    components: Vec<usize>,
    dc: Vec<usize>,
    ac: Vec<usize>,
    start: u8,
    /// Se + 1.
    end: u8,
    ah: u8,
    al: u8,
}

fn is_sof(m: u8) -> bool {
    matches!(m, 0xc0..=0xcf) && !matches!(m, 0xc4 | 0xc8 | 0xcc)
}

fn parse_sof(r: &mut Bytes, marker: u8) -> Result<Frame> {
    let length = r.length()?;
    if length <= 6 {
        return Err(invalid());
    }
    let n = marker - 0xc0;
    let process = match n % 4 {
        0 | 1 => Process::Sequential,
        2 => Process::Progressive,
        _ => Process::Lossless,
    };
    let baseline = n == 0;
    let precision = r.u8()?;
    match precision {
        8 => (),
        12 if baseline => return Err(invalid()),
        12 => (),
        p if process != Process::Lossless || p > 16 => return Err(invalid()),
        _ => (),
    }
    let height = r.u16()?;
    let width = r.u16()?;
    let count = r.u8()?;
    if height == 0 || width == 0 || count == 0 {
        return Err(invalid());
    }
    if (process == Process::Progressive && count > 4) || length != 6 + 3 * count as usize {
        return Err(invalid());
    }
    let mut components: Vec<Component> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let id = r.u8()?;
        if components.iter().any(|c| c.id == id) {
            return Err(invalid());
        }
        let factors = r.u8()?;
        let (h, v) = (factors >> 4, factors & 15);
        let tq = r.u8()?;
        if !(1..=4).contains(&h)
            || !(1..=4).contains(&v)
            || tq > 3
            || (process == Process::Lossless && tq != 0)
        {
            return Err(invalid());
        }
        components.push(Component {
            id,
            h,
            v,
            tq: tq as usize,
            block_w: 0,
            block_h: 0,
        });
    }
    // Differential (hierarchical) and arithmetic-coded frames are unsupported.
    if matches!(n, 5..=7 | 9..=11 | 13..=15) {
        return Err(invalid());
    }
    if (precision != 8 && process != Process::Lossless) || !(2..=16).contains(&precision) {
        return Err(invalid());
    }
    if !matches!(count, 1 | 3 | 4) {
        return Err(invalid());
    }
    let h_max = components.iter().map(|c| c.h).max().unwrap();
    let v_max = components.iter().map(|c| c.v).max().unwrap();
    for c in &components {
        let h1 = c.h == h_max || width == 1;
        let v1 = c.v == v_max || height == 1;
        let h2 = c.h * 2 == h_max;
        let v2 = c.v * 2 == v_max;
        if !((h1 || h2) && (v1 || v2)) && (h_max % c.h != 0 || v_max % c.v != 0) {
            return Err(invalid());
        }
    }
    let mcu_w = (width as usize).div_ceil(8 * h_max as usize);
    let mcu_h = (height as usize).div_ceil(8 * v_max as usize);
    for c in &mut components {
        c.block_w = mcu_w * c.h as usize;
        c.block_h = mcu_h * c.v as usize;
    }
    Ok(Frame {
        baseline,
        process,
        precision,
        width,
        height,
        mcu_w,
        mcu_h,
        components,
    })
}

fn parse_sos(r: &mut Bytes, frame: &Frame) -> Result<Scan> {
    let length = r.length()?;
    if length == 0 {
        return Err(invalid());
    }
    let count = r.u8()?;
    if count == 0 || count > 4 || length != 4 + 2 * count as usize {
        return Err(invalid());
    }
    let mut scan = Scan {
        components: Vec::new(),
        dc: Vec::new(),
        ac: Vec::new(),
        start: 0,
        end: 0,
        ah: 0,
        al: 0,
    };
    for _ in 0..count {
        let id = r.u8()?;
        let index = frame
            .components
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(invalid)?;
        if scan.components.contains(&index) || index < *scan.components.iter().max().unwrap_or(&0) {
            return Err(invalid());
        }
        let tables = r.u8()?;
        let (dc, ac) = (tables >> 4, tables & 15);
        let max = if frame.baseline { 1 } else { 3 };
        if dc > max || ac > max {
            return Err(invalid());
        }
        scan.components.push(index);
        scan.dc.push(dc as usize);
        scan.ac.push(ac as usize);
    }
    let blocks: u32 = scan
        .components
        .iter()
        .map(|&i| frame.components[i].h as u32 * frame.components[i].v as u32)
        .sum();
    if count > 1 && blocks > 10 {
        return Err(invalid());
    }
    let start = r.u8()?;
    let mut end = r.u8()?;
    let approximation = r.u8()?;
    let (ah, al) = (approximation >> 4, approximation & 15);
    if al >= frame.precision {
        return Err(invalid());
    }
    match frame.process {
        Process::Progressive => {
            if end > 63
                || start > end
                || (start == 0 && end != 0)
                || (start != 0 && count != 1)
                || ah > 13
                || al > 13
                || (ah != 0 && ah != al + 1)
            {
                return Err(invalid());
            }
        }
        Process::Lossless => {
            if end != 0 || ah != 0 || start > 7 {
                return Err(invalid());
            }
        }
        Process::Sequential => {
            if end == 0 {
                end = 63;
            }
            if start != 0 || end != 63 || ah != 0 || al != 0 {
                return Err(invalid());
            }
        }
    }
    scan.start = start;
    scan.end = end + 1;
    scan.ah = ah;
    scan.al = al;
    Ok(scan)
}

/// Tables K.3 to K.6, used by Motion-JPEG (AVI1) frames that omit them.
fn mjpeg_table(dc: bool, index: usize) -> Table {
    const AC_LUMA: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52,
        0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
        0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3,
        0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8,
        0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ];
    const AC_CHROMA: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33,
        0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18,
        0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4,
        0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
        0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
        0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ];
    let dc_values: Vec<u8> = (0..12).collect();
    let (counts, values): ([u8; 16], &[u8]) = match (dc, index) {
        (true, 0) => ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], &dc_values),
        (true, _) => ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], &dc_values),
        (false, 0) => (
            [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
            &AC_LUMA,
        ),
        (false, _) => (
            [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
            &AC_CHROMA,
        ),
    };
    Table::new(&counts, values, !dc).expect("standard table")
}

struct Decoder<'a> {
    r: Bytes<'a>,
    frame: Option<Frame>,
    dc: [Option<Table>; 4],
    ac: [Option<Table>; 4],
    quant: [bool; 4],
    restart: u16,
    jfif: bool,
    mjpeg: bool,
    adobe: Option<u8>,
    /// Progressive coefficients per component (zig-zag order per block).
    coefficients: Vec<Vec<i16>>,
    finished: [u64; 4],
    planes: [bool; 4],
    lossless_slots: usize,
}

impl Decoder<'_> {
    fn dht(&mut self) -> Result<()> {
        let mut length = self.r.length()?;
        let baseline = self.frame.as_ref().map(|f| f.baseline);
        while length > 17 {
            let byte = self.r.u8()?;
            let (class, index) = (byte >> 4, (byte & 15) as usize);
            if class > 1 || (baseline == Some(true) && index > 1) || index > 3 {
                return Err(invalid());
            }
            let mut counts = [0u8; 16];
            for c in &mut counts {
                *c = self.r.u8()?;
            }
            let size: usize = counts.iter().map(|&c| c as usize).sum();
            if size == 0 || size > 256 || size > length - 17 {
                return Err(invalid());
            }
            let start = self.r.pos;
            self.r.skip(size)?;
            let table = Table::new(&counts, &self.r.data[start..start + size], class == 1)?;
            if class == 0 {
                self.dc[index] = Some(table);
            } else {
                self.ac[index] = Some(table);
            }
            length -= 17 + size;
        }
        if length != 0 {
            return Err(invalid());
        }
        Ok(())
    }

    fn dqt(&mut self) -> Result<()> {
        let mut length = self.r.length()?;
        while length > 0 {
            let byte = self.r.u8()?;
            let (precision, index) = ((byte >> 4) as usize, (byte & 15) as usize);
            if precision > 1 || index > 3 || length < 65 + 64 * precision {
                return Err(invalid());
            }
            let mut zero = false;
            for _ in 0..64 {
                let q = if precision == 0 {
                    self.r.u8()? as u16
                } else {
                    self.r.u16()?
                };
                zero |= q == 0;
            }
            if zero {
                return Err(invalid());
            }
            self.quant[index] = true;
            length -= 65 + 64 * precision;
        }
        Ok(())
    }

    fn app(&mut self, marker: u8) -> Result<()> {
        let length = self.r.length()?;
        let start = self.r.pos;
        self.r.skip(length)?;
        let body = &self.r.data[start..start + length];
        match marker {
            0xe0 if length >= 5 => {
                if &body[..5] == b"JFIF\0" {
                    self.jfif = true;
                } else if &body[..5] == b"AVI1\0" {
                    self.mjpeg = true;
                }
            }
            0xee if length >= 12 && &body[..6] == b"Adobe\0" => {
                if body[11] > 2 {
                    return Err(invalid());
                }
                self.adobe = Some(body[11]);
            }
            _ => (),
        }
        Ok(())
    }

    /// The marker following the scan, as jpeg-decoder returned it.
    fn end_of_scan(&mut self, bits: &mut Bits) -> Result<Option<u8>> {
        let mut marker = bits.take_marker(&mut self.r)?;
        while let Some(0xd0..=0xd7) = marker {
            marker = self.r.marker().ok();
        }
        Ok(marker)
    }

    fn restart(&mut self, bits: &mut Bits, expected: &mut u8) -> Result<()> {
        match bits.take_marker(&mut self.r)? {
            Some(m) if m == 0xd0 + *expected => {
                bits.bits = 0;
                bits.count = 0;
                *expected = (*expected + 1) % 8;
                Ok(())
            }
            _ => Err(invalid()),
        }
    }

    fn scan_dct(&mut self, frame: &Frame, scan: &Scan) -> Result<Option<u8>> {
        if scan
            .components
            .iter()
            .any(|&i| !self.quant[frame.components[i].tq])
        {
            return Err(invalid());
        }
        if self.mjpeg {
            for slot in 0..2 {
                if self.dc[slot].is_none() && scan.dc.contains(&slot) {
                    self.dc[slot] = Some(mjpeg_table(true, slot));
                }
                if self.ac[slot].is_none() && scan.ac.contains(&slot) {
                    self.ac[slot] = Some(mjpeg_table(false, slot));
                }
            }
        }
        if (scan.start == 0 && scan.dc.iter().any(|&i| self.dc[i].is_none()))
            || (scan.end > 1 && scan.ac.iter().any(|&i| self.ac[i].is_none()))
        {
            return Err(invalid());
        }
        let progressive = frame.process == Process::Progressive;
        let interleaved = scan.components.len() > 1;
        let (max_x, max_y) = if interleaved {
            (frame.mcu_w, frame.mcu_h)
        } else {
            let c = &frame.components[scan.components[0]];
            (c.block_w, c.block_h)
        };
        let (width, height) = (frame.width as usize, frame.height as usize);
        let mut bits = Bits::new();
        let mut predictors = [0i16; 4];
        let mut left = self.restart;
        let mut expected = 0u8;
        let mut eob_run = 0u16;
        let mut scratch = [0i16; 64];
        let mut coefficients = std::mem::take(&mut self.coefficients);
        for mcu_y in 0..max_y {
            if mcu_y * 8 >= height {
                break;
            }
            for mcu_x in 0..max_x {
                if mcu_x * 8 >= width {
                    break;
                }
                if self.restart > 0 {
                    if left == 0 {
                        self.restart(&mut bits, &mut expected)?;
                        predictors = [0; 4];
                        eob_run = 0;
                        left = self.restart;
                    }
                    left -= 1;
                }
                for (i, &index) in scan.components.iter().enumerate() {
                    let c = &frame.components[index];
                    let (bh, bv) = if interleaved {
                        (c.h as usize, c.v as usize)
                    } else {
                        (1, 1)
                    };
                    for v in 0..bv {
                        for h in 0..bh {
                            let block: &mut [i16] = if progressive {
                                let y = mcu_y * bv + v;
                                let x = mcu_x * bh + h;
                                let offset = (y * c.block_w + x) * 64;
                                &mut coefficients[index][offset..offset + 64]
                            } else {
                                &mut scratch
                            };
                            if scan.ah == 0 {
                                decode_block(
                                    &mut self.r,
                                    &mut bits,
                                    block,
                                    self.dc[scan.dc[i]].as_ref(),
                                    self.ac[scan.ac[i]].as_ref(),
                                    scan,
                                    &mut eob_run,
                                    &mut predictors[i],
                                )?;
                            } else {
                                refine_block(
                                    &mut self.r,
                                    &mut bits,
                                    block,
                                    self.ac[scan.ac[i]].as_ref(),
                                    scan,
                                    &mut eob_run,
                                )?;
                            }
                        }
                    }
                }
            }
        }
        self.coefficients = coefficients;
        self.end_of_scan(&mut bits)
    }

    fn scan_lossless(&mut self, frame: &Frame, scan: &Scan) -> Result<Option<u8>> {
        if scan.dc.iter().any(|&i| self.dc[i].is_none()) {
            return Err(invalid());
        }
        let (width, height) = (frame.width as usize, frame.height as usize);
        let mut bits = Bits::new();
        let mut left = self.restart;
        let mut expected = 0u8;
        for _ in 0..height {
            for _ in 0..width {
                if self.restart > 0 {
                    if left == 0 {
                        self.restart(&mut bits, &mut expected)?;
                        left = self.restart;
                    }
                    left -= 1;
                }
                for &table in &scan.dc {
                    let t = self.dc[table].as_ref().unwrap();
                    match bits.decode(&mut self.r, t)? {
                        0 | 16 => (),
                        s @ 1..=15 => {
                            bits.get(&mut self.r, s)?;
                        }
                        _ => return Err(invalid()),
                    }
                }
            }
        }
        self.end_of_scan(&mut bits)
    }

    /// Whether every component's plane was produced, and colour conversion
    /// was possible, at EOI.
    fn complete(&self) -> Result<()> {
        let frame = self.frame.as_ref().ok_or_else(invalid)?;
        let n = frame.components.len();
        if frame.process == Process::Lossless {
            // Panicking output sizes in image for precision other than 8.
            if self.lossless_slots < n || (frame.precision != 8 && (n != 1 || frame.precision < 8))
            {
                return Err(invalid());
            }
            return Ok(());
        }
        for (i, c) in frame.components.iter().enumerate() {
            let rendered_at_end = frame.process == Process::Progressive
                && self.coefficients.len() == n
                && self.finished[i] != !0
                && self.quant[c.tq];
            if !self.planes[i] && !rendered_at_end {
                return Err(invalid());
            }
        }
        if n > 1 {
            let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
            // 0 RGB, 1 YCbCr, 2 YCCK, 3 CMYK, 4 unsupported.
            let transform = match (n, ids.as_slice()) {
                (3, [1, 2, 3]) => 1,
                (3, [82, 71, 66]) => 0,
                (3, [1, 34, 35] | [114, 103, 98]) => 4,
                (3, _) if self.jfif => 1,
                _ => match self.adobe {
                    Some(0) => {
                        if n == 3 {
                            0
                        } else {
                            3
                        }
                    }
                    Some(1) => 1,
                    Some(_) => 2,
                    None if n == 4 => 3,
                    None => 1,
                },
            };
            let ok = match n {
                3 => matches!(transform, 0 | 1),
                _ => matches!(transform, 2 | 3),
            };
            if !ok {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_block(
    r: &mut Bytes,
    bits: &mut Bits,
    block: &mut [i16],
    dc: Option<&Table>,
    ac: Option<&Table>,
    scan: &Scan,
    eob_run: &mut u16,
    predictor: &mut i16,
) -> Result<()> {
    if scan.start == 0 {
        let diff = match bits.decode(r, dc.unwrap())? {
            0 => 0,
            s @ 1..=11 => bits.receive_extend(r, s)?,
            _ => return Err(invalid()),
        };
        *predictor = predictor.wrapping_add(diff);
        block[0] = predictor.wrapping_shl(scan.al as u32);
    }
    let mut index = scan.start.max(1);
    if index < scan.end && *eob_run > 0 {
        *eob_run -= 1;
        return Ok(());
    }
    while index < scan.end {
        let table = ac.unwrap();
        if let Some((value, run)) = bits.fast_ac(r, table)? {
            index += run;
            if index >= scan.end {
                break;
            }
            block[index as usize] = value.wrapping_shl(scan.al as u32);
            index += 1;
        } else {
            let byte = bits.decode(r, table)?;
            let (run, size) = (byte >> 4, byte & 15);
            if size == 0 {
                if run == 15 {
                    index += 16;
                } else {
                    *eob_run = (1 << run) - 1;
                    if run > 0 {
                        *eob_run += bits.get(r, run)?;
                    }
                    break;
                }
            } else {
                index += run;
                if index >= scan.end {
                    break;
                }
                block[index as usize] = bits.receive_extend(r, size)?.wrapping_shl(scan.al as u32);
                index += 1;
            }
        }
    }
    Ok(())
}

fn refine_block(
    r: &mut Bytes,
    bits: &mut Bits,
    block: &mut [i16],
    ac: Option<&Table>,
    scan: &Scan,
    eob_run: &mut u16,
) -> Result<()> {
    let bit = 1i16.wrapping_shl(scan.al as u32);
    if scan.start == 0 {
        if bits.get(r, 1)? == 1 {
            block[0] |= bit;
        }
        return Ok(());
    }
    if *eob_run > 0 {
        *eob_run -= 1;
        refine_non_zeroes(r, bits, block, scan.start, scan.end, 64, bit)?;
        return Ok(());
    }
    let mut index = scan.start;
    while index < scan.end {
        let byte = bits.decode(r, ac.unwrap())?;
        let (run, size) = (byte >> 4, byte & 15);
        let mut zeros = run;
        let mut value = 0i16;
        match size {
            0 => {
                if run != 15 {
                    *eob_run = (1 << run) - 1;
                    if run > 0 {
                        *eob_run += bits.get(r, run)?;
                    }
                    zeros = 64;
                }
            }
            1 => {
                value = if bits.get(r, 1)? == 1 {
                    bit
                } else {
                    bit.wrapping_neg()
                }
            }
            _ => return Err(invalid()),
        }
        index = refine_non_zeroes(r, bits, block, index, scan.end, zeros, bit)?;
        if value != 0 {
            block[index as usize] = value;
        }
        index += 1;
    }
    Ok(())
}

fn refine_non_zeroes(
    r: &mut Bytes,
    bits: &mut Bits,
    block: &mut [i16],
    start: u8,
    end: u8,
    mut zeros: u8,
    bit: i16,
) -> Result<u8> {
    for i in start..end {
        let c = &mut block[i as usize];
        if *c == 0 {
            if zeros == 0 {
                return Ok(i);
            }
            zeros -= 1;
        } else if bits.get(r, 1)? == 1 && *c & bit == 0 {
            *c = if *c > 0 {
                c.checked_add(bit)
            } else {
                c.checked_sub(bit)
            }
            .ok_or_else(invalid)?;
        }
    }
    Ok(end - 1)
}

/// Validate a JPEG file; returns the frame width and height.
pub(crate) fn validate(data: &[u8]) -> Result<(u32, u32)> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err(invalid());
    }
    let mut d = Decoder {
        r: Bytes { data, pos: 2 },
        frame: None,
        dc: Default::default(),
        ac: Default::default(),
        quant: [false; 4],
        restart: 0,
        jfif: false,
        mjpeg: false,
        adobe: None,
        coefficients: Vec::new(),
        finished: [0; 4],
        planes: [false; 4],
        lossless_slots: 0,
    };
    let mut previous = 0xd8u8;
    let mut pending: Option<u8> = None;
    loop {
        let marker = match pending.take() {
            Some(m) => m,
            None => d.r.marker()?,
        };
        match marker {
            m if is_sof(m) => {
                if d.frame.is_some() {
                    return Err(invalid());
                }
                let frame = parse_sof(&mut d.r, m)?;
                // image's limits, checked once the header is read.
                let (w, h) = (frame.width as u64, frame.height as u64);
                let bpp = match (frame.components.len(), frame.precision > 8) {
                    (1, false) => 1,
                    (1, true) => 2,
                    _ => 3,
                };
                if w > crate::png::MAX_DIMENSION as u64
                    || h > crate::png::MAX_DIMENSION as u64
                    || w * h * bpp > MAX_ALLOC
                {
                    return Err(invalid());
                }
                d.frame = Some(frame);
            }
            0xda => {
                // Taken out while the scan is decoded; restored after it.
                let frame = d.frame.take().ok_or_else(invalid)?;
                let scan = parse_sos(&mut d.r, &frame)?;
                if frame.process == Process::Progressive && d.coefficients.is_empty() {
                    d.coefficients = frame
                        .components
                        .iter()
                        .map(|c| vec![0i16; c.block_w * c.block_h * 64])
                        .collect();
                }
                if frame.process == Process::Lossless {
                    d.lossless_slots = d.lossless_slots.max(scan.components.len());
                    pending = d.scan_lossless(&frame, &scan)?;
                } else {
                    let mut newly = [false; 4];
                    if scan.al == 0 {
                        for (&i, done) in scan.components.iter().zip(&mut newly) {
                            if d.finished[i] == !0 {
                                continue;
                            }
                            for j in scan.start..scan.end {
                                d.finished[i] |= 1 << j;
                            }
                            *done = d.finished[i] == !0;
                        }
                    }
                    pending = d.scan_dct(&frame, &scan)?;
                    for (&i, &done) in scan.components.iter().zip(&newly) {
                        if done {
                            d.planes[i] = true;
                        }
                    }
                }
                d.frame = Some(frame);
            }
            0xdb => d.dqt()?,
            0xc4 => d.dht()?,
            0xcc => return Err(invalid()),
            0xdd => {
                if d.r.length()? != 2 {
                    return Err(invalid());
                }
                d.restart = d.r.u16()?;
            }
            0xfe => {
                let length = d.r.length()?;
                d.r.skip(length)?;
            }
            0xe0..=0xef => d.app(marker)?,
            0xd0..=0xd7 => {
                if previous != 0xda {
                    return Err(invalid());
                }
            }
            // DNL (never supported), DHP, EXP; and SOI, JPG, JPGn, TEM, RES.
            0xd9 => break,
            _ => return Err(invalid()),
        }
        previous = marker;
    }
    d.complete()?;
    let frame = d.frame.as_ref().unwrap();
    Ok((frame.width as u32, frame.height as u32))
}
