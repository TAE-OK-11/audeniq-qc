//! Feature detection selects only kernels safe on the current machine.
//! Zen3 uses AVX2 (not AVX512); AArch64 uses NEON. No global target-cpu flag.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Scalar,
    Avx2,
    Neon,
}
crate::json_enum!(Backend { Scalar => "Scalar", Avx2 => "Avx2", Neon => "Neon" });
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
/// LZCNT, BMI1 and BMI2 (Haswell and later; every AVX2 x86 host has them).
/// `std` caches the CPUID result, so this is a load and a test.
#[cfg(target_arch = "x86_64")]
#[cfg_attr(feature = "reference-codecs", allow(dead_code))] // native decoders only
#[inline]
pub(crate) fn bit_ops() -> bool {
    std::is_x86_feature_detected!("lzcnt")
        && std::is_x86_feature_detected!("bmi1")
        && std::is_x86_feature_detected!("bmi2")
}

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
        let sum = residual.iter().map(|&r| r as u64).sum::<u64>();
        let mean = sum / residual.len() as u64;
        let estimate = if mean == 0 {
            0
        } else {
            63 - mean.leading_zeros()
        };
        let first = estimate.saturating_sub(1);
        let count = ((estimate + 1).min(30) - first + 1) as usize;
        // Every shifted sum is at most the plain sum, so it fits u32 lanes
        // when that does.
        let quotients = if sum <= u32::MAX as u64 {
            rice_narrow(residual, first, count)
        } else {
            (self.0)(residual, first, count)
        };
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
/// [`rice_scalar`] for blocks whose plain sum fits u32: the shifted sums
/// accumulate in u32 lanes (eight per AVX2 vector, four per NEON vector).
fn rice_narrow(residual: &[u32], first: u32, count: usize) -> [u64; 3] {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 was detected; the body is safe Rust.
        return unsafe { rice_narrow_avx2(residual, first, count) };
    }
    rice_narrow_body(residual, first, count)
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rice_narrow_avx2(residual: &[u32], first: u32, count: usize) -> [u64; 3] {
    rice_narrow_body(residual, first, count)
}
#[inline(always)]
fn rice_narrow_body(residual: &[u32], first: u32, count: usize) -> [u64; 3] {
    let mut out = [0; 3];
    for (i, sum) in out[..count].iter_mut().enumerate() {
        let shift = first + i as u32;
        *sum = residual.iter().fold(0u32, |a, &r| a + (r >> shift)) as u64;
    }
    out
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

/// Scalar K-weighting for mono and `--scalar`; stereo meters otherwise use
/// `StereoWeight`, which gives the same bits.
#[derive(Clone, Copy)]
pub(crate) struct WeightKernel {
    channels: usize,
}
impl WeightKernel {
    pub(crate) fn new(backend: Backend, channels: usize) -> Self {
        assert!(backend.available());
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
        if self.channels == 2 {
            weight_scalar::<2>(b, a, state, x)
        } else {
            weight_scalar::<1>(b, a, state, x)
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
        // A select here sat on the filter's loop-carried dependency chain
        // (abs, compare, mask). A predictable branch keeps it off the chain;
        // the stored value is identical.
        state[0][ch] = if n.abs() < 1e-30 { flush_denormal() } else { n };
    }
    out
}
#[cold]
#[inline(never)]
fn flush_denormal() -> f64 {
    0.0
}

/// The AArch64 form of the x86 `StereoWeight` below: both channels in one
/// NEON register, `fmul`/`fsub`/`fadd` per lane in `weight_scalar`'s order.
#[cfg(target_arch = "aarch64")]
pub(crate) struct StereoWeight {
    b: [std::arch::aarch64::float64x2_t; 5],
    a: [std::arch::aarch64::float64x2_t; 4],
    state: [std::arch::aarch64::float64x2_t; 4],
}
#[cfg(target_arch = "aarch64")]
impl StereoWeight {
    #[inline(always)]
    pub(crate) fn new(b: &[f64; 5], a: &[f64; 5], state: &[[f64; 2]; 4]) -> Self {
        use std::arch::aarch64::*;
        // SAFETY (all NEON intrinsics here): NEON is part of the AArch64
        // baseline; loads read two f64 from arrays of two.
        unsafe {
            Self {
                b: b.map(|v| vdupq_n_f64(v)),
                a: std::array::from_fn(|i| vdupq_n_f64(a[i + 1])),
                state: state.map(|v| vld1q_f64(v.as_ptr())),
            }
        }
    }
    #[inline(always)]
    pub(crate) fn step(&mut self, x: [f64; 2]) -> [f64; 2] {
        use std::arch::aarch64::*;
        let s = self.state;
        // SAFETY: NEON is part of the AArch64 baseline; `x` and `out` hold
        // two f64.
        unsafe {
            let mut n = vld1q_f64(x.as_ptr());
            for (&a, &h) in self.a.iter().zip(&s) {
                n = vsubq_f64(n, vmulq_f64(a, h));
            }
            let mut y = vmulq_f64(self.b[0], n);
            for (&b, &h) in self.b[1..].iter().zip(&s) {
                y = vaddq_f64(y, vmulq_f64(b, h));
            }
            let tiny = vcltq_f64(vabsq_f64(n), vdupq_n_f64(1e-30));
            // Rare; a branch keeps the select off the loop-carried chain.
            if vmaxvq_u32(vreinterpretq_u32_u64(tiny)) != 0 {
                n = vbslq_f64(tiny, vdupq_n_f64(0.0), n);
            }
            self.state = [n, s[0], s[1], s[2]];
            let mut out = [0.0; 2];
            vst1q_f64(out.as_mut_ptr(), y);
            out
        }
    }
    pub(crate) fn state(&self) -> [[f64; 2]; 4] {
        self.state.map(|v| {
            let mut out = [0.0; 2];
            // SAFETY: `out` holds two f64.
            unsafe { std::arch::aarch64::vst1q_f64(out.as_mut_ptr(), v) };
            out
        })
    }
}

/// Stereo K-weighting with the two channels in the lanes of one SSE2
/// register, for loops that keep the filter in registers. `mulpd`, `subpd`
/// and `addpd` are per-lane IEEE operations, done in `weight_scalar`'s
/// order, so each channel's output and history are bit-identical to it.
#[cfg(target_arch = "x86_64")]
pub(crate) struct StereoWeight {
    b: [std::arch::x86_64::__m128d; 5],
    a: [std::arch::x86_64::__m128d; 4],
    state: [std::arch::x86_64::__m128d; 4],
}
#[cfg(target_arch = "x86_64")]
impl StereoWeight {
    #[inline(always)]
    pub(crate) fn new(b: &[f64; 5], a: &[f64; 5], state: &[[f64; 2]; 4]) -> Self {
        use std::arch::x86_64::*;
        // SAFETY (all SSE2 intrinsics here): SSE2 is part of the x86-64
        // baseline.
        unsafe {
            Self {
                b: b.map(|v| _mm_set1_pd(v)),
                a: std::array::from_fn(|i| _mm_set1_pd(a[i + 1])),
                state: state.map(|v| _mm_set_pd(v[1], v[0])),
            }
        }
    }
    #[inline(always)]
    pub(crate) fn step(&mut self, x: [f64; 2]) -> [f64; 2] {
        use std::arch::x86_64::*;
        let s = self.state;
        // SAFETY: SSE2 is part of the x86-64 baseline; `out` holds two f64.
        unsafe {
            let mut n = _mm_set_pd(x[1], x[0]);
            for (&a, &h) in self.a.iter().zip(&s) {
                n = _mm_sub_pd(n, _mm_mul_pd(a, h));
            }
            let mut y = _mm_mul_pd(self.b[0], n);
            for (&b, &h) in self.b[1..].iter().zip(&s) {
                y = _mm_add_pd(y, _mm_mul_pd(b, h));
            }
            let magnitude = _mm_andnot_pd(_mm_set1_pd(-0.0), n);
            let tiny = _mm_cmplt_pd(magnitude, _mm_set1_pd(1e-30));
            // Rare; a branch keeps the select off the loop-carried chain.
            if _mm_movemask_pd(tiny) != 0 {
                n = _mm_andnot_pd(tiny, n);
            }
            self.state = [n, s[0], s[1], s[2]];
            let mut out = [0.0; 2];
            _mm_storeu_pd(out.as_mut_ptr(), y);
            out
        }
    }
    pub(crate) fn state(&self) -> [[f64; 2]; 4] {
        self.state.map(|v| {
            let mut out = [0.0; 2];
            // SAFETY: `out` holds two f64.
            unsafe { std::arch::x86_64::_mm_storeu_pd(out.as_mut_ptr(), v) };
            out
        })
    }
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

/// Integer LPC prediction across consecutive samples. Widening multiplication
/// preserves all bits; a coefficient has at most 15 signed bits, so eight
/// products of i32 samples cannot overflow the i64 accumulator.
// The backend selects an explicit AVX2 build on x86; AArch64 always uses
// the baseline-NEON auto-vectorized loops.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub(crate) struct LpcKernel(Backend);
impl LpcKernel {
    pub(crate) fn new(backend: Backend) -> Self {
        assert!(backend.available());
        Self(backend)
    }
    #[cfg(test)]
    fn residual(&self, samples: &[i32], coefficients: &[i32], shift: u32) -> Option<Vec<u32>> {
        let mut out = Vec::new();
        self.residual_into(samples, coefficients, shift, &mut out)?;
        Some(out)
    }
    pub(crate) fn residual_into(
        &self,
        samples: &[i32],
        coefficients: &[i32],
        shift: u32,
        out: &mut Vec<u32>,
    ) -> Option<()> {
        assert!(shift <= 15);
        assert!(coefficients.iter().all(|&c| (-16384..=16383).contains(&c)));
        assert!(samples.len() >= coefficients.len());
        out.clear();
        out.reserve(samples.len() - coefficients.len());
        // Constant-order kernels for the orders up to 8; one kernel for any
        // higher order (see `lpc_store_any`).
        match coefficients.len() {
            1 => self.compute::<1>(samples, coefficients, shift, out),
            2 => self.compute::<2>(samples, coefficients, shift, out),
            3 => self.compute::<3>(samples, coefficients, shift, out),
            4 => self.compute::<4>(samples, coefficients, shift, out),
            5 => self.compute::<5>(samples, coefficients, shift, out),
            6 => self.compute::<6>(samples, coefficients, shift, out),
            7 => self.compute::<7>(samples, coefficients, shift, out),
            8 => self.compute::<8>(samples, coefficients, shift, out),
            9..=32 => self.compute::<0>(samples, coefficients, shift, out),
            _ => unreachable!("unsupported LPC order"),
        }
    }
    /// `N` is the order, or 0 for any order (`lpc_store_any`).
    fn compute<const N: usize>(
        &self,
        samples: &[i32],
        coefficients: &[i32],
        shift: u32,
        out: &mut Vec<u32>,
    ) -> Option<()> {
        let order = coefficients.len();
        out.resize(samples.len() - order, 0);
        let peak = peak_abs(samples);
        let narrow = fits_i32(peak, coefficients);
        if N == 0 && narrow && peak < 1 << 15 && self.0 != Backend::Scalar {
            return SHORT_SCRATCH
                .with(|scratch| {
                    lpc_store_short(
                        self.0,
                        samples,
                        coefficients,
                        shift,
                        out,
                        &mut scratch.borrow_mut(),
                    )
                })
                .then_some(());
        }
        #[cfg(target_arch = "x86_64")]
        if self.0 == Backend::Avx2 {
            // SAFETY: AVX2 selected at construction; the bodies are safe Rust.
            return unsafe {
                match (N, narrow) {
                    (0, true) => lpc_store_any_avx2::<true>(samples, coefficients, shift, out),
                    (0, false) => lpc_store_any_avx2::<false>(samples, coefficients, shift, out),
                    (_, true) => {
                        lpc_store_narrow_avx2::<N>(samples, coefficients, shift, out);
                        true
                    }
                    (_, false) => lpc_store_avx2::<N>(samples, coefficients, shift, out),
                }
            }
            .then_some(());
        }
        #[cfg(target_arch = "aarch64")]
        if self.0 == Backend::Neon && !narrow {
            // SAFETY: NEON selected at construction; bounds are asserted.
            return lpc_store_wide_neon(samples, coefficients, shift, out).then_some(());
        }
        match (N, narrow) {
            (0, true) => lpc_store_any::<true>(samples, coefficients, shift, out),
            (0, false) => lpc_store_any::<false>(samples, coefficients, shift, out),
            (_, true) => {
                lpc_store_narrow::<N>(samples, coefficients, shift, out);
                true
            }
            (_, false) => lpc_store::<N>(samples, coefficients, shift, out),
        }
        .then_some(())
    }
}

/// Exact i64 LPC residuals `x[i] - (sum_j c[j] x[i-1-j] >> shift)` for the
/// samples from `c.len()` in whole groups of 16, with widening
/// multiply-accumulates (eight i64x2 accumulators, one coefficient broadcast
/// per group). Each residual is stored truncated to 32 bits at
/// `e[i - c.len()]`. Returns the first sample not done and whether any
/// residual is outside i32 (its stored value is then meaningless).
/// Baseline NEON has no 64-bit vector multiply, so LLVM leaves the generic
/// i64 loops scalar (SMADDL); this serves 24-bit material at every order.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn lpc_wide_groups_neon(
    x: &[i32],
    c: &[i32],
    shift: u32,
    e: &mut [i32],
) -> (usize, bool) {
    use std::arch::aarch64::*;
    let order = c.len();
    let n = x.len();
    assert!(order >= 1 && shift < 64 && e.len() + order >= n);
    let p = x.as_ptr();
    let shift = vdupq_n_s64(-(shift as i64));
    let half = vdupq_n_s64(1 << 31);
    let mut high = vdupq_n_u64(0);
    let mut i = order;
    while i + 16 <= n {
        let mut acc = [vdupq_n_s64(0); 8];
        for (j, &cj) in c.iter().enumerate() {
            let cj = vdupq_n_s32(cj);
            let h = p.add(i - 1 - j);
            for k in 0..4 {
                let v = vld1q_s32(h.add(4 * k));
                acc[2 * k] = vmlal_s32(acc[2 * k], vget_low_s32(v), vget_low_s32(cj));
                acc[2 * k + 1] = vmlal_high_s32(acc[2 * k + 1], v, cj);
            }
        }
        for k in 0..4 {
            let v = vld1q_s32(p.add(i + 4 * k));
            let lo = vsubq_s64(vmovl_s32(vget_low_s32(v)), vshlq_s64(acc[2 * k], shift));
            let hi = vsubq_s64(vmovl_high_s32(v), vshlq_s64(acc[2 * k + 1], shift));
            // Outside i32 exactly when (d + 2^31) has bits above bit 31.
            high = vorrq_u64(
                high,
                vshrq_n_u64::<32>(vreinterpretq_u64_s64(vaddq_s64(lo, half))),
            );
            high = vorrq_u64(
                high,
                vshrq_n_u64::<32>(vreinterpretq_u64_s64(vaddq_s64(hi, half))),
            );
            let d = vmovn_high_s64(vmovn_s64(lo), hi);
            vst1q_s32(e.as_mut_ptr().add(i - order + 4 * k), d);
        }
        i += 16;
    }
    (i, vmaxvq_u32(vreinterpretq_u32_u64(high)) != 0)
}

/// Narrow LPC residuals of 16-bit samples for orders 9..=32: the same
/// values as [`lpc_store_any`]`::<true>`, with 16-bit multiplies. Each
/// sample and coefficient fits i16 and [`fits_i32`] bounds every partial
/// sum, so 16x16->32-bit products accumulated in i32 are exact. Groups of 16
/// predictions, then the remaining samples one at a time. `scratch` holds
/// the samples in the multiplier's layout.
fn lpc_store_short(
    backend: Backend,
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
    scratch: &mut Vec<i32>,
) -> bool {
    let order = coefficients.len();
    assert!((9..=32).contains(&order) && out.len() + order == samples.len());
    let done = match backend {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: AVX2 was selected at construction; loads stay inside
        // `scratch` and `samples`, stores inside `out` (asserted).
        Backend::Avx2 => unsafe { lpc_pairs_avx2(samples, coefficients, shift, out, scratch) },
        #[cfg(target_arch = "aarch64")]
        // SAFETY: NEON is part of the AArch64 baseline and was selected.
        Backend::Neon => unsafe { lpc_short_neon(samples, coefficients, shift, out, scratch) },
        _ => order,
    };
    for i in done..samples.len() {
        let mut prediction = 0i32;
        for (j, &c) in coefficients.iter().enumerate() {
            prediction = prediction.wrapping_add(c.wrapping_mul(samples[i - 1 - j]));
        }
        let r = samples[i].wrapping_sub(prediction >> shift);
        out[i - order] = ((r << 1) ^ (r >> 31)) as u32;
    }
    true
}

/// [`lpc_store_short`] on AVX2: `pairs[m]` packs samples m (low half) and
/// m - 1 (high half) as i16, so one VPMADDWD with a broadcast coefficient
/// pair adds two taps of eight predictions. Returns the first sample not
/// done.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn lpc_pairs_avx2(
    x: &[i32],
    c: &[i32],
    shift: u32,
    out: &mut [u32],
    pairs: &mut Vec<i32>,
) -> usize {
    use std::arch::x86_64::*;
    let order = c.len();
    let n = x.len();
    pairs.clear();
    pairs.push(x[0] & 0xffff);
    pairs.extend(x.windows(2).map(|w| (w[1] & 0xffff) | (w[0] << 16)));
    // Tap pairs (2p, 2p + 1); an odd order's last pair has a zero high
    // coefficient, against sample -1 (zero) at the block start.
    let mut taps = [0i32; 16];
    for (t, c) in taps.iter_mut().zip(c.chunks(2)) {
        *t = (c[0] & 0xffff) | (c.get(1).copied().unwrap_or(0) << 16);
    }
    let taps = &taps[..order.div_ceil(2)];
    let count = _mm_cvtsi32_si128(shift as i32);
    let p = pairs.as_ptr();
    let mut i = order;
    while i + 16 <= n {
        let mut a0 = _mm256_setzero_si256();
        let mut a1 = _mm256_setzero_si256();
        for (k, &t) in taps.iter().enumerate() {
            let t = _mm256_set1_epi32(t);
            let h = p.add(i - 1 - 2 * k);
            a0 = _mm256_add_epi32(a0, _mm256_madd_epi16(_mm256_loadu_si256(h.cast()), t));
            a1 = _mm256_add_epi32(
                a1,
                _mm256_madd_epi16(_mm256_loadu_si256(h.add(8).cast()), t),
            );
        }
        for (k, a) in [a0, a1].into_iter().enumerate() {
            let v = _mm256_loadu_si256(x.as_ptr().add(i + 8 * k).cast());
            let r = _mm256_sub_epi32(v, _mm256_sra_epi32(a, count));
            let folded = _mm256_xor_si256(_mm256_slli_epi32::<1>(r), _mm256_srai_epi32::<31>(r));
            _mm256_storeu_si256(out.as_mut_ptr().add(i - order + 8 * k).cast(), folded);
        }
        i += 16;
    }
    i
}

/// [`lpc_store_short`] on NEON: the samples as i16, sixteen predictions per
/// group in four i32x4 accumulators fed by SMLAL/SMLAL2 (16-bit multiplies
/// issue at twice the rate of 32-bit MLA on Neoverse cores). Returns the
/// first sample not done.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn lpc_short_neon(
    x: &[i32],
    c: &[i32],
    shift: u32,
    out: &mut [u32],
    scratch: &mut Vec<i32>,
) -> usize {
    use std::arch::aarch64::*;
    let order = c.len();
    let n = x.len();
    scratch.clear();
    scratch.resize(n.div_ceil(2), 0);
    // SAFETY: the i32 buffer holds at least n i16 values.
    let short = std::slice::from_raw_parts_mut(scratch.as_mut_ptr().cast::<i16>(), n);
    for (s, &v) in short.iter_mut().zip(x) {
        *s = v as i16;
    }
    let p = short.as_ptr();
    let count = vdupq_n_s32(-(shift as i32));
    let mut i = order;
    while i + 16 <= n {
        let mut acc = [vdupq_n_s32(0); 4];
        for (j, &cj) in c.iter().enumerate() {
            let cj = vdupq_n_s16(cj as i16);
            let h = p.add(i - 1 - j);
            let v0 = vld1q_s16(h);
            let v1 = vld1q_s16(h.add(8));
            acc[0] = vmlal_s16(acc[0], vget_low_s16(v0), vget_low_s16(cj));
            acc[1] = vmlal_high_s16(acc[1], v0, cj);
            acc[2] = vmlal_s16(acc[2], vget_low_s16(v1), vget_low_s16(cj));
            acc[3] = vmlal_high_s16(acc[3], v1, cj);
        }
        for (k, &a) in acc.iter().enumerate() {
            let v = vld1q_s32(x.as_ptr().add(i + 4 * k));
            let r = vsubq_s32(v, vshlq_s32(a, count));
            let folded = veorq_s32(vshlq_n_s32::<1>(r), vshrq_n_s32::<31>(r));
            vst1q_u32(
                out.as_mut_ptr().add(i - order + 4 * k),
                vreinterpretq_u32_s32(folded),
            );
        }
        i += 16;
    }
    i
}

