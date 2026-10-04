use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
pub struct Options {
    pub binary: PathBuf,
    pub output: Option<PathBuf>,
    pub seconds: Option<u32>,
    pub repeats: usize,
    pub fingerprint: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            binary: "target/release/audeniq-qc".into(),
            output: None,
            seconds: None,
            repeats: 3,
            fingerprint: false,
        }
    }
}
pub fn check(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}
pub fn path(path: &Path) -> &str {
    path.to_str().expect("UTF-8 development path")
}
pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

pub fn capture(args: &[String]) -> Result<Output> {
    capture_stdout(args, true)
}
pub fn capture_stdout(args: &[String], keep: bool) -> Result<Output> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdout(if keep { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take().ok_or("stderr pipe")?;
    // Drain both pipes while enforcing a subprocess deadline; a fingerprint
    // JSON may exceed the pipe capacity. The oracle is development-only.
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(mut stdout) = stdout {
            stdout.read_to_end(&mut b)?;
        }
        Ok::<_, std::io::Error>(b)
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err(format!("subprocess deadline: {}", args[0]).into());
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    Ok(Output {
        status,
        stdout: out.join().map_err(|_| "stdout thread")??,
        stderr: err.join().map_err(|_| "stderr thread")??,
    })
}
pub fn run(args: &[String]) -> Result<Output> {
    let output = capture(args)?;
    check(
        output.status.success(),
        &format!("{args:?}: {}", String::from_utf8_lossy(&output.stderr)),
    )?;
    Ok(output)
}
pub fn ff(items: &[&str]) -> Result<Output> {
    let mut args = vec!["ffmpeg".to_owned()];
    args.extend(strings(items));
    run(&args)
}
pub fn native(binary: &Path, command: &str, paths: &[&Path], flags: &[&str]) -> Result<Value> {
    let mut args = strings(&[path(binary), command]);
    args.extend(paths.iter().map(|p| path(p).to_owned()));
    args.extend(strings(flags));
    Ok(serde_json::from_slice(&run(&args)?.stdout)?)
}
pub fn rejected(binary: &Path, command: &str, paths: &[&Path]) -> Result<()> {
    let mut args = strings(&[path(binary), command]);
    args.extend(paths.iter().map(|p| path(p).to_owned()));
    let out = capture(&args)?;
    check(out.status.code() == Some(2), "rejection must exit 2")?;
    let error: Value = serde_json::from_slice(&out.stderr)?;
    check(
        matches!(
            error["error"].as_str(),
            Some("INVALID_INPUT" | "UNSUPPORTED" | "RESOURCE_LIMIT" | "IO" | "DEADLINE")
        ),
        "typed rejection",
    )
}
pub fn hash_output(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes)?.trim();
    let hash = text.strip_prefix("SHA256=").ok_or("oracle hash format")?;
    check(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "oracle hash length",
    )?;
    Ok(hash.to_owned())
}
pub fn oracle_command(input: &Path) -> Vec<String> {
    strings(&[
        "ffmpeg",
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-threads",
        "1",
        "-i",
        path(input),
        "-map",
        "0:a:0",
        "-c:a",
        "pcm_s32le",
        "-f",
        "hash",
        "-hash",
        "sha256",
        "-",
    ])
}
pub fn oracle_hash(input: &Path) -> Result<String> {
    hash_output(&run(&oracle_command(input))?.stdout)
}
pub fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn file_sha(input: &Path) -> Result<String> {
    let mut file = std::fs::File::open(input)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
static COUNTER: AtomicU64 = AtomicU64::new(0);
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new(label: &str) -> Result<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let p = std::env::temp_dir().join(format!(
            "audeniq-{label}-{}-{stamp}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self(p))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn write_wave(
    input: &Path,
    rate: u32,
    depth: u16,
    channels: u16,
    samples: &[i32],
) -> Result<()> {
    check(
        samples.len().is_multiple_of(channels as usize),
        "wave channel alignment",
    )?;
    let bytes = (samples.len() * (depth / 8) as usize) as u32;
    let mut file = std::io::BufWriter::new(std::fs::File::create(input)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + bytes + bytes % 2).to_le_bytes())?;
    file.write_all(b"WAVEfmt \x10\x00\x00\x00\x01\x00")?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&rate.to_le_bytes())?;
    let align = channels * (depth / 8);
    file.write_all(&(rate * align as u32).to_le_bytes())?;
    file.write_all(&align.to_le_bytes())?;
    file.write_all(&depth.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&bytes.to_le_bytes())?;
    for value in samples {
        file.write_all(&value.to_le_bytes()[..(depth / 8) as usize])?;
    }
    if !bytes.is_multiple_of(2) {
        file.write_all(&[0])?;
    }
    file.flush()?;
    Ok(())
}
/// Stable test generator, independent of the media decoders. Seeded SplitMix64.
pub struct Random(u64);
impl Random {
    pub fn new() -> Self {
        Self(1729)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
}
pub fn fixture(
    input: &Path,
    rate: u32,
    depth: u16,
    channels: u16,
    kind: &str,
    duration: f64,
) -> Result<()> {
    let n = (rate as f64 * duration).round() as usize;
    let scale = 1i32 << (depth - 1);
    let mut rng = Random::new();
    let mut samples = Vec::with_capacity(n * channels as usize);
    for i in 0..n {
        let t = i as f64 / rate as f64;
        for ch in 0..channels {
            let value = match kind {
                "silence" => 0,
                "noise" => (rng.next() % (2 * scale) as u64) as i32 - scale,
                "clip" => {
                    if i % 11 < 7 {
                        if (i / 11) % 2 != 0 {
                            scale - 1
                        } else {
                            -scale
                        }
                    } else {
                        0
                    }
                }
                "gated" => {
                    if !(0.4..=1.7).contains(&t) {
                        0
                    } else {
                        (scale as f64
                            * 0.4
                            * (std::f64::consts::TAU * (997 + ch as u32 * 337) as f64 * t).sin())
                        .round() as i32
                    }
                }
                "near_nyquist" => (scale as f64
                    * 0.9
                    * 1.0f64.min(t / 0.01).min((duration - t) / 0.01)
                    * (std::f64::consts::TAU * rate as f64 * 0.24 * t
                        + std::f64::consts::FRAC_PI_4)
                        .sin())
                .round() as i32,
                _ => (scale as f64
                    * (0.4 * (std::f64::consts::TAU * (997 + ch as u32 * 337) as f64 * t).sin()
                        + 0.06 * (std::f64::consts::TAU * 13001.0 * t).sin()))
                .round() as i32,
            };
            samples.push(value.clamp(-scale, scale - 1));
        }
    }
    write_wave(input, rate, depth, channels, &samples)
}
