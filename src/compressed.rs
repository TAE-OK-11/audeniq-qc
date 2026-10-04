//! Native media path; the `reference-codecs` build substitutes Symphonia.
use crate::{AudioSpec, Limits, Result};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
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
    pub raw_frame: Option<Box<[u8]>>,
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
            raw_frame: None,
        })
    }
    pub fn next(&mut self, out: &mut Vec<i32>, limits: &Limits) -> Result<()> {
        self.raw_frame = None;
        match &mut self.input {
            Input::Flac(d) => self.raw_frame = d.next(out, limits, self.retain_frame)?,
            Input::Alac {
                file,
                position,
                track,
                decoder,
                index,
                packet,
            } => {
                out.clear();
                if let Some(p) = track.packets.get(*index) {
                    limits.check()?;
                    if *position != p.offset {
                        file.seek(SeekFrom::Start(p.offset))?;
                    }
                    packet.resize(p.size, 0);
                    file.read_exact(packet)?;
                    *position = p.offset + p.size as u64;
                    decoder.decode(packet, p.frames, out)?;
                    *index += 1;
                }
            }
        }
        Ok(())
    }
}
