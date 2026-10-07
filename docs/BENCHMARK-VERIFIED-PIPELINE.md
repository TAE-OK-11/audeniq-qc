# Verified pipeline optimization and fair FFmpeg 8.1.2 comparison — 2026-10-05

Starting commit: `f097643` (main). Final engine commit: `26ff3bc` (round 1 ended at `6db5c04`)
(branch `perf/verified-pipeline-optimization`). All numbers below were
measured on one host in one session; they are not cross-host claims.

## Conditions

* Host: Intel Xeon @ 2.10 GHz (AVX-512-FP16 capable), **4 vCPU** VM, Linux;
  runtime backend AVX2 (no `target-cpu=native`). No hardware PMU counters.
* FFmpeg **n8.1.2** built from the upstream tag with x86 assembly enabled
  (`--disable-autodetect`, default codecs; nasm 2.16). Development oracle and
  comparator only.
* Real music: **CC0** recordings from the IETF CELLAR FLAC decoder test bench
  (`github.com/ietf-wg-cellar/flac-test-files`, subset): 44 distinct supported
  excerpts (44.1/48 kHz 16-bit stereo, 96/134.56/192 kHz 24-bit stereo, mono
  16/24-bit), decoded to WAV with FFmpeg and re-encoded to ALAC (M4A) and FLAC.
  Albums are concatenations by format (album44 205 s, album48 109 s, album96
  41 s, album192 16 s). Genres are not labelled by the source; the set mixes
  acoustic, electronic, orchestral and dense mastered material but is not a
  genre-balanced corpus. Synthetic: 240 s 48 kHz/24-bit tones and pink noise.
* Median of 7 (conversion) / 5 (QC) / 3 (per track) interleaved runs after a
  warm-up; warm page cache. CPU = user+system from `wait4` (us resolution),
  peak RSS of each engine process from GNU `time` (the harness's own memory
  excluded), wall from the harness.
* Harness: [`tools/scripts/fair_pipeline_bench.py`](../tools/scripts/fair_pipeline_bench.py).

## Workload-matched pipelines

Every engine does the same verified job: read source, decode, canonical PCM
SHA-256 (left-aligned s32le), FLAC level 5 with STREAMINFO MD5, **fsync**,
verify the published output reproduces the source PCM, publish.

| Engine | How verification is done |
| --- | --- |
| Native | Every frame is parsed and checked against its source block before it is written (residual check, see below); the fsynced file is re-read and must equal the verified frames (length + CRC32, STREAMINFO re-parsed); hard-link publish. One process. |
| Reference (Symphonia) | Same encoder; whole-file independent re-decode + PCM SHA-256 compare. |
| FFmpeg 8.1.2 | Process 1: one decode fanned out to the FLAC encoder and the SHA-256 hash muxer (`-threads 1` per codec). `sync FILE`. Process 2: re-decode the output from disk with `-err_detect crccheck+explode -xerror` and hash; hashes must match; `link()` publish. CPU/wall summed, RSS = max child. |

The FFmpeg CLI pipelines demux/decode/encode/mux on separate threads, so its
wall time is below its CPU time; native is single-threaded per job. Every
timed output from every engine was additionally decoded by FFmpeg with CRC
checking (untimed) and matched the source PCM SHA-256.

## Results (final engine 26ff3bc vs baseline f097643)

### Full verified conversion (no QC) — CPU s / wall s / peak RSS MiB

| Input | s | Baseline CPU / wall / RSS MiB | **Final** CPU / wall / RSS | Final vs base CPU | Reference CPU / wall / RSS | FFmpeg 8.1.2 CPU / wall / RSS | Final vs FFmpeg CPU | Final vs FFmpeg wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| album44.wav | 205 | 0.733 / 0.771 / 3.61 | **0.604 / 0.644 / 3.73** | -17.7% | 0.826 / 0.861 / 5.19 | 1.638 / 0.793 / 18.59 | -63.2% | -18.8% |
| album44.m4a | 205 | 1.391 / 1.427 / 3.57 | **1.128 / 1.166 / 3.75** | -18.9% | 1.505 / 1.546 / 5.44 | 2.322 / 1.098 / 18.10 | -51.4% | +6.2% |
| album44.flac | 205 | 0.978 / 1.015 / 3.66 | **0.837 / 0.873 / 3.93** | -14.5% | 1.069 / 1.110 / 5.46 | 1.719 / 0.721 / 18.35 | -51.3% | +21.0% |
| album48.wav | 109 | 0.405 / 0.428 / 3.57 | **0.332 / 0.346 / 3.75** | -18.1% | 0.459 / 0.480 / 4.40 | 0.900 / 0.428 / 18.68 | -63.1% | -19.3% |
| album48.m4a | 109 | 0.754 / 0.774 / 3.54 | **0.655 / 0.675 / 3.74** | -13.1% | 0.870 / 0.892 / 4.57 | 1.324 / 0.674 / 17.92 | -50.5% | +0.2% |
| album48.flac | 109 | 0.545 / 0.561 / 3.67 | **0.446 / 0.462 / 3.88** | -18.1% | 0.580 / 0.598 / 4.77 | 0.940 / 0.387 / 18.01 | -52.6% | +19.5% |
| album96.wav | 41 | 0.348 / 0.373 / 3.57 | **0.312 / 0.341 / 3.77** | -10.5% | 0.405 / 0.432 / 6.29 | 0.651 / 0.328 / 20.87 | -52.1% | +4.0% |
| album96.m4a | 41 | 0.569 / 0.601 / 3.89 | **0.538 / 0.566 / 3.97** | -5.5% | 0.694 / 0.719 / 6.33 | 0.972 / 0.467 / 18.57 | -44.6% | +21.2% |
| album96.flac | 41 | 0.481 / 0.516 / 3.80 | **0.428 / 0.453 / 4.02** | -11.0% | 0.523 / 0.550 / 6.43 | 0.691 / 0.311 / 19.19 | -38.0% | +45.6% |
| album192.wav | 16 | 0.275 / 0.295 / 3.55 | **0.244 / 0.266 / 3.77** | -11.2% | 0.321 / 0.341 / 5.46 | 0.481 / 0.256 / 25.14 | -49.2% | +3.8% |
| album192.m4a | 16 | 0.435 / 0.466 / 3.82 | **0.408 / 0.430 / 4.02** | -6.4% | 0.557 / 0.582 / 5.62 | 0.751 / 0.380 / 18.73 | -45.8% | +13.1% |
| syn_tone.wav | 240 | 0.938 / 0.982 / 3.50 | **0.839 / 0.883 / 3.64** | -10.6% | 1.107 / 1.169 / 5.36 | 1.945 / 0.895 / 19.27 | -56.9% | -1.3% |
| syn_tone.m4a | 240 | 1.529 / 1.572 / 3.63 | **1.380 / 1.429 / 3.89** | -9.8% | 1.763 / 1.816 / 5.38 | 2.741 / 1.291 / 18.67 | -49.6% | +10.7% |
| syn_pink.wav | 240 | 1.006 / 1.075 / 3.55 | **0.933 / 1.004 / 3.73** | -7.3% | 1.156 / 1.247 / 4.55 | 2.059 / 1.000 / 19.23 | -54.7% | +0.4% |
| syn_pink.m4a | 240 | 1.767 / 1.845 / 3.63 | **1.532 / 1.610 / 3.96** | -13.3% | 2.157 / 2.234 / 4.78 | 2.978 / 1.365 / 18.80 | -48.5% | +18.0% |

### FLAC bytes (level 5)

| Input | Baseline | Final | Final vs base | FFmpeg 8.1.2 | Final vs FFmpeg | Final bytes / PCM bytes |
|---|---:|---:|---:|---:|---:|---:|
| album44.wav | 17,082,411 | 17,084,652 | +0.0% | 17,144,474 | -0.3% | 0.4716 |
| album48.wav | 7,825,952 | 7,827,212 | +0.0% | 7,841,748 | -0.2% | 0.3727 |
| album96.wav | 16,463,997 | 16,464,270 | +0.0% | 16,474,306 | -0.1% | 0.7017 |
| album192.wav | 13,256,263 | 13,256,944 | +0.0% | 13,262,734 | -0.0% | 0.7191 |
| syn_tone.wav | 22,895,861 | 22,896,019 | +0.0% | 32,261,824 | -29.0% | 0.3313 |
| syn_pink.wav | 54,203,896 | 54,203,886 | -0.0% | 54,211,179 | -0.0% | 0.7842 |

### PURE CODEC (not end-to-end; no fsync/verification)

| Input | FFmpeg encode-only CPU | FFmpeg decode+SHA-256 CPU | Native decode+SHA-256 CPU |
|---|---:|---:|---:|
| album44.wav | 0.517 | 0.496 | 0.064 |
| album44.m4a | 1.147 | 1.167 | 0.590 |
| album44.flac | 0.672 | 0.637 | 0.299 |
| album48.wav | 0.280 | 0.296 | 0.037 |
| album48.m4a | 0.711 | 0.690 | 0.346 |
| album48.flac | 0.368 | 0.404 | 0.166 |
| album96.wav | 0.196 | 0.168 | 0.028 |
| album96.m4a | 0.494 | 0.473 | 0.241 |
| album96.flac | 0.296 | 0.274 | 0.156 |
| album192.wav | 0.153 | 0.132 | 0.024 |
| album192.m4a | 0.379 | 0.374 | 0.180 |
| syn_tone.wav | 0.603 | 0.567 | 0.078 |
| syn_tone.m4a | 1.460 | 1.369 | 0.610 |
| syn_pink.wav | 0.684 | 0.556 | 0.077 |
| syn_pink.m4a | 1.590 | 1.387 | 0.670 |

### Full verified conversion + QC (native --analyze; FFmpeg ebur128 true peak)

| Input | s | Baseline CPU / wall / RSS MiB | **Final** CPU / wall / RSS | Final vs base CPU | Reference CPU / wall / RSS | FFmpeg 8.1.2 CPU / wall / RSS | Final vs FFmpeg CPU | Final vs FFmpeg wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| album44.wav | 205 | 0.965 / 1.003 / 3.91 | **0.817 / 0.850 / 4.13** | -15.3% | 1.023 / 1.058 / 5.46 | 2.487 / 1.195 / 24.58 | -67.1% | -28.9% |
| album44.m4a | 205 | 1.494 / 1.530 / 3.91 | **1.298 / 1.334 / 4.14** | -13.1% | 1.685 / 1.713 / 5.58 | 3.031 / 1.250 / 23.61 | -57.2% | +6.7% |
| album44.flac | 205 | 1.196 / 1.232 / 4.07 | **1.058 / 1.093 / 4.08** | -11.5% | 1.259 / 1.301 / 5.64 | 2.382 / 1.077 / 23.07 | -55.6% | +1.4% |
| album48.wav | 109 | 0.542 / 0.556 / 3.93 | **0.434 / 0.448 / 4.10** | -19.8% | 0.545 / 0.559 / 4.73 | 1.275 / 0.629 / 24.61 | -65.9% | -28.8% |
| album48.m4a | 109 | 0.893 / 0.921 / 3.82 | **0.762 / 0.784 / 3.99** | -14.7% | 0.985 / 1.004 / 4.86 | 1.690 / 0.668 / 23.46 | -54.9% | +17.5% |
| album48.flac | 109 | 0.704 / 0.725 / 4.02 | **0.592 / 0.619 / 4.20** | -15.8% | 0.682 / 0.706 / 5.05 | 1.454 / 0.614 / 23.34 | -59.3% | +0.7% |
| album96.wav | 41 | 0.434 / 0.461 / 3.92 | **0.398 / 0.428 / 4.09** | -8.2% | 0.485 / 0.515 / 6.39 | 0.817 / 0.365 / 30.28 | -51.3% | +17.3% |
| album96.m4a | 41 | 0.686 / 0.721 / 4.00 | **0.651 / 0.683 / 4.09** | -5.0% | 0.787 / 0.827 / 6.45 | 1.190 / 0.480 / 27.39 | -45.3% | +42.1% |
| album96.flac | 41 | 0.566 / 0.601 / 4.03 | **0.508 / 0.533 / 4.12** | -10.2% | 0.633 / 0.666 / 6.67 | 0.887 / 0.377 / 28.02 | -42.7% | +41.5% |
| album192.wav | 16 | 0.322 / 0.342 / 3.67 | **0.287 / 0.307 / 3.94** | -10.9% | 0.372 / 0.394 / 5.75 | 0.546 / 0.270 / 38.05 | -47.5% | +14.0% |
| album192.m4a | 16 | 0.508 / 0.530 / 3.91 | **0.432 / 0.451 / 3.97** | -14.9% | 0.617 / 0.635 / 5.70 | 0.760 / 0.355 / 32.12 | -43.1% | +26.8% |
| syn_tone.wav | 240 | 1.183 / 1.218 / 3.78 | **1.033 / 1.069 / 4.00** | -12.7% | 1.316 / 1.365 / 5.63 | 2.919 / 1.307 / 24.77 | -64.6% | -18.2% |
| syn_tone.m4a | 240 | 1.830 / 1.882 / 3.94 | **1.613 / 1.653 / 4.28** | -11.9% | 2.079 / 2.141 / 5.68 | 3.586 / 1.336 / 24.04 | -55.0% | +23.7% |
| syn_pink.wav | 240 | 1.383 / 1.466 / 3.95 | **1.271 / 1.351 / 4.14** | -8.1% | 1.490 / 1.588 / 4.79 | 3.075 / 1.371 / 24.99 | -58.7% | -1.5% |
| syn_pink.m4a | 240 | 2.063 / 2.164 / 3.90 | **1.852 / 1.942 / 4.20** | -10.2% | 2.368 / 2.448 / 4.96 | 3.761 / 1.403 / 24.47 | -50.8% | +38.4% |

### FLAC input, production default (verified frame copy; native only)

