use audeniq_qc::{
    audio::{pcm_sha256, AudioReader},
    flac,
    kernels::Backend,
    meter, Error, Limits,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
static COUNTER: AtomicU64 = AtomicU64::new(0);
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let d = std::env::temp_dir().join(format!(
            "audeniq-qc-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&d).unwrap();
        Self(d)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn ffmpeg(src: Option<&Path>, dst: &Path, codec: &str) {
    let mut c = Command::new("ffmpeg");
    c.args(["-v", "error", "-nostdin"]);
    if let Some(src) = src {
        c.arg("-i").arg(src);
    } else {
        c.args([
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.8*sin(2*PI*997*t)|0.2*sin(2*PI*439*t):s=48000:d=1.23",
        ]);
    }
    c.args(["-c:a", codec]);
    if codec == "wavpack" {
        c.args(["-bits_per_raw_sample", "24"]);
    }
    assert!(c
        .arg(dst)
        .status()
        .expect("FFmpeg oracle must be installed for tests")
        .success());
}
#[test]
fn codec_hashes_and_verified_conversion_match_ffmpeg() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    ffmpeg(None, &src, "pcm_s24le");
    let (_, expected, frames) = pcm_sha256(&src, Limits::default(), Backend::Scalar).unwrap();
    for (ext, codec) in [
        ("flac", "flac"),
        ("m4a", "alac"),
        ("tta", "tta"),
        ("wv", "wavpack"),
        ("aiff", "pcm_s24be"),
    ] {
        let p = d.0.join(format!("source.{ext}"));
        ffmpeg(Some(&src), &p, codec);
        let (_, hash, count) = pcm_sha256(&p, Limits::default(), Backend::detect()).unwrap();
        assert_eq!(hash, expected, "{ext}");
        assert_eq!(count, frames);
        let out = d.0.join(format!("{ext}.out.flac"));
        let result = flac::convert(&p, &out, Limits::default(), Backend::detect()).unwrap();
        assert_eq!(result.pcm_sha256, expected);
        let reference = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-c:a", "pcm_s32le", "-f", "hash", "-hash", "sha256", "-"])
            .output()
            .unwrap();
        assert!(reference.status.success());
        assert!(String::from_utf8(reference.stdout)
            .unwrap()
            .contains(&expected));
    }
}
/// TTA and WavPack in their encoder modes (WavPack's higher modes use more
/// and other decorrelation terms, including the cross-channel ones), mono
/// and stereo, 16 and 24 bits, on tone and noise: every decode equals the
/// source PCM.
#[test]
fn tta_and_wavpack_modes_decode_to_the_source() {
    let d = Dir::new();
    for (name, filter, format) in [
        ("tone16", "aevalsrc=0.7*sin(2*PI*997*t)|0.3*sin(2*PI*441*t):s=44100:d=0.9", "pcm_s16le"),
        ("noise16", "anoisesrc=d=0.9:c=pink:r=44100:a=0.5", "pcm_s16le"),
        ("tone24", "aevalsrc=0.6*sin(2*PI*1999*t)|0.6*sin(2*PI*31*t):s=96000:d=0.4", "pcm_s24le"),
    ] {
        for channels in ["1", "2"] {
            let src = d.0.join(format!("{name}-{channels}.wav"));
            assert!(Command::new("ffmpeg")
                .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i", filter, "-ac", channels])
                .args(["-c:a", format])
                .arg(&src)
                .status()
                .expect("FFmpeg oracle must be installed for tests")
                .success());
            let (_, expected, frames) =
                pcm_sha256(&src, Limits::default(), Backend::Scalar).unwrap();
            let mut variants: Vec<(&str, Vec<&str>)> = vec![("tta", vec!["-c:a", "tta"])];
            for level in ["0", "1", "2", "3"] {
                variants.push(("wv", vec!["-c:a", "wavpack", "-compression_level", level]));
            }
            for (i, (ext, args)) in variants.iter().enumerate() {
                let p = d.0.join(format!("{name}-{channels}-{i}.{ext}"));
                let mut c = Command::new("ffmpeg");
                c.args(["-v", "error", "-nostdin", "-i"]).arg(&src).args(args);
                if *ext == "wv" && format == "pcm_s24le" {
                    c.args(["-bits_per_raw_sample", "24"]);
                }
                assert!(c.arg(&p).status().unwrap().success());
                for backend in [Backend::Scalar, Backend::detect()] {
                    let (_, hash, count) = pcm_sha256(&p, Limits::default(), backend)
                        .unwrap_or_else(|e| panic!("{name}-{channels}-{i}.{ext}: {e}"));
                    assert_eq!(hash, expected, "{name}-{channels}-{i}.{ext}");
                    assert_eq!(count, frames);
                }
            }
        }
    }
}
#[test]
fn truncation_never_publishes_output() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    ffmpeg(None, &src, "pcm_s24le");
    let bytes = std::fs::read(&src).unwrap();
    std::fs::write(&src, &bytes[..bytes.len() * 4 / 5]).unwrap();
    let dst = d.0.join("output.flac");
    assert!(flac::convert(&src, &dst, Limits::default(), Backend::detect()).is_err());
    assert!(!dst.exists());
    assert_eq!(std::fs::read_dir(&d.0).unwrap().count(), 1);
}

#[test]
fn flac_md5_validation_is_preserved_with_compact_pcm_packing() {
    let d = Dir::new();
    let wav = d.0.join("source.wav");
    let flac = d.0.join("source.flac");
    ffmpeg(None, &wav, "pcm_s24le");
    ffmpeg(Some(&wav), &flac, "flac");
    let (_, expected, _) = pcm_sha256(&wav, Limits::default(), Backend::Scalar).unwrap();
    assert_eq!(
        pcm_sha256(&flac, Limits::default(), Backend::detect())
            .unwrap()
            .1,
        expected
    );
    let original = std::fs::read(&flac).unwrap();
    assert_eq!(&original[..4], b"fLaC");
    let mut bad = original.clone();
    bad[26] ^= 1; // STREAMINFO's PCM MD5, not a packet CRC.
    std::fs::write(&flac, bad).unwrap();
    assert!(matches!(
        pcm_sha256(&flac, Limits::default(), Backend::detect()),
        Err(Error::Invalid("FLAC MD5 mismatch"))
    ));
    assert!(meter::analyze(&flac, Limits::default(), Backend::detect(), false).is_err());
    let dst = d.0.join("output.flac");
    assert!(flac::convert(&flac, &dst, Limits::default(), Backend::detect()).is_err());
    assert!(!dst.exists());
    let mut unknown = original;
    unknown[26..42].fill(0); // FLAC permits an absent STREAMINFO MD5.
    std::fs::write(&flac, unknown).unwrap();
    assert_eq!(
        pcm_sha256(&flac, Limits::default(), Backend::detect())
            .unwrap()
            .1,
        expected
    );
    assert_eq!(
        flac::convert(&flac, &dst, Limits::default(), Backend::detect())
            .unwrap()
            .pcm_sha256,
        expected
    );
}
#[test]
fn deadline_and_output_no_clobber() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    ffmpeg(None, &src, "pcm_s24le");
    let limits = Limits {
        deadline: Instant::now(),
        ..Default::default()
    };
    assert!(matches!(
        AudioReader::open(&src, limits),
        Err(Error::Deadline)
    ));
    let dst = d.0.join("exists.flac");
    std::fs::write(&dst, b"preserve me").unwrap();
    assert!(flac::convert(&src, &dst, Limits::default(), Backend::Scalar).is_err());
    assert_eq!(std::fs::read(dst).unwrap(), b"preserve me");
}

