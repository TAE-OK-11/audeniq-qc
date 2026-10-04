use audeniq_qc::{kernels::Backend, Error, Limits};
use std::{
    ffi::OsString,
    path::Path,
    time::{Duration, Instant},
};
fn run() -> audeniq_qc::Result<serde_json::Value> {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut limits = Limits::default();
    let mut backend = Backend::detect();
    let mut fingerprint = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--scalar" {
            backend = Backend::Scalar;
            args.remove(i);
        } else if args[i] == "--fingerprint" {
            fingerprint = true;
            args.remove(i);
        } else if args[i] == "--timeout-secs" {
            if i + 1 >= args.len() {
                return Err(Error::Invalid("missing timeout"));
            }
            let n = args[i + 1]
                .to_str()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0 && *n <= 3600)
                .ok_or(Error::Invalid("timeout range 1..3600"))?;
            limits.deadline = Instant::now() + Duration::from_secs(n);
            args.drain(i..i + 2);
        } else {
            i += 1;
        }
    }
    let command = args.first().and_then(|s| s.to_str()).unwrap_or("help");
    match (command, args.len()) {
        ("capabilities", 1) => Ok(
            serde_json::json!({"engine":audeniq_qc::ENGINE_VERSION,"backend":backend,"audio":["WAV/RF64/BW64 PCM16/24","AIFF/AIFC integer PCM","FLAC","M4A ALAC","TTA1","WavPack integer lossless single-block"],"images":["JPEG","PNG"],"metric_version":audeniq_qc::METRIC_VERSION,"resampler_version":audeniq_qc::RESAMPLER_VERSION,"true_peak_certified":false,"ffmpeg_runtime":false}),
        ),
        ("probe", 2) => audeniq_qc::probe::media(Path::new(&args[1]), limits),
        ("image-probe", 2) => audeniq_qc::probe::cover(Path::new(&args[1]), limits),
        ("tags", 2) => Ok(
            serde_json::json!({"format":{"tags":audeniq_qc::probe::tags(Path::new(&args[1]),&limits)?}}),
        ),
        ("analyze", 2) => serde_json::to_value(audeniq_qc::meter::analyze(
            Path::new(&args[1]),
            limits,
            backend,
            fingerprint,
        )?)
        .map_err(|_| Error::Invalid("report serialization")),
        ("fingerprint", 2) => serde_json::to_value(audeniq_qc::resample::fingerprint(
            Path::new(&args[1]),
            limits,
            backend,
        )?)
        .map_err(|_| Error::Invalid("report serialization")),
        ("pcm-hash", 2) => {
            let (spec, hash, frames) =
                audeniq_qc::audio::pcm_sha256(Path::new(&args[1]), limits, backend)?;
            Ok(serde_json::json!({"spec":spec,"pcm_sha256":hash,"frames":frames}))
        }
        ("convert", 3) => serde_json::to_value(audeniq_qc::flac::convert(
            Path::new(&args[1]),
            Path::new(&args[2]),
            limits,
            backend,
        )?)
        .map_err(|_| Error::Invalid("report serialization")),
        ("help" | "--help" | "-h", _) => Ok(
            serde_json::json!({"usage":"audeniq-qc <probe|analyze|fingerprint|pcm-hash|image-probe|tags> FILE | convert INPUT OUTPUT.flac | capabilities; options: --scalar --fingerprint --timeout-secs N"}),
        ),
        _ => Err(Error::Invalid("command/arguments; use --help")),
    }
}
fn main() {
    match run() {
        Ok(v) => println!("{v}"),
        Err(e) => {
            let code = match e {
                Error::Io(_) => "IO",
                Error::Invalid(_) => "INVALID_INPUT",
                Error::Unsupported(_) => "UNSUPPORTED",
                Error::Limit(_) => "RESOURCE_LIMIT",
                Error::Deadline => "DEADLINE",
            };
            eprintln!(
                "{}",
                serde_json::json!({"error":code,"detail":e.to_string()})
            );
            std::process::exit(2);
        }
    }
}