| Input | Baseline CPU/wall/RSS | Final CPU/wall/RSS | Δ CPU |
|---|---:|---:|---:|
| album44.flac | 0.333 / 0.365 / 3.30 | 0.319 / 0.348 / 3.52 | -4.3% |
| album48.flac | 0.194 / 0.208 / 3.26 | 0.190 / 0.205 / 3.39 | -2.2% |
| album96.flac | 0.175 / 0.198 / 3.26 | 0.167 / 0.191 / 3.52 | -4.7% |
| syn_pink.flac | 0.569 / 0.643 / 3.26 | 0.548 / 0.633 / 3.50 | -3.7% |

### Concurrency (24 jobs over album44.wav/.m4a, album48.wav, album96.m4a; 4 vCPU)

| Engine | Workers | Audio s per wall s | CPU/job s | p50 wall s | p95 wall s | Peak total RSS MiB | Max child RSS MiB | Failures |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| native | 1 | 198.8 | 0.665 | 0.601 | 1.234 | 4.0 | 4.03 | 0 |
| native | 2 | 402.3 | 0.659 | 0.609 | 1.205 | 7.9 | 4.05 | 0 |
| native | 4 | 656.8 | 0.670 | 0.683 | 1.399 | 15.5 | 3.95 | 0 |
| native | 8 | 699.3 | 0.684 | 1.236 | 2.625 | 30.8 | 3.89 | 0 |
| baseline | 1 | 171.7 | 0.774 | 0.757 | 1.445 | 3.9 | 3.99 | 0 |
| baseline | 2 | 332.4 | 0.791 | 0.744 | 1.448 | 7.5 | 3.91 | 0 |
| baseline | 4 | 601.7 | 0.766 | 0.798 | 1.430 | 14.8 | 3.91 | 0 |
| baseline | 8 | 602.8 | 0.786 | 1.579 | 3.044 | 29.4 | 3.77 | 0 |
| ffmpeg | 1 | 178.5 | 1.465 | 0.720 | 1.255 | 19.3 | 19.21 | 0 |
| ffmpeg | 2 | 284.2 | 1.401 | 0.944 | 1.582 | 38.2 | 19.22 | 0 |
| ffmpeg | 4 | 353.2 | 1.351 | 1.595 | 2.462 | 76.0 | 19.28 | 0 |
| ffmpeg | 8 | 366.1 | 1.369 | 2.788 | 4.623 | 149.4 | 19.25 | 0 |

## Accepted optimizations

| Commit | Change | Output | Effect |
| --- | --- | --- | --- |
| `5e45114` | **Residual-check frame verification.** The encoder no longer runs the serial LPC/fixed reconstruction recurrence to verify a frame. `flac_decode::verify` parses the frame exactly like `decode` (CRC-8, headers, warm-up, coefficients, Rice partitions/escapes, padding, CRC-16) and requires each parsed residual to equal `x[i] - pred(x[..i])` computed with the decoder's arithmetic on the source signal; by induction the decoder would reproduce the source exactly when this holds. Stereo is checked in the forward direction (each mode is a bijection on in-range samples); wasted/low bits are checked explicitly. Checks are independent per sample and AVX2-dispatched. | Byte-identical | Verification share of WAV-to-FLAC CPU fell from ~26% to ~20% (Rice parsing, ~12%, is unchanged). |
| `34461da` | **One pass for all fixed-predictor orders.** Orders 0..4 per-partition sums from repeated differences in one traversal instead of five passes. | Byte-identical | 3-11% conversion CPU. |
| `3d36f9c` | **One exact LPC candidate fewer.** Levels 4/5 exactly cost the highest order plus the best 128-point-sampled order (was best two). | +0.0007% to +0.016% bytes on real music | 3-8% conversion CPU. Costing all orders exactly would save only 0.015% more bytes for ~20% more CPU. |
| `875f362` | **Denormal flush off the K-weighting dependency chain** (cold branch instead of abs/compare/mask select). | QC JSON byte-identical | 4-12% standalone `analyze` CPU. |
| `6db5c04` | **Stereo mode hoisted out of FLAC reconstruction** with a 32-bit OR range accumulator. Fixes a 2-4% FLAC decode regression caused by `5e45114` changing LLVM's inlining of `decode`. | PCM identical | FLAC decode back to baseline. |
| `e8a9ddf` | **FLAC Rice parsing with LZCNT/BMI2** (runtime dispatch; same safe code). Baseline x86-64 lowered `leading_zeros` to BSR + fix-ups and variable shifts to 3-uop `shl/shr cl` on the serial per-code chain. | PCM identical | FLAC decode -4..-7% vs 6db5c04; also speeds frame verification. |
| `cbe2396` | **ALAC adaptive Rice with LZCNT/BMI2** (runtime dispatch). | PCM identical | ALAC decode -3..-10% vs 6db5c04. |

Equivalence evidence for `5e45114`: a new test encodes constant, verbatim,
fixed, LPC, wasted-bit, extreme and all stereo-assignment blocks at levels
0/3/5/8 (mono/stereo, 16/24-bit, full/17/1-sample blocks) and requires
`verify` and `decode`+compare to agree on more than 10,000 CRC-repaired bit
flips (over 1,000 of which decode cleanly to *different* PCM), source sample
and low-bit perturbations and truncations. The full decoder is unchanged and
still decodes FLAC input; the reference build keeps whole-file re-decode, and
CI keeps independent FFmpeg cross-decoding of outputs.

## Rejected experiments

| Experiment | Result | Reason |
| --- | --- | --- |
| ALAC predictor: single loop merging the error-sign branch (0.48 mispredicts/sample by cachegrind) | 0.65 → 0.82 s decode | Slower; longer per-sample chain. |
| ALAC predictor: two stereo channels interleaved (ILP across independent recurrences) | -1.6% / 0.0% / -4.2% | Within noise; extra code. Previously rejected NEON/prefix designs were not repeated. |
| ALAC Rice: cold branch for the rare history reset | +1.1% / +0.4% / -0.1% | No gain. |
| FLAC Rice reader: one shift per code (cache << q+1+k) | +0.2% / +2.7% / 0.0% | No gain; loop is lzcnt latency bound. |
| AVX2 dispatch of fused fixed sums / stereo estimate | 0.0% / +1.9% / +1.0% | No gain over SSE2 auto-vectorization. |
| Adaptive LPC margin (cost 2nd sampled order only when estimates are close) | size recovered, CPU gain lost in noise | Not reliably better than plain best-1. |
| md-5 `asm` backend | ~15% faster MD5 (~1.5-2% of conversion) | x86-only C/asm build dependency for a small gain. |
| Round 2: FLAC subframe/restore under LZCNT/BMI2 | +0.6% / +1.5% / +6.2% decode | Slower; only the Rice parser benefits. |
| Round 2: ALAC predictor under LZCNT/BMI2 | -6% / -0.4% / +8% vs Rice-only | Inconsistent. |
| Round 2: `choose_rice` and `rice_block` writer under LZCNT/BMI2 | -0.1..+4.1% / +3.3..-5.1% | No consistent gain. |
| Round 2: single-shift Rice step on BMI2 build | no gain over BMI2 two-shift | Chain unchanged in practice. |
| Round 2: fused order-8 + lower-order exact LPC costing in one pass | +0.8% / +2.0% / +4.3% | Two accumulators raise register pressure; separate kernels vectorize better. |
| QC planes via chunked deinterleave | +4.8% / +5.3% / -6.7%, more RSS | Slower on two of three inputs. |


### Per-track summary by group (real music, final vs baseline)

| Group | Audio s | Baseline CPU/audio-s | Final CPU/audio-s | Change | FFmpeg 8.1.2 CPU/audio-s |
| --- | ---: | ---: | ---: | ---: | ---: |
| 44.1/48 kHz 16-bit ALAC | 325 | 0.00671 | 0.00589 | -12.2% | 0.01382 |
| 44.1/48 kHz 16-bit WAV | 320 | 0.00404 | 0.00350 | -13.5% | 0.01361 |
| 96-192 kHz 24-bit ALAC | 65 | 0.01870 | 0.01749 | -6.5% | 0.03437 |
| 96-192 kHz 24-bit WAV | 65 | 0.01151 | 0.01041 | -9.6% | 0.02259 |

Short excerpts (5-60 s) include process start-up; the albums above are the
better steady-state measure.

## Correctness and gates (final commit)

* `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`
  (native and `reference-codecs`), debug and release `cargo test --workspace`:
  native 28 unit + 10 regression tests, reference 12 + 10. All pass.
* Qualification 596/596, codec stress 288/288, synthesized standards pass
  ([qualification.json](verified-pipeline/qualification.json),
  [codec-stress.json](verified-pipeline/codec-stress.json)).
* Mutation fuzz ([`tools/scripts/mutation_fuzz.py`](../tools/scripts/mutation_fuzz.py), rerun on `26ff3bc`):
  1,600 mutated WAV/ALAC/FLAC files x `pcm-hash`/`analyze`/`convert`, zero
  crashes, signals or timeouts; all 327 accepted conversions re-decode in
  FFmpeg to the reported hash. Outcome counts are identical for the baseline
  and final binaries (same seed), i.e. malformed-input behaviour is unchanged.
* GitHub Actions on x86 (`ubuntu-24.04`) and Arm (`ubuntu-24.04-arm`): native
  qualification, native-vs-reference media codecs and WAV/ALAC benchmark
  workflows passed (the Arm job exercises the non-AVX2 verifier path).
* Output: real-music FLAC differs from baseline only by `3d36f9c`
  (+0.0007% to +0.016%); native remains 0.04-0.35% smaller than FFmpeg 8.1.2
  on real music and 29% smaller on the predictable tone.

