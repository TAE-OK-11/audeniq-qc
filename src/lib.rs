//! Local, lossless-only audio QC. No FFmpeg or native media-codec FFI at runtime.
#[cfg(not(feature = "reference-codecs"))]
mod alac;
pub mod audio;
mod bits;
#[cfg(not(feature = "reference-codecs"))]
mod compressed;
pub mod flac;
#[cfg(not(feature = "reference-codecs"))]
mod flac_decode;
pub mod kernels;
mod m4a;
mod md5;
pub mod meter;
mod mp4;
#[cfg(not(feature = "reference-codecs"))]
mod msb;
pub mod pcm;
pub mod probe;
mod profile;
mod sha256;
#[cfg(feature = "profile-native")]
pub use profile::report as native_profile;
pub mod resample;
mod tta;
mod wavpack;

use serde::Serialize;
use std::{
    fmt, io,
    time::{Duration, Instant},
};

pub const ENGINE_VERSION: &str = "audeniq-qc/0.1.0";
pub const METRIC_VERSION: &str = "native-bs1770-v1";
pub const RESAMPLER_VERSION: &str = "native-sinc64-v1";

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
    Deadline,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Invalid(s) => write!(f, "invalid input: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::Limit(s) => write!(f, "resource limit: {s}"),
            Self::Deadline => f.write_str("deadline exceeded"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Limits {
    pub max_file_bytes: u64,
    pub max_frames: u64,
    pub max_packet_bytes: usize,
    pub max_chunks: usize,
    pub deadline: Instant,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 4 * 1024 * 1024 * 1024,
            max_frames: 192_000 * 7200,
            max_packet_bytes: 16 * 1024 * 1024,
            max_chunks: 1_000_000,
            deadline: Instant::now() + Duration::from_secs(600),
        }
    }
}
impl Limits {
    pub fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AudioSpec {
    pub container: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub frames: Option<u64>,
}
impl AudioSpec {
    pub fn validate(&self) -> Result<()> {
        if !(44_100..=192_000).contains(&self.sample_rate)
            || !(1..=2).contains(&self.channels)
            || !matches!(self.bits_per_sample, 16 | 24)
        {
            return Err(Error::Unsupported(
                "requires 16/24-bit integer, 1/2 channels, 44100..192000 Hz",
            ));
        }
        Ok(())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}