#[test]
fn compression_controls_and_pcm_outputs_preserve_samples() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    ffmpeg(None, &src, "pcm_s24le");
    let (_, expected, frames) = pcm_sha256(&src, Limits::default(), Backend::Scalar).unwrap();
    for level in 0..=8 {
        let dst = d.0.join(format!("level{level}.flac"));
        let result = flac::convert_with_level(
            &src,
            &dst,
            Limits::default(),
            Backend::detect(),
            Some(level),
        )
        .unwrap();
        assert_eq!(result.compression_level, Some(level));
        assert_eq!(result.pcm_sha256, expected);
        assert_eq!(result.frames, frames);
    }
    let dst = d.0.join("invalid.flac");
    assert!(
        flac::convert_with_level(&src, &dst, Limits::default(), Backend::detect(), Some(9))
            .is_err()
    );
    assert!(!dst.exists());
    for format in [audeniq_qc::pcm::Format::Wav, audeniq_qc::pcm::Format::S32le] {
        let dst = d.0.join(format!("{format:?}.pcm"));
        let result =
            audeniq_qc::pcm::decode(&src, &dst, format, Limits::default(), Backend::detect())
                .unwrap();
        assert_eq!(result.pcm_sha256, expected);
        assert_eq!(result.source_spec.bits_per_sample, 24);
        assert_eq!(
            result.spec.bits_per_sample,
            if format == audeniq_qc::pcm::Format::Wav {
                24
            } else {
                32
            }
        );
        assert!(
            audeniq_qc::pcm::decode(&src, &dst, format, Limits::default(), Backend::detect())
                .is_err()
        );
        let original = std::fs::read(&src).unwrap();
        let bad = d.0.join("bad.wav");
        std::fs::write(&bad, &original[..original.len() / 2]).unwrap();
        let output = d.0.join(format!("bad-{format:?}.pcm"));
        assert!(audeniq_qc::pcm::decode(
            &bad,
            &output,
            format,
            Limits::default(),
            Backend::detect()
        )
        .is_err());
        assert!(!output.exists());
    }
}
#[test]
fn silent_loudness_is_null_and_metrics_are_reproducible() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    assert!(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-t",
            "1",
            "-c:a",
            "pcm_s24le"
        ])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let a = meter::analyze(&src, Limits::default(), Backend::detect(), true).unwrap();
    assert!(a.integrated_lufs.is_none());
    assert!(a.true_peak_dbtp.is_none());
    assert_eq!(a.silent_blocks, 20);
    assert_eq!(a.longest_silent_run, 20);
    assert_eq!(a.clip_events, 0);
    assert_eq!(a.fingerprint_windows.unwrap()[0].samples.len(), 11025);
}
#[test]
fn forged_headers_return_errors_without_panics() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    ffmpeg(None, &src, "pcm_s24le");
    let bytes = std::fs::read(&src).unwrap();
    for offset in [4, 16, 20, 22, 24, 28, 32, 34, 40, 44, 48] {
        let mut b = bytes.clone();
        b[offset..offset + 4].fill(255);
        let p = d.0.join(format!("mutated-{offset}.wav"));
        std::fs::write(&p, b).unwrap();
        let result =
            std::panic::catch_unwind(|| pcm_sha256(&p, Limits::default(), Backend::detect()));
        assert!(result.is_ok(), "panic at {offset}");
    }
}