RSS: peak RSS per process is 0.1-0.27 MiB (3-7%) higher than baseline, and
all of it is file-backed executable text. Sampling \`/proc/PID/status\` during
a 240 s ALAC conversion gave private RssAnon of 800-828 KiB for the baseline
and 784-792 KiB for the final build, while RssFile rose from 3.0-3.1 MiB to
3.2-3.3 MiB. The binary's text grew 48 KiB (per-order scalar/AVX2 verifier
kernels and LZCNT/BMI2 copies of the Rice readers), and kernel fault-around
maps more of it. Those pages are shared through the page cache by concurrent
jobs; private memory per job did not grow. Heap growth measured with massif
is +10 KiB.


## Round 3: hypothesis checks (engine `01973f4`)

Two hypotheses from the round-2 hotspot list were tested before building.

**H2, table-driven multi-code FLAC Rice decoder: refuted by measurement.**
Instrumented decoding of the corpus gave the code-length distribution the
design depends on. 16-bit CD material uses k = 5..7 for 68% of codes (mean
7.5 bits per code, mean unary quotient about 1). 24-bit high-resolution
material uses k = 14..15 (mean 17 bits). A lookup table of practical size
(12..16 index bits) would decode about one code per lookup, replacing a
3-cycle LZCNT with a ~5-cycle dependent load, and small-k codes where
multi-code lookups pay off are about 6% of codes. It was not built.

An alternative that cuts instructions per code instead was built and
measured: storing folded codes and undoing the zig-zag in a separate
vectorized pass, plus a quotient-OR range check (value < 2^32 iff
q < 2^(32-k)). First run: -1.4..-1.9%. Repeat: -0.3..+0.3% decode,
+2.8..-1.2% conversion. Not reproducible, rejected. The FLAC Rice reader is
at its practical floor for this design.

**H3, keep the winning LPC residual from costing: confirmed and accepted.**
Premise: the writer recomputed the winning LPC residual (`lpc_store`, 3.0%
of WAV-to-FLAC CPU). The first version stored every costed candidate's
residual. It saved 1.7-2.6% CPU but raised private memory from 660 to 720
KiB (RssAnon, sampled during a 240 s conversion), which violates the
memory constraint. Planning and writing each subframe in turn reduced that
to +16 KiB. The accepted version costs candidates from the highest order
down, stores only the first (order 8, which wins most real-music
subframes), and sums the others only. A lower order replaces an equal-cost
LPC model, which selects exactly the same model. Results:

* Output: byte-identical at levels 4, 5 and 8 on nine inputs.
* Private memory: RssAnon 660 KiB before and after.
* CPU: WAV-to-FLAC -2.4..-4.7%; ALAC input -0.3% (decode dominated).

### Cumulative full verified conversion (no QC) — CPU s / wall s / peak RSS MiB

Baseline `f097643`, final `01973f4`, reference column built at `26ff3bc`
(before the H3 encoder change), FFmpeg n8.1.2 with the workload-matched
pipeline. Median of 7 interleaved runs. Raw data:
[round3-convert.json](verified-pipeline/round3-convert.json),
[round3-tracks.json](verified-pipeline/round3-tracks.json).

| Input | s | Baseline CPU / wall / RSS MiB | **Final** CPU / wall / RSS | Final vs base CPU | Reference CPU / wall / RSS | FFmpeg 8.1.2 CPU / wall / RSS | Final vs FFmpeg CPU | Final vs FFmpeg wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| album44.wav | 205 | 0.695 / 0.729 / 3.63 | **0.539 / 0.571 / 3.74** | -22.6% | 0.747 / 0.779 / 5.05 | 1.393 / 0.671 / 18.24 | -61.3% | -15.0% |
| album44.m4a | 205 | 1.286 / 1.323 / 3.56 | **1.085 / 1.140 / 3.75** | -15.6% | 1.408 / 1.455 / 5.25 | 2.220 / 1.112 / 18.09 | -51.1% | +2.5% |
| album44.flac | 205 | 0.944 / 0.972 / 3.76 | **0.788 / 0.819 / 3.87** | -16.6% | 0.986 / 1.026 / 5.24 | 1.644 / 0.672 / 18.14 | -52.1% | +21.8% |
| album48.wav | 109 | 0.401 / 0.417 / 3.54 | **0.316 / 0.333 / 3.70** | -21.1% | 0.440 / 0.460 / 4.36 | 0.880 / 0.421 / 18.55 | -64.0% | -20.9% |
| album48.m4a | 109 | 0.736 / 0.757 / 3.54 | **0.631 / 0.661 / 3.77** | -14.2% | 0.865 / 0.885 / 4.38 | 1.385 / 0.692 / 17.70 | -54.4% | -4.5% |
| album96.wav | 41 | 0.382 / 0.411 / 3.57 | **0.302 / 0.330 / 3.70** | -20.9% | 0.410 / 0.448 / 6.10 | 0.679 / 0.360 / 21.05 | -55.5% | -8.2% |
| album96.m4a | 41 | 0.600 / 0.632 / 3.82 | **0.503 / 0.534 / 3.96** | -16.2% | 0.714 / 0.746 / 6.23 | 0.929 / 0.449 / 18.90 | -45.8% | +18.9% |
| album192.wav | 16 | 0.293 / 0.315 / 3.57 | **0.234 / 0.256 / 3.72** | -20.1% | 0.325 / 0.348 / 5.44 | 0.469 / 0.257 / 24.93 | -50.2% | -0.4% |
| syn_tone.wav | 240 | 0.931 / 0.975 / 3.41 | **0.817 / 0.853 / 3.55** | -12.3% | 1.101 / 1.146 / 5.13 | 2.053 / 0.956 / 19.07 | -60.2% | -10.8% |
| syn_pink.wav | 240 | 1.089 / 1.171 / 3.58 | **0.922 / 0.999 / 3.70** | -15.3% | 1.208 / 1.282 / 4.39 | 2.173 / 1.049 / 19.06 | -57.6% | -4.8% |

Totals over 88 files (780 s audio): baseline 5.58 s CPU, final 4.76 s (-14.8%), FFmpeg 12.78 s (final -62.8%).

Gates on `01973f4`: fmt, clippy (native and reference), debug and release
tests (28 + 10), qualification 596/596, codec stress 288/288, standards
pass. The 1,600-case mutation fuzz found no crashes and its outcome counts
are identical to the baseline's.


## Round 4: in-repository MD5/SHA-256 and fused PCM hashing (`11e6b10`)

The FLAC STREAMINFO MD5 and the canonical PCM SHA-256 are now implemented
in this repository, and RustCrypto md-5/sha2 are dev-only test oracles.

| Commit | Change | Effect |
| --- | --- | --- |
| `03861db` | **MD5 (RFC 1321)**, with round functions arranged to depend on the previous step as late as possible. | 525-575 vs 480-495 MB/s (md-5 soft); FLAC decode -1..-3%. |
| `c321381` | **SHA-256 (FIPS 180-4)** with SHA-NI, ARMv8 SHA2 and portable engines, fully unrolled. A rotating-array loop measured 15% slower than sha2 and was replaced. | Parity with sha2 (1.43-1.44 GB/s); 11 KiB less text. |
| `3cb20ba` | **One interleaved pass for SHA-256 and MD5.** See below. | FLAC decode -5..-10%, FLAC re-encode -13.7%, WAV/ALAC conversion -3..-5%. |
| `11e6b10` | **MD5 constants off the dependency chain.** LLVM moved the constant addend after `f`, adding an extra add to every step; the constants are now read through `black_box`. | MD5 555 -> 600 MB/s; a further -2..-6%. |

**How the fused pass works.** SHA-256 runs on the SHA unit and MD5 on the
scalar ALUs, and each is one serial chain per block. As separate passes,
their costs added up. `src/hash_fused.rs` (generated by
`tools/scripts/gen_fused_hash.py`) interleaves MD5 steps with SHA-NI
four-round groups: 1 MD5 block per 2 SHA blocks for 16-bit, 3 per 4 for
24-bit. In isolation this saves 16-24% of combined hashing time. The ideal
saving (about 45% at 16 bits, bounded by MD5 alone) is not reached because
the two chains share execution resources; burst sizes 1/2/4/8 measured the
same.

Where the fused pass is used:
* `convert` hashes both in one pass.
* Native FLAC input computes the caller's SHA-256 inside the decoder,
  fused with its STREAMINFO MD5 check.
* FLAC re-encode reuses the source's verified STREAMINFO MD5 (same compact
  PCM) instead of a second MD5 pass.

Correctness checks:
* Digests and output files are identical to before.
* Tampered source MD5s are still rejected on every command.
* A new regression test re-encodes 16- and 24-bit FLAC with known, unset
  and tampered source MD5s and compares STREAMINFO with FFmpeg's MD5.
* aarch64 passes all library tests under QEMU with SHA2 detected.
* Private memory (RssAnon) is unchanged or lower; file-backed text grew by
  about 16 KiB of kernel code.

### Round 4 vs current main `01973f4`, workload-matched — CPU s / wall s (/ RSS MiB)

| Input | main | **round 4** | Change | FFmpeg 8.1.2 | vs FFmpeg CPU | vs FFmpeg wall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| album44.wav | 0.587 / 0.620 | **0.548 / 0.578 / 3.79** | -6.7% | 1.698 / 0.848 / 18.40 | -67.7% | -31.8% |
| album44.m4a | 1.084 / 1.124 | **1.023 / 1.073 / 3.77** | -5.6% | 2.196 / 1.085 / 17.96 | -53.4% | -1.1% |
| album44.flac | 0.825 / 0.860 | **0.710 / 0.748 / 3.92** | -13.9% | 1.761 / 0.764 / 18.36 | -59.7% | -2.1% |
| album48.wav | 0.359 / 0.375 | **0.315 / 0.338 / 3.75** | -12.3% | 0.956 / 0.486 / 18.56 | -67.1% | -30.6% |
| album96.wav | 0.296 / 0.328 | **0.279 / 0.306 / 3.88** | -6.0% | 0.634 / 0.337 / 20.83 | -56.1% | -9.2% |
| album96.m4a | 0.540 / 0.571 | **0.533 / 0.574 / 3.90** | -1.3% | 1.039 / 0.525 / 18.81 | -48.7% | +9.4% |
| album96.flac | 0.433 / 0.461 | **0.389 / 0.421 / 3.94** | -10.2% | 0.755 / 0.356 / 19.32 | -48.5% | +18.1% |
| syn_pink.wav | 0.891 / 0.964 | **0.854 / 0.936 / 3.93** | -4.2% | 2.082 / 0.980 / 19.23 | -59.0% | -4.5% |

FLAC input, production default (verified frame copy), CPU s:

| Input | main | round 4 | Change |
| --- | ---: | ---: | ---: |
| album44.flac | 0.313 | 0.293 | -6.5% |
| album48.flac | 0.176 | 0.158 | -10.6% |
| album96.flac | 0.167 | 0.146 | -12.7% |
| syn_pink.flac | 0.530 | 0.469 | -11.5% |

Real-music per-track total (88 files, 780 s): main 4.70 s, round 4
4.53 s (-3.6%), FFmpeg 8.1.2 12.59 s (round 4
-64.0%). FLAC decode + SHA-256 alone: -10.5..-13.7%
([round4-decode.txt](verified-pipeline/round4-decode.txt)); ALAC and WAV
decode are unchanged within noise.

Gates on `11e6b10`: fmt, clippy (native, reference, aarch64), debug and
release tests (34 + 11; reference 18 + 11), qualification 596/596, codec
stress 288/288, standards pass. The 1,600-case mutation fuzz found no
crashes, with outcome counts identical to the baseline's.

## Round 5: in-repository IEEE CRC-32 (`src/crc32.rs`)

crc32fast was already well optimized (AVX-512/AVX2 VPCLMUL, 3-way AArch64
CRC instructions), so the acceptance rule was: keep the replacement only if
it is at least as fast in the real pipeline on every measured CPU; a 1-2%
pipeline slowdown would reject it. CRC-32 is used by the verified FLAC
frame copy (once over the frames as written and once over the re-read
published file) and by TTA header/frame checks.

Design: carry-less-multiply folding with four accumulators (16 lanes with
AVX-512 VPCLMULQDQ, 8 with AVX2 VPCLMULQDQ, 4 with PCLMULQDQ or PMULL),
XORs fused with `vpternlogq` on AVX-512, and the lanes combined in a tree of
independent folds (crc32fast folds its 16 lanes one after another). The
final 128 bits are reduced with Barrett (x86) or the CRC32 instructions
(AArch64); inputs under 64 bytes use slicing-by-8. Tests compare every
engine with a bitwise definition and crc32fast for all lengths 0..1199,
offsets, every split point and large sizes; aarch64 was tested under QEMU
with and without SHA3.

Microbenchmark vs `crc32fast::hash`, GB/s (raw:
[round5-crc32.txt](verified-pipeline/round5-crc32.txt)):

| CPU (engine) | 200 B | 1 KB | 4 KB | 12 KB | 64 KB | 1 MB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Intel Xeon 6 (AVX-512) | 30.3 vs 11.5 | 69.2 vs 40.9 | 110.5 vs 92.5 | 112.2 vs 92.1 | 94.5 vs 93.6 | 95.1 vs 95.9 |
| AMD EPYC 7763 (AVX2) | 8.4 vs 6.8 | 22.3 vs 15.7 | 25.0 vs 24.0 | 25.5 vs 24.5 | 25.6 vs 25.5 | 25.6 vs 25.5 |
| Neoverse N2 (PMULL) | 20.3 vs 12.9 | 21.2 vs 22.2 | 21.8 vs 9.7 | 21.7 vs 13.4 | 21.6 vs 23.2 | 21.3 vs 26.6 |

On N2 the PMULL loop is bound by PMULL throughput: SHA3 `EOR3` and eight
accumulators both measured slower (18.1 GB/s) and were rejected. crc32fast
is faster there from 64 KB up (3-way CRC instructions), but its combine
step makes it 2x slower at FLAC frame sizes, which dominate the frame log.

Verified frame copy, paired CPU-time difference (new vs crc32fast):

| CPU | 16-bit 44.1 kHz | 24-bit 96 kHz |
| --- | ---: | ---: |
| AMD EPYC 7763 (2 runs) | -3.2%, -3.3% | -1.7%, -1.8% |
| Neoverse N2 (final) | -0.33% | -0.05% |
| Intel Xeon 6 | -0.35% | +0.45% |
| Local Xeon, real albums (3) | +0.43%, +0.68%, -0.03% (all CIs include 0) | |

The 16-bit file runs CRC-32 over about 38 MB (frames written, then the
re-read file): roughly 0.2% (Xeon 6), 0.5% (EPYC) and 1% (N2) of its
frame-copy CPU. Differences beyond that share (the EPYC -3%, the Xeon 6
+0.45% where the new code is faster at every size) are code placement
rather than CRC throughput. No CPU shows a slowdown attributable
to CRC-32; the replacement is kept. crc32fast stays in the build as a
transitive dependency of the PNG stack and as a test oracle.

## Round 6: QC meter (`04e2731`)

**Profiling (real music, `perf` cpu-clock).** With QC on, the meter was the
largest single function in every workload: `meter::Analyzer::push` 56% of
`analyze` on WAV and 34% on FLAC, 21% of `convert --analyze` on WAV and 33%
on FLAC. The true-peak FIR (`peak_avx2`) added 11-18%. Without QC,
conversion is spread over the encoder, hashing (at its floor since round 4)
and frame verification, with no dominant line.

**Stage breakdown of the meter.** The loop-carried values (filter state,
block peak, channel peaks) went through memory every frame, so
store-forwarding sat on their chains; `f64::max` ran NaN-handling sequences
per sample; the filter coefficients were spilled to the stack; and
true peak ran its 48 multiply-adds per sample on every sample.

| Change | Effect (album44.wav `analyze` CPU) |
| --- | --- |
| Frames processed per 50 ms block segment with state and sums in locals; peak, clip and zero-crossing tests on integers (thresholds equal to the f64 tests on `s / 2^31`, an exact scaling) | 0.297 -> 0.243 s |
| Sample peak as an integer max over f32 bit patterns; channels split in one pass | 0.234 -> 0.212 s |
| True-peak span bound: \|output\| <= max\|x\| x 2.0228 (largest phase L1 norm, with f32 rounding margin); 256-output spans that cannot exceed the running maximum are skipped (75-95% on real music) | -21% on real music; synthetic noise unchanged |
| Stereo K-weighting with both channels in one SSE2/NEON register inlined in the loop (x86 previously scalar per lane; AArch64 previously an indirect call per frame) | -13..-19% |
| Rejected: AVX-512 true-peak kernel | +7..+11% (slower) |

Every change preserves each floating-point operation and its order, or
replaces a comparison with an exactly equivalent one, so all outputs are
bit-identical: 450 corpus comparisons (`analyze`, `--fingerprint`,
`convert --analyze` JSON and FLAC bytes) on x86, AArch64 under QEMU against
its previous build, and both GitHub runners. New tests pin the integer
thresholds, the f32 conversion, the SSE2/NEON filter against scalar bits,
the L1 bound against the coefficients, and skipped against unskipped true
peak (a deliberately wrong bound fails it).

Results vs main `ba0155b` (raw:
[round6-qc-meter.txt](verified-pipeline/round6-qc-meter.txt)):

| Workload | Local Xeon (real music) | GitHub x86 | GitHub Neoverse N2 |
| --- | ---: | ---: | ---: |
| `analyze` WAV 44.1/16 | -50.9% | -49.4% | -34.6% |
| `analyze` WAV 96/24 | -49.4% | -47.4% | -33.9% |
| `analyze` FLAC | -30.2% | -35.2% | -23.5% |
| `analyze` ALAC | -23.7% | | |
| `convert --analyze` WAV | -19.0% | -24.2% | -16.4% |
| `convert --analyze` FLAC | -35.0% | -33.6% | -22.8% |
| `convert --analyze` ALAC | -15.5% | | |
| `convert` (no QC) | +2.6% / +0.1% (CIs include 0) | | |
| 44 real tracks x 3 formats, `analyze` total | -24.5% | | |
| 44 real tracks x 3 formats, `convert --analyze` total | -15.7% | | |

After this round `analyze` on WAV is about 46% meter and 37% SHA-256. The
meter runs at about 7 ns per stereo frame, close to its floor: the
K-weighting recursion is 1 multiply and 4 dependent subtractions per
frame (about 20 cycles), and its order is fixed by bit-identical output.

## Round 7: in-repository JPEG/PNG cover validation

`probe::cover` (the `image-probe` command and covers found by `probe`) now
uses `src/jpeg.rs`, `src/png.rs` and `src/inflate.rs` instead of image
0.24.9; image, png, jpeg-decoder, fdeflate and crc32fast are no longer in
the default build (binary 1.49 -> 1.14 MB). The goal was the same accept /
reject decision and reported size as before, not a stricter or looser one:

* PNG: everything png 0.17 checked before the end of the first image-data
  run (chunk order and CRCs, per-chunk rules, zlib header, complete DEFLATE
  stream with the Adler-32 present but unchecked, every row's filter type,
  APNG frame-size rules, its memory budget), and nothing after it, as png
  0.17 never read further. Empty chunks are not parsed, as there.
* JPEG: jpeg-decoder 0.3.2's marker and segment rules and a full Huffman
  decode of every scan with the same 64-bit bit reader, whose look-ahead
  decides where markers are found and pads zero bits after one; restart
  markers, EOB runs, progressive refinement (coefficients kept) and the
  component and colour-transform requirements at EOI. Pixels are not
  reconstructed (IDCT, upsampling and colour conversion cannot fail).
* Files on which the previous decoder **panicked** (abort in release
  builds): indexed PNGs with a PLTE length that is not a multiple of three
  or over 768, and lossless JPEGs whose precision image mis-sized. These are
  now rejected with an error instead of crashing the process.

Differential runs against the previous engine binary (stdout JSON and exit
status) over 250 generated covers (every PNG colour type and bit depth,
palette and tRNS, interlace; baseline, progressive, grey, CMYK, restart and
subsampling JPEGs; five sizes), 254 generated lossless JPEGs and 35,602
mutations (random, PNG chunk-aware with recomputed CRCs and recompressed
IDAT, JPEG segment-aware): **no difference except the 376 previous panics**
([raw](verified-pipeline/round7-covers.txt);
scripts in `tools/scripts/cover_differential/`). `tests/cover_oracle.rs`
repeats this on committed fixtures and 3,060 deterministic mutations with
image as a dev-dependency oracle on every CI run.

CPU (median of 15) and peak RSS, previous -> in-repository:

| Cover | CPU ms | RSS MiB |
| --- | ---: | ---: |
| PNG 600x600 (0.3 MB) | 6.4 -> 4.9 | 10.1 -> 10.1 |
| PNG 1400x1400 (1.5 MB) | 26.9 -> 17.8 | 10.3 -> 10.3 |
| JPEG 600x600 baseline | 9.2 -> 2.5 | 10.3 -> 10.5 |
| JPEG 1400x1400 baseline | 34.1 -> 7.4 | 14.7 -> 10.5 |
| JPEG 1400x1400 progressive | 62.6 -> 31.3 | 26.0 -> 14.4 |
| JPEG 4000x4000 baseline | 278.7 -> 81.3 | 95.2 -> 10.5 |
| JPEG 4000x4000 progressive | 590.6 -> 353.6 | 186.7 -> 98.9 |
| PNG 4000x4000 noise (24.7 MB, incompressible) | 146.2 -> 209.6 | 49.3 -> 26.6 |

The one slower case is a near-incompressible PNG at the 64 MiB input limit's
scale: fdeflate's literal decoding is faster than this decoder's; typical
covers are faster and no case uses more memory.

## Round 8: in-repository JSON (`src/json.rs`)

Reports are written by `src/json.rs` instead of serde 1.0.219 and
serde_json 1.0.140; the default build now has **no third-party crates**
(`cargo tree --locked -e normal` lists only the two workspace crates).
Report types implement `ToJson` through `json_struct!` / `json_enum!`, which
write fields in declaration order with the same `skip_serializing_if`
rules, so no intermediate `Value` tree is built. A `Value` (sorted map, as
serde_json's default), `json!` and a parser serve the probe output and the
tools.

The output format is serde_json's, byte for byte: escapes (`\"`, `\\`,
`\n`-style and lowercase `\u00xx`), integers, `null` for non-finite floats,
and ryu's float text. Float digits come from Rust's own shortest
round-trip `{:e}`; where the exact value lies halfway between two shortest
candidates (Rust rounds up, ryu to even) the tie is detected and resolved
to even, and ryu's positional/exponent layout (f64 up to 1e16 and down to
1e-5, f32 up to 1e13 and down to 1e-6) is applied.

