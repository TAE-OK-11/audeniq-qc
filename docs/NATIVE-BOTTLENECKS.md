# Native codec/container bottleneck work

Scope: the Rust ALAC/FLAC decoders and M4A sample-table reader that replaced
Symphonia in the default build, plus their conversion/QC consumers. Backend
integration remains outside this repository's current scope.

Latest fresh three-way rerun: [2026-10-05 KST results](BENCHMARK-THREEWAY-RERUN.md).
It uses N2 and EPYC 9V74 and preserves this earlier Zen3 dataset separately.

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
results are not AWS Graviton4 results. Production source `b8918279c97a5069ea8e479e6343b9ba3092901c` passed native ARM/x86 acceptance; the final paired results follow below.

CRC, FLAC MD5, source/output PCM SHA-256, exact sample counts, packet bounds,
deadline checks and no-clobber output publication are preserved. The regression
suite covers borrowed packet boundaries, truncation, common/variable sample
sizes, timing-run changes, chunk-map changes and 32/64-bit offset tables.

FFmpeg-derived port provenance and LGPL obligations remain in
[MEDIA-CODECS.md](MEDIA-CODECS.md) and [THIRD_PARTY.md](../THIRD_PARTY.md). Generic
JSON/hash/image dependencies remain; `reference-codecs` is only a comparison
build. Benchmarks are workload-specific and do not establish better loudness or
true-peak accuracy than FFmpeg.

## Accepted production measurements (2026-10-04)

