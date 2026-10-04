//! Development-only inclusive wall-clock stage accounting. Default guards are
//! zero-sized with no Drop, clock reads or counters; no runtime profiling cost.
#[derive(Clone, Copy)]
#[allow(dead_code)] // Reference codecs do not exercise native decoder stages.
pub(crate) enum Stage {
    M4aOpen,
    PacketRead,
    AlacRice,
    AlacPredict,
    AlacOutput,
    FlacResidual,
    FlacPredict,
    FlacOutput,
    FlacMd5,
    Encoder,
    EncoderPlan,
    SourceHash,
    Qc,
    OutputVerify,
}
pub(crate) struct Guard {
    #[cfg(feature = "profile-native")]
    stage: usize,
    #[cfg(feature = "profile-native")]
    start: std::time::Instant,
}
#[inline(always)]
pub(crate) fn scope(_stage: Stage) -> Guard {
    Guard {
        #[cfg(feature = "profile-native")]
        stage: _stage as usize,
        #[cfg(feature = "profile-native")]
        start: std::time::Instant::now(),
    }
}
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum Counter {
    PacketBorrowedBytes,
    PacketCopiedBytes,
    FlacBorrowedBytes,
    SizeTableReadRequests,
    MonoBufferSwaps,
    EncoderResidualFresh,
    EncoderResidualReused,
}
#[inline(always)]
pub(crate) fn count(_counter: Counter, _value: u64) {
    #[cfg(feature = "profile-native")]
    COUNTERS[_counter as usize].fetch_add(_value, std::sync::atomic::Ordering::Relaxed);
}
#[cfg(feature = "profile-native")]
static COUNTERS: [std::sync::atomic::AtomicU64; 7] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 7];
#[cfg(feature = "profile-native")]
const COUNT: usize = 14;
#[cfg(feature = "profile-native")]
static NS: [std::sync::atomic::AtomicU64; COUNT] =
    [const { std::sync::atomic::AtomicU64::new(0) }; COUNT];
#[cfg(feature = "profile-native")]
static CALLS: [std::sync::atomic::AtomicU64; COUNT] =
    [const { std::sync::atomic::AtomicU64::new(0) }; COUNT];
#[cfg(feature = "profile-native")]
impl Drop for Guard {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        NS[self.stage].fetch_add(self.start.elapsed().as_nanos() as u64, Relaxed);
        CALLS[self.stage].fetch_add(1, Relaxed);
    }
}
#[cfg(feature = "profile-native")]
pub fn report() -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    let names = [
        "m4a_open",
        "packet_read",
        "alac_rice",
        "alac_predict",
        "alac_output",
        "flac_residual",
        "flac_predict",
        "flac_output",
        "flac_md5",
        "encoder",
        "encoder_plan",
        "source_hash",
        "qc",
        "output_verify",
    ];
    let stages: serde_json::Map<String, serde_json::Value> = names.iter().enumerate().map(|(i, &name)| {
        (name.into(), serde_json::json!({"calls": CALLS[i].load(Relaxed), "wall_ns": NS[i].load(Relaxed)}))
    }).collect();
    let names = [
        "packet_borrowed_bytes",
        "packet_copied_bytes",
        "flac_borrowed_bytes",
        "size_table_read_requests",
        "mono_buffer_swaps",
        "encoder_residual_fresh_buffers",
        "encoder_residual_reused_buffers",
    ];
    let counters: serde_json::Map<String, serde_json::Value> = names
        .iter()
        .enumerate()
        .map(|(i, &name)| (name.into(), serde_json::json!(COUNTERS[i].load(Relaxed))))
        .collect();
    serde_json::json!({"kind":"inclusive wall time with profiling overhead; stages overlap, do not sum", "stages":stages,"counters":counters})
}
