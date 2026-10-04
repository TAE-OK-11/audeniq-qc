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

/// Report available CPU features separately from selected DSP kernels. SVE2
/// availability alone does not imply an SVE2 kernel was selected.
pub fn cpu_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    #[cfg(target_arch = "aarch64")]
    {
        for (name, available) in [
            ("neon", std::arch::is_aarch64_feature_detected!("neon")),
            ("sve", std::arch::is_aarch64_feature_detected!("sve")),
            ("sve2", std::arch::is_aarch64_feature_detected!("sve2")),
            ("sha2", std::arch::is_aarch64_feature_detected!("sha2")),
            ("crc", std::arch::is_aarch64_feature_detected!("crc")),
            ("pmull", std::arch::is_aarch64_feature_detected!("pmull")),
        ] {
            if available {
                features.push(name);
            }
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        for (name, available) in [
            ("avx2", std::is_x86_feature_detected!("avx2")),
            ("sha", std::is_x86_feature_detected!("sha")),
            ("pclmulqdq", std::is_x86_feature_detected!("pclmulqdq")),
        ] {
            if available {
                features.push(name);
            }
        }
    }
    features
}

/// Direct planar S32 -> interleaved S32. NEON structure stores avoid an extra
/// ALAC/FLAC staging buffer; all loads and stores cover complete four-frame runs.
pub(crate) fn interleave_i32(left: &[i32], right: &[i32], out: &mut [i32]) {
    assert_eq!(left.len(), right.len());
    assert_eq!(out.len(), left.len() * 2);
    #[cfg(target_arch = "aarch64")]
    {
        // AArch64's baseline includes NEON; no optional SVE requirement.
        unsafe { interleave_neon(left, right, out) };
    }
    #[cfg(not(target_arch = "aarch64"))]
    for ((&l, &r), row) in left.iter().zip(right).zip(out.as_chunks_mut::<2>().0) {
        row.copy_from_slice(&[l, r]);
    }
}

/// FLAC independent stereo -> canonical PCM in one read/store pass. The decoded
/// planes remain at source width; avoid shifting and rewriting both planes.
#[cfg(not(feature = "reference-codecs"))]
pub(crate) fn interleave_shift_i32(left: &[i32], right: &[i32], out: &mut [i32], shift: u32) {
    assert_eq!(left.len(), right.len());
    assert_eq!(out.len(), left.len() * 2);
    assert!(matches!(shift, 8 | 16));
    #[cfg(target_arch = "aarch64")]
    // SAFETY: baseline NEON; complete four-frame loads/stores, bounded tail.
    let start = unsafe {
        use std::arch::aarch64::*;
        let shifts = vdupq_n_s32(shift as i32);
        let mut i = 0;
        while i + 4 <= left.len() {
            let l = vshlq_s32(vld1q_s32(left.as_ptr().add(i)), shifts);
            let r = vshlq_s32(vld1q_s32(right.as_ptr().add(i)), shifts);
            vst2q_s32(out.as_mut_ptr().add(i * 2), int32x4x2_t(l, r));
            i += 4;
        }
        i
    };
    #[cfg(not(target_arch = "aarch64"))]
    let start = 0;
    for ((&l, &r), row) in left[start..]
        .iter()
        .zip(&right[start..])
        .zip(out[start * 2..].as_chunks_mut::<2>().0)
    {
        row[0] = l.wrapping_shl(shift);
        row[1] = r.wrapping_shl(shift);
    }
}

