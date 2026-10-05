#![allow(dead_code)]
#[path = "crc32_copy.rs"]
mod crc32;
#[cfg(target_arch = "x86_64")]
mod ref_kernels;
use crc32::{Crc32, Engine};
use std::time::Instant;

fn ours(e: Engine, d: &[u8]) -> u32 {
    let mut c = Crc32::with_engine(e);
    c.update(d);
    c.finalize()
}

fn main() {
    let mut cands: Vec<(String, Box<dyn Fn(&[u8]) -> u32>)> =
        vec![("crc32fast".into(), Box::new(|d: &[u8]| crc32fast::hash(d)))];
    #[cfg(target_arch = "x86_64")]
    {
        cands.push(("ref-sse".into(), Box::new(|d: &[u8]| unsafe { ref_kernels::calculate(0, d) })));
        if std::is_x86_feature_detected!("vpclmulqdq") && std::is_x86_feature_detected!("avx2") {
            cands.push(("ref-avx2".into(), Box::new(|d: &[u8]| unsafe { ref_kernels::calculate_avx2(0, d) })));
        }
    }
    println!("detected engine: {:?}", Engine::detect());
    let mut engines = vec![Engine::Portable, Engine::detect()];
    #[cfg(target_arch = "x86_64")]
    {
        if Engine::detect() != Engine::Portable { engines.push(Engine::Pclmul); }
        if matches!(Engine::detect(), Engine::Avx512) { engines.push(Engine::Avx2); }
    }
    engines.dedup();
    for e in engines {
        cands.push((format!("ours-{e:?}"), Box::new(move |d: &[u8]| ours(e, d))));
    }
    let buf: Vec<u8> = (0..(1usize << 20) + 64).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    for (name, f) in &cands {
        for len in 0..3000 {
            assert_eq!(f(&buf[3..3 + len]), crc32fast::hash(&buf[3..3 + len]), "{name} {len}");
        }
    }
    print!("{:>8}", "bytes");
    for (name, _) in &cands { print!(" {name:>14}"); }
    println!("   (GB/s, best of 7)");
    let total: usize = 256 << 20;
    for &size in &[18usize, 64, 200, 1000, 2048, 4096, 8192, 12000, 65536, 1 << 20] {
        let iters = (total / size).max(1);
        let mut best = vec![f64::MAX; cands.len()];
        for _ in 0..7 {
            for (i, (_, f)) in cands.iter().enumerate() {
                let t = Instant::now();
                let mut acc = 0u32;
                for j in 0..iters {
                    let off = (j * 7) & 63;
                    acc ^= f(std::hint::black_box(&buf[off..off + size]));
                }
                std::hint::black_box(acc);
                best[i] = best[i].min(t.elapsed().as_secs_f64());
            }
        }
        let gb = (iters * size) as f64 / 1e9;
        print!("{size:>8}");
        for b in &best { print!(" {:>14.2}", gb / b); }
        println!();
    }
}
