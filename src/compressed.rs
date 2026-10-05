//! Native media path; the `reference-codecs` build substitutes Symphonia.
use crate::{AudioSpec, Limits, Result};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};
enum Input {
    Flac(crate::flac_decode::Decoder),
    Alac {
        file: BufReader<File>,
        position: u64,
        track: crate::m4a::Track,
        decoder: crate::alac::Decoder,
        index: usize,
        packet: Vec<u8>,
    },
}
pub(crate) struct Compressed {
    input: Input,
    pub spec: AudioSpec,
    pub flac_info: Option<[u8; 34]>,
    pub retain_frame: bool,
}
impl Compressed {
    pub fn open(mut file: File, limits: &Limits, is_flac: bool) -> Result<Self> {
        let (input, spec, flac_info) = if is_flac {
            let d = crate::flac_decode::Decoder::open(file, limits)?;
            let spec = d.spec.clone();
            let info = d.info;
            (Input::Flac(d), spec, Some(info))
        } else {
            let track = crate::m4a::open(&mut file, limits)?;
            let spec = track.spec.clone();
            let decoder = crate::alac::Decoder::new(track.config);
            (
                Input::Alac {
                    file: BufReader::with_capacity(65536, file),
                    position: u64::MAX,
                    track,
                    decoder,
                    index: 0,
                    packet: Vec::new(),
                },
                spec,
                None,
            )
        };
        Ok(Self {
            input,
            spec,
            flac_info,
            retain_frame: false,
        })
    }
    /// With `sha`, FLAC input also hashes `out` into it (fused with the
    /// STREAMINFO MD5 check); returns whether it did. ALAC never does.
    pub fn next(
        &mut self,
        out: &mut Vec<i32>,
        limits: &Limits,
        sha: Option<&mut crate::sha256::Sha256>,
    ) -> Result<bool> {
        let hashed = sha.is_some() && matches!(self.input, Input::Flac(_));
        match &mut self.input {
            Input::Flac(d) => d.next(out, limits, self.retain_frame, sha)?,
            Input::Alac {
                file,
                position,
                track,
                decoder,
                index,
                packet,
            } => {
                if let Some(p) = track.packets.get(*index) {
                    limits.check()?;
                    if *position != p.offset {
                        file.seek(SeekFrom::Start(p.offset))?;
                    }
                    with_packet(file, packet, p.size, |data| {
                        decoder.decode(data, p.frames, out)
                    })?;
                    *position = p.offset + p.size as u64;
                    *index += 1;
                } else {
                    out.clear();
                }
            }
        }
        Ok(hashed)
    }
    /// Whether FLAC input checks a STREAMINFO MD5.
    pub fn flac_md5_active(&self) -> bool {
        matches!(&self.input, Input::Flac(d) if d.md5_active())
    }
    /// The STREAMINFO MD5 of FLAC input once verified at the end.
    pub fn verified_md5(&self) -> Option<[u8; 16]> {
        match &self.input {
            Input::Flac(d) => d.verified_md5(),
            Input::Alac { .. } => None,
        }
    }
    pub fn flac_frame(&self) -> Option<&[u8]> {
        match &self.input {
            Input::Flac(d) => d.flac_frame(),
            Input::Alac { .. } => None,
        }
    }
}

// Most packets fit the existing bounded read buffer. Decode that slice directly;
// only packets crossing a fill boundary need the reusable scratch allocation.
fn with_packet(
    file: &mut BufReader<File>,
    scratch: &mut Vec<u8>,
    size: usize,
    decode: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<()> {
    let buffered = {
        let _profile = crate::profile::scope(crate::profile::Stage::PacketRead);
        file.fill_buf()?
    };
    if buffered.len() >= size {
        crate::profile::count(crate::profile::Counter::PacketBorrowedBytes, size as u64);
        decode(&buffered[..size])?;
        file.consume(size);
    } else {
        crate::profile::count(crate::profile::Counter::PacketCopiedBytes, size as u64);
        scratch.resize(size, 0);
        {
            let _profile = crate::profile::scope(crate::profile::Stage::PacketRead);
            file.read_exact(scratch)?;
        }
        decode(scratch)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_borrow_crossing_and_truncation_preserve_exact_bytes() {
        let path =
            std::env::temp_dir().join(format!("audeniq-packet-borrow-{}", std::process::id()));
        struct Temp(std::path::PathBuf);
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let temp = Temp(path);
        let expected: Vec<u8> = (0..65).collect();
        std::fs::write(&temp.0, &expected).unwrap();
        let mut file = BufReader::with_capacity(16, File::open(&temp.0).unwrap());
        let mut scratch = Vec::new();
        let ptr = file.fill_buf().unwrap().as_ptr();
        with_packet(&mut file, &mut scratch, 8, |bytes| {
            assert_eq!(bytes.as_ptr(), ptr);
            assert_eq!(bytes, &expected[..8]);
            Ok(())
        })
        .unwrap();
        assert_eq!(scratch.capacity(), 0);
        with_packet(&mut file, &mut scratch, 20, |bytes| {
            assert_eq!(bytes, &expected[8..28]);
            Ok(())
        })
        .unwrap();
        assert!(scratch.capacity() >= 20);
        let ptr = file.fill_buf().unwrap().as_ptr();
        with_packet(&mut file, &mut scratch, 4, |bytes| {
            assert_eq!(bytes.as_ptr(), ptr);
            assert_eq!(bytes, &expected[28..32]);
            Ok(())
        })
        .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        with_packet(&mut file, &mut scratch, 65, |bytes| {
            assert_eq!(bytes, expected);
            Ok(())
        })
        .unwrap();
        let mut decoded = false;
        assert!(with_packet(&mut file, &mut scratch, 1, |_| {
            decoded = true;
            Ok(())
        })
        .is_err());
        assert!(!decoded);
    }
}
