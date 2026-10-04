# Native codec/container bottleneck work

Scope: the Rust ALAC/FLAC decoders and M4A sample-table reader that replaced
Symphonia in the default build, plus their conversion/QC consumers. Backend
integration remains outside this repository's current scope.

## Removed work

| Path | Before | Current candidate |
| --- | --- | --- |
| M4A size-table preflight | One `read_exact(4)` request per sample | Bounded 4 KiB batches; identical entry limits |
| M4A indexing | Owned size, duration, offset and map arrays, then a packet index | Borrow bounded moov tables and advance a timing-run cursor directly into the packet index |
| ALAC packet input | Copy every packet into scratch | Borrow BufReader payload when contiguous; reusable scratch for crossing/large packets |
| ALAC stereo output | Reconstruct planes, append extra bits, align planes, interleave | Fuse those integer operations into one output pass |
| ALAC/FLAC mono output | Copy decoded plane into output | Align the plane and swap reusable Vec buffers |
| FLAC independent stereo | Align both planes, then interleave | Fused alignment/interleave; bounded baseline NEON structure stores on AArch64 |
| Verified FLAC frame copy | Allocate/copy an owned encoded frame per packet | Borrow the validated read-buffer range until the next decoder call |
| Decode output reuse | Clear then resize per packet | Preserve initialized storage and overwrite complete output |

This is partial zero-copy. OS file reads, buffer-boundary fallback, decoded PCM
materialization, hashes, output encoding and required verification remain.

## Calculation bottleneck

A diagnostic local four-minute 48 kHz/24-bit stereo ALAC run after the copy
changes measured about 317 ms in adaptive prediction, 160 ms in Rice decoding,
4 ms in final output construction and 71 us opening the container. These are
inclusive instrumented wall times, not an additive CPU profile. Reducing parser
work alone cannot materially fix this predictor cost.

The ARM candidate retains order-4/order-8 predictor history and signed adaptive
coefficients in NEON registers. Wrapped integer products reproduce ALAC's
32-bit arithmetic. A weighted inclusive prefix mask replaces the sequential
coefficient-update/early-exit loop. The optimization is restricted to <=25-bit
predictor samples, where the complete adaptation prefix cannot overflow; other
orders/widths and an initial sample outside its signed width use the existing
scalar code. No FMA or lossy arithmetic is used.
Tests compare complete PCM **and final coefficients**, including extreme
residuals, coefficient wrap, quantizers and short blocks. This is a separate
candidate from the previously rejected FLAC 64-bit NEON LPC implementation.

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
