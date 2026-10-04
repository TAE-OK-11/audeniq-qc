mod benchmark;
mod common;
mod qualify;
mod standards;
mod stress;

use common::{Error, Options, Result};

fn main() {
    if let Err(error) = entry() {
        eprintln!("{}", serde_json::json!({"error": error.to_string()}));
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .ok_or("expected qualify, standards, codec-stress, benchmark, or benchmark-convert")?;
    if command == "ci-report" {
        let mut reports = serde_json::Map::new();
        for (name, path) in [
            ("analysis", "analysis-benchmark.json"),
            ("fingerprint", "fingerprint-benchmark.json"),
            ("normalization", "normalization-benchmark.json"),
            ("conversion_all", "conversion-all-benchmark.json"),
            ("review", "review-benchmark.json"),
            ("review_fingerprint", "review-fingerprint-benchmark.json"),
        ] {
            if !std::path::Path::new(path).exists() {
                continue;
            }
            reports.insert(
                name.to_owned(),
                serde_json::from_slice(&std::fs::read(path)?)?,
            );
        }
        println!(
            "AUDENIQ_BENCHMARK_JSON={}",
            serde_json::Value::Object(reports)
        );
        return Ok(());
    }
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--binary" => options.binary = args.next().ok_or("missing --binary")?.into(),
            "--baseline-binary" => {
                options.baseline_binary =
                    Some(args.next().ok_or("missing --baseline-binary")?.into())
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
    let report = match command.as_str() {
        "qualify" => qualify::execute(&options)?,
        "standards" => standards::execute(&options)?,
        "codec-stress" => stress::execute(&options)?,
        "benchmark" => benchmark::analysis(&options)?,
        "benchmark-convert" => benchmark::convert(&options)?,
        "benchmark-review" => {
            options.review = true;
            benchmark::convert(&options)?
        }
        _ => return Err("unknown development command".into()),
    };
    if let Some(path) = options.output {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&report)?),
        )?;
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