* Engine differential: 2,931 command outputs over the corpus, **0
  differences**; qualify and codec-stress reports identical; mutation
  fuzzing gives the previous binary's counts.
* One improvement in the tools: the standards report read back an engine
  float with serde_json, whose default float parser is not correctly
  rounded (about 30% of shortest-digit floats come back 1 ULP off;
  `3.0296717961197754` read as `3.029671796119775`). The new parser is
  correctly rounded, so that report now shows the engine's exact value.
* Unit tests compare against serde_json (dev-dependency oracle): 1M random
  f64 and 1M random f32 plus edge values, strings, integers, `Value` trees
  and parse round trips.

Paired CPU, previous -> in-repository (median ratio, 95% CI): fingerprint
(4.6 MB of JSON) -0.8% [-2.6, +1.2], `analyze --fingerprint` +1.0%
[-6.1, +3.7], `analyze` +0.2% [-1.5, +1.5], `convert --analyze` +0.6%
[-0.9, +2.7]: no measurable difference. The first version cost +3.9% on
fingerprint and was fixed with a two-digit integer writer and a cheap
tie pre-check that skips the exact expansion for almost every float.
Float formatting alone is still about 3x slower than ryu (bounded by Rust's
`{:e}`), which no current report has enough floats to show. A clean
release build of the engine takes 6.2 s instead of 13.3 s (no proc-macro
derive). [Raw](verified-pipeline/round8-json.txt).

## Round 9: profiling the in-repository components

Every command was profiled again on real music (perf cpu-clock sampling; the
VM has no hardware counters), first as whole commands and then per function
and per source line, and each in-repository component was checked against its
floor. Three were not at it; everything else was (see the list below).

| Change | Where it showed | Effect |
| --- | --- | --- |
| FLAC CRC-16 folded with carry-less multiplication (`src/crc16.rs`): the non-reflected form of the CRC-32 folding, PCLMULQDQ / PMULL, four lanes, constants x^n mod P computed at compile time; the 128-bit residue and the tail go through the slicing table | 3-4% of every FLAC decode and of the encoder's frame CRC | 17.1 -> 1.6 ms on album44 (about 10x) |
| LPC restore with the last N samples in a register ring and the loop unrolled N times; the residual is added before the shift ((s + r*2^k) >> k == (s >> k) + r) and the newest product last, all exact in i64 | 22-26% of FLAC decode; the old loop was at one multiply per tap plus a store-to-load round trip per sample | order 8: 67.7 -> 41.8 ms; order 7: 25.4 -> 16.2 ms |
| Fingerprint tap: the next output's input count is cached instead of recomputed per input; the dedicated `fingerprint` command counts, without mixing, the inputs that no retained 30 s window reads; `round()` (a libm call) replaced by an exact branch-free equivalent | 8-36% of fingerprint workloads | tap 67.6 -> 23.0 ms, mixing 43.5 -> 11.3 ms, roundf 9.7 -> 0 ms |

Paired CPU, previous -> this round (median ratio, 95% CI):

| Command | Input | Change |
| --- | --- | --- |
| `pcm-hash` | album44.flac / album96.flac | -14.2% [-16.8, -11.1] / -19.8% [-22.2, -18.3] |
| `analyze` | album44.flac / album44.wav / album44.m4a | -12.0% [-18.4, -9.1] / -0.7% (noise) / 0.0% (noise) |
| `analyze --fingerprint` | album44.flac / album44.wav | -18.7% [-22.5, -9.4] / -13.0% [-20.0, -6.0] |
| `fingerprint` | album44.flac / album44.wav | -23.5% [-28.8, -18.4] / -46.7% [-53.5, -39.6] |
| `convert` | album44.flac / album44.m4a / album44.wav | -11.3% [-17.6, -8.7] / -6.6% [-9.2, +0.3] / -2.0% (noise) |

Peak RSS is unchanged within run-to-run variation (binary +3.9 KB). WAV and
ALAC inputs gain little because their time is in the meter, SHA-256 and the
ALAC predictor, which earlier rounds left at their floors.

Outputs are unchanged: 2,931 corpus command outputs identical to the previous
binary; `analyze --fingerprint`, `fingerprint` and `pcm-hash` identical on x86
and AArch64 (QEMU); qualify, codec-stress and standards reports identical;
mutation-fuzz counts identical. New tests compare the CRC-16 with a bit-wise
definition for every length up to 1,200 bytes at four offsets, the
fingerprint's skipped inputs with pushing every input (several rates, lengths
around the three-window threshold, any stretch size), and the rounding
replacement with `round().clamp()` on 6 million values and the special
cases. The LPC tests already cover every order, width, shift and short tail.

Measured and rejected this round: the register-window LPC without unrolling
(slower, extra moves), four fingerprint dot products per call (+10..13%), a
branchy rounding replacement (+13..16%, mispredicts), and a fixed-width JSON
integer copy (slower). [Raw](verified-pipeline/round9-profile.txt).

## Round 10: FLAC encoder, frame verification and ALAC decoder

Conversion was profiled stage by stage (new `profile-native` encoder stages)
and per source line. The encoder's planning still had the structure of its
FFmpeg flacenc origin; it now uses its own kernels and data flow, and the
frame verification uses a different method:

| Stage (convert album44.wav) | Before | After | Design |
| --- | ---: | ---: | --- |
| Fixed predictors | 96 ms | 31 ms | Partition sums of all five orders accumulate in u32 lanes where the residual bound proves they fit; level 5 searches Rice partition orders up to 6 (was 8) |
| LPC exact costing | 96 ms | 45 ms | Residuals of the whole block per candidate, in i32 lanes when sum\|c\| x max\|x\| < 2^30 (exact), else i64; the non-kept candidate in 64-sample stack pieces |
| Levinson + sampled costs | 43 ms | 25 ms | Sample positions stepped without divisions; constant-order sample kernels |
| Stereo estimate | 28 ms | 12 ms | One pass for the four second-order sums, u32 runs where bounded |
| Rice parameter choice | 22 ms | 6 ms | Shifted sums in u32 lanes when the partition sum fits |
| Rice writing | 58 ms | 34 ms | Output sized once from the exact bit count; each code ORed into a word that is stored as 8 bytes unconditionally (two codes per store), byte-granular advance, BMI2 shifts; long zero runs are skipped in the zeroed buffer |
| Frame verification | 109 ms | 68 ms | Instead of decoding each Rice code after the previous one, the expected residuals are derived from the source with the decoder's arithmetic, every code's position follows from them, and the bits at each position are compared (pairs of codes per load). A decoder reading those bits takes the same codes at the same places, so acceptance is unchanged |
| Encoder total | 511 ms | 277 ms | |

Encoded bytes are unchanged except through the partition order cap: on the
55-file corpus (300.07 MB of FLAC) it changed 4 files by 292 bytes in
total; levels 3-4 got slightly smaller. Decision changes that saved more
CPU but cost compression were measured and rejected: choosing the fixed
order by a whole-block estimate (+36 KB, up to +2.0% on single tracks),
ranking LPC orders by the Levinson error (+2.0%), and skipping the exact
cost of the lower order when sampling ranks it worse (+7.5 KB).

The ALAC decoder was analysed the same way (predictor 394 ms, Rice 158 ms
for album44.m4a). Order 5, 22% of the samples, ran the generic predictor
loop; it now has a constant-order loop (predictor 394 -> 355 ms). The
predictor's coefficient adaptation stops at an unpredictable point
(coefficients updated per sample spread evenly over 1..6), and three
branch-free formulations with identical output were measured slower (439 to
566 ms): the misprediction costs less than evaluating every coefficient.
ALAC Rice decoding is at the latency of its adaptive parameter chain.

Paired CPU, previous -> this round (median ratio, 95% CI):

| Command | Input | Change |
| --- | --- | --- |
| `convert` | album44.wav / album48.wav / album192.wav | -27.3% [-33.1, -24.0] / -23.0% [-27.1, -18.7] / -15.8% [-26.3, -5.7] |
| `convert` | album44.m4a | -18.2% [-23.4, -6.3] |
| `convert --analyze` | album44.wav / album44.m4a | -24.4% [-37.0, -21.2] / -15.4% [-21.8, -12.1] |
| `convert --compression-level 5` | album44.flac | -22.0% [-26.4, -21.0] |
| `pcm-hash` | album44.m4a / album44.flac | -1.3% / -2.6% (noise) |

All decode, analyze, fingerprint and hash outputs are identical; converted
files decode to the same PCM; qualify, codec-stress and standards reports and
the mutation-fuzz counts are identical; peak RSS is unchanged within
run-to-run variation. New tests cover the i32 LPC residual path against the
i64 definition, the chunked partition sums, the bounded stereo sums, and the
branch-free Rice writer against per-code writes (all offsets, k = 0..30,
long quotients); the existing bit-flip differential test covers the new
verification. [Raw](verified-pipeline/round10-codec.txt).

## Round 11: FFmpeg-derived decoders redesigned; every codec against FFmpeg

