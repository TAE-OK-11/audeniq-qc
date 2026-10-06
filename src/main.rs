use audeniq_qc::{kernels::Backend, Error, Limits};
use std::{
    ffi::OsString,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, Instant},
};
fn write_json(value: &impl audeniq_qc::json::ToJson) -> audeniq_qc::Result<()> {
    // Serialize retained i16 windows directly. An intermediate JSON Value
    // expands each two-byte sample into a much larger boxed JSON number.
    let stdout = std::io::stdout();
    let mut writer = BufWriter::with_capacity(65536, stdout.lock());
    audeniq_qc::json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
fn run() -> audeniq_qc::Result<()> {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut limits = Limits::default();
    let mut backend = Backend::detect();
    let mut fingerprint = false;
    let mut analyze_conversion = false;
    let mut compression_level = None;
    let mut pcm_format = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--scalar" {
            backend = Backend::Scalar;
            args.remove(i);
        } else if args[i] == "--analyze" {
            if analyze_conversion {
                return Err(Error::Invalid("duplicate conversion analysis"));
            }
            analyze_conversion = true;
            args.remove(i);
        } else if args[i] == "--fingerprint" {
            fingerprint = true;
            args.remove(i);
        } else if args[i] == "--compression-level" {
            if compression_level.is_some() {
                return Err(Error::Invalid("duplicate compression level"));
            }
            compression_level = Some(
                args.get(i + 1)
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|n| *n <= 9)
                    .ok_or(Error::Invalid("compression level range 0..9"))?,
            );
            args.drain(i..i + 2);
        } else if args[i] == "--format" {
            if pcm_format.is_some() {
                return Err(Error::Invalid("duplicate PCM format"));
            }
            pcm_format = Some(match args.get(i + 1).and_then(|s| s.to_str()) {
                Some("wav") => audeniq_qc::pcm::Format::Wav,
                Some("s32le") => audeniq_qc::pcm::Format::S32le,
                _ => return Err(Error::Invalid("PCM format wav or s32le")),
            });
            args.drain(i..i + 2);
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
    if compression_level.is_some() && command != "convert" {
        return Err(Error::Invalid("compression level requires convert"));
    }
    if pcm_format.is_some() && command != "decode" {
        return Err(Error::Invalid("PCM format requires decode"));
    }
    if analyze_conversion && command != "convert" {
        return Err(Error::Invalid("--analyze requires convert"));
    }
    if fingerprint && command != "analyze" && !(command == "convert" && analyze_conversion) {
        return Err(Error::Invalid(
            "--fingerprint requires analyze or convert --analyze",
        ));
    }
    match (command, args.len()) {
        ("capabilities", 1) => write_json(
            &audeniq_qc::json!({"engine":audeniq_qc::ENGINE_VERSION,"backend":backend,"cpu_features":audeniq_qc::kernels::cpu_features(),"conversion_qc":true,"audio":["WAV/RF64/BW64 PCM16/24","AIFF/AIFC integer PCM","FLAC","M4A ALAC","TTA1","WavPack integer lossless single-block"],"images":["JPEG","PNG"],"pcm_outputs":["wav","s32le"],"compression_levels":{"min":0,"max":9,"default":5},"metric_version":audeniq_qc::METRIC_VERSION,"resampler_version":audeniq_qc::RESAMPLER_VERSION,"true_peak_certified":false,"media_codecs":if cfg!(feature="reference-codecs") {"symphonia-reference"} else {"audeniq-native"},"ffmpeg_runtime":false}),
        ),
        ("probe", 2) => write_json(&audeniq_qc::probe::media(Path::new(&args[1]), limits)?),
        ("image-probe", 2) => write_json(&audeniq_qc::probe::cover(Path::new(&args[1]), limits)?),
        ("tags", 2) => write_json(
            &audeniq_qc::json!({"format":{"tags":audeniq_qc::probe::tags(Path::new(&args[1]),&limits)?}}),
        ),
        ("analyze", 2) => write_json(&audeniq_qc::meter::analyze(
            Path::new(&args[1]),
            limits,
            backend,
            fingerprint,
        )?),
        ("fingerprint", 2) => write_json(&audeniq_qc::resample::fingerprint(
            Path::new(&args[1]),
            limits,
            backend,
        )?),
        ("pcm-hash", 2) => {
            let (spec, hash, frames) =
                audeniq_qc::audio::pcm_sha256(Path::new(&args[1]), limits, backend)?;
            write_json(&audeniq_qc::json!({"spec":spec,"pcm_sha256":hash,"frames":frames}))
        }
        ("decode", 3) => write_json(&audeniq_qc::pcm::decode(
            Path::new(&args[1]),
            Path::new(&args[2]),
            pcm_format.unwrap_or(audeniq_qc::pcm::Format::Wav),
            limits,
            backend,
        )?),
        ("convert", 3) => write_json(&audeniq_qc::flac::convert_with_options(
            Path::new(&args[1]),
            Path::new(&args[2]),
            limits,
            backend,
            audeniq_qc::flac::ConvertOptions {
                compression_level,
                analyze: analyze_conversion,
                fingerprint,
            },
        )?),
        ("help" | "--help" | "-h", _) => write_json(
            &audeniq_qc::json!({"usage":"audeniq-qc <probe|analyze|fingerprint|pcm-hash|image-probe|tags> FILE | convert INPUT OUTPUT.flac [--compression-level 0..9] [--analyze [--fingerprint]] | decode INPUT OUTPUT [--format wav|s32le] | capabilities; options: --scalar --fingerprint --timeout-secs N"}),
        ),
        _ => Err(Error::Invalid("command/arguments; use --help")),
    }
}
fn main() {
    let outcome = run();
    #[cfg(feature = "profile-native")]
    eprintln!(
        "{}",
        audeniq_qc::json!({"native_profile":audeniq_qc::native_profile(),"success":outcome.is_ok()})
    );
    match outcome {
        Ok(()) => (),
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
                audeniq_qc::json!({"error":code,"detail":e.to_string()})
            );
            std::process::exit(2);
        }
    }
}
