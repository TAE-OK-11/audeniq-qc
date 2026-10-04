use crate::common::*;
use serde_json::{json, Value};

pub fn execute(options: &Options) -> Result<Value> {
    let temp = Temp::new("stress")?;
    let mut checks = 0;
    let mut rows = Vec::new();
    for depth in [16, 24] {
        for channels in [1, 2] {
            for pattern in [
                "zero",
                "constant",
                "impulse",
                "ramp",
                "limits",
                "random",
                "identical",
                "opposite",
            ] {
                let mut rng = Random::new();
                let scale = 1i32 << (depth - 1);
                let mut samples = Vec::new();
                let mut canonical = Vec::new();
                for i in 0..8193 {
                    let mono = (0.9 * scale as f64 * (i as f64 * 0.17).sin()).round() as i32;
                    for ch in 0..channels {
                        let value = match pattern {
                            "zero" => 0,
                            "constant" => scale / 3,
                            "impulse" => {
                                if i % 127 == 0 {
                                    -scale
                                } else {
                                    0
                                }
                            }
                            "ramp" => (i * 7919 + ch as i32 * 71) % (2 * scale) - scale,
                            "limits" => {
                                if (i + ch as i32) % 2 != 0 {
                                    -scale
                                } else {
                                    scale - 1
                                }
                            }
                            "random" => (rng.next() % (2 * scale) as u64) as i32 - scale,
                            "identical" => mono,
                            "opposite" => {
                                if ch == 0 {
                                    mono
                                } else {
                                    -mono
                                }
                            }
                            _ => unreachable!(),
                        };
                        samples.push(value);
                        canonical.extend_from_slice(&(value << (32 - depth)).to_le_bytes());
                    }
                }
                let expected = sha(&canonical);
                let src = temp.0.join("source.wav");
                write_wave(&src, 48000, depth, channels, &samples)?;
                let bits = depth.to_string();
                let mut profiles = Vec::new();
                for level in [0, 5, 12] {
                    profiles.push((
                        "flac",
                        strings(&["-c:a", "flac", "-compression_level", &level.to_string()]),
                    ));
                }
                for level in [0, 3, 8] {
                    profiles.push((
                        "wv",
                        strings(&[
                            "-c:a",
                            "wavpack",
                            "-bits_per_raw_sample",
                            &bits,
                            "-compression_level",
                            &level.to_string(),
                        ]),
                    ));
                }
                profiles.push(("tta", strings(&["-c:a", "tta"])));
                profiles.push(("m4a", strings(&["-c:a", "alac"])));
                for (ext, opts) in &profiles {
                    let input = temp.0.join(format!("encoded.{ext}"));
                    let mut args =
                        strings(&["ffmpeg", "-nostdin", "-v", "error", "-y", "-i", path(&src)]);
                    args.extend(opts.clone());
                    args.push(path(&input).to_owned());
                    run(&args)?;
                    check(
                        native(&options.binary, "pcm-hash", &[&input], &[])?["pcm_sha256"]
                            == expected,
                        "stress decode must preserve PCM",
                    )?;
                    checks += 1;
                }
                let output = temp.0.join("native.flac");
                if output.exists() {
                    std::fs::remove_file(&output)?;
                }
                native(&options.binary, "convert", &[&src, &output], &[])?;
                check(
                    oracle_hash(&output)? == expected,
                    "stress encode must preserve PCM",
                )?;
                checks += 1;
                rows.push(json!({"depth":depth,"channels":channels,"pattern":pattern,"profiles":profiles.len(),"pcm_exact":true,"native_flac_exact":true}));
            }
        }
    }
    Ok(
        json!({"harness":"audeniq-qc-tools Rust","status":"passed","checks":checks,"fixture_rng":"SplitMix64 seed 1729","source":"independent deterministic PCM integers + FFmpeg development oracle","sample_rate":48000,"frames_per_case":8193,"cases":rows}),
    )
}
