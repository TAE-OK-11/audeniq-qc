//! Feature detection selects only kernels safe on the current machine.
//! Zen3 uses AVX2 (not AVX512); AArch64 uses NEON. No global target-cpu flag.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum Backend {
    Scalar,
    Avx2,
    Neon,
}
impl Backend {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            return Self::Avx2;
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            return Self::Neon;
        }
        Self::Scalar
    }
    pub fn available(self) -> bool {
        self == Self::Scalar || self == Self::detect()
    }
}

pub fn pcm_le(bytes: &[u8], depth: u16, dst: &mut [i32], backend: Backend) {
    assert!(matches!(depth, 16 | 24));
    assert!(bytes.len() >= dst.len() * (depth / 8) as usize);
    assert!(backend.available());
    #[cfg(target_arch = "x86_64")]
    if backend == Backend::Avx2 {
        // SAFETY: feature detected; function bounds-checks every vector access.
        unsafe {
            pcm_avx2(bytes, depth, dst);
        }
        return;
    }
    #[cfg(target_arch = "aarch64")]
    if backend == Backend::Neon {
        // SAFETY: feature detected; all loads/stores stay in supplied slices.
        unsafe {
            pcm_neon(bytes, depth, dst);
        }
        return;
    }
    pcm_scalar(bytes, depth, dst);
}

fn pcm_scalar(bytes: &[u8], depth: u16, dst: &mut [i32]) {
    if depth == 16 {
        for (s, b) in dst.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *s = i16::from_le_bytes([b[0], b[1]]) as i32 * 65536;
        }
    } else {
        for (s, b) in dst.iter_mut().zip(bytes.as_chunks::<3>().0) {
            *s = i32::from_le_bytes([0, b[0], b[1], b[2]]);
        }
    }
}

pub fn dot(a: &[f32], b: &[f32], backend: Backend) -> f32 {
    assert_eq!(a.len(), b.len());
    assert!(backend.available());
    #[cfg(target_arch = "x86_64")]
    if backend == Backend::Avx2 {
        return unsafe { dot_avx2(a, b) };
    }
    #[cfg(target_arch = "aarch64")]
    if backend == Backend::Neon {
        return unsafe { dot_neon(a, b) };
    }
    dot_scalar(a, b)
}

// Fixed eight-lane reduction makes fingerprint quantization identical across
// scalar, AVX2 and NEON, rather than letting their accumulation orders differ.
fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut sums = [0.0f32; 8];
    let mut i = 0;
    while i + 8 <= a.len() {
        for lane in 0..8 {
            sums[lane] += a[i + lane] * b[i + lane];
        }
        i += 8;
    }
    sums.iter().sum::<f32>() + a[i..].iter().zip(&b[i..]).map(|(x, y)| x * y).sum::<f32>()
}

/// Resolve once outside FIR sample loops. Checking CPU features per tap/sample
/// wastes instructions even when CPUID results themselves are cached.
pub struct DotKernel(fn(&[f32], &[f32]) -> f32);

