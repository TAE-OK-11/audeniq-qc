//! Streaming MD5 (RFC 1321) for the FLAC STREAMINFO PCM signature.
//!
//! MD5 is a format integrity field here, not a security authenticator. A
//! single stream is a serial chain of 64 steps per block, each depending on
//! the previous step's output `b`. The round functions below are written so
//! that as much of each step as possible is independent of `b`:
//!
//! * F(b,c,d) = d ^ (b & (c ^ d)): `c ^ d` is ready before `b`.
//! * G(b,c,d) = (b & d) + (c & !d): the two terms have disjoint bits, so
//!   OR equals ADD, and `c & !d` is added to `a + K + M` before `b` arrives.
//! * H(b,c,d) = b ^ (c ^ d): `c ^ d` is ready before `b`.
//! * I(b,c,d) = c ^ (b | !d): `!d` is ready before `b`.
//!
//! Each is the RFC function bit for bit; the tests compare against the RFC
//! vectors and an independent implementation.

pub(crate) const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];
const INIT: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];

#[derive(Clone)]
pub(crate) struct Md5 {
    state: [u32; 4],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    pub(crate) fn new() -> Self {
        Self {
            state: INIT,
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Count `data`, complete a partially buffered block from its start,
    /// and return the rest; it is empty unless the buffer is now empty.
    pub(crate) fn absorb_head<'a>(&mut self, mut data: &'a [u8]) -> &'a [u8] {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered != 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered < 64 {
                return &[];
            }
            let block = self.buffer;
            compress(&mut self.state, std::slice::from_ref(&block));
            self.buffered = 0;
        }
        data
    }
    /// Whole blocks of already counted data (buffer empty).
    pub(crate) fn absorb_blocks(&mut self, blocks: &[[u8; 64]]) {
        debug_assert!(blocks.is_empty() || self.buffered == 0);
        compress(&mut self.state, blocks);
    }
    /// The final partial block of already counted data (buffer empty
    /// unless `tail` is empty).
    pub(crate) fn absorb_tail(&mut self, tail: &[u8]) {
        if tail.is_empty() {
            return;
        }
        debug_assert!(self.buffered == 0 && tail.len() < 64);
        self.buffer[..tail.len()].copy_from_slice(tail);
        self.buffered = tail.len();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[cfg(target_arch = "x86_64")]
    pub(crate) fn state_mut(&mut self) -> &mut [u32; 4] {
        &mut self.state
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        let rest = self.absorb_head(data);
        let (blocks, tail) = rest.as_chunks::<64>();
        self.absorb_blocks(blocks);
        self.absorb_tail(tail);
    }

    pub(crate) fn finalize(mut self) -> [u8; 16] {
        let bits = self.length.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        // Pad to 56 bytes mod 64, then append the bit length (little-endian).
        let zeros = (55usize.wrapping_sub(self.buffered)) % 64;
        let end = 1 + zeros;
        pad[end..end + 8].copy_from_slice(&bits.to_le_bytes());
        let length = self.length;
        self.update(&pad[..end + 8]);
        self.length = length;
        debug_assert_eq!(self.buffered, 0);
        let mut out = [0u8; 16];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *chunk = word.to_le_bytes();
        }
        out
    }
}

/// One MD5 step: `a = b + rotl(a + f + K + M, s)` with `f` already formed.
/// The round constants are read through `black_box` so LLVM cannot treat
/// them as immediates: it moves constant addends to the end of a sum, which
/// put an extra add between `f` (the chain) and the rotate.
macro_rules! step {
    ($a:ident, $b:ident, $f:expr, $k:expr, $m:expr, $s:expr) => {
        $a = $b.wrapping_add(
            $a.wrapping_add($k)
                .wrapping_add($m)
                .wrapping_add($f)
                .rotate_left($s),
        );
    };
}
macro_rules! ff {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        step!(
            $a,
            $b,
            $d ^ ($b & ($c ^ $d)),
            std::hint::black_box(&K)[$i],
            $m,
            $s
        )
    };
}
macro_rules! gg {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        // (b & d) | (c & !d) with disjoint bits: the `c & !d` term and the
        // constants are summed before `b` is needed.
        $a = $b.wrapping_add(
            $a.wrapping_add(std::hint::black_box(&K)[$i])
                .wrapping_add($m)
                .wrapping_add($c & !$d)
                .wrapping_add($b & $d)
                .rotate_left($s),
        );
    };
}
macro_rules! hh {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        step!($a, $b, $b ^ ($c ^ $d), std::hint::black_box(&K)[$i], $m, $s)
    };
}
macro_rules! ii {
    ($a:ident, $b:ident, $c:ident, $d:ident, $m:expr, $i:expr, $s:expr) => {
        step!(
            $a,
            $b,
            $c ^ ($b | !$d),
            std::hint::black_box(&K)[$i],
            $m,
            $s
        )
    };
}

