//! Streaming SHA-256 (FIPS 180-4) for the canonical PCM hash.
//!
//! Blocks are compressed with the x86 SHA extensions or the ARMv8 SHA2
//! instructions when the CPU reports them (checked once per hasher), and with
//! a portable implementation otherwise. All three produce the FIPS 180-4
//! digest; the tests compare them with the FIPS vectors and an independent
//! implementation.

pub(crate) const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];
const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Engine {
    Portable,
    #[cfg(target_arch = "x86_64")]
    ShaNi,
    #[cfg(target_arch = "aarch64")]
    Sha2,
}

impl Engine {
    fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("sha")
            && std::is_x86_feature_detected!("sse4.1")
            && std::is_x86_feature_detected!("ssse3")
        {
            return Self::ShaNi;
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("sha2") {
            return Self::Sha2;
        }
        Self::Portable
    }
}

#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
    engine: Engine,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self::with_engine(Engine::detect())
    }

    fn with_engine(engine: Engine) -> Self {
        Self {
            state: INIT,
            buffer: [0; 64],
            buffered: 0,
            length: 0,
            engine,
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
            self.compress(std::slice::from_ref(&block));
            self.buffered = 0;
        }
        data
    }
    /// Whole blocks of already counted data (buffer empty).
    pub(crate) fn absorb_blocks(&mut self, blocks: &[[u8; 64]]) {
        debug_assert!(blocks.is_empty() || self.buffered == 0);
        self.compress(blocks);
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

    /// The compression state when blocks are compressed with the SHA
    /// extensions (for interleaving with another hash), otherwise `None`.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn sha_ni_state(&mut self) -> Option<&mut [u32; 8]> {
        (self.engine == Engine::ShaNi).then_some(&mut self.state)
    }

    pub fn update(&mut self, data: impl AsRef<[u8]>) {
        let rest = self.absorb_head(data.as_ref());
        let (blocks, tail) = rest.as_chunks::<64>();
        self.absorb_blocks(blocks);
        self.absorb_tail(tail);
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        // Pad to 56 bytes mod 64, then append the bit length (big-endian).
        let zeros = (55usize.wrapping_sub(self.buffered)) % 64;
        let end = 1 + zeros;
        pad[end..end + 8].copy_from_slice(&bits.to_be_bytes());
        self.update(&pad[..end + 8]);
        debug_assert_eq!(self.buffered, 0);
        let mut out = [0u8; 32];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *chunk = word.to_be_bytes();
        }
        out
    }

    fn compress(&mut self, blocks: &[[u8; 64]]) {
        if blocks.is_empty() {
            return;
        }
        match self.engine {
            Engine::Portable => compress_portable(&mut self.state, blocks),
            // SAFETY: the engine is selected only after runtime detection of
            // the instructions these functions are compiled for.
            #[cfg(target_arch = "x86_64")]
            Engine::ShaNi => unsafe { compress_sha_ni(&mut self.state, blocks) },
            #[cfg(target_arch = "aarch64")]
            Engine::Sha2 => unsafe { compress_arm_sha2(&mut self.state, blocks) },
        }
    }
}

