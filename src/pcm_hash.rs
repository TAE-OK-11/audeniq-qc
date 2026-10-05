//! Canonical PCM SHA-256 and, optionally, the FLAC STREAMINFO MD5 of the
//! same samples in one traversal.
//!
//! SHA-256 covers left-aligned little-endian i32 samples (4 bytes each); the
//! MD5 covers compact little-endian samples (2 or 3 bytes each). Both are
//! serial per-block chains. When the SHA extensions are in use, runs of
//! whole blocks are compressed by kernels that interleave one MD5 block with
//! two SHA blocks (16-bit) or three MD5 blocks with four SHA blocks
//! (24-bit), so the core overlaps the two chains. Partial blocks and any
//! remainder go through each hash's own path; digests are identical to
//! hashing the two byte streams separately.
use crate::{md5::Md5, sha256::Sha256};

pub(crate) struct PcmHash {
    sha: Sha256,
    md5: Option<Md5>,
    /// The MD5 will be the source's verified STREAMINFO MD5 (same PCM).
    adopted: bool,
    bits: u16,
    compact: Vec<u8>,
}

impl PcmHash {
    /// `bits` is the source depth (16 or 24); `md5` requests the FLAC MD5.
    pub(crate) fn new(bits: u16, md5: bool) -> Self {
        Self {
            sha: Sha256::new(),
            md5: md5.then(Md5::new),
            adopted: false,
            bits,
            compact: Vec::new(),
        }
    }

    pub(crate) fn update(&mut self, samples: &[i32]) {
        let canonical = crate::audio::pcm_bytes(samples);
        let Some(md5) = &mut self.md5 else {
            self.sha.update(&canonical);
            return;
        };
        crate::audio::compact_pcm(samples, self.bits, &mut self.compact);
        update_both(&mut self.sha, &canonical, md5, &self.compact, self.bits);
    }

    pub(crate) fn sha_mut(&mut self) -> &mut Sha256 {
        &mut self.sha
    }
    pub(crate) fn wants_md5(&self) -> bool {
        self.md5.is_some() || self.adopted
    }
    /// The MD5 alone, when the SHA-256 was computed elsewhere.
    pub(crate) fn update_md5_only(&mut self, samples: &[i32]) {
        if let Some(md5) = &mut self.md5 {
            crate::audio::compact_pcm(samples, self.bits, &mut self.compact);
            md5.update(&self.compact);
        }
    }
    /// Use the source's STREAMINFO MD5 (of the same compact PCM, verified by
    /// its decoder) instead of computing it again. Only valid from the first
    /// update on, so nothing has been hashed into this MD5 yet.
    pub(crate) fn adopt_source_md5(&mut self) {
        if !self.adopted {
            debug_assert!(self.md5.as_ref().is_some_and(Md5::is_empty));
            self.md5 = None;
            self.adopted = true;
        }
    }
    pub(crate) fn finish(self) -> ([u8; 32], Option<[u8; 16]>) {
        debug_assert!(!self.adopted);
        (self.sha.finalize(), self.md5.map(Md5::finalize))
    }
    /// [`Self::finish`] where an adopted MD5 is the source's verified one.
    pub(crate) fn finish_with(
        self,
        verified_source_md5: Option<[u8; 16]>,
    ) -> crate::Result<([u8; 32], Option<[u8; 16]>)> {
        let md5 = if self.adopted {
            Some(verified_source_md5.ok_or(crate::Error::Invalid("unverified source MD5"))?)
        } else {
            self.md5.map(Md5::finalize)
        };
        Ok((self.sha.finalize(), md5))
    }
}

/// Hash `sha_bytes` into `sha` and `md5_bytes` into `md5`.
pub(crate) fn update_both(
    sha: &mut Sha256,
    sha_bytes: &[u8],
    md5: &mut Md5,
    md5_bytes: &[u8],
    bits: u16,
) {
    let (sha_blocks, sha_tail) = sha.absorb_head(sha_bytes).as_chunks::<64>();
    let (md5_blocks, md5_tail) = md5.absorb_head(md5_bytes).as_chunks::<64>();
    #[cfg(target_arch = "x86_64")]
    let (sha_blocks, md5_blocks) = interleave(sha, sha_blocks, md5, md5_blocks, bits);
    #[cfg(not(target_arch = "x86_64"))]
    let _ = bits;
    sha.absorb_blocks(sha_blocks);
    sha.absorb_tail(sha_tail);
    md5.absorb_blocks(md5_blocks);
    md5.absorb_tail(md5_tail);
}

/// Compress the longest run of whole interleaved units with the fused
/// kernels (SHA extensions only) and return the blocks left for each hash.
#[cfg(target_arch = "x86_64")]
fn interleave<'a, 'b>(
    sha: &mut Sha256,
    sha_blocks: &'a [[u8; 64]],
    md5: &mut Md5,
    md5_blocks: &'b [[u8; 64]],
    bits: u16,
) -> (&'a [[u8; 64]], &'b [[u8; 64]]) {
    // (MD5 blocks, SHA blocks) per interleaved unit.
    let (m, s) = if bits == 16 { (1, 2) } else { (3, 4) };
    let units = (md5_blocks.len() / m).min(sha_blocks.len() / s);
    let Some(state) = sha.sha_ni_state().filter(|_| units > 0) else {
        return (sha_blocks, md5_blocks);
    };
    let (md5_run, md5_rest) = md5_blocks.split_at(units * m);
    let (sha_run, sha_rest) = sha_blocks.split_at(units * s);
    // SAFETY: `sha_ni_state` is Some only when the SHA extensions (and
    // SSSE3/SSE4.1) were detected at runtime.
    unsafe {
        if bits == 16 {
            crate::hash_fused::md5_1_sha_2(md5.state_mut(), md5_run, state, sha_run);
        } else {
            crate::hash_fused::md5_3_sha_4(md5.state_mut(), md5_run, state, sha_run);
        }
    }
    (sha_rest, md5_rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any chunking, both depths, with and without MD5: digests equal the
    /// separately computed SHA-256 of canonical bytes and MD5 of compact
    /// bytes (which the md5/sha256 tests check against independent code).
    #[test]
    fn fused_digests_equal_separate_hashes_for_any_chunking() {
        let mut seed = 0x2545f491u32;
        for bits in [16u16, 24] {
            let shift = 32 - bits as u32;
            let samples: Vec<i32> = (0..40_000)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    ((seed as i32) >> shift) << shift
                })
                .collect();
            let mut sha = Sha256::new();
            sha.update(crate::audio::pcm_bytes(&samples));
            let mut compact = Vec::new();
            crate::audio::compact_pcm(&samples, bits, &mut compact);
            let mut md5 = Md5::new();
            md5.update(&compact);
            let expected = (sha.finalize(), md5.finalize());
            for chunk in [1usize, 2, 3, 7, 31, 64, 96, 1000, 4608, 40_000] {
                for with_md5 in [true, false] {
                    let mut hash = PcmHash::new(bits, with_md5);
                    for part in samples.chunks(chunk) {
                        hash.update(part);
                    }
                    let (sha, md5) = hash.finish();
                    assert_eq!(sha, expected.0, "bits {bits} chunk {chunk}");
                    assert_eq!(
                        md5,
                        with_md5.then_some(expected.1),
                        "bits {bits} chunk {chunk}"
                    );
                }
            }
        }
    }
}
