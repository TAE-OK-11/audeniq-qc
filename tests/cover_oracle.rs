//! The in-repository JPEG/PNG cover validation accepts and rejects exactly
//! what the previous decoder (image 0.24.9 with the same limits) did, and
//! reports the same size: compared on every fixture and on deterministic
//! mutations of them. Where the previous decoder panicked (process abort in
//! release builds), the validation must reject.
use std::path::{Path, PathBuf};

/// Some((width, height)), None for an error, Err(()) for a panic.
fn reference(path: &Path) -> Result<Option<(u32, u32)>, ()> {
    let path = path.to_owned();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(move || {
        let mut reader = image_reference::io::Reader::open(&path)
            .ok()?
            .with_guessed_format()
            .ok()?;
        let format = reader.format()?;
        if !matches!(
            format,
            image_reference::ImageFormat::Jpeg | image_reference::ImageFormat::Png
        ) {
            return None;
        }
        let mut limits = image_reference::io::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().ok()?;
        Some((image.width(), image.height()))
    });
    std::panic::set_hook(previous);
    result.map_err(|_| ())
}

fn native(path: &Path) -> Option<(u32, u32)> {
    let report = audeniq_qc::probe::cover(path, audeniq_qc::Limits::default()).ok()?;
    let stream = &report["streams"][0];
    Some((
        stream["width"].as_u64()? as u32,
        stream["height"].as_u64()? as u32,
    ))
}

fn fixtures() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir("tests/fixtures/covers")
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
}

fn compare(path: &Path, label: &str, counts: &mut [usize; 3]) {
    match reference(path) {
        Ok(expected) => {
            assert_eq!(native(path), expected, "{label}");
            counts[expected.is_some() as usize] += 1;
        }
        Err(()) => {
            assert_eq!(native(path), None, "{label}: reference panicked");
            counts[2] += 1;
        }
    }
}

#[test]
fn fixtures_and_mutations_match_the_previous_decoder() {
    let dir = std::env::temp_dir().join(format!("audeniq-cover-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // [rejected, accepted, reference panicked]
    let mut counts = [0usize; 3];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for file in fixtures() {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        compare(&file, &name, &mut counts);
        let data = std::fs::read(&file).unwrap();
        let ext = file.extension().unwrap().to_string_lossy().into_owned();
        for k in 0..60 {
            let mut d = data.clone();
            let at = |r: u64, len: usize| (r % len.max(1) as u64) as usize;
            match next() % 5 {
                0 => {
                    for _ in 0..1 + next() % 3 {
                        let i = at(next(), d.len());
                        d[i] ^= 1 << (next() % 8);
                    }
                }
                1 => d.truncate(at(next(), d.len())),
                2 => {
                    let i = at(next(), d.len());
                    d[i] = [0, 0xff, 0xd9, 0xd0, 0xda, 0xc4][(next() % 6) as usize];
                }
                3 => {
                    let i = at(next(), d.len());
                    let n = (1 + next() % 16) as usize;
                    d.drain(i..(i + n).min(d.len()));
                }
                _ => {
                    let i = at(next(), d.len());
                    let v = next() as u8;
                    d.insert(i, v);
                }
            }
            let path = dir.join(format!("m.{ext}"));
            std::fs::write(&path, &d).unwrap();
            compare(&path, &format!("{name} mutation {k}"), &mut counts);
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    // The mutations must exercise both outcomes.
    assert!(counts[0] > 500 && counts[1] > 500, "{counts:?}");
}