/// FLAC LPC autocorrelation needs f64 stability, including near-singular tones.
pub struct Dot64Kernel(fn(&[f64], &[f64]) -> f64);
impl Dot64Kernel {
    pub fn new(backend: Backend) -> Self {
        assert!(backend.available());
        #[cfg(target_arch = "x86_64")]
        if backend == Backend::Avx2 {
            return Self(|a, b| unsafe { dot64_avx2(a, b) });
        }
        #[cfg(target_arch = "aarch64")]
        if backend == Backend::Neon {
            return Self(|a, b| unsafe { dot64_neon(a, b) });
        }
        Self(|a, b| a.iter().zip(b).map(|(x, y)| x * y).sum())
    }
    pub fn apply(&self, a: &[f64], b: &[f64]) -> f64 {
        assert_eq!(a.len(), b.len());
        (self.0)(a, b)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot64_avx2(a: &[f64], b: &[f64]) -> f64 {
    use std::arch::x86_64::*;
    let mut v = _mm256_setzero_pd();
    let mut i = 0;
    while i + 4 <= a.len() {
        v = _mm256_add_pd(
            v,
            _mm256_mul_pd(
                _mm256_loadu_pd(a.as_ptr().add(i)),
                _mm256_loadu_pd(b.as_ptr().add(i)),
            ),
        );
        i += 4;
    }
    let mut sums = [0.0; 4];
    _mm256_storeu_pd(sums.as_mut_ptr(), v);
    sums.iter().sum::<f64>() + a[i..].iter().zip(&b[i..]).map(|(x, y)| x * y).sum::<f64>()
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn dot64_neon(a: &[f64], b: &[f64]) -> f64 {
    use std::arch::aarch64::*;
    let mut v = vdupq_n_f64(0.0);
    let mut i = 0;
    while i + 2 <= a.len() {
        v = vaddq_f64(
            v,
            vmulq_f64(vld1q_f64(a.as_ptr().add(i)), vld1q_f64(b.as_ptr().add(i))),
        );
        i += 2;
    }
    vaddvq_f64(v) + a[i..].iter().zip(&b[i..]).map(|(x, y)| x * y).sum::<f64>()
}

type PeakFn = fn(&[f32], &[f32], &[[f32; 4]; 12]) -> f32;
pub struct PeakKernel(PeakFn);
impl PeakKernel {
    pub fn new(backend: Backend, interpolate: bool) -> Self {
        assert!(backend.available());
        if !interpolate {
            return Self(|_, _, _| 0.0);
        }
        #[cfg(target_arch = "x86_64")]
        if backend == Backend::Avx2 {
            return Self(|l, r, c| unsafe { peak_avx2(l, r, c) });
        }
        #[cfg(target_arch = "aarch64")]
        if backend == Backend::Neon {
            return Self(|l, r, c| unsafe { peak_neon(l, r, c) });
        }
        Self(peak_scalar)
    }
    #[inline]
    pub fn apply(&self, l: &[f32], r: &[f32], c: &[[f32; 4]; 12]) -> f32 {
        assert_eq!(l.len(), 12);
        assert_eq!(r.len(), 12);
        (self.0)(l, r, c)
    }
}
fn peak_scalar(l: &[f32], r: &[f32], c: &[[f32; 4]; 12]) -> f32 {
    let mut a = [0.0f32; 4];
    let mut b = [0.0f32; 4];
    for i in 0..12 {
        for p in 0..4 {
            a[p] += l[i] * c[i][p];
            b[p] += r[i] * c[i][p];
        }
    }
    a.iter().chain(&b).fold(0.0, |m, x| m.max(x.abs()))
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn peak_avx2(l: &[f32], r: &[f32], c: &[[f32; 4]; 12]) -> f32 {
    use std::arch::x86_64::*;
    let mut acc = _mm256_setzero_ps();
    for i in 0..12 {
        let v =
            _mm256_insertf128_ps::<1>(_mm256_castps128_ps256(_mm_set1_ps(l[i])), _mm_set1_ps(r[i]));
        let coefficient = _mm_loadu_ps(c[i].as_ptr());
        let coefficient =
            _mm256_insertf128_ps::<1>(_mm256_castps128_ps256(coefficient), coefficient);
        acc = _mm256_add_ps(acc, _mm256_mul_ps(v, coefficient));
    }
    let mut out = [0f32; 8];
    _mm256_storeu_ps(out.as_mut_ptr(), acc);
    out.iter().fold(0.0, |m, x| m.max(x.abs()))
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn peak_neon(l: &[f32], r: &[f32], c: &[[f32; 4]; 12]) -> f32 {
    use std::arch::aarch64::*;
    let mut a = vdupq_n_f32(0.0);
    let mut b = vdupq_n_f32(0.0);
    for i in 0..12 {
        let v = vld1q_f32(c[i].as_ptr());
        a = vaddq_f32(a, vmulq_n_f32(v, l[i]));
        b = vaddq_f32(b, vmulq_n_f32(v, r[i]));
    }
    vmaxvq_f32(vmaxq_f32(vabsq_f32(a), vabsq_f32(b)))
}
impl DotKernel {
    pub fn new(backend: Backend) -> Self {
        assert!(backend.available());
        #[cfg(target_arch = "x86_64")]
        if backend == Backend::Avx2 {
            return Self(|a, b| unsafe { dot_avx2(a, b) });
        }
        #[cfg(target_arch = "aarch64")]
        if backend == Backend::Neon {
            return Self(|a, b| unsafe { dot_neon(a, b) });
        }
        Self(dot_scalar)
    }
    #[inline]
    pub fn apply(&self, a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len());
        (self.0)(a, b)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pcm_avx2(bytes: &[u8], depth: u16, dst: &mut [i32]) {
    use std::arch::x86_64::*;
    let mut i = 0;
    if depth == 16 {
        while i + 8 <= dst.len() {
            let v = _mm_loadu_si128(bytes.as_ptr().add(i * 2).cast());
            let v = _mm256_slli_epi32::<16>(_mm256_cvtepi16_epi32(v));
            _mm256_storeu_si256(dst.as_mut_ptr().add(i).cast(), v);
            i += 8;
        }
    } else {
        let mask = _mm_setr_epi8(-1, 0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11);
        // Each 4-sample load reads 16 bytes, although only 12 are consumed.
        while i + 8 <= dst.len() && i * 3 + 28 <= bytes.len() {
            let lo = _mm_shuffle_epi8(_mm_loadu_si128(bytes.as_ptr().add(i * 3).cast()), mask);
            let hi = _mm_shuffle_epi8(_mm_loadu_si128(bytes.as_ptr().add(i * 3 + 12).cast()), mask);
            let v = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(lo), hi);
            _mm256_storeu_si256(dst.as_mut_ptr().add(i).cast(), v);
            i += 8;
        }
    }
    pcm_scalar(&bytes[i * (depth / 8) as usize..], depth, &mut dst[i..]);
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let mut v = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= a.len() {
        v = _mm256_add_ps(
            v,
            _mm256_mul_ps(
                _mm256_loadu_ps(a.as_ptr().add(i)),
                _mm256_loadu_ps(b.as_ptr().add(i)),
            ),
        );
        i += 8;
    }
    let mut sums = [0f32; 8];
    _mm256_storeu_ps(sums.as_mut_ptr(), v);
    sums.iter().sum::<f32>() + a[i..].iter().zip(&b[i..]).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pcm_neon(bytes: &[u8], depth: u16, dst: &mut [i32]) {
    use std::arch::aarch64::*;
    let mut i = 0;
    if depth == 16 {
        while i + 8 <= dst.len() {
            let v = vreinterpretq_s16_u8(vld1q_u8(bytes.as_ptr().add(i * 2)));
            vst1q_s32(
                dst.as_mut_ptr().add(i),
                vshlq_n_s32::<16>(vmovl_s16(vget_low_s16(v))),
            );
            vst1q_s32(
                dst.as_mut_ptr().add(i + 4),
                vshlq_n_s32::<16>(vmovl_s16(vget_high_s16(v))),
            );
            i += 8;
        }
    } else {
        let mask = [255, 0, 1, 2, 255, 3, 4, 5, 255, 6, 7, 8, 255, 9, 10, 11];
        let mask = vld1q_u8(mask.as_ptr());
        while i + 4 <= dst.len() && i * 3 + 16 <= bytes.len() {
            let v = vqtbl1q_u8(vld1q_u8(bytes.as_ptr().add(i * 3)), mask);
            vst1q_s32(dst.as_mut_ptr().add(i), vreinterpretq_s32_u8(v));
            i += 4;
        }
    }
    pcm_scalar(&bytes[i * (depth / 8) as usize..], depth, &mut dst[i..]);
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let mut v = vdupq_n_f32(0.0);
    let mut w = vdupq_n_f32(0.0);
    let mut i = 0;
    while i + 8 <= a.len() {
        // Separate multiply/add retains the same rounding policy as AVX2.
        v = vaddq_f32(
            v,
            vmulq_f32(vld1q_f32(a.as_ptr().add(i)), vld1q_f32(b.as_ptr().add(i))),
        );
        w = vaddq_f32(
            w,
            vmulq_f32(
                vld1q_f32(a.as_ptr().add(i + 4)),
                vld1q_f32(b.as_ptr().add(i + 4)),
            ),
        );
        i += 8;
    }
    let mut sums = [0.0f32; 8];
    vst1q_f32(sums.as_mut_ptr(), v);
    vst1q_f32(sums.as_mut_ptr().add(4), w);
    sums.iter().sum::<f32>() + a[i..].iter().zip(&b[i..]).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tails_and_extremes_match() {
        for depth in [16, 24] {
            for n in 0..129 {
                let b: Vec<_> = (0..n * (depth / 8) as usize)
                    .map(|i| (i * 131 + 197) as u8)
                    .collect();
                let mut s = vec![0; n];
                let mut v = vec![0; n];
                pcm_le(&b, depth, &mut s, Backend::Scalar);
                pcm_le(&b, depth, &mut v, Backend::detect());
                assert_eq!(s, v);
            }
        }
    }
    #[test]
    fn dot_tails_match() {
        for n in 0..129 {
            let a: Vec<_> = (0..n).map(|i| (i as f32).sin()).collect();
            assert_eq!(
                dot(&a, &a, Backend::Scalar).to_bits(),
                dot(&a, &a, Backend::detect()).to_bits()
            );
            let b: Vec<_> = a.iter().map(|x| *x as f64).collect();
            assert!(
                (Dot64Kernel::new(Backend::Scalar).apply(&b, &b)
                    - Dot64Kernel::new(Backend::detect()).apply(&b, &b))
                .abs()
                    < 1e-10
            );
        }
    }
}