#[test]
fn standalone_fingerprint_matches_fused_tap() {
    let d = Dir::new();
    let src = d.0.join("source.wav");
    ffmpeg(None, &src, "pcm_s24le");
    let fused = meter::analyze(&src, Limits::default(), Backend::detect(), true).unwrap();
    let fallback =
        audeniq_qc::resample::fingerprint(&src, Limits::default(), Backend::detect()).unwrap();
    assert_eq!(
        audeniq_qc::json::to_value(&fused.fingerprint_windows.unwrap()),
        audeniq_qc::json::to_value(&fallback.fingerprint_windows)
    );
}

#[test]
fn fused_conversion_qc_and_fingerprint_match_separate_analysis() {
    let d = Dir::new();
    let wav = d.0.join("source.wav");
    ffmpeg(None, &wav, "pcm_s24le");
    for (ext, codec) in [("wav", "pcm_s24le"), ("m4a", "alac"), ("flac", "flac")] {
        let input = d.0.join(format!("input.{ext}"));
        ffmpeg(Some(&wav), &input, codec);
        for backend in [Backend::Scalar, Backend::detect()] {
            let analysis = meter::analyze(&input, Limits::default(), backend, true).unwrap();
            let output = d.0.join(format!("{ext}-{backend:?}.flac"));
            let conversion = flac::convert_with_options(
                &input,
                &output,
                Limits::default(),
                backend,
                flac::ConvertOptions {
                    analyze: true,
                    fingerprint: true,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                audeniq_qc::json::to_value(&analysis),
                audeniq_qc::json::to_value(&conversion.analysis.unwrap())
            );
            assert_eq!(conversion.pcm_sha256, analysis.pcm_sha256);
            assert_eq!(
                pcm_sha256(&output, Limits::default(), backend).unwrap().1,
                analysis.pcm_sha256
            );
        }
        let mut corrupt = std::fs::read(&input).unwrap();
        corrupt.truncate(corrupt.len() / 2);
        let bad = d.0.join(format!("bad.{ext}"));
        std::fs::write(&bad, corrupt).unwrap();
        let output = d.0.join(format!("bad-{ext}.flac"));
        assert!(flac::convert_with_options(
            &bad,
            &output,
            Limits::default(),
            Backend::detect(),
            flac::ConvertOptions {
                analyze: true,
                fingerprint: true,
                ..Default::default()
            }
        )
        .is_err());
        assert!(!output.exists());
    }
    let output = d.0.join("invalid.flac");
    assert!(flac::convert_with_options(
        &wav,
        &output,
        Limits::default(),
        Backend::detect(),
        flac::ConvertOptions {
            fingerprint: true,
            ..Default::default()
        }
    )
    .is_err());
    assert!(!output.exists());
}
#[test]
fn encoder_models_stereo_modes_and_wasted_bits_round_trip() {
    // Independent WAV writer: these fixtures exercise wasted bits (16-bit
    // content in 24-bit containers), every stereo assignment (identical,
    // antiphase, one silent channel, independent), mono, tiny/odd tails,
    // full-scale extremes and noise, at every compression level.
    fn wav(path: &Path, channels: u16, depth: u16, samples: &[i32]) {
        let bytes = (depth / 8) as usize;
        let data = samples.len() * bytes;
        let mut out = Vec::with_capacity(44 + data);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data as u32 + (data as u32 & 1)).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&48000u32.to_le_bytes());
        out.extend_from_slice(&(48000 * channels as u32 * bytes as u32).to_le_bytes());
        out.extend_from_slice(&(channels * bytes as u16).to_le_bytes());
        out.extend_from_slice(&depth.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data as u32).to_le_bytes());
        for &s in samples {
            out.extend_from_slice(&s.to_le_bytes()[..bytes]);
        }
        if data % 2 == 1 {
            out.push(0);
        }
        std::fs::write(path, out).unwrap();
    }
    let d = Dir::new();
    let mut seed = 1729u64;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as i32
    };
    for (name, channels, depth, frames) in [
        ("identical", 2u16, 24u16, 9001usize),
        ("antiphase", 2, 24, 4609),
        ("left-silent", 2, 16, 4608),
        ("independent", 2, 24, 13),
        ("wasted", 2, 24, 7000),
        ("mono-wasted", 1, 24, 5000),
        ("extremes", 2, 16, 4700),
        ("noise", 1, 24, 3),
    ] {
        let max = (1i32 << (depth - 1)) - 1;
        let mut samples = Vec::with_capacity(frames * channels as usize);
        for i in 0..frames {
            let tone = ((i as f64 * 0.031).sin() * max as f64 * 0.6) as i32;
            let row: [i32; 2] = match name {
                "identical" => [tone, tone],
                "antiphase" => [tone, -tone],
                "left-silent" => [0, tone],
                "wasted" | "mono-wasted" => [tone & !0xff, (tone / 3) & !0xff],
                "extremes" => [if i % 2 == 0 { max } else { -max - 1 }, -max - 1],
                _ => [noise() >> (33 - depth), noise() >> (33 - depth)],
            };
            samples.extend_from_slice(&row[..channels as usize]);
        }
        let src = d.0.join(format!("{name}.wav"));
        wav(&src, channels, depth, &samples);
        let (_, expected, count) = pcm_sha256(&src, Limits::default(), Backend::Scalar).unwrap();
        for level in 0..=8 {
            for backend in [Backend::Scalar, Backend::detect()] {
                let out = d.0.join(format!("{name}-{level}-{backend:?}.flac"));
                let result =
                    flac::convert_with_level(&src, &out, Limits::default(), backend, Some(level))
                        .unwrap();
                assert_eq!(result.pcm_sha256, expected, "{name} level {level}");
                let (_, hash, n) = pcm_sha256(&out, Limits::default(), backend).unwrap();
                assert_eq!((hash, n), (expected.clone(), count), "{name} {level}");
            }
        }
    }
}

