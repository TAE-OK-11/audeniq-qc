use crate::common::*;
use audeniq_qc::{json, json::Value};
use std::{fs, path::Path};

fn loudness(input: &Path) -> Result<(f64, f64)> {
    let output = ff(&[
        "-hide_banner",
        "-nostats",
        "-i",
        path(input),
        "-af",
        "ebur128=peak=true:framelog=quiet",
        "-f",
        "null",
        "-",
    ])?;
    let text = String::from_utf8(output.stderr)?;
    let mut integrated = None;
    let mut peak = None;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("I:") {
            integrated = value
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok());
        }
        if let Some(value) = line.strip_prefix("Peak:") {
            peak = value
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok());
        }
    }
    Ok((integrated.ok_or("oracle LUFS")?, peak.ok_or("oracle peak")?))
}
pub fn execute(options: &Options) -> Result<Value> {
    let temp = Temp::new("qualify")?;
    let root = &temp.0;
    let binary = &options.binary;
    let mut checks = 0;
    let mut results = Vec::new();
    macro_rules! verify {
        ($condition:expr, $message:expr) => {{
            check($condition, $message)?;
            checks += 1;
        }};
    }
    for (rate, depth, channels) in [
        (44100, 16, 1),
        (48000, 24, 2),
        (96000, 24, 2),
        (192000, 16, 2),
    ] {
        let src = root.join(format!("{rate}-{depth}-{channels}.wav"));
        fixture(&src, rate, depth, channels, "tone", 1.2)?;
        let expected = oracle_hash(&src)?;
        let pcm = format!("pcm_s{depth}le");
        let be = format!("pcm_s{depth}be");
        let bits = depth.to_string();
        let formats = [
            ("wav", strings(&["-c:a", &pcm])),
            (
                "flac",
                strings(&[
                    "-c:a",
                    "flac",
                    "-sample_fmt",
                    if depth == 16 { "s16" } else { "s32" },
                ]),
            ),
            ("m4a", strings(&["-c:a", "alac"])),
            ("aiff", strings(&["-c:a", &be])),
            ("tta", strings(&["-c:a", "tta"])),
            (
                "wv",
                strings(&["-c:a", "wavpack", "-bits_per_raw_sample", &bits]),
            ),
            ("rf64.wav", strings(&["-c:a", &pcm, "-rf64", "always"])),
        ];
        for (ext, opts) in formats {
            let input = root.join(format!("{rate}-{depth}-{channels}.encoded.{ext}"));
            let mut args = strings(&["ffmpeg", "-v", "error", "-i", path(&src)]);
            args.extend(opts);
            args.extend(strings(&["-y", path(&input)]));
            run(&args)?;
            let report = native(binary, "pcm-hash", &[&input], &[])?;
            verify!(report["pcm_sha256"] == expected, "codec PCM hash");
            verify!(
                report["frames"] == (rate as f64 * 1.2).round() as u64,
                "declared frame count"
            );
            let analysis = native(binary, "analyze", &[&input], &[])?;
            verify!(analysis["pcm_sha256"] == expected, "analysis PCM hash");
            let output = root.join(format!(
                "{}.out.flac",
                input.file_name().unwrap().to_string_lossy()
            ));
            let converted = native(binary, "convert", &[&input, &output], &[])?;
            let fused_output = output.with_extension("qc.flac");
            let fused = native(binary, "convert", &[&input, &fused_output], &["--analyze"])?;
            verify!(
                fused["analysis"] == analysis,
                "fused QC exactly matches standalone analysis"
            );
            verify!(
                oracle_hash(&fused_output)? == expected,
                "fused conversion PCM hash"
            );
            verify!(
                converted["pcm_sha256"] == expected && oracle_hash(&output)? == expected,
                "lossless round trip"
            );
            if ext == "flac" {
                verify!(
                    converted["encoder"] == "verified-flac-frame-copy-v1",
                    "verified FLAC copy"
                );
            }
            verify!(
                converted["spec"]["bits_per_sample"] == depth
                    && converted["spec"]["sample_rate"] == rate,
                "converted specification"
            );
            let probe = native(binary, "probe", &[&input], &[])?;
            verify!(probe["streams"][0]["codec_type"] == "audio", "audio probe");
            for format in ["wav", "s32le"] {
                let output = root.join(format!(
                    "{}.pcm.{format}",
                    input.file_name().unwrap().to_string_lossy()
                ));
                let exported = native(binary, "decode", &[&input, &output], &["--format", format])?;
                verify!(
                    exported["pcm_sha256"] == expected && exported["frames"] == report["frames"],
                    "PCM export count/hash"
                );
                let actual = if format == "wav" {
                    oracle_hash(&output)?
                } else {
                    raw_oracle_hash(&output, rate, channels)?
                };
                verify!(actual == expected, "independent PCM export hash");
            }
            if matches!(ext, "wav" | "flac") {
                for level in 0..=10 {
                    let output = root.join(format!(
                        "{}.level{level}.flac",
                        input.file_name().unwrap().to_string_lossy()
                    ));
                    let result = native(
                        binary,
                        "convert",
                        &[&input, &output],
                        &["--compression-level", &level.to_string()],
                    )?;
                    verify!(
                        result["compression_level"] == level
                            && result["encoder"] != "verified-flac-frame-copy-v1",
                        "explicit compression level re-encodes"
                    );
                    verify!(
                        result["pcm_sha256"] == expected && oracle_hash(&output)? == expected,
                        "compression level preserves PCM"
                    );
                }
            }
            results.push(json!({"case":input.file_name().unwrap().to_string_lossy(),"pcm_exact":true,"roundtrip_exact":true}));
        }
    }
    for kind in ["tone", "silence", "noise", "clip", "gated", "near_nyquist"] {
        let src = root.join(format!("{kind}.wav"));
        fixture(&src, 48000, 24, 2, kind, 2.0)?;
        let n = native(binary, "analyze", &[&src], &[])?;
        let s = native(binary, "analyze", &[&src], &["--scalar"])?;
        for key in [
            "clip_events",
            "clipped_samples",
            "silent_blocks",
            "longest_silent_run",
            "zero_crossing_rate",
            "pcm_sha256",
            "samples_per_channel",
            "integrated_lufs",
        ] {
            verify!(n[key] == s[key], &format!("scalar equality {kind} {key}"));
        }
        let (i, tp) = loudness(&src)?;
        if kind == "silence" {
            verify!(
                n["integrated_lufs"].is_null()
                    && n["true_peak_dbtp"].is_null()
                    && n["silent_blocks"] == 40,
                "silence metrics"
            );
        } else {
            verify!(
                (n["integrated_lufs"].as_f64().ok_or("LUFS value")? - i).abs() <= 0.11,
                "LUFS oracle tolerance"
            );
            if kind != "noise" {
                verify!(
                    (n["true_peak_dbtp"].as_f64().ok_or("peak value")? - tp).abs() <= 0.5,
                    "true peak oracle tolerance"
                );
            }
        }
        if kind == "clip" {
            verify!(
                n["clip_events"].as_u64().unwrap_or(0) > 100
                    && n["clipped_samples"].as_u64().unwrap_or(0) > 1000,
                "clipping detection"
            );
        }
        results.push(json!({"case":kind,"native_lufs":n["integrated_lufs"],"ffmpeg_lufs":if i.is_finite(){Some(i)}else{None},"native_true_peak":n["true_peak_dbtp"],"ffmpeg_true_peak":if tp.is_finite(){Some(tp)}else{None}}));
    }
    let src = root.join("bad-source.wav");
    fixture(&src, 48000, 24, 2, "tone", 2.0)?;
    for (ext, opts) in [
        ("wav", vec![]),
        ("flac", strings(&["-c:a", "flac"])),
        ("m4a", strings(&["-c:a", "alac"])),
        ("tta", strings(&["-c:a", "tta"])),
        (
            "wv",
            strings(&["-c:a", "wavpack", "-bits_per_raw_sample", "24"]),
        ),
    ] {
        let good = if ext == "wav" {
            src.clone()
        } else {
            root.join(format!("good.{ext}"))
        };
        if ext != "wav" {
            let mut args = strings(&["ffmpeg", "-v", "error", "-i", path(&src)]);
            args.extend(opts);
            args.extend(strings(&["-y", path(&good)]));
            run(&args)?;
        }
        let bytes = fs::read(&good)?;
        let bad = root.join(format!("truncated.{ext}"));
        fs::write(&bad, &bytes[..bytes.len() * 4 / 5])?;
        rejected(binary, "analyze", &[&bad])?;
        checks += 1;
        let output = root.join(format!("bad.{ext}.flac"));
        rejected(binary, "convert", &[&bad, &output])?;
        verify!(!output.exists(), "invalid conversion must not publish");
        for format in ["wav", "s32le"] {
            let output = root.join(format!("truncated-{ext}.{format}"));
            let args = strings(&[
                path(binary),
                "decode",
                path(&bad),
                path(&output),
                "--format",
                format,
            ]);
            let rejection = capture(&args)?;
            verify!(
                rejection.status.code() == Some(2) && !output.exists(),
                "truncated PCM export never publishes"
            );
        }
        let prefix = format!(".{}.", output.file_name().unwrap().to_string_lossy());
        verify!(
            !fs::read_dir(root)?
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(&prefix)),
            "partial output cleanup"
        );
        if matches!(ext, "tta" | "wv" | "flac") {
            let mut corrupt = bytes.clone();
            let mid = corrupt.len() / 2;
            corrupt[mid] ^= 0x80;
            let bad = root.join(format!("corrupt.{ext}"));
            fs::write(&bad, corrupt)?;
            rejected(binary, "analyze", &[&bad])?;
            checks += 1;
        }
    }
    let lossy = root.join("lossy.m4a");
    ff(&["-v", "error", "-i", path(&src), "-c:a", "aac", path(&lossy)])?;
    rejected(binary, "analyze", &[&lossy])?;
    checks += 1;
    let bytes = fs::read(root.join("good.wv"))?;
    let mut hidden = bytes.clone();
    let flags = u32::from_le_bytes(hidden[24..28].try_into()?);
    hidden[24..28].copy_from_slice(&(flags | 8).to_le_bytes());
    let mut forged = bytes;
    forged.extend_from_slice(&hidden);
    forged.extend_from_slice(b"APETAGEX");
    for value in [2000, hidden.len() as u32 + 32, 0, 0] {
        forged.extend_from_slice(&value.to_le_bytes());
    }
    forged.extend_from_slice(&[0; 8]);
    let input = root.join("ape-hidden-hybrid.wv");
    fs::write(&input, forged)?;
    rejected(binary, "analyze", &[&input])?;
    checks += 1;
    let mut bytes = fs::read(root.join("good.flac"))?;
    let packed = u64::from_be_bytes(bytes[18..26].try_into()?);
    bytes[18..26].copy_from_slice(&(packed & !((1u64 << 36) - 1)).to_be_bytes());
    let input = root.join("unknown-length.flac");
    fs::write(&input, bytes)?;
    rejected(binary, "analyze", &[&input])?;
    checks += 1;
    let fp = native(binary, "analyze", &[&src], &["--fingerprint"])?;
    verify!(
        fp["fingerprint_windows"][0]["samples"]
            .as_array()
            .ok_or("fingerprint samples")?
            .len()
            == 22050,
        "short fingerprint cardinality"
    );
    let alias = root.join("alias.wav");
    ff(&[
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=15000:sample_rate=48000:duration=2",
        "-ac",
        "2",
        "-c:a",
        "pcm_s24le",
        path(&alias),
    ])?;
    let fp = native(binary, "analyze", &[&alias], &["--fingerprint"])?;
    let samples = fp["fingerprint_windows"][0]["samples"]
        .as_array()
        .ok_or("alias samples")?;
    let mid = &samples[100..samples.len() - 100];
    let rms =
        (mid.iter().map(|x| x.as_f64().unwrap().powi(2)).sum::<f64>() / mid.len() as f64).sqrt();
    verify!(rms < 10.0, "fingerprint anti-aliasing");
    let long = root.join("long.wav");
    ff(&[
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=437:sample_rate=48000:duration=121.235",
        "-ac",
        "2",
        "-c:a",
        "pcm_s24le",
        path(&long),
    ])?;
    let fused = native(binary, "analyze", &[&long], &["--fingerprint"])?;
    let normalized = root.join("long.review.flac");
    let converted = native(
        binary,
        "convert",
        &[&long, &normalized],
        &["--analyze", "--fingerprint"],
    )?;
    verify!(
        converted["analysis"] == fused,
        "long fused conversion/QC/fingerprint equality"
    );
    verify!(
        oracle_hash(&normalized)? == fused["pcm_sha256"].as_str().ok_or("long PCM hash")?,
        "long fused output PCM hash"
    );
    let fallback = native(binary, "fingerprint", &[&long], &[])?;
    verify!(
        fused["fingerprint_windows"] == fallback["fingerprint_windows"],
        "fused fingerprint equality"
    );
    verify!(
        fallback["fingerprint_windows"]
            == native(binary, "fingerprint", &[&long], &["--scalar"])?["fingerprint_windows"],
        "scalar fingerprint equality"
    );
    let windows = fallback["fingerprint_windows"]
        .as_array()
        .ok_or("windows")?;
    verify!(
        windows.len() == 3
            && windows
                .iter()
                .all(|w| w["samples"].as_array().is_some_and(|a| a.len() == 330750)),
        "bounded 90-second retention"
    );
    verify!(
        windows
            .iter()
            .zip([0.0, 45.6175, 91.235])
            .all(|(w, e)| (w["start_secs"].as_f64().unwrap() - e).abs() < 1e-6),
        "fractional window starts"
    );
    let png = root.join("cover.png");
    let jpg = root.join("cover.jpg");
    ff(&[
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=64x64",
        "-frames:v",
        "1",
        "-threads",
        "1",
        path(&png),
    ])?;
    ff(&[
        "-v",
        "error",
        "-i",
        path(&png),
        "-frames:v",
        "1",
        "-threads",
        "1",
        path(&jpg),
    ])?;
    for input in [&png, &jpg] {
        verify!(
            native(binary, "image-probe", &[input], &[])?["streams"][0]["width"] == 64,
            "cover dimensions"
        );
    }
    for (ext, opts) in [
        ("flac", vec!["-c:a", "flac"]),
        ("m4a", vec!["-c:a", "alac"]),
        ("aiff", vec!["-c:a", "pcm_s24be"]),
        ("wav", vec!["-c:a", "pcm_s24le"]),
        ("wv", vec!["-c:a", "wavpack", "-bits_per_raw_sample", "24"]),
        ("tta", vec!["-c:a", "tta"]),
    ] {
        let input = root.join(format!("tagged.{ext}"));
        let mut args = strings(&[
            "ffmpeg",
            "-v",
            "error",
            "-i",
            path(&src),
            "-metadata",
            "comment=AUDENIQ oracle",
        ]);
        args.extend(strings(&opts));
        args.push(path(&input).to_owned());
        run(&args)?;
        verify!(
            native(binary, "tags", &[&input], &[])?["format"]["tags"]["comment"]
                == "AUDENIQ oracle",
            "provenance comment"
        );
        let output = root.join(format!("tagged-{ext}.normalized.flac"));
        native(binary, "convert", &[&input, &output], &[])?;
        verify!(
            native(binary, "tags", &[&output], &[])?["format"]["tags"] == json!({}),
            "metadata removal"
        );
    }
    Ok(
        json!({"harness":"audeniq-qc-tools Rust", "fixture_rng":"SplitMix64 seed 1729", "checks":checks,"status":"passed","cases":results,"true_peak_tolerance_db":0.5,"true_peak_comparison_exclusions":"Full-band white noise difference is recorded and is not claimed bit-identical. Near-Nyquist tone has 10ms fade.","lufs_tolerance_lu":0.11,"true_peak_certified":false}),
    )
}
