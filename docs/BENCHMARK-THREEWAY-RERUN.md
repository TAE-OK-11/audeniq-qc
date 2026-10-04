# Three-way benchmark rerun — 2026-10-05 KST

Compared **FFmpeg**, **in-repository Rust codecs/containers**, and **optional
Symphonia reference codecs with the same Rust encoder/QC**. This reruns the
accepted code; no decoder/encoder/backend changes were made for this measurement.

Engine source: [`b8918279c97a5069ea8e479e6343b9ba3092901c`](https://github.com/TAE-OK-11/audeniq-qc/commit/b8918279c97a5069ea8e479e6343b9ba3092901c).
The intervening `f7effcf8` commit changes only documentation and raw reports.
[ARM attempt 2](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37209634333/attempts/2)
and [x86 attempt 3](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37209634333/attempts/3)
completed successfully. Actual rerun jobs are ARM **111464825923** and x86
**111467079517**. GitHub reruns preserve the other architecture's previous job;
these reports deliberately use the fresh ARM attempt-2 and x86 attempt-3 logs,
rather than treating a preserved job as a new benchmark.

Raw reports: [ARM](benchmark-threeway-rerun-arm.json),
[x86](benchmark-threeway-rerun-x86.json).

## Conditions and units

Every table cell is **CPU seconds / wall seconds / peak RSS MiB**.
CPU is user+system. Each metric is an independent median; median user and system
values need not sum to median CPU. Raw RSS uses KiB, converted here by /1024.
Input is 48 kHz, 24-bit stereo synthetic tones with warm page cache.
Conversion and standalone QC use 240-second inputs and seven repeats;
conversion+QC uses 120-second inputs and five repeats. Comparisons are within
each job on the same host/input; they are not cross-host speedup measurements.

Native and reference conversions include source PCM hashing, lossless encoding,
output redecoding/hash verification, fsync and no-clobber publication. FFmpeg
conversion includes source PCM hashing and a second timed output verification
process; CPU/wall are summed and RSS is their maximum. Additional independent
FFmpeg checks on native/reference outputs occur outside their timed subprocesses.
QC uses one decode pass for LUFS/true peak and PCM SHA-256; native/reference also
calculate peak/clipping/silence. The main table excludes fingerprint; full
fingerprint and decode/hash comparisons are retained in the raw reports.

Default uninstrumented release binaries are timed. Separate `profile-native`
snapshots include overhead/overlapping stages and are not added to benchmark
CPU time. Native and reference retain the same generic JSON/hash/image dependencies.
Native means the codec/container implementation is in this repository; it does
not mean zero external crates. FFmpeg is a development oracle and comparison,
not a runtime dependency of either Rust build.

ARM identifies as implementer 0x41, part 0xd49 (Neoverse N2). The **new x86 host
is EPYC 9V74**, not the previous EPYC 7763 Zen3. This rerun does not establish
Zen3 remeasurement or AWS Graviton4 performance. Both jobs used the packaged
`ffmpeg version 6.1.1-3ubuntu5`; this is not a benchmark of FFmpeg master.

## ARM — Neoverse N2

| Input / work | Native Rust | Optional codec dependency | FFmpeg 6.1.1 |
| --- | ---: | ---: | ---: |
| WAV FLAC conversion + verification | 0.90 / 1.01 / 2.72 | 0.83 / 0.92 / 3.09 | 2.02 / 1.45 / 53.58 |
| ALAC FLAC conversion + verification | 1.30 / 1.40 / 2.84 | 1.39 / 1.48 / 3.09 | 2.01 / 1.50 / 53.33 |
| WAV QC + PCM hash | 0.36 / 0.37 / 2.47 | 0.36 / 0.37 / 2.59 | 2.69 / 2.41 / 53.86 |
| ALAC QC + PCM hash | 0.75 / 0.76 / 2.71 | 0.91 / 0.92 / 3.09 | 2.67 / 2.43 / 53.40 |
| WAV FLAC conversion + QC + verification | 0.60 / 0.65 / 2.84 | 0.56 / 0.61 / 3.22 | 1.97 / 1.68 / 58.95 |
| ALAC FLAC conversion + QC + verification | 0.80 / 0.92 / 2.97 | 0.85 / 0.89 / 3.22 | 1.94 / 1.68 / 58.83 |

## x86 — AMD EPYC 9V74

| Input / work | Native Rust | Optional codec dependency | FFmpeg 6.1.1 |
| --- | ---: | ---: | ---: |
| WAV FLAC conversion + verification | 0.83 / 0.94 / 3.55 | 0.83 / 0.95 / 3.95 | 2.36 / 1.41 / 57.01 |
| ALAC FLAC conversion + verification | 1.25 / 1.37 / 3.74 | 1.35 / 1.47 / 4.13 | 2.19 / 1.52 / 56.95 |
| WAV QC + PCM hash | 0.37 / 0.37 / 3.47 | 0.37 / 0.37 / 3.65 | 2.21 / 1.59 / 55.33 |
| ALAC QC + PCM hash | 0.80 / 0.80 / 3.54 | 0.88 / 0.88 / 3.93 | 2.08 / 1.72 / 54.38 |
| WAV FLAC conversion + QC + verification | 0.56 / 0.63 / 3.82 | 0.58 / 0.64 / 4.16 | 1.75 / 1.25 / 62.76 |
| ALAC FLAC conversion + QC + verification | 0.79 / 0.92 / 3.83 | 0.83 / 0.91 / 4.16 | 1.66 / 1.31 / 62.50 |

## What this rerun shows

Native ALAC conversion used **1.30 vs 1.39 vs 2.01 CPU seconds** on N2 and
**1.25 vs 1.35 vs 2.19** on EPYC 9V74, for native/reference/FFmpeg. Native also
had the lowest wall time and RSS in both conversion comparisons. Relative to
FFmpeg within the fresh jobs, native ALAC conversion CPU was 35.3% lower on N2
and 42.9% lower on 9V74; RSS was 94.7% and 93.4% lower.

ALAC standalone QC favored native in CPU, wall and RSS on both hosts. WAV QC
native/reference CPU and wall essentially tied; native used less RSS.
**WAV conversion was still slower than reference on ARM** (1.01 vs 0.92 seconds,
CPU 0.90 vs 0.83). On 9V74, WAV conversion CPU tied (0.83 seconds), and wall
medians were near-equal (0.94 vs 0.95). These do not establish a substantial
native WAV compute gain.

ALAC conversion+QC native CPU and RSS were lower, but its wall median remained
slightly **higher than reference**: N2 0.92 vs 0.89 seconds; 9V74 0.92 vs 0.91.
ARM WAV conversion+QC was slower than reference in CPU and wall too.
No confidence interval or production latency claim is established by these
medians. All per-run values remain available for inspection.

## Output size and correctness

Native/reference conversion outputs were **byte-for-byte equal**, and all
converted outputs independently decoded through FFmpeg to the expected PCM
SHA-256. The four-minute output sizes are:

| Host | Native / reference FLAC bytes | FFmpeg FLAC bytes | Native size difference |
| --- | ---: | ---: | ---: |
| N2 | 32,427,602 | 32,297,578 | +0.40% |
| EPYC 9V74 | 32,427,602 | 32,261,824 | +0.51% |

The same compression-level number does not mean identical compression models.
The smaller CPU/RSS figures therefore do not establish better compression.
Native/reference QC values and fingerprints must match exactly in the harness.
FFmpeg loudness/true-peak comparisons use the documented tolerances; this is
not a claim of bit-identical meters or better measurement accuracy.

On each architecture, 22 native unit tests and nine native regression tests
passed; the reference build's 10 unit and nine regression tests passed.
Both builds passed qualification (596 checks each) and codec stress (288 each);
the native synthesized standards suite passed 20 cases. Extracted validation
summaries and exact standards results are in each raw report. Neither these
synthetic cases nor the benchmarks replace official EBU certification, a real
music corpus, concurrent service latency measurements or a Graviton4 hardware run.
Backend integration remains deferred as requested.

The raw bundle additionally preserves the six-format lossless conversion,
FLAC/ALAC decode/hash, fingerprint review, previous-native comparisons and
separate copy/buffer diagnostics. Earlier native storage/prediction changes and
rejected SIMD experiments remain documented in
[NATIVE-BOTTLENECKS.md](NATIVE-BOTTLENECKS.md).