/// Predictions computed together by [`lpc_store_any`]: each coefficient is
/// broadcast once per group, and the group's lanes fill two AVX2 (four
/// NEON) vectors of i32 or four (eight) of i64.
const ANY_GROUP: usize = 16;

/// [`lpc_store_narrow`] (`NARROW`, for blocks where [`fits_i32`] holds) or
/// [`lpc_store`] for any order: the predictions of `ANY_GROUP` consecutive
/// samples accumulate side by side over the coefficients, so one kernel
/// serves every order above the constant-order ones with the same
/// arithmetic, instead of one instantiation per order.
#[inline(always)]
fn lpc_store_any<const NARROW: bool>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
) -> bool {
    let order = coefficients.len();
    let n = samples.len();
    let mut bad = 0u64;
    let mut i = order;
    while i + ANY_GROUP <= n {
        let x: &[i32; ANY_GROUP] = samples[i..i + ANY_GROUP].try_into().unwrap();
        let o: &mut [u32; ANY_GROUP] = (&mut out[i - order..i - order + ANY_GROUP])
            .try_into()
            .unwrap();
        if NARROW {
            let mut acc = [0i32; ANY_GROUP];
            for (j, &c) in coefficients.iter().enumerate() {
                let h: &[i32; ANY_GROUP] = samples[i - 1 - j..i - 1 - j + ANY_GROUP]
                    .try_into()
                    .unwrap();
                for (a, &h) in acc.iter_mut().zip(h) {
                    *a = a.wrapping_add(c.wrapping_mul(h));
                }
            }
            for ((o, &x), &a) in o.iter_mut().zip(x).zip(&acc) {
                let r = x.wrapping_sub(a >> shift);
                *o = ((r << 1) ^ (r >> 31)) as u32;
            }
        } else {
            let mut acc = [0i64; ANY_GROUP];
            for (j, &c) in coefficients.iter().enumerate() {
                let h: &[i32; ANY_GROUP] = samples[i - 1 - j..i - 1 - j + ANY_GROUP]
                    .try_into()
                    .unwrap();
                for (a, &h) in acc.iter_mut().zip(h) {
                    *a += c as i64 * h as i64;
                }
            }
            for ((o, &x), &a) in o.iter_mut().zip(x).zip(&acc) {
                let r = x as i64 - (a >> shift);
                bad |= (r.wrapping_add(1 << 31) as u64) >> 32;
                *o = ((r << 1) ^ (r >> 63)) as u32;
            }
        }
        i += ANY_GROUP;
    }
    for i in i..n {
        let mut prediction = 0i64;
        for (j, &c) in coefficients.iter().enumerate() {
            prediction += c as i64 * samples[i - 1 - j] as i64;
        }
        let r = samples[i] as i64 - (prediction >> shift);
        bad |= (r.wrapping_add(1 << 31) as u64) >> 32;
        out[i - order] = ((r << 1) ^ (r >> 63)) as u32;
    }
    bad == 0
}

