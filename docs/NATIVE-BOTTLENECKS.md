# Native codec/container bottleneck work

Scope: the Rust ALAC/FLAC decoders and M4A sample-table reader that replaced
Symphonia in the default build, plus their conversion/QC consumers. Backend
integration remains outside this repository's current scope.

## Removed work

| Path | Before | Current implementation |
| --- | --- | --- |
| M4A size-table preflight | One `read_exact(4)` request per sample | Bounded 4 KiB batches; identical entry limits |
| M4A indexing | Owned size, duration, offset and map arrays, then a packet index | Borrow bounded moov tables and advance a timing-run cursor directly into the packet index |
| ALAC packet input | Copy every packet into scratch | Borrow BufReader payload when contiguous; reusable scratch for crossing/large packets |
| ALAC stereo output | Reconstruct planes, append extra bits, align planes, interleave | Fuse those integer operations into one output pass |
| ALAC/FLAC mono output | Copy decoded plane into output | Align the plane and swap reusable Vec buffers |
| FLAC independent stereo | Align both planes, then interleave | Fused alignment/interleave; bounded baseline NEON structure stores on AArch64 |
| Verified FLAC frame copy | Allocate/copy an owned encoded frame per packet | Borrow the validated read-buffer range until the next decoder call |
| Decode output reuse | Clear then resize per packet | Preserve initialized storage and overwrite complete output |
| FLAC encoding residuals | New fixed/LPC residual Vec per evaluated model and block | Recycle losing, replaced, emitted and rejected stereo plans through a bounded five-buffer pool; LPC kernels fill caller scratch |

This is partial zero-copy. OS file reads, buffer-boundary fallback, decoded PCM
materialization, hashes, output encoding and required verification remain.
The encoder still evaluates the same models in the same order with identical
costs and tie decisions. This pool changes storage ownership, not compression
quality or the prediction mathematics. Other small candidate/coefficient
allocations remain. Diagnostic fresh/reuse counters count buffer acquisitions,
not every allocator call or reallocation.

## Calculation bottleneck

A diagnostic local four-minute 48 kHz/24-bit stereo ALAC run after the copy
changes measured about 317 ms in adaptive prediction, 160 ms in Rice decoding,
4 ms in final output construction and 71 us opening the container. These are
inclusive instrumented wall times, not an additive CPU profile. Reducing parser
work alone cannot materially fix this predictor cost.

Predictor-order counters found all 5,626 calls in the measured local ALAC file
used **order 6**, so the initial order-4/order-8 candidate did not accelerate it.
The actual common order-6 scalar loop is now also a compile-time specialization.
The order-6 NEON candidate was rejected after actual N2 measurements in
workflow 37207867394 (`da4c1b57`): four-minute decode/hash consumed **0.55 CPU
seconds with NEON versus 0.45 with the new specialized scalar order 6**. The
old native predecessor needed 0.57 CPU seconds on that same host. Instrumented
prediction alone was 267 ms with NEON versus 174 ms scalar. The prefix method
does more work than scalar's early-exit adaptation on this fixture. Production
order 6 now uses the faster compile-time scalar specialization, including when
the other DSP kernels select NEON. Rejected raw reports are retained separately.

The remaining order-4/order-8 register-history/adaptation-prefix NEON kernels
were also rejected by the forced-order benchmark (workflow 37209080021,
`6d3a2fc4`). On N2, order 4 used 0.52 CPU seconds versus scalar's 0.43; order 8
used 0.55 versus 0.47, across seven four-minute decode/hash repeats. Actual
decoded orders were confirmed by independent diagnostic counters. These kernels
passed PCM/coefficient checks, but SIMD prefix work lost to the scalar early
exit. All explicit ALAC predictor NEON code and its backend plumbing were removed.
The previously rejected FLAC 64-bit NEON LPC kernel remains removed too.

Production orders 4/6/8 use compile-time-specialized integer loops; all other
orders retain the bounded generic loop. Tests compare complete PCM **and final
coefficients** against the original dynamic-order loop, including extreme
residuals, coefficient wrap, quantizers and short blocks. No FMA, reduced
precision or relaxed corruption checks are used. Faster measured NEON/AVX2
encoder, meter, resampler and PCM layout operations remain. The Rust
`benchmark-alac-predictors` command forces FFmpeg's min/max order to 4/6/8 for
future dispatch experiments, separately identifying decoded orders and timing
uninstrumented binaries.

## Measurement and acceptance

`profile-native` enables Rust stage timers and copy/read-request counters. It is
off by default: default release builds contain no profiling clocks or atomics.
Instrumented timings include overhead and overlap; never sum the stages or use
that binary for speed claims. Read-request counters are not syscall counters.

The media-codecs workflow tests native/reference builds on real x86 and ARM,
checks PCM preservation/corruption cases, then benchmarks default builds against
both Symphonia/FFmpeg and the validated native predecessor
`ed72fab7b6c31c57b1e25e5a4f97c00e26934a57` on the same host. Both predecessor
and candidate use fused conversion/QC, avoiding the older two-process baseline.
It also emits a separate instrumented ALAC auto/scalar comparison and verified
FLAC frame-copy counters. Seven repeated four-minute decode/convert measurements
and five repeated two-minute fused-QC measurements are retained in raw reports.

Runtime dispatch uses supported NEON/AVX2 and scalar fallback; no global
`target-cpu=native`, forced optional instructions or LTO tuning is involved.
GitHub's ARM runner must be identified from its actual CPU record. Neoverse N2
results are not AWS Graviton4 results. This candidate's actual ARM acceptance
and before/after results are pending the associated workflow run.

CRC, FLAC MD5, source/output PCM SHA-256, exact sample counts, packet bounds,
deadline checks and no-clobber output publication are preserved. The regression
suite covers borrowed packet boundaries, truncation, common/variable sample
sizes, timing-run changes, chunk-map changes and 32/64-bit offset tables.

FFmpeg-derived port provenance and LGPL obligations remain in
[MEDIA-CODECS.md](MEDIA-CODECS.md) and [THIRD_PARTY.md](../THIRD_PARTY.md). Generic
JSON/hash/image dependencies remain; `reference-codecs` is only a comparison
build. Benchmarks are workload-specific and do not establish better loudness or
true-peak accuracy than FFmpeg.
