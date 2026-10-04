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
        serde_json::to_value(fused.fingerprint_windows.unwrap()).unwrap(),
        serde_json::to_value(fallback.fingerprint_windows).unwrap()
    );
}