/// [`lpc_store`] for any order on NEON: [`lpc_wide_groups_neon`], folded,
/// then the remaining samples one at a time.
#[cfg(target_arch = "aarch64")]
fn lpc_store_wide_neon(samples: &[i32], coefficients: &[i32], shift: u32, out: &mut [u32]) -> bool {
    let order = coefficients.len();
    // SAFETY: u32 and i32 have the same size and alignment; NEON is part of
    // the AArch64 baseline and was selected by the caller.
    let (done, high) = unsafe {
        let e = std::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<i32>(), out.len());
        lpc_wide_groups_neon(samples, coefficients, shift, e)
    };
    // In range, folding the stored 32 bits equals folding the i64 residual.
    for o in &mut out[..done - order] {
        let r = *o as i32;
        *o = ((r << 1) ^ (r >> 31)) as u32;
    }
    let mut bad = u64::from(high);
    for i in done..samples.len() {
        let mut prediction = 0i64;
        for (j, &c) in coefficients.iter().enumerate() {
            prediction += c as i64 * samples[i - 1 - j] as i64;
        }
        let r = samples[i] as i64 - (prediction >> shift);
        bad |= (r.wrapping_add(1 << 31) as u64) >> 32;
        out[i - order] = ((r << 1) ^ (r >> 63)) as u32;
    }
    bad == 0
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn lpc_store_any_avx2<const NARROW: bool>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
) -> bool {
    lpc_store_any::<NARROW>(samples, coefficients, shift, out)
}