/// FLAC re-encoding reuses the verified source STREAMINFO MD5 when there is
/// one and computes it otherwise; both must equal the MD5 of the compact
/// PCM, and a tampered source MD5 must still fail before publication.
#[test]
fn reencoded_flac_streaminfo_md5_is_correct_for_known_unknown_and_bad_source_md5() {
    let d = Dir::new();
    for codec in ["pcm_s16le", "pcm_s24le"] {
        let wav = d.0.join(format!("{codec}.wav"));
        let flac = d.0.join(format!("{codec}.flac"));
        ffmpeg(None, &wav, codec);
        ffmpeg(Some(&wav), &flac, "flac");
        let md5 = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&wav)
            .args(["-c:a", codec, "-f", "md5", "-"])
            .output()
            .unwrap();
        assert!(md5.status.success());
        let expected = String::from_utf8(md5.stdout).unwrap();
        let expected = expected.trim().strip_prefix("MD5=").unwrap().to_owned();
        let original = std::fs::read(&flac).unwrap();
        for variant in ["known", "unknown", "bad"] {
            let mut bytes = original.clone();
            match variant {
                "unknown" => bytes[26..42].fill(0),
                "bad" => bytes[30] ^= 4,
                _ => (),
            }
            let src = d.0.join(format!("{codec}-{variant}.flac"));
            std::fs::write(&src, bytes).unwrap();
            let dst = d.0.join(format!("{codec}-{variant}.out.flac"));
            let result =
                flac::convert_with_level(&src, &dst, Limits::default(), Backend::detect(), Some(5));
            if variant == "bad" {
                assert!(matches!(result, Err(Error::Invalid("FLAC MD5 mismatch"))));
                assert!(!dst.exists());
                continue;
            }
            result.unwrap();
            let out = std::fs::read(&dst).unwrap();
            let written: String = out[26..42].iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(written, expected, "{codec} {variant}");
        }
    }
}