The modules that started as FFmpeg ports were checked part by part. Parts
already redesigned in earlier rounds stay (FLAC LPC restore, CRC-16, frame
verification and encoder planning; ALAC output stage and constant-order
predictors; the QC meter). The TTA and WavPack decoders, untouched ports
until now, were restructured; the format arithmetic is unchanged (and the
attribution in THIRD_PARTY.md stays):

* **TTA**: entropy decoding, filtering and output are separate passes over
  a frame. The adaptive Rice state lives in registers and adapts without
  branches; the eight-tap filter is rewritten as `C + x * Q` (the delay
  line's four newest values are affine in the new sample), so the serial
  path is one multiply, two adds and a shift. A shared LSB reader with
  unaligned word refills replaces the byte-copying one.
* **WavPack**: the entropy state is copied into registers per block, the
  medians update without branches (shifts while no overflow is possible),
  decorrelation keeps the per-sample term order (overlapping the terms'
  chains) with branch-free weight steps, and output has a check-free path
  when there are no extra bits.
* **FLAC fixed predictors** keep their history in locals.

Errors are reported in the same order as before (an out-of-range or
extra-bits error before a later decoding error), which 5,400 CRC-repaired
TTA and 3,000 WavPack mutations confirm against the previous binary.

Single-thread CPU against FFmpeg 8.1.2. Ours decodes with every check and
hashes (SHA-256 of the canonical PCM); FFmpeg is shown both doing the same
hash and only decoding:

| Input | Ours | FFmpeg decode + SHA-256 | FFmpeg decode only |
| --- | ---: | ---: | ---: |
| FLAC 16/44.1 (album44) | 0.283 s | 0.898 s (-68%) | 0.403 s (-30%) |
| FLAC 24/96 (album96) | 0.137 s | 0.310 s (-56%) | 0.166 s (-18%) |
| ALAC 16/44.1 | 0.587 s | 1.312 s (-55%) | 0.916 s (-36%) |
| ALAC 24/96 | 0.235 s | 0.533 s (-56%) | 0.381 s (-38%) |
| TTA 16/44.1 | 0.505 s | 0.894 s (-43%) | 0.554 s (-9%) |
| TTA 24/96 | 0.240 s | 0.425 s (-43%) | 0.269 s (-11%) |
| WavPack default | 0.682 s | 1.031 s (-34%) | 0.728 s (-6%) |
| WavPack high | 1.113 s | 1.847 s (-40%) | 1.638 s (-32%) |

Round 10 -> 11: TTA -35%, WavPack -13% (default) and -16% (high).

Conversion to FLAC level 5, ours with SHA-256, STREAMINFO MD5, per-frame
verification and fsync against FFmpeg encoding only: WAV 16/44.1 -16%,
ALAC -25%, TTA -18%, WavPack -5%, with 0.06-0.35% smaller files. The one
row where FFmpeg's encode-only time is lower is WAV 24/96 (+8%): there the
extra SHA-256 of 32-bit PCM and the verification are a larger share; with
the same hashing and verification FFmpeg needs a second decode and hash
pass, which alone takes longer than the difference.

Measured and not kept: an AVX2 TTA filter with the state in vector
registers (equal: cross-lane latency), WavPack decorrelation as one
whole-block pass per term (2.3x slower: no overlap between terms), and
i64 / exact-f64 AVX2 kernels for 24-bit FLAC frame verification (equal).
[Raw](verified-pipeline/round11-ffmpeg.txt).

## Round 12: FLAC encoder cost model, block size and stereo trials

The encoder parts still following FFmpeg's choices were rederived from the
format and measured on the 55-file corpus (decoders are unchanged):

* **Rice estimate**: with parameter k a value u costs k + 1 + floor(u / 2^k)
  bits; if the k low bits are spread evenly the quotients of a partition
  total about (2 sum - count (2^k - 1)) / 2^(k+1). The parameter is
  floor(log2(mean)), found from bit lengths without a division (a division
  per partition and order made the fixed-predictor stage 15% slower).
  Evaluating its neighbours as well saved 0.0006% for 2% more CPU. The
  writer still picks each partition's exact best parameter.
* **Block size**: levels 2-8 use 4096-frame blocks. Level 5 had 4608 and
  levels 6-8 up to 32768, which compressed worse than level 5.
* **Stereo assignment**: level 7 plans the channels of the two best
  estimated assignments and level 8 all four, and the smallest exact pair
  is written; lower levels plan only the best estimate.
* **LPC quantization**: the error-feedback rounding stays; plain rounding
  made the corpus 0.25% larger.

| Level | Bytes | vs level 5 before |
| --- | ---: | ---: |
| 5 before | 300,072,692 | |
| 5 | 299,882,124 | -0.064% |
| 6 | 299,846,990 | -0.075% |
| 7 | 299,519,096 | -0.184% |
| 8 | 299,494,407 | -0.193% |

Every level now compresses at least as well as the one below it. Level 5
CPU is neutral (whole corpus, eight alternating runs: 11.48 s before,
11.50 s after) and peak RSS unchanged; levels 6-8 use about 1.6 MiB less.
[Raw](verified-pipeline/round12-encoder.txt).

## Round 13: FLAC low levels and level-5 LPC search

> Superseded in part by round 14: round 13 used LPC orders above 12 at
> 44.1-48 kHz, which is valid FLAC but outside the streamable subset. Round
> 14 restores subset compliance; its tables are the current results.

Goal: levels 0-4 compressed worse than FFmpeg at the same number, and level 5
should get smaller, faster and no larger in memory, with Arm (Graviton) as the
deployment target. Every change below was a hypothesis measured before it was
kept; refuted ones were removed again.

Conditions: 4-vCPU Intel Xeon VM (AVX2), FFmpeg **6.1.1** (Ubuntu package,
`-threads 1`, encode only) as comparator. Corpus: the 49 supported tracks of the
IETF CELLAR FLAC test files (CC0; 44.1/48 kHz 16-bit stereo, 96/134.56/192 kHz
24-bit stereo, mono 16/24-bit), decoded to WAV. Bytes are corpus totals; CPU is
user+system summed over the corpus (min of 3), RSS is the largest peak RSS of
any one process, from `wait4` in a small C runner. The VM is noisy (+-10%), so
album CPU is also given as instruction counts (valgrind, SHA-256 excluded
because valgrind lacks SHA-NI) and as the paired median of 21 interleaved runs.
Arm code was cross-built (aarch64 musl) and run under QEMU for correctness
only; Arm timing comes from the repository's Arm CI.

### Result

| Level | Before bytes | After bytes | Change | FFmpeg 6.1 | After vs FFmpeg | Before CPU s | After CPU s |
|---|---:|---:|---:|---:|---:|---:|---:|
| 0 | 98,442,777 | 71,739,018 | -27.13% | 73,909,130 | -2.94% | 0.793 | 0.809 |
| 1 | 79,056,344 | 71,667,680 | -9.35% | 72,199,900 | -0.74% | 0.754 | 0.914 |
| 2 | 73,483,284 | 68,697,644 | -6.51% | 72,127,350 | -4.75% | 0.780 | 0.955 |
| 3 | 71,893,208 | 67,710,085 | -5.82% | 68,574,784 | -1.26% | 0.846 | 1.006 |
| 4 | 68,698,832 | 67,436,173 | -1.84% | 68,295,424 | -1.26% | 1.050 | 1.090 |
| **5** | 67,718,241 | **67,310,249** | **-0.60%** | 68,251,657 | -1.38% | 1.238 | **1.182** |
| 6 | 67,700,212 | 67,108,721 | -0.87% | 68,226,043 | -1.64% | 1.846 | 1.408 |
| 7 | 67,561,958 | 66,933,180 | -0.93% | 68,224,152 | -1.89% | 2.553 | 2.138 |
| 8 | 67,552,497 | 66,907,684 | -0.95% | 67,939,273 | -1.52% | 2.658 | 3.295 |

FFmpeg 6.1 needed 6.5-8.3 s of CPU per level on the same corpus. Peak RSS at
level 5: 3,824 KiB before, 3,856 KiB after (run-to-run spread is about
+-150 KiB); the binary's text is 988,798 bytes (before 1,006,730).

Level 5 by format (bytes before -> after, FFmpeg 6.1 level 5):

| Rate/bits/channels | Files | Before | After | Change | FFmpeg |
|---|---:|---:|---:|---:|---:|
| 44100/16/2 | 27 | 24,504,807 | 24,254,381 | -1.02% | 24,847,045 |
| 48000/16/2 | 10 | 8,386,367 | 8,345,316 | -0.49% | 8,474,757 |
| 96000/24/2 | 5 | 16,467,044 | 16,405,859 | -0.37% | 16,509,847 |
| 134560/24/2 | 1 | 4,605,520 | 4,594,978 | -0.23% | 4,611,838 |
| 192000/24/2 | 2 | 13,260,895 | 13,237,647 | -0.18% | 13,270,397 |
| 44100/16/1 | 3 | 252,062 | 239,273 | -5.07% | 279,366 |
| 44100/24/1 | 1 | 241,546 | 232,795 | -3.62% | 258,407 |

Level 5 on albums (album44 = 25 tracks, 205 s 16-bit; album96 = 5 tracks,
41 s 24-bit): encoder stage time 265 -> 215 ms and 141 -> 117 ms (min of 7,
profile build); instructions without SHA-256 -5.6% / -8.0%; paired whole-
process CPU -3.3% / -11.1%.

### Hypotheses

| # | Hypothesis | Measurement | Verdict |
|---|---|---|---|
| L1 | Low levels lose because they restrict fixed orders (level 0: none, 1: order 1) although the fused pass computes orders 0-4 at once | All levels with orders 0-4: level 0 98.4 -> 71.7 MB at equal CPU | Kept |
| L2 | 1024/2048-frame blocks at levels 0-1 cost more than they save | 4096 blocks smaller (level 0: 71.74 vs 71.86 MB) | Kept |
| L3 | FFmpeg's level 3-4 lead comes from LPC (order 6), which fixed predictors cannot reach | Best fixed-only setting 71.67 MB vs FFmpeg level 3 68.57 MB; LPC order 4 68.70, order 8 67.71 | Kept: LPC from level 2 |
| P1 | A maximum LPC order of 8 limits level 5 | Order 12 -0.39%, 16 -0.56%, 20 -0.66%, 32 -0.76% (old order search) | Kept (16 at level 5, 32 from level 6) |
| P2 | 15-bit coefficients are wider than useful | Per format best 12-15; 13 within 0.003% of best everywhere, 15 +0.03% at order 12-16; keeps 16-bit verification sums in i32 to order 16 | Kept (13) |
| P3 | Ranking orders by 128-point sampled costs is unnecessary: the Levinson error with the coefficient overhead estimates the best order (raw error alone, measured in round 10, always prefers the top order) | Order 16: 67,310,394 vs 67,312,943 with sampled ranking + 2 exact; order 8 within 0.004%; one exact residual, no sampled costs | Kept |
| P4 | Costing only the highest order is enough | +0.03% (order 12) / +0.07% (order 16) | Rejected |
| P5 | Fewer sampled points (32/64) | +0.005% | Superseded by P3 |
| P6 | Tukey(0.5)/(0.25), Hann or sqrt-Welch window instead of Welch | -0.003% / 0.000% / +0.03% / +0.06% | Rejected (noise level) |
| P7 | Blocks 3072/3584/4608/8192 | All larger than 4096 | Rejected |
| P8 | Weight the estimate's coefficient overhead by 0.25-0.75 | Best (0.6) -0.03% for +6-10% CPU | Rejected for level 5 |
| P9 | Skip fixed predictors at LPC levels | +0.04% | Rejected |
| P10 | f32 autocorrelation (twice the SIMD lanes, important on Arm) | +0.32% (level 5), +0.46% (level 6) | Rejected: f64 stays |
| P11 | Level 8 exhaustive search over 32 orders | -0.09% vs level 7 for 5x its CPU (11.9 s); estimate +-2 and the top order: 72% of that gain at 30% of the CPU | Kept: +-2 |
| P12 | Partition order 8 at levels 6-8 silently capped LPC at order 15 (warm-up must fit the finest partition) | Per-order partition limit: level 6 67.35 -> 67.11 MB | Fixed |
| S1 | One fused autocorrelation pass (all lags of a group per sample load) beats a dot product per lag | First version slower at order 8 (16 lags computed for 9); with group widths 8/4/2 (AVX2) and 16/8/4/2 FMA chains (NEON): 25 -> 20 ms at order 8, 41 -> 33 at 16, 77 -> 58 at 32 | Kept |
| S2 | Frame verification of orders above 8 falls to the generic scalar i64 loop | 64 -> 126 ms at order 12; bound sum\|c\| max\|x\| (was max\|c\| N max\|x\|) and a vectorized any-order kernel | Kept |
| R1 | Peak RSS follows binary size (fault-around maps nearly all text: 940 of 1,006 KB resident) and one instantiation per order 1-32 adds 400 KB of text | Constant-order kernels only to order 8; one any-order kernel (16 predictions per coefficient broadcast) for 9-32: text 1,348,302 -> 988,798 bytes, RSS back to before | Kept |
| A1 | On aarch64 the generic i64 LPC loops stay scalar (SMADDL; baseline NEON has no 64-bit multiply), so 24-bit material does not vectorize | Disassembly confirmed; explicit NEON SMLAL/SMLAL2 kernel for encoder and verification at every order when the i32 bound fails | Kept; Arm timing from CI |
| A2 | The same i64 grouping speeds x86 24-bit verification | album96 verification 48 -> 28 ms (before this round: 36) | Kept |

Correctness: unit, regression and reference-codec tests (x86, and aarch64
under QEMU); `qualify` (596 checks), `codec-stress` (288) and `standards`
passed; all 441 corpus outputs (49 tracks x 9 levels) and the aarch64
binary's outputs at levels 0/3/5/8 decode in FFmpeg with CRC checking to the
source PCM. Tests now cover LPC orders 1-32 (residual kernels against the
i64 definition, i32/i64 boundaries) and the autocorrelation kernel against
its definition for 1-33 lags. [Raw](verified-pipeline/round13-encoder.txt).

## Round 14: streamable subset, stereo estimate and block size

Same corpus, harness and comparator as round 13 (FFmpeg 6.1.1). Upper bounds
were measured first with exact costing; only cheap approximations of the
promising ones were built.

**Compatibility fix.** The FLAC streamable subset, which hardware decoders
and FFmpeg's default encoder follow, allows LPC orders up to 12 and blocks
up to 4608 frames at 48 kHz and below (32 and 16384 above). Round 13 used
order 16 (level 5) and 32 (levels 6-8) at 44.1-48 kHz. Every level is now
capped per sample rate, and a test checks all levels and rates.

### Hypotheses

| # | Hypothesis | Measurement (level 5 unless noted) | Verdict |
|---|---|---|---|
| U1 | Upper bounds of further search | exact 2 / 4 stereo trials -0.229% / -0.242%; precisions 12-15 -0.049%; estimated order +-1 -0.039%; partition order 8 -0.001%; five windows (Welch, Tukey, partial and punch-out Tukey) chosen by estimate +0.15%, each costed exactly -0.14% at 2.3x CPU | Stereo choice is the largest lever |
| E1 | The stereo assignment is ranked by second-order fixed residual sums, a poor proxy for LPC-coded channels; the Levinson estimate of each channel ranks better | Order 2/3/4/8/16: -0.09/-0.11/-0.15/-0.16/-0.17% (63-73% of exact two-trial gain); order 4 on the central half block -0.125% for about +7 ms encoder per 205 s album (stereo stage 9 -> 16 ms); every level 0-6 improves 0.06-0.15% | Kept (order 4, half block) |
| E2 | A rule can pick the coefficient precision per subframe | 13,087 subframes costed at 11-15: oracle -0.069%; best rule from order and bits per sample -0.023%; would push high orders off the i32 verification path | Rejected |
| E3 | Variable block size | Oracle from exact frame sizes: 2048..8192 tree -0.49%, 1024..16384 -0.68%; needs several encodings per block and variable-blocksize streams, which some decoders and ingest systems handle poorly | Not built; decision for the product owner |
| E4 | One fixed block size per file | Oracle -0.31%: 88.2 kHz and above prefers 8192-16384, 44.1/48 kHz around 4096. Rule "8192 above 48 kHz": -0.10..-0.16% of the high-resolution files, +250 KiB peak RSS (album96 3.64 -> 3.93 MiB) | Kept for levels 6-8 only; level 5 keeps 4096 and its memory |
| E5 | Autocorrelation lag groups of 4 + 2 instead of one pass of 8 for five or six lags | Same sums, fewer operations for the stereo estimate and order-4 LPC | Kept |

### Result

| Level | Round 12 bytes | Round 14 bytes | Change | FFmpeg 6.1 | vs FFmpeg | Round 12 CPU s | Round 14 CPU s | Round 12 RSS KiB | Round 14 RSS KiB |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 98,442,777 | 71,700,400 | -27.17% | 73,909,130 | -2.99% | 0.793 | 0.835 | 3512 | 3840 |
| 1 | 79,056,344 | 71,628,179 | -9.40% | 72,199,900 | -0.79% | 0.754 | 0.891 | 3516 | 3836 |
| 2 | 73,483,284 | 68,619,943 | -6.62% | 72,127,350 | -4.86% | 0.780 | 0.942 | 3528 | 3844 |
| 3 | 71,893,208 | 67,627,433 | -5.93% | 68,574,784 | -1.38% | 0.846 | 1.020 | 3516 | 3840 |
| 4 | 68,698,832 | 67,349,714 | -1.96% | 68,295,424 | -1.38% | 1.050 | 1.128 | 3852 | 3848 |
| **5** | 67,718,241 | **67,325,634** | **-0.58%** | 68,251,657 | -1.36% | 1.238 | **1.129** | 3824 | 3852 |
| 6 | 67,700,212 | 67,223,444 | -0.70% | 68,226,043 | -1.47% | 1.846 | 1.253 | 3848 | 4100 |
| 7 | 67,561,958 | 67,137,764 | -0.63% | 68,224,152 | -1.59% | 2.553 | 1.892 | 3980 | 4340 |
| 8 | 67,552,497 | 67,123,667 | -0.63% | 67,939,273 | -1.20% | 2.658 | 2.750 | 3980 | 4284 |

Level 5 on the albums against round 12: paired median CPU (21 interleaved
runs) -8.5% (album44, 16-bit) and -8.3% (album96, 24-bit); instructions
-2.8% / -8.2% (album44 without SHA-256: -6.5%); peak RSS unchanged.

Correctness: tests (x86, reference codecs, aarch64 under QEMU) including the
subset test; `qualify` 596, `codec-stress` 288 and `standards` passed; all
441 x86 outputs and the aarch64 outputs at levels 5 and 8 decode in FFmpeg
with CRC checking to the source PCM. Real Arm timing: the workflow
`flac-levels.yml` runs [`flac_level_bench.py`](../tools/scripts/flac_level_bench.py)
on the same corpus for round 12, this engine and FFmpeg on x86 and Arm
runners. [Raw](verified-pipeline/round14-encoder.txt).

## Round 15: same compression, less CPU

Goal: rounds 13-14 made every level smaller but levels 0-4 and level 8
dearer than round 12. Keep the round-14 bytes (or better) and remove CPU.
Every speed change below is byte-identical unless stated; instruction
counts are valgrind/cachegrind on album44 (205 s, 16-bit) and album96 (41 s,
24-bit), with SHA-256 counted in software (valgrind has no SHA-NI); times are
paired interleaved runs pinned to one core. Real Arm and x86 runner timing
comes from `flac-levels.yml` on long albums (16-bit and 24-bit, about 20 and
6 minutes), minimum of five interleaved runs.

### Hypotheses

| # | Hypothesis | Measurement | Verdict |
|---|---|---|---|
| H1 | The autocorrelation windowing loop compiled to scalar int-to-float conversions | Moved into the AVX2/NEON kernel: -42M (L1) / -84M (L3, L5) instructions | Kept |
| H2 | Run the Rice partition search only for the best 2-3 fixed orders by a whole-block estimate | -53..-82M instructions but +0.009..+0.023% bytes | Rejected (compression) |
| H2' | Branch-free `rice_estimate` | Same instructions, 10% fewer mispredicts in `choose_rice` | Kept (neutral, simpler) |
| H3 | The fixed-residual writer pushed one value at a time through a closure (no vectorization) | Zipped slices: -233M instructions at level 1 | Kept |
| H5 | Fused fixed partition sums in an AVX2 build (8 u32 lanes instead of SSE2's 4) | -119M instructions, -7.5% CPU at level 1 (round 2 had found no gain in the older structure) | Kept |
| H6 | Skip exact fixed predictors when the LPC estimate beats the fixed predictors' autocorrelation estimate by a margin | Any margin that saves CPU costs +0.004..+0.11% bytes | Rejected |
| H7 | `choose_rice` keeps only per-order totals while searching | +5..+7M instructions | Rejected |
| H8 | f32 stereo estimate (ranking only) | +0.012..+0.018% bytes, no instruction gain | Rejected |
| H9 | Rice writer: four codes per 8-byte store when they fit 56 bits | -107..-113M instructions, -4.1% CPU | Kept |
| H10 | Frame verification: four expected codes per load | -133M instructions, -2.1% CPU; bit-flip differential test unchanged | Kept |
| H11 | Short autocorrelation groups were latency bound (4 or 2 accumulation chains) | 2-4 sample vectors per step keep 8 (AVX2) / 16 (NEON) chains; FMA on x86: same bytes at every level, -6.8% CPU (L2), -5.9% (L5) | Kept |
| H12 | Reuse the stereo-estimate lags in planning (whole-block lags 0-4 at LPC levels, exactly rescaled for wasted bits) | -37M instructions at L2, and 0.013-0.017% smaller files at levels 2-6 | Kept |
| H13 | Fixed-only levels: plan all four stereo channels exactly instead of estimating | -0.10% bytes but +59M (L0) / +226M (L1) instructions | Rejected (CPU) |
| H14 | Integer autocorrelation for 16-bit material | 16-bit quantization +0.0004..+0.002% bytes but no AVX2 gain (i32 pair sums need widening every step); 15-bit +0.002..+0.004% | Not adopted |
| H15-17 | The quad writer/verifier's pair closures were compiled out of line (no BMI2, a call per pair), doubling Rice work on 24-bit material | `inline(always)` pair functions, a cold pair only for non-fitting quads, pairs directly when k > 12: 24-bit L1 3,404M -> 3,178M instructions (round 12: 3,245M), 16-bit slightly lower | Kept |
| H18 | Level 8 above 48 kHz: orders within 1 of the estimate instead of 2 | +246 bytes on 34 MB of high-resolution files (+0.0007%), -20% CPU, below round 12 | Kept |
| H19 | i32 coefficients in the wide (24-bit) x86 kernel to obtain VPMULDQ | LLVM still emulates 64-bit products; no gain | Rejected |

### Result on GitHub runners (commit `d332a09`)

CPU seconds on long albums (16-bit, about 20 min / 24-bit, about 6 min),
minimum of five interleaved runs; bytes over the 49-track corpus (every
output cross-decoded by FFmpeg); round 12 -> this round.

| Level | Bytes | Arm 16-bit | Arm 24-bit | x86 16-bit | x86 24-bit |
|---|---|---:|---:|---:|---:|
| 0 | 98,442,777 -> 71,700,400 | 1.34 -> 1.34 (0%) | 0.48 -> 0.49 (+2%) | | |
| 1 | 79,056,344 -> 71,628,179 | 1.36 -> 1.42 (+4%) | 0.48 -> 0.51 (+6%) | | |
| 2 | 73,483,284 -> 68,610,902 | 1.37 -> 1.59 (+16%) | 0.48 -> 0.60 (+25%) | 1.36 -> 1.44 (+6%) | 0.49 -> 0.56 (+14%) |
| 3 | 71,893,208 -> 67,615,972 | 1.48 -> 1.71 (+16%) | 0.51 -> 0.66 (+29%) | 1.49 -> 1.52 (+2%) | 0.52 -> 0.60 (+15%) |
| 4 | 68,698,832 -> 67,339,439 | 1.73 -> 1.83 (+6%) | 0.67 -> 0.68 (+1%) | 1.62 -> 1.58 (-2%) | 0.70 -> 0.61 (-13%) |
| 5 | 67,718,241 -> 67,315,197 | 1.98 -> 1.83 (-8%) | 0.77 -> 0.71 (-8%) | 1.80 -> 1.59 (-12%) | 0.79 -> 0.63 (-20%) |
| 6 | 67,700,212 -> 67,212,070 | 2.88 -> 2.13 (-26%) | 1.22 -> 0.86 (-30%) | 2.86 -> 1.90 (-34%) | 1.22 -> 0.71 (-42%) |
| 7 | 67,561,958 -> 67,137,198 | 4.12 -> 2.99 (-27%) | 1.94 -> 1.53 (-21%) | 4.13 -> 2.59 (-37%) | 1.87 -> 1.15 (-39%) |
| 8 | 67,552,497 -> 67,123,906 | 4.49 -> 4.31 (-4%) | 1.97 -> 2.12 (+8%) | 4.51 -> 3.61 (-20%) | 1.91 -> 1.55 (-19%) |

Before this round's 24-bit fixes (commit `177a0f1`) Arm 24-bit was +15%,
+19%, +38%, +44% at levels 0-3 and +35% at level 8. Peak RSS: levels 4-5
unchanged; levels 0-3 about 0.3 MiB above round 12, which used no LPC or
window buffers there.

Remaining cost: levels 2-3 now run LPC (the source of their 5.9-6.6% smaller
files); on Arm its f64 autocorrelation (two lanes) and 64-bit residuals for
24-bit material are the largest items.

## Round 16: level 5 smaller with less CPU; static musl build

| # | Hypothesis | Measurement | Verdict |
|---|---|---|---|
| H20 | On AArch64 the constant-order LPC loops (orders 1-8) became two-lane widening SMLAL; route them to the grouped four-lane MLA kernel | Arm runner levels 2/3: 1.59/1.71 s -> 1.62/1.74 s (no gain) | Rejected, reverted |
| H21 | Stereo estimate with lags up to min(LPC order, 8) instead of 4 (planning reuses them) | Levels 3-6 0.009-0.013% smaller (level 5 67,315,197 -> 67,306,865); order 12 saved no more; level 2 unchanged | Kept |
| H22 | LPC partition sums and peak scans ran in baseline SSE2 | AVX2 builds: level 5 instructions 7,333M -> 7,210M (16-bit album), 3,623M -> 3,563M (24-bit), including H21 | Kept |
| H14' | Integer (16-bit) autocorrelation for Arm | Arm's integer multiply pipes give no more products per cycle than its f64 FMA pipes; not built | Rejected |
| M1 | Peak RSS is mostly shared-library pages (glibc, libm), not the heap (374 KB peak by massif) | Static musl build: peak RSS 3.7 -> 1.54 MiB on x86, same bytes, CPU within 2% | Build option (README) |

GitHub runners, commit `3421869`, long albums (16-bit / 24-bit) CPU s and
peak RSS KiB over the corpus, round 12 -> glibc build -> static musl build:

| Level | Arm CPU | Arm RSS | x86 CPU | x86 RSS |
|---|---|---:|---|---:|
| 5 | 1.98+0.77 -> 1.85+0.71 -> 1.86+0.72 | 2972 -> 2912 -> 1308 | 1.81+0.79 -> 1.57+0.63 -> 1.61+0.64 | 3808 -> 3840 -> 1604 |
| 0 | 1.34+0.48 -> 1.33+0.49 -> 1.34+0.49 | 2656 -> 2912 -> 1180 | 1.45+0.54 -> 1.19+0.46 -> 1.23+0.47 | 3472 -> 3816 -> 1604 |
| 3 | 1.48+0.51 -> 1.69+0.65 -> 1.70+0.66 | 2716 -> 2912 -> 1308 | 1.49+0.52 -> 1.48+0.59 -> 1.52+0.60 | 3500 -> 3844 -> 1608 |
| 8 | 4.47+1.96 -> 4.31+2.11 -> 4.33+2.11 | 3100 -> 3424 -> 1692 | 4.50+1.89 -> 3.39+1.49 -> 3.46+1.51 | 3948 -> 4176 -> 1992 |

## Round 17: release LTO on x86_64 and aarch64

Cargo's default release profile already runs thin-local LTO over 16 codegen
units; `lto = "off"` is the real no-LTO build. Same source (`b947a4d`), x86
(AVX2 kernels), level 5, 16-bit / 24-bit album:

| Build | text bytes | Instructions (valgrind) | Paired CPU vs default |
|---|---:|---|---|
| `lto = "off"`, 16 units | 1,043,310 | 11,656M / 5,111M | - |
| default (thin-local, 16 units) | 1,026,232 | 7,210M / 3,563M | 0 |
| `lto = "thin"`, 16 units | 1,026,232 | 7,209M / 3,563M | +1.7% / -1.1% (noise) |
| `lto = "thin"`, 1 unit | 999,732 | 7,271M | - |
| `lto = "fat"`, 16 units | 950,626 | 7,232M | - |
| `lto = "fat"`, 1 unit | 955,230 | 7,269M / 3,579M | +3.0% / +1.4% (noise; levels 0 and 8 +0.4% / +2.0%) |

Every variant writes byte-identical FLAC (levels 0/3/5/8, both albums). On
x86 the hot loops are already one crate with runtime-dispatched kernels, so
LTO beyond the default is CPU-neutral within +-1% of instructions; fat LTO in
one unit gives the smallest binary (-7% text, -11% file). The release profile
now uses it on both architectures (one profile, no per-target split), and
`flac-levels.yml` builds the previous default as `new-nolto` so each runner
(Arm and x86) measures the same engine with and without it.

Runners, commit `e353fe4` (long albums 16-bit + 24-bit CPU s, fat LTO vs the
previous thin-local profile): x86 level 5 1.64+0.66 vs 1.67+0.68, levels
6-8 1-2% faster, peak RSS about 100 KiB lower; Arm level 5 1.86+0.72 vs
1.85+0.72, levels 0-8 within +-1%. Same bytes everywhere.

## Round 18: level 5 orders to 32 again; 16-bit LPC kernels

Round 14 capped every level at LPC order 12 up to 48 kHz (streamable
subset). On real 44.1 kHz material that cap made level 5 noticeably larger,
so the cap is lifted for levels 5-8 (levels 0-4 never exceed 12 and stay
subset streams; blocks stay within the subset everywhere). Every FLAC
decoder (libFLAC, FFmpeg) reads these streams; strict subset hardware may
not. Same corpus; x86 instructions by valgrind on album44 (16-bit) at
level 5, encoder only = total minus software SHA-256 and MD5.

| # | Hypothesis | Measurement | Verdict |
|---|---|---|---|
| C1 | Order cap 12 at 44.1-48 kHz costs level 5 | Uncapped order 16 / 20 / 24 / 32: corpus -0.15 / -0.27 / -0.35 / -0.45% (44.1 kHz stereo -0.37 / -0.65 / -0.85 / -1.12%); encoder instructions +5% / +11% / +15% / +23% | Order 32 at levels 5-8 |
| C2 | Above 48 kHz level 5 order 32 instead of 16 | High-resolution files -0.05%, 24-bit album encoder +7.6% | Rejected; level 5 stays 16 there |
| H24 | Lags 13-32 only while the Levinson estimate still falls (staged autocorrelation) | Threshold 0 / 0.01 / 0.03 / 0.1 bit per sample: -1.1 / -1.5 / -2.4 / -4.8% instructions for +0.03 / +0.06 / +0.13 / +0.34% bytes | Rejected |
| H25 | Extra penalty per LPC order (fewer taps in residual and verify) | 8 / 16 / 32 / 64 bits per order: +0.06 / +0.13 / +0.31 / +0.64% bytes; the Levinson estimate is already calibrated | Rejected |
| H26 | 16-bit samples (every subframe with peak < 2^15) need only 16x16-bit products: VPMADDWD on sample pairs (x86), SMLAL/SMLAL2 (AArch64, twice the issue rate of 32-bit MLA on Neoverse) for orders 9-32; the verifier gets its own 16-bit kernel (highest taps paired first on x86, multiply-by-lane on NEON) so it stays independent of the encoder | Encoder LPC residual 305M -> 117M, verify 316M -> 110M; identical bytes on x86 and aarch64 (QEMU) at levels 5 and 8; a corrupted encoder kernel fails verification | Kept |
| S1 | Level 8 costs orders +-1 around the estimate at every rate (was +-2 up to 48 kHz) | +0.007% bytes, -12% level 8 instructions | Kept |
| R1 | Runners after H26 (commit `0bcb0fe`) | x86 level 5 16-bit 6% below round 12; Arm 1.86 -> 2.13 s, 7% above round 12 (1.99 s) and 16% above round 15 (`2baeaf8`, 1.83 s) | Arm regression to remove |
| A1 | Why round 15 (`2baeaf8`) level 5 is faster: per-function instructions against it | Encoder 2,226M (round 15) vs 2,203M; LPC residual + verify 370M -> 227M (H26), autocorrelation 162M -> 312M: lags 13-32 in f64, two lanes per NEON vector | Autocorrelation is the difference |
| H27 | Lags 13-32 in f32 (twice the lanes) | Corpus +0.33%, mono +2.5%: the high-order Levinson recursion needs the precision | Rejected |
| H28a | Exact integer lags 9-32 of the windowed block scaled to 14-16 bits, f64 lags 0-8 | +0.5..+1.2%: lags of different precision in one Toeplitz system | Rejected |
| H28 | Every lag (stereo estimate and planning) exact in integers of the windowed block scaled to 15 bits (scale = 16383 / max \|x\|, round to even): VPMADDWD (16 products) / SMLAL (4 products) into i32 lanes, widened to i64 every eight products; f64 FMA does 4 / 2 | Levels 5-8 0.001-0.003% smaller than f64 (16-bit scaling within 0.001%, 13-bit +0.02%); levels 2-4 +0.001-0.004%, so they keep f64. x86 level 5 paired CPU vs round 15: -6% (16-bit), -13% (24-bit). Exact sums: x86 and Arm (QEMU) write identical bytes | Kept for levels 5-8 |

Level 5 result: corpus 67,306,865 -> 67,017,261 bytes (-0.43%; 44.1 kHz
stereo -1.1%, mono -2.8%; high resolution unchanged), FFmpeg 6.1 level 5
68,251,657. Levels 6 / 7 / 8: 66,915,041 / 66,840,693 / 66,826,844 bytes
(were 67,204,708 / 67,137,178 / 67,123,887). Arm and x86 runner results,
including round 15 and a level-5 stage profile of both engines:
`flac-levels.yml`.

## Remaining hotspots (after round 10), ranked by expected ROI

0. **Conversion after round 10**: the fused MD5/SHA-256 pass is now the
   largest single item of WAV -> FLAC (about a quarter), bound by MD5's
   serial chain; ALAC input is dominated by the adaptive predictor (below).
1. **MD5 and SHA-256** (round 4): in-repository, fused into one pass, and
   near the per-step latency floor (MD5 about 4.5 cycles per step; SHA-NI
   bound by its `sha256rnds2` chain). No further lever without changing the
   hash definitions.
2. **FLAC Rice parsing** (~10% of conversion after LZCNT/BMI2): table decoding
   refuted by the k distribution and instruction-count reduction not
   reproducible (round 3); no remaining design with measured headroom.
3. **Exact LPC costing** (~8% after round 3): fused pair costing measured
   slower; winning-residual reuse accepted in round 3.
4. **ALAC adaptive predictor** (~35% of ALAC input): branch-mispredict bound;
   six designs measured and rejected, three more branch-free ones in round 10.
5. **QC meter** (round 6): near the K-weighting recursion's latency floor
   (about 20 cycles per frame); shortening it would change the operation
   order and therefore the reported bits. True peak is mostly skipped.
6. **FLAC LPC restore** (round 9): now bound by its loop-carried multiply,
   add and shift per sample; a SIMD history term would put a vector
   reduction on that path.
7. **PNG inflate** (round 7): one table load per literal (~7 cycles); only
   near-incompressible covers spend noticeable time in it.

Repository hygiene: `reference-target/` (1,784 build artifacts) is tracked in git.

### Per-track real music (CC0 IETF FLAC test-bench recordings)

| Track | Codec | Rate/bits | s | Base CPU/audio-s | Final CPU/audio-s | Δ | FFmpeg CPU/audio-s | Final wall s | FFmpeg wall s | Final RSS MiB | Final bytes/PCM | FFmpeg bytes/PCM |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| t01 | alac | 44100/16x2 | 7.0 | 0.0064 | 0.0056 | -13.1% | 0.0135 | 0.042 | 0.060 | 3.87 | 0.4405 | 0.4481 |
| t01 | pcm_s16le | 44100/16x2 | 7.0 | 0.0041 | 0.0036 | -11.0% | 0.0154 | 0.029 | 0.078 | 3.77 | 0.4405 | 0.4481 |
| t02 | alac | 44100/16x2 | 7.0 | 0.0073 | 0.0061 | -16.8% | 0.0145 | 0.047 | 0.061 | 3.91 | 0.4593 | 0.4661 |
| t02 | pcm_s16le | 44100/16x2 | 7.0 | 0.0038 | 0.0032 | -14.0% | 0.0158 | 0.027 | 0.087 | 3.81 | 0.4593 | 0.4661 |
| t03 | alac | 44100/16x2 | 7.0 | 0.0067 | 0.0059 | -12.1% | 0.0140 | 0.044 | 0.067 | 3.87 | 0.4216 | 0.4283 |
| t03 | pcm_s16le | 44100/16x2 | 7.0 | 0.0044 | 0.0054 | +21.7% | 0.0147 | 0.041 | 0.081 | 3.89 | 0.4216 | 0.4283 |
| t04 | alac | 44100/16x2 | 7.0 | 0.0068 | 0.0058 | -15.2% | 0.0137 | 0.044 | 0.062 | 3.80 | 0.4325 | 0.4393 |
| t04 | pcm_s16le | 44100/16x2 | 7.0 | 0.0040 | 0.0033 | -17.7% | 0.0153 | 0.026 | 0.082 | 3.73 | 0.4325 | 0.4393 |
| t05 | alac | 44100/16x2 | 7.0 | 0.0069 | 0.0064 | -7.1% | 0.0143 | 0.050 | 0.063 | 3.79 | 0.4680 | 0.4748 |
| t05 | pcm_s16le | 44100/16x2 | 7.0 | 0.0039 | 0.0032 | -16.1% | 0.0144 | 0.025 | 0.078 | 3.94 | 0.4680 | 0.4748 |
| t06 | alac | 44100/16x2 | 7.0 | 0.0066 | 0.0057 | -12.8% | 0.0141 | 0.043 | 0.068 | 3.84 | 0.4572 | 0.4641 |
| t06 | pcm_s16le | 44100/16x2 | 7.0 | 0.0040 | 0.0037 | -7.1% | 0.0147 | 0.028 | 0.078 | 3.66 | 0.4572 | 0.4641 |
| t07 | alac | 44100/16x2 | 7.0 | 0.0065 | 0.0069 | +5.3% | 0.0135 | 0.051 | 0.060 | 3.91 | 0.4611 | 0.4679 |
| t07 | pcm_s16le | 44100/16x2 | 7.0 | 0.0039 | 0.0032 | -19.0% | 0.0141 | 0.024 | 0.075 | 3.85 | 0.4611 | 0.4679 |
| t08 | alac | 44100/16x2 | 7.0 | 0.0067 | 0.0058 | -13.2% | 0.0132 | 0.044 | 0.059 | 3.87 | 0.4515 | 0.4583 |
| t08 | pcm_s16le | 44100/16x2 | 7.0 | 0.0039 | 0.0040 | +0.7% | 0.0171 | 0.030 | 0.090 | 3.77 | 0.4515 | 0.4583 |
| t09 | alac | 44100/16x2 | 7.0 | 0.0069 | 0.0061 | -11.4% | 0.0160 | 0.045 | 0.075 | 3.82 | 0.4524 | 0.4593 |
| t09 | pcm_s16le | 44100/16x2 | 7.0 | 0.0040 | 0.0032 | -20.3% | 0.0148 | 0.025 | 0.079 | 3.80 | 0.4524 | 0.4593 |
| t10 | alac | 44100/16x2 | 7.0 | 0.0064 | 0.0055 | -13.8% | 0.0135 | 0.041 | 0.064 | 3.85 | 0.3811 | 0.3888 |
| t10 | pcm_s16le | 44100/16x2 | 7.0 | 0.0038 | 0.0032 | -17.1% | 0.0136 | 0.025 | 0.074 | 3.71 | 0.3811 | 0.3888 |
| t11 | alac | 44100/16x2 | 5.5 | 0.0065 | 0.0057 | -12.5% | 0.0151 | 0.034 | 0.055 | 3.89 | 0.5099 | 0.5279 |
| t11 | pcm_s16le | 44100/16x2 | 5.5 | 0.0040 | 0.0035 | -13.3% | 0.0183 | 0.022 | 0.082 | 3.77 | 0.5099 | 0.5279 |
| t12 | alac | 44100/16x2 | 5.0 | 0.0070 | 0.0058 | -18.1% | 0.0149 | 0.031 | 0.048 | 3.89 | 0.5506 | 0.5629 |
| t12 | pcm_s16le | 44100/16x2 | 5.0 | 0.0041 | 0.0035 | -16.1% | 0.0190 | 0.020 | 0.075 | 3.68 | 0.5506 | 0.5629 |
| t13 | alac | 44100/16x2 | 5.0 | 0.0072 | 0.0059 | -18.4% | 0.0170 | 0.032 | 0.056 | 3.79 | 0.5431 | 0.5548 |
| t13 | pcm_s16le | 44100/16x2 | 5.0 | 0.0043 | 0.0035 | -17.1% | 0.0180 | 0.020 | 0.071 | 3.75 | 0.5431 | 0.5548 |
| t14 | alac | 44100/16x2 | 4.9 | 0.0061 | 0.0055 | -8.6% | 0.0138 | 0.029 | 0.045 | 3.89 | 0.3318 | 0.3507 |
| t14 | pcm_s16le | 44100/16x2 | 4.9 | 0.0041 | 0.0034 | -17.5% | 0.0171 | 0.019 | 0.070 | 3.75 | 0.3318 | 0.3507 |
| t15 | alac | 44100/16x2 | 5.0 | 0.0065 | 0.0057 | -10.9% | 0.0137 | 0.031 | 0.045 | 3.90 | 0.5479 | 0.5568 |
| t15 | pcm_s16le | 44100/16x2 | 5.0 | 0.0040 | 0.0037 | -9.4% | 0.0177 | 0.020 | 0.071 | 3.89 | 0.5479 | 0.5568 |
| t16 | alac | 44100/16x2 | 4.7 | 0.0069 | 0.0062 | -9.9% | 0.0150 | 0.031 | 0.047 | 3.87 | 0.5588 | 0.5719 |
| t16 | pcm_s16le | 44100/16x2 | 4.7 | 0.0043 | 0.0035 | -18.5% | 0.0187 | 0.019 | 0.070 | 3.76 | 0.5588 | 0.5719 |
| t17 | alac | 44100/16x2 | 5.3 | 0.0078 | 0.0062 | -21.2% | 0.0175 | 0.035 | 0.059 | 3.90 | 0.5375 | 0.5474 |
| t17 | pcm_s16le | 44100/16x2 | 5.3 | 0.0043 | 0.0044 | +1.2% | 0.0226 | 0.025 | 0.097 | 3.80 | 0.5375 | 0.5474 |
| t18 | alac | 44100/16x2 | 5.0 | 0.0068 | 0.0062 | -8.4% | 0.0188 | 0.034 | 0.061 | 3.89 | 0.5430 | 0.5546 |
| t18 | pcm_s16le | 44100/16x2 | 5.0 | 0.0042 | 0.0036 | -13.9% | 0.0177 | 0.020 | 0.075 | 3.78 | 0.5430 | 0.5546 |
| t24 | alac | 44100/16x2 | 25.0 | 0.0065 | 0.0056 | -13.9% | 0.0130 | 0.147 | 0.183 | 3.85 | 0.5278 | 0.5309 |
| t24 | pcm_s16le | 44100/16x2 | 25.0 | 0.0035 | 0.0029 | -16.1% | 0.0089 | 0.080 | 0.132 | 3.78 | 0.5278 | 0.5309 |
| t25 | alac | 44100/16x2 | 25.0 | 0.0066 | 0.0054 | -17.2% | 0.0116 | 0.144 | 0.161 | 3.91 | 0.5304 | 0.5336 |
| t25 | pcm_s16le | 44100/16x2 | 25.0 | 0.0041 | 0.0031 | -24.6% | 0.0112 | 0.086 | 0.180 | 3.71 | 0.5304 | 0.5336 |
| t26 | alac | 44100/16x2 | 25.0 | 0.0065 | 0.0058 | -11.8% | 0.0112 | 0.150 | 0.156 | 3.88 | 0.5203 | 0.5232 |
| t26 | pcm_s16le | 44100/16x2 | 25.0 | 0.0038 | 0.0031 | -19.4% | 0.0086 | 0.087 | 0.126 | 3.77 | 0.5203 | 0.5232 |
| t28 | alac | 96000/24x2 | 8.7 | 0.0145 | 0.0130 | -10.7% | 0.0265 | 0.120 | 0.133 | 4.02 | 0.6897 | 0.6913 |
| t28 | pcm_s24le | 96000/24x2 | 8.7 | 0.0089 | 0.0086 | -3.4% | 0.0181 | 0.081 | 0.095 | 3.78 | 0.6897 | 0.6913 |
| t29 | alac | 96000/24x2 | 8.0 | 0.0144 | 0.0133 | -7.7% | 0.0295 | 0.113 | 0.150 | 3.84 | 0.7074 | 0.7098 |
| t29 | pcm_s24le | 96000/24x2 | 8.0 | 0.0091 | 0.0080 | -11.7% | 0.0195 | 0.071 | 0.096 | 3.76 | 0.7074 | 0.7098 |
| t30 | alac | 96000/24x2 | 8.0 | 0.0141 | 0.0158 | +11.9% | 0.0308 | 0.133 | 0.140 | 3.94 | 0.7032 | 0.7053 |
| t30 | pcm_s24le | 96000/24x2 | 8.0 | 0.0086 | 0.0083 | -3.7% | 0.0192 | 0.072 | 0.095 | 3.75 | 0.7032 | 0.7053 |
| t31 | alac | 96000/24x2 | 8.0 | 0.0144 | 0.0122 | -15.7% | 0.0257 | 0.106 | 0.118 | 3.91 | 0.7023 | 0.7043 |
| t31 | pcm_s24le | 96000/24x2 | 8.0 | 0.0090 | 0.0080 | -11.4% | 0.0214 | 0.070 | 0.109 | 3.76 | 0.7023 | 0.7043 |
| t32 | alac | 96000/24x2 | 8.0 | 0.0146 | 0.0124 | -14.7% | 0.0242 | 0.107 | 0.112 | 4.00 | 0.7067 | 0.7087 |
| t32 | pcm_s24le | 96000/24x2 | 8.0 | 0.0088 | 0.0076 | -13.6% | 0.0184 | 0.067 | 0.091 | 3.75 | 0.7067 | 0.7087 |
| t33 | alac | 192000/24x2 | 8.0 | 0.0297 | 0.0294 | -1.0% | 0.0467 | 0.247 | 0.206 | 3.88 | 0.7178 | 0.7184 |
| t33 | pcm_s24le | 192000/24x2 | 8.0 | 0.0173 | 0.0162 | -6.7% | 0.0306 | 0.141 | 0.139 | 3.66 | 0.7178 | 0.7184 |
| t34 | alac | 192000/24x2 | 8.0 | 0.0274 | 0.0250 | -8.6% | 0.0526 | 0.214 | 0.209 | 3.86 | 0.7205 | 0.7214 |
| t34 | pcm_s24le | 192000/24x2 | 8.0 | 0.0187 | 0.0162 | -13.5% | 0.0311 | 0.141 | 0.145 | 3.77 | 0.7205 | 0.7214 |
| t35 | alac | 134560/24x2 | 8.0 | 0.0209 | 0.0193 | -7.6% | 0.0397 | 0.167 | 0.179 | 3.94 | 0.7124 | 0.7135 |
| t35 | pcm_s24le | 134560/24x2 | 8.0 | 0.0119 | 0.0106 | -10.7% | 0.0229 | 0.095 | 0.108 | 3.77 | 0.7124 | 0.7135 |
| t46 | alac | 48000/16x2 | 5.9 | 0.0075 | 0.0064 | -15.1% | 0.0176 | 0.041 | 0.065 | 3.79 | 0.3744 | 0.3821 |
| t46 | pcm_s16le | 48000/16x2 | 5.9 | 0.0049 | 0.0037 | -24.8% | 0.0160 | 0.024 | 0.075 | 3.66 | 0.3744 | 0.3821 |
| t47 | alac | 48000/16x2 | 4.8 | 0.0076 | 0.0067 | -11.0% | 0.0164 | 0.036 | 0.056 | 3.85 | 0.3593 | 0.3684 |
| t47 | pcm_s16le | 48000/16x2 | 4.8 | 0.0051 | 0.0044 | -13.2% | 0.0190 | 0.024 | 0.073 | 3.73 | 0.3593 | 0.3684 |
| t48 | alac | 48000/16x2 | 5.4 | 0.0077 | 0.0066 | -13.9% | 0.0155 | 0.038 | 0.057 | 3.88 | 0.3787 | 0.3870 |
| t48 | pcm_s16le | 48000/16x2 | 5.4 | 0.0045 | 0.0036 | -19.6% | 0.0176 | 0.022 | 0.079 | 3.74 | 0.3787 | 0.3870 |
| t49 | alac | 48000/16x2 | 5.4 | 0.0073 | 0.0064 | -12.5% | 0.0166 | 0.037 | 0.062 | 3.88 | 0.3649 | 0.3734 |
| t49 | pcm_s16le | 48000/16x2 | 5.4 | 0.0044 | 0.0037 | -15.5% | 0.0205 | 0.022 | 0.090 | 3.70 | 0.3649 | 0.3734 |
| t50 | alac | 48000/16x2 | 5.5 | 0.0073 | 0.0063 | -13.1% | 0.0149 | 0.037 | 0.053 | 3.90 | 0.3574 | 0.3666 |
| t50 | pcm_s16le | 48000/16x2 | 5.5 | 0.0045 | 0.0041 | -9.4% | 0.0163 | 0.025 | 0.071 | 3.70 | 0.3574 | 0.3666 |
| t51 | alac | 48000/16x2 | 6.0 | 0.0078 | 0.0085 | +9.0% | 0.0183 | 0.054 | 0.072 | 3.77 | 0.3868 | 0.3943 |
| t51 | pcm_s16le | 48000/16x2 | 6.0 | 0.0045 | 0.0039 | -12.1% | 0.0175 | 0.028 | 0.084 | 3.94 | 0.3868 | 0.3943 |
| t52 | alac | 48000/16x2 | 6.6 | 0.0075 | 0.0065 | -13.3% | 0.0146 | 0.046 | 0.061 | 3.88 | 0.3724 | 0.3793 |
| t52 | pcm_s16le | 48000/16x2 | 6.6 | 0.0043 | 0.0037 | -14.5% | 0.0153 | 0.027 | 0.075 | 3.89 | 0.3724 | 0.3793 |
| t53 | alac | 48000/16x2 | 60.6 | 0.0070 | 0.0062 | -11.8% | 0.0131 | 0.383 | 0.452 | 3.89 | 0.3764 | 0.3775 |
| t53 | pcm_s16le | 48000/16x2 | 60.6 | 0.0040 | 0.0038 | -5.3% | 0.0098 | 0.246 | 0.340 | 3.76 | 0.3764 | 0.3775 |
| t54 | alac | 48000/16x2 | 9.0 | 0.0073 | 0.0062 | -15.1% | 0.0153 | 0.059 | 0.090 | 3.87 | 0.3534 | 0.3589 |
| t54 | pcm_s16le | 48000/16x2 | 9.0 | 0.0048 | 0.0033 | -30.8% | 0.0135 | 0.032 | 0.089 | 3.80 | 0.3534 | 0.3589 |
| t56 | alac | 44100/16x2 | 5.0 | 0.0065 | 0.0056 | -14.9% | 0.0149 | 0.030 | 0.050 | 3.88 | 0.2848 | 0.2975 |
| t56 | pcm_s16le | 44100/16x2 | 5.0 | 0.0043 | 0.0036 | -16.7% | 0.0180 | 0.020 | 0.076 | 3.78 | 0.2848 | 0.2975 |
| t57 | alac | 44100/16x2 | 5.0 | 0.0064 | 0.0055 | -14.3% | 0.0146 | 0.030 | 0.048 | 3.89 | 0.2789 | 0.2927 |
| t57 | pcm_s16le | 44100/16x2 | 5.0 | 0.0042 | 0.0035 | -17.5% | 0.0169 | 0.020 | 0.069 | 3.61 | 0.2789 | 0.2927 |
| t58 | alac | 44100/16x2 | 5.0 | 0.0064 | 0.0056 | -11.7% | 0.0149 | 0.030 | 0.046 | 3.86 | 0.2660 | 0.2790 |
| t58 | pcm_s16le | 44100/16x2 | 5.0 | 0.0041 | 0.0041 | -1.1% | 0.0201 | 0.022 | 0.083 | 3.71 | 0.2660 | 0.2790 |
| t59 | alac | 44100/16x2 | 5.0 | 0.0067 | 0.0059 | -11.4% | 0.0167 | 0.032 | 0.055 | 3.89 | 0.2738 | 0.2882 |
| t59 | pcm_s16le | 44100/16x2 | 5.0 | 0.0047 | 0.0036 | -23.7% | 0.0176 | 0.020 | 0.070 | 3.73 | 0.2738 | 0.2882 |
| t60 | alac | 44100/16x1 | 5.2 | 0.0019 | 0.0020 | +7.9% | 0.0083 | 0.012 | 0.032 | 3.66 | 0.0921 | 0.1105 |
| t60 | pcm_s16le | 44100/16x1 | 5.2 | 0.0016 | 0.0014 | -14.3% | 0.0127 | 0.008 | 0.057 | 3.62 | 0.0921 | 0.1105 |
| t63 | alac | 44100/24x1 | 5.2 | 0.0031 | 0.0030 | -2.3% | 0.0097 | 0.020 | 0.036 | 3.66 | 0.3598 | 0.3790 |
| t63 | pcm_s24le | 44100/24x1 | 5.2 | 0.0021 | 0.0022 | +4.0% | 0.0082 | 0.014 | 0.031 | 3.64 | 0.3598 | 0.3790 |

Totals over 88 files (780 s audio): baseline 5.44 s CPU, final 4.85 s (-10.8%), FFmpeg 12.58 s (final -61.4%).