fn compress_portable(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    for block in blocks {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for t in 16..64 {
            let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
            let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
            w[t] = w[t - 16]
                .wrapping_add(s0)
                .wrapping_add(w[t - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for t in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[t])
                .wrapping_add(w[t]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// Intel SHA extensions. Each `sha256rnds2` performs two rounds on the
/// (A,B,E,F)/(C,D,G,H) register halves; `sha256msg1/msg2` extend the
/// message schedule four words at a time.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
#[inline]
pub(crate) unsafe fn compress_sha_ni(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    use std::arch::x86_64::*;
    let byte_swap = _mm_set_epi64x(0x0c0d0e0f08090a0b, 0x0405060700010203);
    // SAFETY (all loads/stores below): every pointer comes from a live
    // array or slice element of at least 16 bytes; loads are unaligned.
    let dcba = _mm_loadu_si128(state.as_ptr().cast());
    let hgfe = _mm_loadu_si128(state.as_ptr().add(4).cast());
    let cdab = _mm_shuffle_epi32(dcba, 0xb1);
    let efgh = _mm_shuffle_epi32(hgfe, 0x1b);
    let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
    let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xf0);
    for block in blocks {
        let (abef_saved, cdgh_saved) = (abef, cdgh);
        let load = |i: usize| {
            _mm_shuffle_epi8(
                _mm_loadu_si128(block.as_ptr().add(16 * i).cast()),
                byte_swap,
            )
        };
        let (mut w0, mut w1, mut w2, mut w3) = (load(0), load(1), load(2), load(3));
        // Four rounds: two `sha256rnds2` on message words m (+ K) lanes 0-1
        // and 2-3.
        macro_rules! rounds {
            ($m:expr, $i:expr) => {{
                let message = _mm_add_epi32($m, _mm_loadu_si128(K.as_ptr().add(4 * $i).cast()));
                cdgh = _mm_sha256rnds2_epu32(cdgh, abef, message);
                abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(message, 0x0e));
            }};
        }
        // Schedule: next = msg2(next + alignr(cur, prev3, 4), cur).
        macro_rules! extend {
            ($next:ident, $cur:ident, $prev:ident) => {
                $next = _mm_sha256msg2_epu32(
                    _mm_add_epi32($next, _mm_alignr_epi8($cur, $prev, 4)),
                    $cur,
                );
            };
        }
        rounds!(w0, 0);
        rounds!(w1, 1);
        w0 = _mm_sha256msg1_epu32(w0, w1);
        rounds!(w2, 2);
        w1 = _mm_sha256msg1_epu32(w1, w2);
        rounds!(w3, 3);
        extend!(w0, w3, w2);
        w2 = _mm_sha256msg1_epu32(w2, w3);
        rounds!(w0, 4);
        extend!(w1, w0, w3);
        w3 = _mm_sha256msg1_epu32(w3, w0);
        rounds!(w1, 5);
        extend!(w2, w1, w0);
        w0 = _mm_sha256msg1_epu32(w0, w1);
        rounds!(w2, 6);
        extend!(w3, w2, w1);
        w1 = _mm_sha256msg1_epu32(w1, w2);
        rounds!(w3, 7);
        extend!(w0, w3, w2);
        w2 = _mm_sha256msg1_epu32(w2, w3);
        rounds!(w0, 8);
        extend!(w1, w0, w3);
        w3 = _mm_sha256msg1_epu32(w3, w0);
        rounds!(w1, 9);
        extend!(w2, w1, w0);
        w0 = _mm_sha256msg1_epu32(w0, w1);
        rounds!(w2, 10);
        extend!(w3, w2, w1);
        w1 = _mm_sha256msg1_epu32(w1, w2);
        rounds!(w3, 11);
        extend!(w0, w3, w2);
        w2 = _mm_sha256msg1_epu32(w2, w3);
        rounds!(w0, 12);
        extend!(w1, w0, w3);
        w3 = _mm_sha256msg1_epu32(w3, w0);
        rounds!(w1, 13);
        extend!(w2, w1, w0);
        rounds!(w2, 14);
        extend!(w3, w2, w1);
        rounds!(w3, 15);
        abef = _mm_add_epi32(abef, abef_saved);
        cdgh = _mm_add_epi32(cdgh, cdgh_saved);
    }
    let feba = _mm_shuffle_epi32(abef, 0x1b);
    let dchg = _mm_shuffle_epi32(cdgh, 0xb1);
    let dcba = _mm_blend_epi16(feba, dchg, 0xf0);
    let hgfe = _mm_alignr_epi8(dchg, feba, 8);
    _mm_storeu_si128(state.as_mut_ptr().cast(), dcba);
    _mm_storeu_si128(state.as_mut_ptr().add(4).cast(), hgfe);
}

/// ARMv8 SHA2 instructions: `sha256h/h2` perform four rounds on the
/// (A,B,C,D)/(E,F,G,H) halves; `sha256su0/su1` extend the schedule.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon,sha2")]
unsafe fn compress_arm_sha2(state: &mut [u32; 8], blocks: &[[u8; 64]]) {
    use std::arch::aarch64::*;
    // SAFETY (all loads/stores below): pointers come from live arrays or
    // slice elements of at least 16 bytes.
    let mut abcd = vld1q_u32(state.as_ptr());
    let mut efgh = vld1q_u32(state.as_ptr().add(4));
    for block in blocks {
        let (abcd_saved, efgh_saved) = (abcd, efgh);
        let load =
            |i: usize| vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block.as_ptr().add(16 * i))));
        let (mut w0, mut w1, mut w2, mut w3) = (load(0), load(1), load(2), load(3));
        // Four rounds on message words m (+ K).
        macro_rules! rounds {
            ($m:expr, $i:expr) => {{
                let message = vaddq_u32($m, vld1q_u32(K.as_ptr().add(4 * $i)));
                let previous = abcd;
                abcd = vsha256hq_u32(abcd, efgh, message);
                efgh = vsha256h2q_u32(efgh, previous, message);
            }};
        }
        // Rounds on `cur`, then extend `cur` for four rounds later from the
        // three following words (message words are read before extension).
        macro_rules! rounds_extend {
            ($cur:ident, $a:ident, $b:ident, $c:ident, $i:expr) => {{
                let original = $cur;
                $cur = vsha256su0q_u32($cur, $a);
                rounds!(original, $i);
                $cur = vsha256su1q_u32($cur, $b, $c);
            }};
        }
        rounds_extend!(w0, w1, w2, w3, 0);
        rounds_extend!(w1, w2, w3, w0, 1);
        rounds_extend!(w2, w3, w0, w1, 2);
        rounds_extend!(w3, w0, w1, w2, 3);
        rounds_extend!(w0, w1, w2, w3, 4);
        rounds_extend!(w1, w2, w3, w0, 5);
        rounds_extend!(w2, w3, w0, w1, 6);
        rounds_extend!(w3, w0, w1, w2, 7);
        rounds_extend!(w0, w1, w2, w3, 8);
        rounds_extend!(w1, w2, w3, w0, 9);
        rounds_extend!(w2, w3, w0, w1, 10);
        rounds_extend!(w3, w0, w1, w2, 11);
        rounds!(w0, 12);
        rounds!(w1, 13);
        rounds!(w2, 14);
        rounds!(w3, 15);
        abcd = vaddq_u32(abcd, abcd_saved);
        efgh = vaddq_u32(efgh, efgh_saved);
    }
    vst1q_u32(state.as_mut_ptr(), abcd);
    vst1q_u32(state.as_mut_ptr().add(4), efgh);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engines() -> Vec<Engine> {
        let mut engines = vec![Engine::Portable];
        let detected = Engine::detect();
        if detected != Engine::Portable {
            engines.push(detected);
        }
        engines
    }

    fn digest(engine: Engine, data: &[u8]) -> [u8; 32] {
        let mut hash = Sha256::with_engine(engine);
        hash.update(data);
        hash.finalize()
    }

    #[test]
    fn fips_180_vectors_on_every_engine() {
        let million_a = vec![b'a'; 1_000_000];
        for engine in engines() {
            for (input, expected) in [
                (
                    &b""[..],
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                ),
                (
                    b"abc",
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                ),
                (
                    b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                    "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
                ),
                (
                    b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
                    "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
                ),
                (
                    &million_a[..],
                    "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
                ),
            ] {
                assert_eq!(crate::hex(&digest(engine, input)), expected, "{engine:?}");
            }
        }
    }

    /// Every length 0..300 (all padding cases), split points and tiny
    /// updates across block boundaries, against the independent RustCrypto
    /// implementation, on every available engine.
    #[test]
    fn matches_independent_implementation_for_all_lengths_and_splits() {
        use sha2_reference::Digest;
        let data: Vec<u8> = (0..5000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 11) as u8)
            .collect();
        for engine in engines() {
            for length in 0..300 {
                assert_eq!(
                    digest(engine, &data[..length])[..],
                    sha2_reference::Sha256::digest(&data[..length])[..],
                    "{engine:?} length {length}"
                );
            }
            for length in [63, 64, 65, 119, 120, 128, 1000, 5000] {
                let expected = sha2_reference::Sha256::digest(&data[..length]);
                for split in 0..=length.min(200) {
                    let mut hash = Sha256::with_engine(engine);
                    hash.update(&data[..split]);
                    hash.update(&data[split..length]);
                    assert_eq!(
                        hash.finalize()[..],
                        expected[..],
                        "{engine:?} {length}/{split}"
                    );
                }
                let mut hash = Sha256::with_engine(engine);
                for chunk in data[..length].chunks(11) {
                    hash.update(chunk);
                }
                assert_eq!(hash.finalize()[..], expected[..]);
            }
        }
    }
}
