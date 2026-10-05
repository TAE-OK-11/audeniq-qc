mod benchmark;
mod common;
mod qualify;
mod standards;
mod stress;

use audeniq_qc::{json, json::Value};
use common::{Error, Options, Result};

fn main() {
    if let Err(error) = entry() {
        eprintln!("{}", json!({"error": error.to_string()}));
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("expected qualify, standards, codec-stress, benchmark, or benchmark-convert")?;
    if command == "ci-report" {
        let mut reports = audeniq_qc::json::Map::new();
        for (name, path) in [
            ("analysis", "analysis-benchmark.json"),
            ("decoding", "decoding-benchmark.json"),
            ("fingerprint", "fingerprint-benchmark.json"),
            ("normalization", "normalization-benchmark.json"),
            ("conversion_all", "conversion-all-benchmark.json"),
            ("review", "review-benchmark.json"),
            ("review_fingerprint", "review-fingerprint-benchmark.json"),
            ("streaming_decoding", "streaming-decoding-benchmark.json"),
            (
                "streaming_normalization",
                "streaming-normalization-benchmark.json",
            ),
            ("streaming_review", "streaming-review-benchmark.json"),
            ("alac_predictors", "alac-predictors.json"),
        ] {
            if !std::path::Path::new(path).exists() {
                continue;
            }
            reports.insert(
                name.to_owned(),
                audeniq_qc::json::from_slice(&std::fs::read(path)?)?,
            );
        }
        println!("AUDENIQ_BENCHMARK_JSON={}", Value::Object(reports));
        return Ok(());
    }
    let mut options = Options::default();
    let mut comparison_mode = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--binary" => options.binary = args.next().ok_or("missing --binary")?.into(),
            "--baseline-binary" => {
                options.baseline_binary =
                    Some(args.next().ok_or("missing --baseline-binary")?.into())
            }
            "--reference-binary" => {
                options.baseline_binary =
                    Some(args.next().ok_or("missing --reference-binary")?.into());
                options.reference_codecs = true;
            }
            "--comparison-binary" => {
                options.baseline_binary =
                    Some(args.next().ok_or("missing --comparison-binary")?.into());
                options.reference_codecs = true;
                comparison_mode = true;
            }
            "--output" => options.output = Some(args.next().ok_or("missing --output")?.into()),
            "--seconds" => options.seconds = Some(args.next().ok_or("missing --seconds")?.parse()?),
            "--repeats" => options.repeats = args.next().ok_or("missing --repeats")?.parse()?,
            "--fingerprint" => options.fingerprint = true,
            "--wav-alac-only" => options.wav_alac_only = true,
            _ => return Err(Error::from(format!("unknown argument {arg}"))),
        }
    }
    options.binary = std::fs::canonicalize(&options.binary)?;
    options.baseline_binary = options
        .baseline_binary
        .map(std::fs::canonicalize)
        .transpose()?;
    if options.repeats < 3 || options.seconds == Some(0) {
        return Err("expected at least 3 repeats and positive seconds".into());
    }
    let mut report = match command.as_str() {
        "qualify" => qualify::execute(&options)?,
        "standards" => standards::execute(&options)?,
        "codec-stress" => stress::execute(&options)?,
        "benchmark" => benchmark::analysis(&options)?,
        "benchmark-decode" => benchmark::decoding(&options)?,
        "benchmark-alac-predictors" => benchmark::alac_predictors(&options)?,
        "benchmark-convert" => benchmark::convert(&options)?,
        "benchmark-review" => {
            options.review = true;
            benchmark::convert(&options)?
        }
        _ => return Err("unknown development command".into()),
    };
    if options.reference_codecs {
        fn rename(value: &mut Value, name: &str) {
            match value {
                Value::Object(map) => {
                    let old = std::mem::take(map);
                    for (key, mut value) in old {
                        rename(&mut value, name);
                        map.insert(key.replace("baseline", name), value);
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        rename(value, name);
                    }
                }
                _ => (),
            }
        }
        rename(
            &mut report,
            if comparison_mode {
                "comparison"
            } else {
                "reference"
            },
        );
        if comparison_mode {
            report["comparison_mode"] = json!("Explicit comparison binary; both review builds use fused conversion/QC. Consult binary SHA-256 and workflow checkout SHA for provenance.");
        } else {
            report["reference_mode"] = json!("Same engine/encoder/QC, reference-codecs feature enables Symphonia ALAC/FLAC decoding and demuxing. Generic JSON/hash/image dependencies remain in both builds.");
        }
    }
    if let Some(path) = options.output {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            format!("{}\n", audeniq_qc::json::to_string_pretty(&report)),
        )?;
    }
    println!("{}", audeniq_qc::json::to_string_pretty(&report));
    Ok(())
}