Tested code: [`b8918279`](https://github.com/TAE-OK-11/audeniq-qc/commit/b8918279c97a5069ea8e479e6343b9ba3092901c).
[Native/reference media comparison](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37209634333),
[native checks](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37209634353) and
[forced-order validation](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37209634329)
all completed successfully. Full repeated samples, commands, binary/input hashes,
capabilities and separate diagnostic snapshots are saved in
[ARM raw report](benchmark-native-streaming-arm.json) and
[x86 raw report](benchmark-native-streaming-x86.json).

The main ARM host identifies as implementer 0x41, part 0xd49 (Neoverse N2).
The main x86 comparison identifies as AMD EPYC 7763 (Zen3). The separate final
forced-order x86 run used EPYC 9V74; its numbers are not attributed to Zen3.
Inputs are synthetic 48 kHz/24-bit stereo tones with warm page cache. These are
process measurements, not AUDENIQ production workloads. Default uninstrumented
release binaries are timed. CPU is median user+system; wall and peak RSS are
independently calculated medians, so median user and system need not sum to
the displayed median CPU. Repeats and all per-run values remain in the JSON.

### Same-host previous native implementation

The comparator is `ed72fab7b6c31c57b1e25e5a4f97c00e26934a57`, rebuilt and measured in the same job.
These reductions are relative to that native implementation, not FFmpeg.
Both builds fuse conversion and QC. Each timed conversion includes source PCM
hashing, FLAC encoding/copy, output redecoding and PCM verification; native output
also uses fsync and no-clobber publication. FFmpeg's output verification runs as
a second timed process: CPU/wall are summed and RSS is the maximum. Independent
FFmpeg decoding of native output is an additional check outside native timings.

| Host | Input / work | CPU seconds old → current | CPU reduction | Wall seconds old → current | Peak RSS KiB old → current |
| --- | --- | ---: | ---: | ---: | ---: |
| Neoverse N2 | WAV decode + PCM SHA-256 (240 s, 7 runs) | 0.05 → 0.05 | 0.0% | 0.06 → 0.06 | 2260 → 2272 |
| Neoverse N2 | FLAC decode + PCM SHA-256 (240 s, 7 runs) | 0.37 → 0.37 | 0.0% | 0.38 → 0.38 | 2528 → 2528 |
| Neoverse N2 | ALAC decode + PCM SHA-256 (240 s, 7 runs) | 0.57 → 0.45 | 21.1% | 0.58 → 0.45 | 2528 → 2528 |
| Neoverse N2 | WAV FLAC conversion + verification (240 s, 7 runs) | 0.90 → 0.90 | 0.0% | 0.99 → 1.02 | 2784 → 2780 |
| Neoverse N2 | ALAC FLAC conversion + verification (240 s, 7 runs) | 1.42 → 1.30 | 8.5% | 1.51 → 1.39 | 3040 → 2912 |
| Neoverse N2 | WAV FLAC conversion + QC + verification (120 s, 5 runs) | 0.59 → 0.60 | -1.7% | 0.65 → 0.65 | 2912 → 2908 |
| Neoverse N2 | ALAC FLAC conversion + QC + verification (120 s, 5 runs) | 0.86 → 0.80 | 7.0% | 0.91 → 0.90 | 3040 → 3040 |
| EPYC 7763 | WAV decode + PCM SHA-256 (240 s, 7 runs) | 0.06 → 0.06 | 0.0% | 0.06 → 0.06 | 3000 → 2996 |
| EPYC 7763 | FLAC decode + PCM SHA-256 (240 s, 7 runs) | 0.47 → 0.47 | 0.0% | 0.48 → 0.47 | 3164 → 3076 |
| EPYC 7763 | ALAC decode + PCM SHA-256 (240 s, 7 runs) | 0.73 → 0.59 | 19.2% | 0.73 → 0.60 | 3168 → 3212 |
| EPYC 7763 | WAV FLAC conversion + verification (240 s, 7 runs) | 1.05 → 1.04 | 1.0% | 1.06 → 1.05 | 3468 → 3492 |
| EPYC 7763 | ALAC FLAC conversion + verification (240 s, 7 runs) | 1.73 → 1.59 | 8.1% | 1.74 → 1.59 | 3700 → 3680 |
| EPYC 7763 | WAV FLAC conversion + QC + verification (120 s, 5 runs) | 0.74 → 0.73 | 1.4% | 0.74 → 0.74 | 3544 → 3724 |
| EPYC 7763 | ALAC FLAC conversion + QC + verification (120 s, 5 runs) | 1.07 → 1.01 | 5.6% | 1.08 → 1.02 | 3724 → 3768 |

The accepted gain is concentrated in ALAC prediction. WAV conversion CPU is
essentially unchanged. In the paired ARM WAV run, wall median increased from
0.99 to 1.02 seconds; its cause is not established by these measurements.
Some x86 RSS medians rose slightly. Buffer reuse removes documented work but
does not prove a uniform CPU, latency or memory reduction for every input.

### Native / optional external codec path / FFmpeg

Reference uses Symphonia decoding with the same Rust encoder and QC; native uses
the in-repository decoders. Generic JSON/hash/image dependencies remain in both.
The same compression preset number does not imply identical encoder choices.

| Host | Work / input (240 s) | Native CPU / wall s / RSS KiB | Reference CPU / wall s / RSS KiB | FFmpeg CPU / wall s / RSS KiB |
| --- | --- | ---: | ---: | ---: |
| Neoverse N2 | conversion + verification WAV | 0.90 / 0.99 / 2784 | 0.82 / 0.92 / 3168 | 2.05 / 1.48 / 54728 |
| Neoverse N2 | conversion + verification ALAC | 1.30 / 1.39 / 2912 | 1.39 / 1.48 / 3168 | 2.02 / 1.51 / 54728 |
| Neoverse N2 | standalone QC + hash WAV | 0.36 / 0.36 / 2528 | 0.36 / 0.37 / 2656 | 2.76 / 2.45 / 55152 |
| Neoverse N2 | standalone QC + hash ALAC | 0.75 / 0.76 / 2784 | 0.92 / 0.92 / 3168 | 2.68 / 2.43 / 54620 |
| EPYC 7763 | conversion + verification WAV | 1.04 / 1.05 / 3484 | 1.01 / 1.02 / 3896 | 3.36 / 1.99 / 58388 |
| EPYC 7763 | conversion + verification ALAC | 1.59 / 1.60 / 3752 | 1.67 / 1.68 / 4132 | 2.92 / 1.95 / 58136 |
| EPYC 7763 | standalone QC + hash WAV | 0.48 / 0.48 / 3452 | 0.48 / 0.48 / 3540 | 3.00 / 2.12 / 56564 |
| EPYC 7763 | standalone QC + hash ALAC | 1.01 / 1.02 / 3484 | 1.13 / 1.13 / 3880 | 2.59 / 2.09 / 55648 |

All native/reference FLAC outputs in the conversion comparisons are byte-for-byte
equal, and native/predecessor outputs are also byte-for-byte equal. Four-minute
outputs are 32,427,602 bytes for native/reference, versus 32,297,578 bytes for
ARM FFmpeg and 32,261,824 for x86 FFmpeg: native files are approximately 0.40%
and 0.51% larger on this fixture. PCM preservation is independently checked;
smaller CPU/RSS is not a claim of better compression or LUFS/true-peak accuracy.

### Copies and buffer acquisitions

The four-minute ARM diagnostic reports 36,770,253 ALAC packet bytes borrowed
directly and 13,911,571 copied at boundaries/oversized-packet fallbacks: 72.6%
of payload avoids that scratch copy. Variable-size-table preflight makes three
read requests instead of the former per-entry loop's 2,813. These count Rust
read requests, not OS syscalls. ALAC conversion+QC acquires three fresh encoder
residual buffers and reuses buffers 33,753 times. These count pool acquisitions,
not total allocations. The verified FLAC-copy diagnostic borrows 31,083,355
encoded bytes and invokes no encoder/residual acquisition; decoding, integrity
verification and output redecoding still occur.

Final forced-order 4/6/8 ARM decode/hash CPU medians are 0.43/0.45/0.46 seconds
over seven four-minute repeats; auto and scalar medians match. Separate
diagnostics confirm the actual predictor orders. All explicit slower ALAC NEON
predictors are removed. Rejected experiments and their source/host provenance
remain in [order-6 ARM](benchmark-alac-neon6-rejected-arm.json),
[order-6 x86](benchmark-alac-neon6-rejected-x86.json),
[order-4/8 ARM](benchmark-alac-dispatch-rejected-arm.json) and
[order-4/8 x86](benchmark-alac-dispatch-rejected-x86.json).

### Validation and remaining limits

On each architecture, native tests passed 22 unit and nine regression tests.
Native and reference qualification each passed 596 checks and codec stress each
passed 288 checks; the native synthesized standards suite passed 20 cases.
Native/reference QC and fingerprint results match exactly; converted PCM hashes
match independent FFmpeg decoding. Predictor tests compare both PCM and final
adaptive coefficients to the original dynamic-order implementation. Bounds,
CRC, STREAMINFO MD5, sample counts, deadlines and source/output SHA-256 remain.

Remaining slower paths are disclosed: ARM FLAC decode/hash wall is 0.38 seconds
versus FFmpeg's 0.31 and reference's 0.30. ARM's 60-second WavPack conversion wall
is 0.39 versus FFmpeg's 0.37, despite lower CPU (0.35 versus 0.50 seconds).
ARM WAV conversion is still slower than the optional reference path
(0.99 versus 0.92 seconds in the three-way comparison).
The raw reports include every supported-format conversion, full QC and
fingerprint runs, not only the improved ALAC cases.

No Graviton4 hardware run, official full EBU certification, production music
corpus, fuzz campaign or concurrent service p95 benchmark is established here.
Backend integration remains deferred. This optimization changes work and
storage ownership while retaining validated integer/PCM behavior.