#[inline(always)]
pub(crate) fn compress(state: &mut [u32; 4], blocks: &[[u8; 64]]) {
    let [mut a, mut b, mut c, mut d] = *state;
    for block in blocks {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*bytes);
        }
        let (a0, b0, c0, d0) = (a, b, c, d);

        ff!(a, b, c, d, m[0], 0, 7);
        ff!(d, a, b, c, m[1], 1, 12);
        ff!(c, d, a, b, m[2], 2, 17);
        ff!(b, c, d, a, m[3], 3, 22);
        ff!(a, b, c, d, m[4], 4, 7);
        ff!(d, a, b, c, m[5], 5, 12);
        ff!(c, d, a, b, m[6], 6, 17);
        ff!(b, c, d, a, m[7], 7, 22);
        ff!(a, b, c, d, m[8], 8, 7);
        ff!(d, a, b, c, m[9], 9, 12);
        ff!(c, d, a, b, m[10], 10, 17);
        ff!(b, c, d, a, m[11], 11, 22);
        ff!(a, b, c, d, m[12], 12, 7);
        ff!(d, a, b, c, m[13], 13, 12);
        ff!(c, d, a, b, m[14], 14, 17);
        ff!(b, c, d, a, m[15], 15, 22);

        gg!(a, b, c, d, m[1], 16, 5);
        gg!(d, a, b, c, m[6], 17, 9);
        gg!(c, d, a, b, m[11], 18, 14);
        gg!(b, c, d, a, m[0], 19, 20);
        gg!(a, b, c, d, m[5], 20, 5);
        gg!(d, a, b, c, m[10], 21, 9);
        gg!(c, d, a, b, m[15], 22, 14);
        gg!(b, c, d, a, m[4], 23, 20);
        gg!(a, b, c, d, m[9], 24, 5);
        gg!(d, a, b, c, m[14], 25, 9);
        gg!(c, d, a, b, m[3], 26, 14);
        gg!(b, c, d, a, m[8], 27, 20);
        gg!(a, b, c, d, m[13], 28, 5);
        gg!(d, a, b, c, m[2], 29, 9);
        gg!(c, d, a, b, m[7], 30, 14);
        gg!(b, c, d, a, m[12], 31, 20);

        hh!(a, b, c, d, m[5], 32, 4);
        hh!(d, a, b, c, m[8], 33, 11);
        hh!(c, d, a, b, m[11], 34, 16);
        hh!(b, c, d, a, m[14], 35, 23);
        hh!(a, b, c, d, m[1], 36, 4);
        hh!(d, a, b, c, m[4], 37, 11);
        hh!(c, d, a, b, m[7], 38, 16);
        hh!(b, c, d, a, m[10], 39, 23);
        hh!(a, b, c, d, m[13], 40, 4);
        hh!(d, a, b, c, m[0], 41, 11);
        hh!(c, d, a, b, m[3], 42, 16);
        hh!(b, c, d, a, m[6], 43, 23);
        hh!(a, b, c, d, m[9], 44, 4);
        hh!(d, a, b, c, m[12], 45, 11);
        hh!(c, d, a, b, m[15], 46, 16);
        hh!(b, c, d, a, m[2], 47, 23);

        ii!(a, b, c, d, m[0], 48, 6);
        ii!(d, a, b, c, m[7], 49, 10);
        ii!(c, d, a, b, m[14], 50, 15);
        ii!(b, c, d, a, m[5], 51, 21);
        ii!(a, b, c, d, m[12], 52, 6);
        ii!(d, a, b, c, m[3], 53, 10);
        ii!(c, d, a, b, m[10], 54, 15);
        ii!(b, c, d, a, m[1], 55, 21);
        ii!(a, b, c, d, m[8], 56, 6);
        ii!(d, a, b, c, m[15], 57, 10);
        ii!(c, d, a, b, m[6], 58, 15);
        ii!(b, c, d, a, m[13], 59, 21);
        ii!(a, b, c, d, m[4], 60, 6);
        ii!(d, a, b, c, m[11], 61, 10);
        ii!(c, d, a, b, m[2], 62, 15);
        ii!(b, c, d, a, m[9], 63, 21);

        a = a.wrapping_add(a0);
        b = b.wrapping_add(b0);
        c = c.wrapping_add(c0);
        d = d.wrapping_add(d0);
    }
    *state = [a, b, c, d];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: [u8; 16]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn rfc_1321_test_suite() {
        for (input, expected) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            let mut md5 = Md5::new();
            md5.update(input.as_bytes());
            assert_eq!(hex(md5.finalize()), expected, "{input:?}");
        }
    }

    /// Every length 0..300 (all padding cases) and every split point of
    /// several messages, against the independent RustCrypto implementation.
    #[test]
    fn matches_independent_implementation_for_all_lengths_and_splits() {
        use md5_reference::Digest;
        let data: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        for length in 0..300 {
            let mut ours = Md5::new();
            ours.update(&data[..length]);
            assert_eq!(
                ours.finalize()[..],
                md5_reference::Md5::digest(&data[..length])[..],
                "length {length}"
            );
        }
        for length in [63, 64, 65, 127, 128, 129, 1000, 4096] {
            let expected = md5_reference::Md5::digest(&data[..length]);
            for split in 0..=length.min(200) {
                let mut ours = Md5::new();
                ours.update(&data[..split]);
                ours.update(&data[split..length]);
                assert_eq!(
                    ours.clone().finalize()[..],
                    expected[..],
                    "{length} split {split}"
                );
            }
            // Many tiny updates crossing every block boundary.
            let mut ours = Md5::new();
            for chunk in data[..length].chunks(7) {
                ours.update(chunk);
            }
            assert_eq!(ours.finalize()[..], expected[..]);
        }
    }

    /// Round functions are exactly the RFC definitions.
    #[test]
    fn rewritten_round_functions_equal_rfc_definitions() {
        let mut seed = 0x9e3779b9u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..100_000 {
            let (b, c, d) = (next(), next(), next());
            assert_eq!(d ^ (b & (c ^ d)), (b & c) | (!b & d));
            assert_eq!((b & d).wrapping_add(c & !d), (b & d) | (c & !d));
            assert_eq!(b ^ (c ^ d), b ^ c ^ d);
            assert_eq!(c ^ (b | !d), c ^ (b | !d));
        }
    }
}
