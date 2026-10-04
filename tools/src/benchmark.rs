use crate::common::*;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const KEYS: [&str; 5] = ["wall_s", "user_s", "system_s", "cpu_s", "peak_rss_kib"];
fn cpu_identity() -> Result<String> {
    let text = fs::read_to_string("/proc/cpuinfo")?;
    if let Some(model) = text
        .lines()
        .find(|s| s.starts_with("model name"))
        .and_then(|s| s.split_once(':').map(|(_, v)| v.trim()))
    {
        return Ok(model.to_owned());
    }
    let mut fields: Vec<_> = text
        .lines()
        .filter(|s| {
            [
                "Hardware",
                "CPU implementer",
                "CPU architecture",
                "CPU part",
                "CPU revision",
            ]
            .iter()
            .any(|key| s.starts_with(key))
        })
        .map(str::trim)
        .collect();
    fields.sort();
    fields.dedup();
    Ok(fields.join("; "))
}
fn measured(args: &[String], root: &Path, keep: bool) -> Result<(Value, std::process::Output)> {
    let metrics = root.join("time.txt");
    let time = std::env::var("AUDENIQ_GNU_TIME").unwrap_or_else(|_| "/usr/bin/time".to_owned());
    let mut command = strings(&[&time, "-f", "%e %U %S %M", "-o", path(&metrics)]);
    command.extend_from_slice(args);
    let output = capture_stdout(&command, keep)?;
    check(
        output.status.success(),
        &format!(
            "benchmark subprocess: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let values: Vec<f64> = fs::read_to_string(metrics)?
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    check(values.len() == 4, "GNU time fields")?;
    Ok((
        json!({"wall_s":values[0],"user_s":values[1],"system_s":values[2],"cpu_s":values[1]+values[2],"peak_rss_kib":values[3] as u64}),
        output,
    ))
}
fn medians(runs: &BTreeMap<&str, Vec<Value>>) -> Value {
    let mut medians = serde_json::Map::new();
    for (&name, values) in runs {
        let mut row = serde_json::Map::new();
        for key in KEYS {
            let mut ordered: Vec<_> = values.iter().map(|r| r[key].as_f64().unwrap()).collect();
            ordered.sort_by(f64::total_cmp);
            let n = ordered.len();
            let median = if n % 2 == 1 {
                ordered[n / 2]
            } else {
                (ordered[n / 2 - 1] + ordered[n / 2]) / 2.0
            };
            row.insert(key.to_owned(), json!(median));
        }
        medians.insert(name.to_owned(), Value::Object(row));
    }
    Value::Object(medians)
}
fn ratios(medians: &Value) -> Value {
    let mut ratios = serde_json::Map::new();
    for key in ["wall_s", "cpu_s", "peak_rss_kib"] {
        ratios.insert(
            key.to_owned(),
            json!(
                medians["ffmpeg"][key].as_f64().unwrap() / medians["native"][key].as_f64().unwrap()
            ),
        );
    }
    Value::Object(ratios)
}
fn source(root: &Path, seconds: u32) -> Result<std::path::PathBuf> {
    let wav = root.join("master.wav");
    ff(&["-v","error","-f","lavfi","-i",&format!("aevalsrc=0.4*sin(2*PI*997*t)+0.05*sin(2*PI*13001*t)|0.3*sin(2*PI*437*t)+0.04*sin(2*PI*9011*t):s=48000:d={seconds}"),"-c:a","pcm_s24le",path(&wav)])?;
    Ok(wav)
}
fn encoded(wav: &Path, root: &Path, codec: &str, ext: &str) -> Result<std::path::PathBuf> {
    if ext == "wav" {
        return Ok(wav.to_owned());
    }
    let input = root.join(format!("master.{ext}"));
    let mut args = strings(&["ffmpeg", "-v", "error", "-i", path(wav), "-c:a", codec]);
    if ext == "wv" {
        args.extend(strings(&["-bits_per_raw_sample", "24"]));
    }
    args.push(path(&input).to_owned());
    run(&args)?;
    Ok(input)
}
fn date() -> Result<String> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut days = seconds / 86400;
    let mut year = 1970;
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    while days >= if leap(year) { 366 } else { 365 } {
        days -= if leap(year) { 366 } else { 365 };
        year += 1;
    }
    let lengths = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= lengths[month] {
        days -= lengths[month];
        month += 1;
    }
    Ok(format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        month + 1,
        days + 1,
        seconds % 86400 / 3600,
        seconds % 3600 / 60,
        seconds % 60
    ))
}
fn metadata(options: &Options, seconds: u32, results: Vec<Value>) -> Result<Value> {
    let output = ff(&["-version"])?;
    let ffmpeg = std::str::from_utf8(&output.stdout)?
        .lines()
        .next()
        .ok_or("FFmpeg version")?
        .to_owned();
    let mut metadata = json!({"harness":"audeniq-qc-tools Rust","date_utc":date()?,"host_cpu":cpu_identity()?,"arch":std::env::consts::ARCH,"binary_sha256":file_sha(&options.binary)?,"ffmpeg_version":ffmpeg,"seconds":seconds,"sample_rate":48000,"channels":2,"bits":24,"repeats":options.repeats,"qualification":"Synthetic tones, warm page cache, median repeated runs. Applies only to the reported host; not a production corpus or end-to-end AUDENIQ measurement.","results":results});
    if let Some(binary) = &options.baseline_binary {
        metadata["baseline_binary_sha256"] = json!(file_sha(binary)?);
    }
    Ok(metadata)
}
pub fn analysis(options: &Options) -> Result<Value> {
    let temp = Temp::new("benchmark")?;
    let root = &temp.0;
    let seconds = options.seconds.unwrap_or(240);
    let wav = source(root, seconds)?;
    let mut results = Vec::new();
    for (codec, ext) in [
        ("pcm_s24le", "wav"),
        ("flac", "flac"),
        ("pcm_s24be", "aiff"),
        ("alac", "m4a"),
        ("wavpack", "wv"),
        ("tta", "tta"),
    ] {
        let input = encoded(&wav, root, codec, ext)?;
        let mut commands = BTreeMap::from([
            (
                "native",
                strings(&[path(&options.binary), "analyze", path(&input)]),
            ),
            (
                "native_scalar",
                strings(&[path(&options.binary), "analyze", path(&input), "--scalar"]),
            ),
            (
                "ffmpeg",
                strings(&[
                    "ffmpeg",
                    "-nostdin",
                    "-hide_banner",
                    "-nostats",
                    "-threads",
                    "1",
                    "-i",
                    path(&input),
                    "-map",
                    "0:a:0",
                    "-af",
                    "ebur128=peak=true:framelog=quiet",
                    "-f",
                    "null",
                    "-",
                    "-map",
                    "0:a:0",
                    "-c:a",
                    "pcm_s32le",
                    "-f",
                    "hash",
                    "-hash",
                    "sha256",
                    "-",
                ]),
            ),
        ]);
        if options.fingerprint {
            for name in ["native", "native_scalar"] {
                commands
                    .get_mut(name)
                    .unwrap()
                    .push("--fingerprint".to_owned());
            }
            commands.get_mut("ffmpeg").unwrap().extend(strings(&[
                "-map",
                "0:a:0",
                "-ac",
                "1",
                "-ar",
                "11025",
                "-c:a",
                "pcm_s16le",
                "-f",
                "s16le",
                "-",
            ]));
        }
        if let Some(binary) = &options.baseline_binary {
            let mut baseline = commands["native"].clone();
            baseline[0] = path(binary).to_owned();
            commands.insert("baseline", baseline);
        }
        for command in commands.values() {
            run(command)?;
        }
        let order: Vec<_> = commands.keys().copied().collect();
        let mut runs: BTreeMap<_, _> = order.iter().map(|&name| (name, Vec::new())).collect();
        for rep in 0..options.repeats {
            for i in 0..order.len() {
                let name = order[(i + rep) % order.len()];
                let (metrics, _) = measured(&commands[name], root, false)?;
                runs.get_mut(name).unwrap().push(metrics);
            }
        }
        let median = medians(&runs);
        let ratio = ratios(&median);
        eprintln!("{codec}: {}", serde_json::to_string(&median)?);
        results.push(json!({"codec":codec,"file_bytes":fs::metadata(&input)?.len(),"fixture_sha256":file_sha(&input)?,"commands":commands,"runs":runs,"median":median,"ffmpeg_div_native":ratio}));
    }
    let mut report = metadata(options, seconds, results)?;
    report["fingerprint"] = json!(options.fingerprint);
    report["comparison"]=json!("Both decode once and measure LUFS/true peak plus left-aligned s32le SHA256. Native additionally computes QC metrics. FFmpeg baseline excludes FFprobe and backend meter overhead. True-peak filters differ. With --fingerprint both downmix/resample; native retains <=90s windows and emits JSON, FFmpeg emits a continuous raw tap discarded here without backend retention cost. Resamplers are versioned and not bit-identical.");
    Ok(report)
}
pub fn convert(options: &Options) -> Result<Value> {
    let temp = Temp::new("benchmark-convert")?;
    let root = &temp.0;
    let seconds = options.seconds.unwrap_or(60);
    let wav = source(root, seconds)?;
    let expected = oracle_hash(&wav)?;
    let mut results = Vec::new();
    for (codec, ext) in [
        ("pcm_s24le", "wav"),
        ("flac", "flac"),
        ("pcm_s24be", "aiff"),
        ("alac", "m4a"),
        ("wavpack", "wv"),
        ("tta", "tta"),
    ] {
        let input = encoded(&wav, root, codec, ext)?;
        if options.review && !matches!(ext, "wav" | "m4a") {
            continue;
        }
        let mut outputs = BTreeMap::from([
            ("native", root.join("native.flac")),
            ("ffmpeg", root.join("ffmpeg.flac")),
        ]);
        let mut commands = BTreeMap::from([
            (
                "native",
                strings(&[
                    path(&options.binary),
                    "convert",
                    path(&input),
                    path(&outputs["native"]),
                ]),
            ),
            (
                "ffmpeg",
                strings(&[
                    "ffmpeg",
                    "-nostdin",
                    "-v",
                    "error",
                    "-xerror",
                    "-threads",
                    "1",
                    "-i",
                    path(&input),
                    "-map",
                    "0:a:0",
                    "-map_metadata",
                    "-1",
                    "-c:a",
                    "flac",
                    "-threads",
                    "1",
                    "-compression_level",
                    "5",
                    path(&outputs["ffmpeg"]),
                    "-map",
                    "0:a:0",
                    "-c:a",
                    "pcm_s32le",
                    "-f",
                    "hash",
                    "-hash",
                    "sha256",
                    "-",
                ]),
            ),
        ]);
        if let Some(binary) = &options.baseline_binary {
            outputs.insert("baseline", root.join("baseline.flac"));
            commands.insert(
                "baseline",
                strings(&[
                    path(binary),
                    "convert",
                    path(&input),
                    path(&outputs["baseline"]),
                ]),
            );
        }
        if options.review {
            commands
                .get_mut("ffmpeg")
                .unwrap()
                .insert(1, "-y".to_owned());
            commands
                .get_mut("native")
                .unwrap()
                .push("--analyze".to_owned());
            if options.fingerprint {
                commands
                    .get_mut("native")
                    .unwrap()
                    .push("--fingerprint".to_owned());
            }
            commands.get_mut("ffmpeg").unwrap().extend(strings(&[
                "-map",
                "0:a:0",
                "-af",
                "ebur128=peak=true:framelog=quiet",
                "-f",
                "null",
                "-",
            ]));
            if options.fingerprint {
                commands.get_mut("ffmpeg").unwrap().extend(strings(&[
                    "-map",
                    "0:a:0",
                    "-ac",
                    "1",
                    "-ar",
                    "11025",
                    "-c:a",
                    "pcm_s16le",
                    "-f",
                    "s16le",
                    "/dev/null",
                ]));
            }
        }
        let order: Vec<_> = commands.keys().copied().collect();
        let mut runs: BTreeMap<_, _> = order.iter().map(|&name| (name, Vec::new())).collect();
        let mut sizes = BTreeMap::new();
        for rep in 0..=options.repeats {
            let mut qc = BTreeMap::new();
            for i in 0..order.len() {
                let name = order[(i + rep) % order.len()];
                let output = &outputs[name];
                if output.exists() {
                    fs::remove_file(output)?;
                }
                let (mut metrics, out) = measured(&commands[name], root, true)?;
                sizes.insert(name, fs::metadata(output)?.len());
                if name != "ffmpeg" {
                    let report: Value = serde_json::from_slice(&out.stdout)?;
                    check(report["pcm_sha256"] == expected, "native conversion hash")?;
                    check(
                        oracle_hash(output)? == expected,
                        "independent native output hash",
                    )?;
                    if options.review {
                        let analysis = if name == "native" {
                            report["analysis"].clone()
                        } else {
                            let mut command = strings(&[
                                path(options.baseline_binary.as_ref().unwrap()),
                                "analyze",
                                path(output),
                            ]);
                            if options.fingerprint {
                                command.push("--fingerprint".to_owned());
                            }
                            let (analysis_metrics, out) = measured(&command, root, true)?;
                            combine_metrics(&mut metrics, &analysis_metrics);
                            serde_json::from_slice(&out.stdout)?
                        };
                        check(analysis["pcm_sha256"] == expected, "review QC hash")?;
                        let mut comparable = analysis;
                        for field in ["spec", "engine", "backend"] {
                            comparable
                                .as_object_mut()
                                .ok_or("review JSON")?
                                .remove(field);
                        }
                        qc.insert(name, comparable);
                    }
                } else {
                    check(hash_output(&out.stdout)? == expected, "FFmpeg source hash")?;
                    let (verify, out) = measured(&oracle_command(output), root, true)?;
                    check(
                        hash_output(&out.stdout)? == expected,
                        "FFmpeg verification hash",
                    )?;
                    combine_metrics(&mut metrics, &verify);
                }
                if rep > 0 {
                    runs.get_mut(name).unwrap().push(metrics);
                }
            }
            if let Some(baseline) = qc.get("baseline") {
                check(
                    baseline == &qc["native"],
                    "review baseline/current QC equality",
                )?;
            }
        }
        let median = medians(&runs);
        let ratio = ratios(&median);
        eprintln!("{codec}: {}", serde_json::to_string(&median)?);
        results.push(json!({"codec":codec,"source_bytes":fs::metadata(&input)?.len(),"fixture_sha256":file_sha(&input)?,"pcm_sha256":expected,"commands":commands,"ffmpeg_verification_command":oracle_command(&outputs["ffmpeg"]),"output_bytes":sizes,"runs":runs,"median":median,"ffmpeg_div_native":ratio}));
    }
    let mut report = metadata(options, seconds, results)?;
    report["review"] = json!(options.review);
    report["fingerprint"] = json!(options.fingerprint && options.review);
    report["comparison"]=json!("Both source decode/hash/FLAC encode and output decode/hash are timed. Native also includes fsync and no-clobber publication. FFmpeg excludes separate probe and backend overhead. Adaptive LPC/Rice or verified FLAC frame-copy can produce different sizes; sizes are reported. FFmpeg verification is a separate subprocess: sum CPU/wall, maximum child RSS. Native independent FFmpeg verification is outside timing.");
    Ok(report)
}

fn combine_metrics(total: &mut Value, part: &Value) {
    for key in ["wall_s", "user_s", "system_s", "cpu_s"] {
        total[key] = json!(total[key].as_f64().unwrap() + part[key].as_f64().unwrap());
    }
    total["peak_rss_kib"] = json!(total["peak_rss_kib"]
        .as_u64()
        .unwrap()
        .max(part["peak_rss_kib"].as_u64().unwrap()));
}