/// Whether every LPC prediction sum over `samples` is below 2^30 in
/// magnitude: sum |c| times the largest |x|. Then the sums are exact in i32
/// and every residual x - (sum >> shift) is too (|x| < 2^30 as well), so
/// none can be out of range.
fn fits_i32(peak: u32, coefficients: &[i32]) -> bool {
    let total: u64 = coefficients.iter().map(|&c| c.unsigned_abs() as u64).sum();
    let largest = peak as u64;
    total * largest < 1 << 30 && largest < 1 << 30
}

thread_local! {
    /// Sample layout buffer of [`lpc_store_short`], one per encoding thread.
    static SHORT_SCRATCH: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Largest |x| of `x` (an AVX2 build on x86, eight lanes instead of the
/// baseline's four).
pub(crate) fn peak_abs(x: &[i32]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 was detected; the body is safe Rust.
        return unsafe { peak_abs_avx2(x) };
    }
    peak_abs_body(x)
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn peak_abs_avx2(x: &[i32]) -> u32 {
    peak_abs_body(x)
}
#[inline(always)]
fn peak_abs_body(x: &[i32]) -> u32 {
    x.iter().fold(0u32, |m, &v| m.max(v.unsigned_abs()))
}

/// [`lpc_store`] with i32 arithmetic, for blocks where [`fits_i32`] holds:
/// the same residuals, eight lanes per AVX2 multiply instead of four.
#[inline(always)]
fn lpc_store_narrow<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
) {
    let mut c = [0i32; N];
    c.copy_from_slice(&coefficients[..N]);
    for ((window, &x), o) in samples.windows(N).zip(&samples[N..]).zip(out.iter_mut()) {
        let window: &[i32; N] = window.try_into().unwrap();
        let mut prediction = 0i32;
        for j in 0..N {
            prediction = prediction.wrapping_add(c[j].wrapping_mul(window[N - 1 - j]));
        }
        let r = x.wrapping_sub(prediction >> shift);
        *o = ((r << 1) ^ (r >> 31)) as u32;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn lpc_store_narrow_avx2<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
) {
    lpc_store_narrow::<N>(samples, coefficients, shift, out)
}

/// Per-partition sums of stored folded residuals (`residual[i]` belongs to
/// sample `order + i`), excluding the warm-up samples.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sums_from_residual_avx2(residual: &[u32], order: usize, size: usize, sums: &mut [u64]) {
    sums_from_residual(residual, order, size, sums)
}
#[inline(always)]
fn sums_from_residual(residual: &[u32], order: usize, size: usize, sums: &mut [u64]) {
    for (p, sum) in sums.iter_mut().enumerate() {
        let start = (p * size).max(order) - order;
        let end = (p + 1) * size - order;
        *sum = residual[start..end].iter().map(|&v| v as u64).sum();
    }
}

/// Store folded residuals for every sample from N. Fixed-size windows and an
/// accumulated range flag (instead of an early return per lane) let LLVM
/// vectorize the widening multiply-accumulate on AVX2 and baseline NEON.
#[inline(always)]
fn lpc_store<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
) -> bool {
    let mut c = [0i64; N];
    for (c, &x) in c.iter_mut().zip(coefficients) {
        *c = x as i64;
    }
    let mut bad = 0u64;
    for ((window, &x), o) in samples.windows(N).zip(&samples[N..]).zip(out.iter_mut()) {
        let window: &[i32; N] = window.try_into().unwrap();
        let mut prediction = 0i64;
        for j in 0..N {
            prediction += c[j] * window[N - 1 - j] as i64;
        }
        let r = x as i64 - (prediction >> shift);
        bad |= (r.wrapping_add(1 << 31) as u64) >> 32;
        *o = ((r << 1) ^ (r >> 63)) as u32;
    }
    bad == 0
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn lpc_store_avx2<const N: usize>(
    samples: &[i32],
    coefficients: &[i32],
    shift: u32,
    out: &mut [u32],
) -> bool {
    lpc_store::<N>(samples, coefficients, shift, out)
}

impl LpcKernel {
    /// Per-partition sums of folded LPC residuals, for ranking models.
    /// `sums[p]` covers samples `p * size..(p + 1) * size`, excluding the
    /// first `order` warm-up samples; the folded residuals of samples
    /// `order..` are left in `out`, exactly as [`Self::residual_into`] stores
    /// them, so the winning model need not be recomputed when it is written.
    /// Returns false when any residual is outside i32 (FLAC cannot code that
    /// model).
    pub(crate) fn partition_sums_into(
        &self,
        samples: &[i32],
        coefficients: &[i32],
        shift: u32,
        size: usize,
        sums: &mut [u64],
        out: &mut Vec<u32>,
    ) -> bool {
        assert!(shift <= 15 && size > coefficients.len());
        assert!(sums.len() * size == samples.len());
        if self
            .residual_into(samples, coefficients, shift, out)
            .is_none()
        {
            return false;
        }
        #[cfg(target_arch = "x86_64")]
        if self.0 == Backend::Avx2 {
            // SAFETY: AVX2 selected at construction; the body is safe Rust.
            unsafe { sums_from_residual_avx2(out, coefficients.len(), size, sums) };
            return true;
        }
        sums_from_residual(out, coefficients.len(), size, sums);
        true
    }
}

/// Zero samples kept before the windowed block (and after it, rounded up to
/// a whole group) so every lag of every group reads inside the buffer.
pub const AUTOCORR_PAD: usize = 48;

/// FLAC LPC autocorrelation of a windowed block for lags `0..out.len()`, in
/// f64. All lags of a group are accumulated in one pass over the block, so
/// each sample is loaded once per group instead of twice per lag.
/// `w[AUTOCORR_PAD..AUTOCORR_PAD + n]` holds the block and every other
/// element of `w` is zero; `w.len()` is at least `2 * AUTOCORR_PAD + n`.
type WindowFn = fn(&[i32], &[f64], &mut [f64]);
pub struct AutocorrKernel(fn(&[f64], usize, usize, &mut [f64]), WindowFn);
impl AutocorrKernel {
    pub fn new(backend: Backend) -> Self {
        assert!(backend.available());
        #[cfg(target_arch = "x86_64")]
        if backend == Backend::Avx2 {
            if std::is_x86_feature_detected!("fma") {
                return Self(
                    |w, n, first, out| unsafe { autocorr_fma(w, n, first, out) },
                    |x, w, out| unsafe { window_avx2(x, w, out) },
                );
            }
            return Self(
                |w, n, first, out| unsafe { autocorr_avx2(w, n, first, out) },
                |x, w, out| unsafe { window_avx2(x, w, out) },
            );
        }
        #[cfg(target_arch = "aarch64")]
        if backend == Backend::Neon {
            return Self(
                |w, n, first, out| unsafe { autocorr_neon(w, n, first, out) },
                window_body,
            );
        }
        Self(autocorr_scalar, window_body)
    }
    pub fn apply(&self, w: &[f64], n: usize, out: &mut [f64]) {
        self.apply_from(w, n, 0, out)
    }
    /// [`Self::apply`] for lags `first..first + out.len()` only.
    pub fn apply_from(&self, w: &[f64], n: usize, first: usize, out: &mut [f64]) {
        assert!(first + out.len() <= AUTOCORR_PAD - 16 + 1 && w.len() >= 2 * AUTOCORR_PAD + n);
        assert!(w[..AUTOCORR_PAD].iter().all(|&v| v == 0.0));
        (self.0)(w, n, first, out)
    }
    /// `out[i] = x[i] * w[i]`. Exact products of integers below 2^25 and
    /// f64 window values, identical for every backend; vectorized here
    /// because the planner's generic loop compiled to scalar conversions.
    pub fn window(&self, x: &[i32], w: &[f64], out: &mut [f64]) {
        assert!(x.len() == w.len() && x.len() == out.len());
        (self.1)(x, w, out)
    }
}
#[inline(always)]
fn window_body(x: &[i32], w: &[f64], out: &mut [f64]) {
    let n = out.len();
    let (x, w) = (&x[..n], &w[..n]);
    for i in 0..n {
        out[i] = x[i] as f64 * w[i];
    }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn window_avx2(x: &[i32], w: &[f64], out: &mut [f64]) {
    window_body(x, w, out)
}
fn autocorr_scalar(w: &[f64], n: usize, first: usize, out: &mut [f64]) {
    let x = &w[AUTOCORR_PAD..AUTOCORR_PAD + n];
    for (i, r) in out.iter_mut().enumerate() {
        let lag = first + i;
        let later = x.get(lag..).unwrap_or_default();
        *r = later.iter().zip(x).map(|(a, b)| a * b).sum();
    }
}
/// Lag-group widths: the widest group that is still filled, so at most
/// one partly used group (of at most 1 or 3 unused lags) per block.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn autocorr_groups(lags: usize, widest: usize, mut group: impl FnMut(usize, usize)) {
    let mut first = 0;
    while first < lags {
        let left = lags - first;
        // Five or six lags: 4 + 2 costs less than one pass of 8.
        let width = if left >= widest {
            widest
        } else if left > 6 {
            8.min(widest)
        } else if left > 2 {
            4
        } else {
            2
        };
        group(first, width);
        first += width;
    }
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn autocorr_avx2(w: &[f64], n: usize, lag0: usize, out: &mut [f64]) {
    // At most eight lags per pass: eight accumulators plus the shared
    // samples fit the sixteen YMM registers. Groups of 4 and 2 lags take 2
    // and 4 sample vectors per step, so there are always eight independent
    // accumulation chains (the add latency, not the loads, bounds a short
    // group otherwise).
    let end = AUTOCORR_PAD + n.next_multiple_of(16);
    assert!(end <= w.len());
    autocorr_groups(out.len(), 8, |first, width| match width {
        8 => autocorr_group_avx2::<8, 1, false>(w, end, lag0 + first, &mut out[first..]),
        4 => autocorr_group_avx2::<4, 2, false>(w, end, lag0 + first, &mut out[first..]),
        _ => autocorr_group_avx2::<2, 4, false>(w, end, lag0 + first, &mut out[first..]),
    });
}
/// [`autocorr_avx2`] with fused multiply-adds, on hosts with FMA.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn autocorr_fma(w: &[f64], n: usize, lag0: usize, out: &mut [f64]) {
    let end = AUTOCORR_PAD + n.next_multiple_of(16);
    assert!(end <= w.len());
    autocorr_groups(out.len(), 8, |first, width| match width {
        8 => autocorr_group_avx2::<8, 1, true>(w, end, lag0 + first, &mut out[first..]),
        4 => autocorr_group_avx2::<4, 2, true>(w, end, lag0 + first, &mut out[first..]),
        _ => autocorr_group_avx2::<2, 4, true>(w, end, lag0 + first, &mut out[first..]),
    });
}
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn autocorr_group_avx2<const G: usize, const U: usize, const FMA: bool>(
    w: &[f64],
    end: usize,
    first: usize,
    out: &mut [f64],
) {
    use std::arch::x86_64::*;
    let p = w.as_ptr();
    let mut acc = [[_mm256_setzero_pd(); G]; U];
    let mut i = AUTOCORR_PAD;
    while i < end {
        for (u, acc) in acc.iter_mut().enumerate() {
            let at = i + 4 * u;
            let x = _mm256_loadu_pd(p.add(at));
            for (j, acc) in acc.iter_mut().enumerate() {
                let y = _mm256_loadu_pd(p.add(at - first - j));
                *acc = if FMA {
                    _mm256_fmadd_pd(x, y, *acc)
                } else {
                    _mm256_add_pd(*acc, _mm256_mul_pd(x, y))
                };
            }
        }
        i += 4 * U;
    }
    for j in 0..G {
        if let Some(r) = out.get_mut(j) {
            let mut sum = acc[0][j];
            for acc in &acc[1..] {
                sum = _mm256_add_pd(sum, acc[j]);
            }
            let mut lanes = [0.0; 4];
            _mm256_storeu_pd(lanes.as_mut_ptr(), sum);
            *r = (lanes[0] + lanes[1]) + (lanes[2] + lanes[3]);
        }
    }
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn autocorr_neon(w: &[f64], n: usize, lag0: usize, out: &mut [f64]) {
    // Groups of twelve lags over two sample vectors per step: lag j of the
    // second vector reads the same window as lag j + 2 of the first, so 24
    // FMA chains need 16 loads per step instead of 26 (two passes of 12
    // lags cover the 24 lags beyond the stereo estimate's 0-8 at order 32).
    // Smaller groups keep at least sixteen chains: four FP pipes (Neoverse
    // V2) stay busy, in at most 28 of 32 registers.
    let end = AUTOCORR_PAD + n.next_multiple_of(16);
    assert!(end <= w.len());
    let mut first = 0;
    while first < out.len() {
        let left = out.len() - first;
        let at = lag0 + first;
        let width = if left >= 12 {
            autocorr_group_neon::<12, 2>(w, end, at, &mut out[first..]);
            12
        } else if left > 6 {
            autocorr_group_neon::<8, 2>(w, end, at, &mut out[first..]);
            8
        } else if left > 2 {
            autocorr_group_neon::<4, 4>(w, end, at, &mut out[first..]);
            4
        } else {
            autocorr_group_neon::<2, 8>(w, end, at, &mut out[first..]);
            2
        };
        first += width;
    }
}
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn autocorr_group_neon<const G: usize, const U: usize>(
    w: &[f64],
    end: usize,
    first: usize,
    out: &mut [f64],
) {
    use std::arch::aarch64::*;
    let p = w.as_ptr();
    let mut acc = [[vdupq_n_f64(0.0); G]; U];
    let mut i = AUTOCORR_PAD;
    while i < end {
        for (u, acc) in acc.iter_mut().enumerate() {
            let at = i + 2 * u;
            let x = vld1q_f64(p.add(at));
            for (j, acc) in acc.iter_mut().enumerate() {
                *acc = vfmaq_f64(*acc, x, vld1q_f64(p.add(at - first - j)));
            }
        }
        i += 2 * U;
    }
    for j in 0..G {
        if let Some(r) = out.get_mut(j) {
            let mut sum = acc[0][j];
            for acc in &acc[1..] {
                sum = vaddq_f64(sum, acc[j]);
            }
            *r = vaddvq_f64(sum);
        }
    }
}

type PeakFn = fn(&[f32], &[[f32; 4]; 12]) -> f32;
pub struct PeakKernel(PeakFn);
impl PeakKernel {
    pub fn new(backend: Backend, interpolate: bool) -> Self {
        assert!(backend.available());
        if !interpolate {
            return Self(|_, _| 0.0);
        }
        #[cfg(target_arch = "x86_64")]
        if backend == Backend::Avx2 {
            // SAFETY: AVX2 selected at construction; the body is safe Rust.
            return Self(|x, c| unsafe { peak_avx2(x, c) });
        }
        #[cfg(target_arch = "aarch64")]
        if backend == Backend::Neon {
            return Self(peak_lanes::<8>);
        }
        Self(peak_lanes::<1>)
    }
    /// Maximum |interpolated value| over every 12-sample window of `x`.
    #[inline]
    pub fn apply(&self, x: &[f32], c: &[[f32; 4]; 12]) -> f32 {
        assert!(x.len() >= 12);
        (self.0)(x, c)
    }
}
/// `LANES` consecutive outputs at once. Per lane the arithmetic is exactly
/// the scalar sum over taps 0..12 (mul then add, no FMA contraction), so the
/// lane count changes speed only. LANES = 1 is the scalar backend.
#[inline(always)]
fn peak_lanes<const LANES: usize>(x: &[f32], c: &[[f32; 4]; 12]) -> f32 {
    let outputs = x.len() - 11;
    let mut best = [0.0f32; LANES];
    let mut t = 0;
    while t + LANES <= outputs {
        let mut acc = [[0.0f32; LANES]; 4];
        for (i, taps) in c.iter().enumerate() {
            let v: &[f32; LANES] = x[t + i..t + i + LANES].try_into().unwrap();
            for p in 0..4 {
                for l in 0..LANES {
                    acc[p][l] += v[l] * taps[p];
                }
            }
        }
        for row in &acc {
            for l in 0..LANES {
                best[l] = best[l].max(row[l].abs());
            }
        }
        t += LANES;
    }
    let mut m = best.iter().fold(0.0f32, |m, &v| m.max(v));
    for t in t..outputs {
        let mut acc = [0.0f32; 4];
        for (i, taps) in c.iter().enumerate() {
            for p in 0..4 {
                acc[p] += x[t + i] * taps[p];
            }
        }
        m = acc.iter().fold(m, |m, v| m.max(v.abs()));
    }
    m
}
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn peak_avx2(x: &[f32], c: &[[f32; 4]; 12]) -> f32 {
    peak_lanes::<8>(x, c)
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
                            let kernel = LpcKernel::new(backend);
                            let mut reused = vec![u32::MAX; n + 17];
                            let status =
                                kernel.residual_into(&samples, &coefficients, shift, &mut reused);
                            assert_eq!(status.is_some(), expected.is_some());
                            if let Some(ref expected) = expected {
                                assert_eq!(&reused, expected);
                            }
                            // A failed predictor can leave partial scratch. A
                            // subsequent valid call must overwrite it fully.
                            let zeros = vec![0; n + 17];
                            assert!(kernel
                                .residual_into(&zeros, &coefficients, shift, &mut reused)
                                .is_some());
                            assert_eq!(reused, vec![0; zeros.len() - order]);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn lpc_partition_sums_match_stored_residuals() {
        let mut seed = 1729u32;
        let base: Vec<i32> = (0..4608)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let tone = ((i as f64 * 0.02).sin() * 8_000_000.0) as i32;
                tone + ((seed as i32) >> 12)
            })
            .collect();
        // Large samples take the i64 path; scaled down, the i32 one.
        for divisor in [1, 4096] {
            let samples: Vec<i32> = base.iter().map(|&x| x / divisor).collect();
            for order in 1..=32 {
                for shift in [0, 5, 15] {
                    for size in [36, 72, 4608] {
                        let coefficients: Vec<i32> = (0..order)
                            .map(|j| [16383, -16384, 9000, -1, 0, 77, -5000, 3][j % 8] >> (j / 8))
                            .collect();
                        for backend in [Backend::Scalar, Backend::detect()] {
                            let kernel = LpcKernel::new(backend);
                            let mut residual = Vec::new();
                            let stored = kernel
                                .residual_into(&samples, &coefficients, shift, &mut residual)
                                .is_some();
                            // The partition variant agrees on the verdict and,
                            // when valid, every stored residual and every sum.
                            let mut sums = vec![u64::MAX; samples.len() / size];
                            let mut into = vec![7u32; 3];
                            let into_ok = kernel.partition_sums_into(
                                &samples,
                                &coefficients,
                                shift,
                                size,
                                &mut sums,
                                &mut into,
                            );
                            let ok = into_ok;
                            assert_eq!(ok, stored, "order {order} shift {shift}");
                            if ok {
                                assert_eq!(into, residual);
                                for (p, &sum) in sums.iter().enumerate() {
                                    let start = (p * size).max(order) - order;
                                    let end = (p + 1) * size - order;
                                    let expected: u64 =
                                        residual[start..end].iter().map(|&r| r as u64).sum();
                                    assert_eq!(sum, expected);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    /// Blocks within the i32 bound (small amplitudes, every order, shift
    /// and coefficient extreme) give the residuals of the i64 definition.
    #[test]
    fn narrow_lpc_residuals_match_wide_definition() {
        let mut seed = 99u32;
        for amplitude in [1i32, 3, 255, 2047, 32767] {
            let samples: Vec<i32> = (0..700)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed as i32) % (amplitude + 1)
                })
                .collect();
            // Constant-order kernels (1..=8) and the any-order kernel.
            for order in 1..=32usize {
                for shift in [0u32, 3, 9, 15] {
                    for set in 0..3 {
                        let coefficients: Vec<i32> = (0..order)
                            .map(|j| match set {
                                0 => [16383, -16384, 1, -1, 0, 2, -2, 3][j % 8],
                                1 => j as i32 * 37 - 100,
                                _ => -16384,
                            })
                            .collect();
                        let total: i64 = coefficients.iter().map(|&c| (c as i64).abs()).sum();
                        let expected: Option<Vec<u32>> = (order..samples.len())
                            .map(|i| {
                                let p: i64 = (0..order)
                                    .map(|j| coefficients[j] as i64 * samples[i - j - 1] as i64)
                                    .sum();
                                let r = samples[i] as i64 - (p >> shift);
                                i32::try_from(r).ok()?;
                                Some(((r << 1) ^ (r >> 63)) as u32)
                            })
                            .collect();
                        for backend in [Backend::Scalar, Backend::detect()] {
                            let mut out = Vec::new();
                            let ok = LpcKernel::new(backend).residual_into(
                                &samples,
                                &coefficients,
                                shift,
                                &mut out,
                            );
                            if total * (amplitude as i64) < 1 << 30 {
                                assert!(fits_i32(peak_abs(&samples), &coefficients));
                            }
                            assert_eq!(ok.is_some(), expected.is_some());
                            if let Some(expected) = &expected {
                                assert_eq!(
                                    &out, expected,
                                    "amp {amplitude} order {order} shift {shift}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    #[allow(clippy::needless_range_loop)] // mirrors the reference loop order
    fn block_true_peak_matches_per_window_reference() {
        let c = crate::resample::TP_COEFFICIENTS;
        let mut seed = 99u32;
        let x: Vec<f32> = (0..300)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                if i % 50 == 7 {
                    1.0
                } else {
                    (seed as i32) as f32 / 2147483648.0
                }
            })
            .collect();
        for len in 12..x.len() {
            let x = &x[..len];
            let mut expected = 0.0f32;
            for w in x.windows(12) {
                for p in 0..4 {
                    let mut a = 0.0f32;
                    for i in 0..12 {
                        a += w[i] * c[i][p];
                    }
                    expected = expected.max(a.abs());
                }
            }
            for backend in [Backend::Scalar, Backend::detect()] {
                let got = PeakKernel::new(backend, true).apply(x, &c);
                assert_eq!(got.to_bits(), expected.to_bits(), "len {len}");
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
        }
    }
    /// Every lag count (1..=33) and block length, including vector and lag
    /// group boundaries, against the per-lag definition.
    #[test]
    fn autocorrelation_matches_definition() {
        for n in (1..70).chain([4095, 4096, 4097]) {
            let x: Vec<f64> = (0..n)
                .map(|i| ((i as f64 * 0.137).sin() + (i as f64 * 0.011).cos()) * 8388607.0)
                .collect();
            let mut w = vec![0.0; AUTOCORR_PAD];
            w.extend_from_slice(&x);
            w.resize(2 * AUTOCORR_PAD + n, 0.0);
            for lags in 1..=33 {
                for backend in [Backend::Scalar, Backend::detect()] {
                    let mut out = vec![f64::NAN; lags];
                    AutocorrKernel::new(backend).apply(&w, n, &mut out);
                    for (lag, &r) in out.iter().enumerate() {
                        let pairs = x.iter().skip(lag).zip(&x);
                        let reference: f64 = pairs.clone().map(|(a, b)| a * b).sum();
                        let magnitude: f64 = pairs.map(|(a, b)| (a * b).abs()).sum();
                        assert!(
                            (r - reference).abs() <= magnitude.max(1.0) * 1e-12,
                            "n={n} lags={lags} lag={lag}"
                        );
                    }
                }
            }
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