/// Pack left-aligned s32 into FLAC's signed 16/24-bit little-endian MD5 bytes.
/// This lossless byte-layout operation uses baseline NEON, like interleaving;
/// it does not change numerical DSP behavior selected by --scalar.
pub(crate) fn compact_pcm(samples: &[i32], bits: u16, out: &mut [u8]) {
    assert!(matches!(bits, 16 | 24));
    assert_eq!(out.len(), samples.len() * (bits / 8) as usize);
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    // SAFETY: AArch64 baseline NEON; complete 16-sample loads and stores.
    let pos = unsafe { compact_neon(samples, bits, out) };
    #[cfg(not(all(target_arch = "aarch64", target_endian = "little")))]
    let pos = 0;
    let samples = &samples[pos..];
    let out = &mut out[pos * (bits / 8) as usize..];
    if bits == 16 {
        for (&x, row) in samples.iter().zip(out.as_chunks_mut::<2>().0) {
            row.copy_from_slice(&((x >> 16) as i16).to_le_bytes());
        }
    } else {
        for (&x, row) in samples.iter().zip(out.as_chunks_mut::<3>().0) {
            row.copy_from_slice(&(x >> 8).to_le_bytes()[..3]);
        }
    }
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[target_feature(enable = "neon")]
unsafe fn compact_neon(samples: &[i32], bits: u16, out: &mut [u8]) -> usize {
    use std::arch::aarch64::*;
    let mut i = 0;
    if bits == 16 {
        while i + 16 <= samples.len() {
            let bytes = vld4q_u8(samples.as_ptr().add(i).cast());
            vst2q_u8(out.as_mut_ptr().add(i * 2), uint8x16x2_t(bytes.2, bytes.3));
            i += 16;
        }
    } else {
        while i + 16 <= samples.len() {
            let bytes = vld4q_u8(samples.as_ptr().add(i).cast());
            vst3q_u8(
                out.as_mut_ptr().add(i * 3),
                uint8x16x3_t(bytes.1, bytes.2, bytes.3),
            );
            i += 16;
        }
    }
    i
}

type RiceFn = fn(&[u32], u32, usize) -> [u64; 3];
pub(crate) struct RiceKernel(RiceFn);
impl RiceKernel {
    pub(crate) fn new(backend: Backend) -> Self {
        assert!(backend.available());
        #[cfg(target_arch = "aarch64")]
        if backend == Backend::Neon {
            return Self(|r, k, count| unsafe { rice_neon(r, k, count) });
        }
        Self(rice_scalar)
    }
    pub(crate) fn choose(&self, residual: &[u32], overhead: u64) -> (u64, u32) {
        assert!(!residual.is_empty());
        let mean = residual.iter().map(|&r| r as u64).sum::<u64>() / residual.len() as u64;
        let estimate = if mean == 0 {
            0
        } else {
            63 - mean.leading_zeros()
        };
        let first = estimate.saturating_sub(1);
        let count = ((estimate + 1).min(30) - first + 1) as usize;
        let quotients = (self.0)(residual, first, count);
        (0..count)
            .map(|i| {
                let k = first + i as u32;
                (
                    overhead + quotients[i] + residual.len() as u64 * (1 + k as u64),
                    k,
                )
            })
            .min_by_key(|&(cost, _)| cost)
            .unwrap()
    }
}
fn rice_scalar(residual: &[u32], first: u32, count: usize) -> [u64; 3] {
    let mut out = [0; 3];
    // Keep each reduction simple enough for baseline-ISA auto-vectorization.
    // The Arm kernel below evaluates all neighbors with one vector load.
    for (i, sum) in out[..count].iter_mut().enumerate() {
        let shift = first + i as u32;
        *sum = residual.iter().map(|&r| (r >> shift) as u64).sum();
    }
    out
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn rice_neon(residual: &[u32], first: u32, count: usize) -> [u64; 3] {
    use std::arch::aarch64::*;
    let shifts = std::array::from_fn::<_, 3, _>(|i| vdupq_n_s32(-(first as i32 + i as i32)));
    let mut sums = [vdupq_n_u64(0); 3];
    let mut pos = 0;
    while pos + 4 <= residual.len() {
        let r = vld1q_u32(residual.as_ptr().add(pos));
        for i in 0..count {
            // Widen before accumulation: arbitrary u32 residuals must not wrap.
            sums[i] = vaddq_u64(sums[i], vpaddlq_u32(vshlq_u32(r, shifts[i])));
        }
        pos += 4;
    }
    let mut out = rice_scalar(&residual[pos..], first, count);
    for i in 0..count {
        out[i] += vaddvq_u64(sums[i]);
    }
    out
}

#[cfg(target_arch = "aarch64")]
type WeightFn = fn(&[f64; 5], &[f64; 5], &mut [[f64; 2]; 4], [f64; 2]) -> [f64; 2];
pub(crate) struct WeightKernel {
    #[cfg(target_arch = "aarch64")]
    apply: WeightFn,
    #[cfg(not(target_arch = "aarch64"))]
    channels: usize,
}
impl WeightKernel {
    pub(crate) fn new(backend: Backend, channels: usize) -> Self {
        assert!(backend.available());
        #[cfg(target_arch = "aarch64")]
        {
            if channels == 2 && backend == Backend::Neon {
                return Self {
                    apply: |b, a, s, x| unsafe { weight_neon(b, a, s, x) },
                };
            }
            Self {
                apply: if channels == 2 {
                    weight_scalar::<2>
                } else {
                    weight_scalar::<1>
                },
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        Self { channels }
    }
    #[inline]
    pub(crate) fn apply(
        &self,
        b: &[f64; 5],
        a: &[f64; 5],
        state: &mut [[f64; 2]; 4],
        x: [f64; 2],
    ) -> [f64; 2] {
        #[cfg(target_arch = "aarch64")]
        {
            (self.apply)(b, a, state, x)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            // x86's existing scalar arithmetic benefits from inlining into the
            // fused meter loop. An extra per-frame SIMD call regressed that CPU.
            if self.channels == 2 {
                weight_scalar::<2>(b, a, state, x)
            } else {
                weight_scalar::<1>(b, a, state, x)
            }
        }
    }
}
#[inline]
fn weight_scalar<const CHANNELS: usize>(
    b: &[f64; 5],
    a: &[f64; 5],
    state: &mut [[f64; 2]; 4],
    x: [f64; 2],
) -> [f64; 2] {
    let mut out = [0.0; 2];
    for ch in 0..CHANNELS {
        let mut n = x[ch];
        for i in 0..4 {
            n -= a[i + 1] * state[i][ch];
        }
        out[ch] = b[0] * n;
        for i in 0..4 {
            out[ch] += b[i + 1] * state[i][ch];
        }
        for i in (1..4).rev() {
            state[i][ch] = state[i - 1][ch];
        }
        state[0][ch] = if n.abs() < 1e-30 { 0.0 } else { n };
    }
    out
}
// Independent channels share coefficients. Separate multiply/add preserves
// the scalar filter's per-channel rounding; no FMA/fast-math approximation.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn weight_neon(
    b: &[f64; 5],
    a: &[f64; 5],
    state: &mut [[f64; 2]; 4],
    x: [f64; 2],
) -> [f64; 2] {
    use std::arch::aarch64::*;
    let history = state.map(|v| vld1q_f64(v.as_ptr()));
    let mut n = vld1q_f64(x.as_ptr());
    for i in 0..4 {
        n = vsubq_f64(n, vmulq_n_f64(history[i], a[i + 1]));
    }
    let mut y = vmulq_n_f64(n, b[0]);
    for i in 0..4 {
        y = vaddq_f64(y, vmulq_n_f64(history[i], b[i + 1]));
    }
    let n = vbslq_f64(
        vcltq_f64(vabsq_f64(n), vdupq_n_f64(1e-30)),
        vdupq_n_f64(0.0),
        n,
    );
    for i in (1..4).rev() {
        vst1q_f64(state[i].as_mut_ptr(), history[i - 1]);
    }
    vst1q_f64(state[0].as_mut_ptr(), n);
    let mut out = [0.0; 2];
    vst1q_f64(out.as_mut_ptr(), y);
    out
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn interleave_neon(left: &[i32], right: &[i32], out: &mut [i32]) {
    use std::arch::aarch64::*;
    let mut i = 0;
    while i + 4 <= left.len() {
        let pair = int32x4x2_t(
            vld1q_s32(left.as_ptr().add(i)),
            vld1q_s32(right.as_ptr().add(i)),
        );
        vst2q_s32(out.as_mut_ptr().add(i * 2), pair);
        i += 4;
    }
    for j in i..left.len() {
        out[j * 2] = left[j];
        out[j * 2 + 1] = right[j];
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

/// Integer LPC prediction across consecutive samples. Widening multiplication
/// preserves all bits; a coefficient has at most 12 signed bits, so eight
/// products of i32 samples cannot overflow the i64 accumulator.
pub(crate) struct LpcKernel(Backend);
impl LpcKernel {
    pub(crate) fn new(backend: Backend) -> Self {
        assert!(backend.available());
        Self(backend)
    }
    pub(crate) fn residual(
        &self,
        samples: &[i32],
        coefficients: &[i32],
        shift: u32,
    ) -> Option<Vec<u32>> {
        assert!(shift <= 15);
        assert!(coefficients.iter().all(|&c| (-2048..=2047).contains(&c)));
        assert!(samples.len() >= coefficients.len());
        match coefficients.len() {
            2 => self.compute::<2>(samples, coefficients, shift),
            4 => self.compute::<4>(samples, coefficients, shift),
            8 => self.compute::<8>(samples, coefficients, shift),
            _ => unreachable!("unsupported LPC order"),
        }
    }
    fn compute<const N: usize>(
        &self,
        samples: &[i32],
        coefficients: &[i32],
        shift: u32,
    ) -> Option<Vec<u32>> {
        #[cfg(target_arch = "x86_64")]
        if self.0 == Backend::Avx2 {
            // SAFETY: selected at construction; all loads are bounded below.
            return unsafe { lpc_avx2::<N>(samples, coefficients, shift) };
        }
        #[cfg(target_arch = "aarch64")]
        if self.0 == Backend::Neon {
            return unsafe { lpc_neon::<N>(samples, coefficients, shift) };
        }
        let mut out = Vec::with_capacity(samples.len() - N);
        lpc_tail::<N>(samples, coefficients, shift, N, &mut out)?;
        Some(out)
    }
}

fn fold_residual(sample: i32, prediction: i64, shift: u32) -> Option<u32> {
    let delta = sample as i64 - (prediction >> shift);
    i32::try_from(delta).ok()?;
    Some(((delta << 1) ^ (delta >> 63)) as u32)
}
fn lpc_tail<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    start: usize,
    out: &mut Vec<u32>,
) -> Option<()> {
    for i in start..samples.len() {
        let mut prediction = 0i64;
        for j in 0..N {
            prediction += coefficients[j] as i64 * samples[i - j - 1] as i64;
        }
        out.push(fold_residual(samples[i], prediction, shift)?);
    }
    Some(())
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn lpc_avx2<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
) -> Option<Vec<u32>> {
    use std::arch::x86_64::*;
    let mut out = Vec::with_capacity(samples.len() - N);
    let mut i = N;
    while i + 4 <= samples.len() {
        let mut sum = _mm256_setzero_si256();
        for (j, &c) in coefficients.iter().enumerate().take(N) {
            let past = _mm_loadu_si128(samples.as_ptr().add(i - j - 1).cast());
            let past = _mm256_cvtepi32_epi64(past);
            sum = _mm256_add_epi64(sum, _mm256_mul_epi32(past, _mm256_set1_epi64x(c as i64)));
        }
        let mut predictions = [0i64; 4];
        _mm256_storeu_si256(predictions.as_mut_ptr().cast(), sum);
        for lane in 0..4 {
            out.push(fold_residual(samples[i + lane], predictions[lane], shift)?);
        }
        i += 4;
    }
    lpc_tail::<N>(samples, coefficients, shift, i, &mut out)?;
    Some(out)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn lpc_neon<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
) -> Option<Vec<u32>> {
    use std::arch::aarch64::*;
    let mut out = Vec::with_capacity(samples.len() - N);
    let mut i = N;
    while i + 4 <= samples.len() {
        let mut lo = vdupq_n_s64(0);
        let mut hi = vdupq_n_s64(0);
        for (j, &c) in coefficients.iter().enumerate().take(N) {
            let past = vld1q_s32(samples.as_ptr().add(i - j - 1));
            lo = vmlal_n_s32(lo, vget_low_s32(past), c);
            hi = vmlal_n_s32(hi, vget_high_s32(past), c);
        }
        let mut predictions = [0i64; 4];
        vst1q_s64(predictions.as_mut_ptr(), lo);
        vst1q_s64(predictions.as_mut_ptr().add(2), hi);
        for lane in 0..4 {
            out.push(fold_residual(samples[i + lane], predictions[lane], shift)?);
        }
        i += 4;
    }
    lpc_tail::<N>(samples, coefficients, shift, i, &mut out)?;
    Some(out)
}
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
    let mut accumulators = [_mm256_setzero_pd(); 4];
    let mut i = 0;
    while i + 16 <= a.len() {
        for (lane, accumulator) in accumulators.iter_mut().enumerate() {
            let offset = i + lane * 4;
            *accumulator = _mm256_add_pd(
                *accumulator,
                _mm256_mul_pd(
                    _mm256_loadu_pd(a.as_ptr().add(offset)),
                    _mm256_loadu_pd(b.as_ptr().add(offset)),
                ),
            );
        }
        i += 16;
    }
    let mut v = _mm256_add_pd(
        _mm256_add_pd(accumulators[0], accumulators[1]),
        _mm256_add_pd(accumulators[2], accumulators[3]),
    );
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
    // Independent accumulation chains let the CPU overlap multiply/add work
    // instead of waiting on one accumulator for every two samples. No FMA.
    let mut accumulators = [vdupq_n_f64(0.0); 4];
    let mut i = 0;
    while i + 8 <= a.len() {
        for (lane, accumulator) in accumulators.iter_mut().enumerate() {
            let offset = i + lane * 2;
            *accumulator = vaddq_f64(
                *accumulator,
                vmulq_f64(
                    vld1q_f64(a.as_ptr().add(offset)),
                    vld1q_f64(b.as_ptr().add(offset)),
                ),
            );
        }
        i += 8;
    }
    let mut v = vaddq_f64(
        vaddq_f64(accumulators[0], accumulators[1]),
        vaddq_f64(accumulators[2], accumulators[3]),
    );
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
    fn integer_lpc_matches_independent_reference() {
        for order in [2, 4, 8] {
            for n in order..=129 {
                let samples: Vec<i32> = (0..n)
                    .map(|i| (i as u32).wrapping_mul(2654435761) as i32)
                    .collect();
                for shift in [0, 7, 15] {
                    for extreme in [false, true] {
                        let coefficients: Vec<i32> = (0..order)
                            .map(|i| {
                                if extreme {
                                    if i % 2 == 0 {
                                        -2048
                                    } else {
                                        2047
                                    }
                                } else {
                                    if i % 2 == 0 {
                                        -1
                                    } else {
                                        1
                                    }
                                }
                            })
                            .collect();
                        let expected: Option<Vec<u32>> = (order..n)
                            .map(|i| {
                                let prediction: i64 = coefficients
                                    .iter()
                                    .enumerate()
                                    .map(|(j, &c)| c as i64 * samples[i - j - 1] as i64)
                                    .sum();
                                let delta =
                                    i32::try_from(samples[i] as i64 - (prediction >> shift))
                                        .ok()?;
                                Some(((delta as i64 * 2) ^ (delta as i64 >> 63)) as u32)
                            })
                            .collect();
                        for backend in [Backend::Scalar, Backend::detect()] {
                            assert_eq!(
                                LpcKernel::new(backend).residual(&samples, &coefficients, shift),
                                expected
                            );
                        }
                    }
                }
            }
        }
    }
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
        // Mixed signs, cancellation, full blocks and vector-boundary tails.
        for n in (0..129).chain([4095, 4096, 4097, 32768]) {
            let a: Vec<f64> = (0..n)
                .map(|i| (i as f64 * 0.137).sin() * 8388607.0)
                .collect();
            let b: Vec<f64> = (0..n)
                .map(|i| (i as f64 * 0.173).cos() * 8388607.0)
                .collect();
            let reference: f64 = a.iter().zip(&b).map(|(a, b)| a * b).sum();
            let magnitude: f64 = a.iter().zip(&b).map(|(a, b)| (a * b).abs()).sum();
            let actual = Dot64Kernel::new(Backend::detect()).apply(&a, &b);
            assert!(
                (actual - reference).abs() <= magnitude.max(1.0) * 1e-10,
                "n={n}"
            );
        }
    }
}

#[cfg(test)]
mod planning_tests {
    use super::*;
    #[cfg(not(feature = "reference-codecs"))]
    #[test]
    fn fused_flac_alignment_interleave_preserves_widths_and_tails() {
        for shift in [8, 16] {
            for n in [0, 1, 2, 3, 4, 5, 7, 16, 65, 4095, 4096, 4097] {
                let left: Vec<i32> = (0..n)
                    .map(|i| i32::MIN.wrapping_add((i as i32).wrapping_mul(982451653)))
                    .collect();
                let right: Vec<i32> = left.iter().map(|&x| !x).collect();
                let expected: Vec<i32> = left
                    .iter()
                    .zip(&right)
                    .flat_map(|(&l, &r)| [l.wrapping_shl(shift), r.wrapping_shl(shift)])
                    .collect();
                let mut out = vec![123; n * 2];
                interleave_shift_i32(&left, &right, &mut out, shift);
                assert_eq!(out, expected);
            }
        }
    }
    #[test]
    fn compact_pcm_preserves_signed_widths_and_all_vector_tails() {
        for bits in [16, 24] {
            for n in (0usize..129).chain([4095, 4096, 4097]) {
                let samples: Vec<i32> = (0..n)
                    .map(|i| i32::MIN.wrapping_add((i as i32).wrapping_mul(982451653)))
                    .collect();
                let mut expected = Vec::new();
                for &x in &samples {
                    expected.extend_from_slice(
                        &(x >> (32 - bits)).to_le_bytes()[..(bits / 8) as usize],
                    );
                }
                let mut actual = vec![0; expected.len()];
                compact_pcm(&samples, bits, &mut actual);
                assert_eq!(actual, expected, "bits={bits} n={n}");
            }
        }
    }
    #[test]
    fn rice_costs_and_stereo_interleave_preserve_integer_extremes() {
        for len in [1, 2, 3, 4, 5, 7, 16, 31, 128, 4095, 32768] {
            let left: Vec<_> = (0..len)
                .map(|i| i32::MIN.wrapping_add((i as i32).wrapping_mul(982451653)))
                .collect();
            let right: Vec<_> = left.iter().map(|&v| !v).collect();
            let mut actual = vec![0; len * 2];
            interleave_i32(&left, &right, &mut actual);
            let expected: Vec<_> = left
                .iter()
                .zip(&right)
                .flat_map(|(&l, &r)| [l, r])
                .collect();
            assert_eq!(actual, expected);
            for shift in 0..32 {
                let residual: Vec<_> = left.iter().map(|&v| (v as u32) >> shift).collect();
                let mean = residual.iter().map(|&v| v as u64).sum::<u64>() / len as u64;
                let center = if mean == 0 {
                    0
                } else {
                    63 - mean.leading_zeros()
                };
                let expected = (center.saturating_sub(1)..=(center + 1).min(30))
                    .map(|k| {
                        (
                            123 + residual
                                .iter()
                                .map(|&v| (v as u64 >> k) + 1 + k as u64)
                                .sum::<u64>(),
                            k,
                        )
                    })
                    .min_by_key(|&(cost, _)| cost)
                    .unwrap();
                for backend in [Backend::Scalar, Backend::detect()] {
                    assert_eq!(RiceKernel::new(backend).choose(&residual, 123), expected);
                }
            }
            let expected: Vec<_> = left.iter().flat_map(|v| v.to_le_bytes()).collect();
            assert_eq!(crate::audio::pcm_bytes(&left).as_ref(), expected);
        }
    }
}
