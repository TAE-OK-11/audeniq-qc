# Verified pipeline optimization and fair FFmpeg 8.1.2 comparison — 2026-10-05

Starting commit: `f097643` (main). Final engine commit: `6db5c04`
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

## Results

### Full verified conversion (no QC) — CPU s / wall s / peak RSS MiB

| Input | s | Baseline CPU / wall / RSS MiB | **Final** CPU / wall / RSS | Final vs base CPU | Reference CPU / wall / RSS | FFmpeg 8.1.2 CPU / wall / RSS | Final vs FFmpeg CPU | Final vs FFmpeg wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| album44.wav | 205 | 0.714 / 0.744 / 3.61 | **0.592 / 0.625 / 3.68** | -17.1% | 0.778 / 0.805 / 5.14 | 1.496 / 0.738 / 18.59 | -60.4% | -15.4% |
| album44.m4a | 205 | 1.293 / 1.328 / 3.54 | **1.198 / 1.232 / 3.69** | -7.3% | 1.484 / 1.519 / 5.38 | 2.161 / 1.114 / 18.00 | -44.5% | +10.6% |
| album44.flac | 205 | 0.990 / 1.044 / 3.68 | **0.882 / 0.925 / 3.82** | -10.8% | 1.026 / 1.064 / 5.29 | 1.724 / 0.723 / 18.27 | -48.8% | +27.9% |
| album48.wav | 109 | 0.419 / 0.433 / 3.57 | **0.354 / 0.377 / 3.70** | -15.6% | 0.442 / 0.459 / 4.37 | 0.998 / 0.501 / 18.44 | -64.5% | -24.8% |
| album48.m4a | 109 | 0.742 / 0.764 / 3.61 | **0.673 / 0.705 / 3.66** | -9.3% | 0.858 / 0.884 / 4.52 | 1.386 / 0.695 / 17.95 | -51.4% | +1.4% |
| album48.flac | 109 | 0.545 / 0.563 / 3.63 | **0.483 / 0.500 / 3.77** | -11.3% | 0.587 / 0.611 / 4.74 | 1.052 / 0.467 / 17.95 | -54.1% | +7.3% |
| album96.wav | 41 | 0.370 / 0.396 / 3.61 | **0.308 / 0.337 / 3.70** | -16.7% | 0.403 / 0.433 / 6.34 | 0.620 / 0.322 / 21.12 | -50.3% | +4.7% |
| album96.m4a | 41 | 0.624 / 0.655 / 3.80 | **0.576 / 0.616 / 3.88** | -7.7% | 0.729 / 0.765 / 6.32 | 1.084 / 0.501 / 18.79 | -46.8% | +22.9% |
| album96.flac | 41 | 0.468 / 0.496 / 3.86 | **0.437 / 0.462 / 3.90** | -6.7% | 0.526 / 0.556 / 6.50 | 0.718 / 0.327 / 19.35 | -39.2% | +41.6% |
| album192.wav | 16 | 0.268 / 0.287 / 3.57 | **0.246 / 0.269 / 3.68** | -8.1% | 0.305 / 0.328 / 5.54 | 0.485 / 0.280 / 24.82 | -49.3% | -3.7% |
| album192.m4a | 16 | 0.445 / 0.477 / 3.83 | **0.425 / 0.460 / 3.91** | -4.3% | 0.576 / 0.601 / 5.73 | 0.826 / 0.408 / 18.78 | -48.5% | +12.7% |
| syn_tone.wav | 240 | 0.910 / 0.953 / 3.45 | **0.844 / 0.890 / 3.57** | -7.3% | 1.106 / 1.142 / 5.27 | 2.033 / 0.968 / 18.97 | -58.5% | -8.1% |
| syn_tone.m4a | 240 | 1.526 / 1.563 / 3.64 | **1.402 / 1.450 / 3.78** | -8.2% | 1.754 / 1.795 / 5.36 | 2.684 / 1.275 / 18.67 | -47.8% | +13.7% |
| syn_pink.wav | 240 | 1.035 / 1.108 / 3.61 | **0.926 / 1.009 / 3.66** | -10.5% | 1.169 / 1.242 / 4.59 | 1.913 / 0.914 / 19.25 | -51.6% | +10.4% |
| syn_pink.m4a | 240 | 1.670 / 1.748 / 3.74 | **1.632 / 1.699 / 3.90** | -2.3% | 1.995 / 2.092 / 4.70 | 2.838 / 1.370 / 18.68 | -42.5% | +24.0% |

### FLAC bytes (level 5)

| Input | Baseline | Final | Final vs base | FFmpeg 8.1.2 | Final vs FFmpeg | Final bytes / PCM bytes |
|---|---:|---:|---:|---:|---:|---:|
| album44.wav | 17,082,411 | 17,084,652 | +0.0131% | 17,144,474 | -0.3489% | 0.4716 |
| album48.wav | 7,825,952 | 7,827,212 | +0.0161% | 7,841,748 | -0.1854% | 0.3727 |
| album96.wav | 16,463,997 | 16,464,270 | +0.0017% | 16,474,306 | -0.0609% | 0.7017 |
| album192.wav | 13,256,263 | 13,256,944 | +0.0051% | 13,262,734 | -0.0437% | 0.7191 |
| syn_tone.wav | 22,895,861 | 22,896,019 | +0.0007% | 32,261,824 | -29.0306% | 0.3313 |
| syn_pink.wav | 54,203,896 | 54,203,886 | -0.0000% | 54,211,179 | -0.0135% | 0.7842 |

### PURE CODEC (not end-to-end; no fsync/verification)

| Input | FFmpeg encode-only CPU | FFmpeg decode+SHA-256 CPU | Native decode+SHA-256 CPU |
|---|---:|---:|---:|
| album44.wav | 0.448 | 0.455 | 0.060 |
| album44.m4a | 1.154 | 1.147 | 0.599 |
| album44.flac | 0.660 | 0.655 | 0.319 |
| album48.wav | 0.287 | 0.290 | 0.036 |
| album48.m4a | 0.727 | 0.702 | 0.383 |
| album48.flac | 0.366 | 0.364 | 0.173 |
| album96.wav | 0.196 | 0.170 | 0.029 |
| album96.m4a | 0.538 | 0.524 | 0.280 |
| album96.flac | 0.283 | 0.250 | 0.149 |
| album192.wav | 0.149 | 0.135 | 0.024 |
| album192.m4a | 0.371 | 0.365 | 0.192 |
| syn_tone.wav | 0.671 | 0.646 | 0.081 |
| syn_tone.m4a | 1.413 | 1.395 | 0.653 |
| syn_pink.wav | 0.635 | 0.554 | 0.078 |
| syn_pink.m4a | 1.489 | 1.420 | 0.725 |

### Full verified conversion + QC (native --analyze; FFmpeg ebur128 true peak)

| Input | s | Baseline CPU / wall / RSS MiB | **Final** CPU / wall / RSS | Final vs base CPU | Reference CPU / wall / RSS | FFmpeg 8.1.2 CPU / wall / RSS | Final vs FFmpeg CPU | Final vs FFmpeg wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| album44.wav | 205 | 0.926 / 0.964 / 3.89 | **0.814 / 0.852 / 3.95** | -12.1% | 0.957 / 1.007 / 5.40 | 2.265 / 1.078 / 24.39 | -64.1% | -20.9% |
| album44.m4a | 205 | 1.538 / 1.573 / 3.85 | **1.417 / 1.462 / 4.03** | -7.8% | 1.719 / 1.775 / 5.66 | 3.038 / 1.191 / 23.67 | -53.3% | +22.8% |
| album44.flac | 205 | 1.185 / 1.237 / 4.08 | **1.084 / 1.128 / 4.01** | -8.6% | 1.285 / 1.351 / 5.54 | 2.499 / 1.099 / 23.48 | -56.6% | +2.6% |
| album48.wav | 109 | 0.539 / 0.559 / 3.85 | **0.496 / 0.513 / 4.03** | -8.0% | 0.567 / 0.593 / 4.67 | 1.381 / 0.653 / 24.65 | -64.1% | -21.5% |
| album48.m4a | 109 | 0.875 / 0.909 / 3.80 | **0.799 / 0.818 / 3.91** | -8.6% | 0.971 / 1.006 / 4.78 | 1.665 / 0.674 / 23.59 | -52.0% | +21.5% |
| album48.flac | 109 | 0.676 / 0.696 / 4.04 | **0.584 / 0.599 / 4.13** | -13.5% | 0.674 / 0.697 / 5.04 | 1.319 / 0.589 / 23.33 | -55.7% | +1.8% |
| album96.wav | 41 | 0.442 / 0.465 / 3.77 | **0.389 / 0.412 / 4.02** | -12.0% | 0.488 / 0.516 / 6.34 | 0.780 / 0.346 / 29.94 | -50.1% | +18.8% |
| album96.m4a | 41 | 0.673 / 0.705 / 4.02 | **0.626 / 0.651 / 4.07** | -6.9% | 0.759 / 0.810 / 6.48 | 1.121 / 0.471 / 27.82 | -44.1% | +38.4% |
| album96.flac | 41 | 0.587 / 0.615 / 3.91 | **0.549 / 0.583 / 4.09** | -6.5% | 0.619 / 0.645 / 6.64 | 0.899 / 0.398 / 27.94 | -38.9% | +46.5% |
| album192.wav | 16 | 0.329 / 0.349 / 3.74 | **0.291 / 0.320 / 3.84** | -11.5% | 0.374 / 0.395 / 5.73 | 0.551 / 0.283 / 37.50 | -47.1% | +12.9% |
| album192.m4a | 16 | 0.534 / 0.562 / 3.89 | **0.491 / 0.518 / 4.00** | -8.1% | 0.603 / 0.640 / 5.71 | 0.880 / 0.437 / 32.06 | -44.2% | +18.8% |
| syn_tone.wav | 240 | 1.225 / 1.281 / 3.79 | **1.130 / 1.196 / 3.97** | -7.8% | 1.372 / 1.423 / 5.65 | 2.968 / 1.392 / 24.71 | -61.9% | -14.1% |
| syn_tone.m4a | 240 | 1.856 / 1.917 / 3.91 | **1.748 / 1.796 / 4.21** | -5.8% | 2.074 / 2.145 / 5.64 | 3.589 / 1.354 / 24.23 | -51.3% | +32.6% |
| syn_pink.wav | 240 | 1.435 / 1.530 / 3.88 | **1.307 / 1.393 / 4.12** | -8.9% | 1.501 / 1.596 / 4.82 | 3.013 / 1.361 / 25.07 | -56.6% | +2.4% |
| syn_pink.m4a | 240 | 2.091 / 2.210 / 3.93 | **1.834 / 1.909 / 4.20** | -12.3% | 2.405 / 2.517 / 4.98 | 3.808 / 1.409 / 24.22 | -51.8% | +35.5% |

### FLAC input, production default (verified frame copy; native only)

| Input | Baseline CPU/wall/RSS | Final CPU/wall/RSS | Δ CPU |
|---|---:|---:|---:|
| album44.flac | 0.322 / 0.357 / 3.32 | 0.339 / 0.373 / 3.37 | +5.1% |
| album48.flac | 0.188 / 0.203 / 3.25 | 0.187 / 0.206 / 3.33 | -0.4% |
| album96.flac | 0.177 / 0.196 / 3.25 | 0.175 / 0.199 / 3.38 | -1.1% |
| syn_pink.flac | 0.529 / 0.612 / 3.27 | 0.538 / 0.615 / 3.37 | +1.9% |

### Concurrency (24 jobs over album44.wav/.m4a, album48.wav, album96.m4a; 4 vCPU)

| Engine | Workers | Audio s per wall s | CPU/job s | p50 wall s | p95 wall s | Peak total RSS MiB | Max child RSS MiB | Failures |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| native | 1 | 190.9 | 0.689 | 0.633 | 1.269 | 3.9 | 4.03 | 0 |
| native | 2 | 384.4 | 0.680 | 0.640 | 1.254 | 7.7 | 4.05 | 0 |
| native | 4 | 629.1 | 0.679 | 0.646 | 1.381 | 15.2 | 3.95 | 0 |
| native | 8 | 746.8 | 0.654 | 1.247 | 2.722 | 30.2 | 3.87 | 0 |
| baseline | 1 | 180.1 | 0.737 | 0.708 | 1.388 | 3.8 | 3.91 | 0 |
| baseline | 2 | 355.3 | 0.742 | 0.745 | 1.344 | 7.5 | 3.86 | 0 |
| baseline | 4 | 617.4 | 0.750 | 0.792 | 1.438 | 14.9 | 3.86 | 0 |
| baseline | 8 | 625.4 | 0.771 | 1.494 | 2.848 | 29.6 | 3.93 | 0 |
| ffmpeg | 1 | 196.7 | 1.348 | 0.701 | 1.139 | 19.2 | 19.07 | 0 |
| ffmpeg | 2 | 308.5 | 1.299 | 0.881 | 1.495 | 38.6 | 19.32 | 0 |
| ffmpeg | 4 | 374.2 | 1.287 | 1.497 | 2.239 | 75.2 | 19.14 | 0 |
| ffmpeg | 8 | 390.4 | 1.288 | 2.531 | 4.503 | 149.7 | 19.11 | 0 |

## Accepted optimizations

| Commit | Change | Output | Effect |
| --- | --- | --- | --- |
| `5e45114` | **Residual-check frame verification.** The encoder no longer runs the serial LPC/fixed reconstruction recurrence to verify a frame. `flac_decode::verify` parses the frame exactly like `decode` (CRC-8, headers, warm-up, coefficients, Rice partitions/escapes, padding, CRC-16) and requires each parsed residual to equal `x[i] - pred(x[..i])` computed with the decoder's arithmetic on the source signal; by induction the decoder would reproduce the source exactly when this holds. Stereo is checked in the forward direction (each mode is a bijection on in-range samples); wasted/low bits are checked explicitly. Checks are independent per sample and AVX2-dispatched. | Byte-identical | Verification share of WAV-to-FLAC CPU fell from ~26% to ~20% (Rice parsing, ~12%, is unchanged). |
| `34461da` | **One pass for all fixed-predictor orders.** Orders 0..4 per-partition sums from repeated differences in one traversal instead of five passes. | Byte-identical | 3-11% conversion CPU. |
| `3d36f9c` | **One exact LPC candidate fewer.** Levels 4/5 exactly cost the highest order plus the best 128-point-sampled order (was best two). | +0.0007% to +0.016% bytes on real music | 3-8% conversion CPU. Costing all orders exactly would save only 0.015% more bytes for ~20% more CPU. |
| `875f362` | **Denormal flush off the K-weighting dependency chain** (cold branch instead of abs/compare/mask select). | QC JSON byte-identical | 4-12% standalone `analyze` CPU. |
| `6db5c04` | **Stereo mode hoisted out of FLAC reconstruction** with a 32-bit OR range accumulator. Fixes a 2-4% FLAC decode regression caused by `5e45114` changing LLVM's inlining of `decode`. | PCM identical | FLAC decode back to baseline. |

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
| QC planes via chunked deinterleave | +4.8% / +5.3% / -6.7%, more RSS | Slower on two of three inputs. |


### Per-track summary by group (real music, final vs baseline)

| Group | Audio s | Baseline CPU/audio-s | Final CPU/audio-s | Change | FFmpeg 8.1.2 CPU/audio-s |
| --- | ---: | ---: | ---: | ---: | ---: |
| 44.1/48 kHz 16-bit WAV | 320 | 0.00390 | 0.00342 | -12.2% | 0.01284 |
| 44.1/48 kHz 16-bit ALAC | 325 | 0.00668 | 0.00630 | -5.7% | 0.01374 |
| 96-192 kHz 24-bit WAV | 65 | 0.01163 | 0.01066 | -8.4% | 0.02265 |
| 96-192 kHz 24-bit ALAC | 65 | 0.01959 | 0.01766 | -9.9% | 0.03213 |

Short excerpts (5-60 s) include process start-up; the albums above are the
better steady-state measure.

## Correctness and gates (final commit)

* `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`
  (native and `reference-codecs`), debug and release `cargo test --workspace`:
  native 28 unit + 10 regression tests, reference 12 + 10. All pass.
* Qualification 596/596, codec stress 288/288, synthesized standards pass
  ([qualification.json](verified-pipeline/qualification.json),
  [codec-stress.json](verified-pipeline/codec-stress.json)).
* Mutation fuzz ([`tools/scripts/mutation_fuzz.py`](../tools/scripts/mutation_fuzz.py)):
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

RSS: median engine RSS rose about 0.05-0.15 MiB (1-4%), from about 40 KiB more
text (per-order scalar and AVX2 verifier kernels) and a 10 KiB planning table;
heap growth measured with massif is +10 KiB.

## Remaining hotspots (WAV to FLAC, final), ranked by expected ROI

1. **FLAC Rice parsing** (~12% of conversion, ~26% of FLAC frame copy, shared by
   all FLAC decoding): latency bound on lzcnt/shift per code; a table-driven
   multi-code decoder for small k is the next candidate.
2. **Exact LPC costing** (~10%): fuse the remaining two candidates into one pass
   over shared windows, or store the winning residual during costing to avoid
   recomputing it in the writer (~2%).
3. **MD5 (~11%) and SHA-256 (~8%)**: required by STREAMINFO and the PCM-hash
   contract; only a faster MD5 implementation would help.
4. **ALAC adaptive predictor** (~40% of ALAC input): branch-mispredict bound;
   five designs have now been measured and rejected.
5. **QC K-weighting IIR**: serial f64 chain fixed by bit-exact output.
6. `choose_rice` (~7%) and the bit writer (~6%): micro-level only.

Recommended next work (not started): a multi-code Rice table decoder with the
existing mutation tests, and a fused two-candidate LPC costing kernel.
Repository hygiene: `reference-target/` (1,784 build artifacts) is tracked in git.

### Per-track real music (CC0 IETF FLAC test-bench recordings)

| Track | Codec | Rate/bits | s | Base CPU/audio-s | Final CPU/audio-s | Δ | FFmpeg CPU/audio-s | Final wall s | FFmpeg wall s | Final RSS MiB | Final bytes/PCM | FFmpeg bytes/PCM |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| t01 | alac | 44100/16x2 | 7.0 | 0.0067 | 0.0067 | +1.0% | 0.0146 | 0.050 | 0.075 | 3.81 | 0.4405 | 0.4481 |
| t01 | pcm_s16le | 44100/16x2 | 7.0 | 0.0040 | 0.0032 | -19.9% | 0.0158 | 0.025 | 0.082 | 3.69 | 0.4405 | 0.4481 |
| t02 | alac | 44100/16x2 | 7.0 | 0.0064 | 0.0061 | -5.8% | 0.0147 | 0.045 | 0.066 | 3.82 | 0.4593 | 0.4661 |
| t02 | pcm_s16le | 44100/16x2 | 7.0 | 0.0038 | 0.0034 | -11.8% | 0.0144 | 0.028 | 0.079 | 3.77 | 0.4593 | 0.4661 |
| t03 | alac | 44100/16x2 | 7.0 | 0.0062 | 0.0060 | -3.5% | 0.0134 | 0.054 | 0.059 | 3.76 | 0.4216 | 0.4283 |
| t03 | pcm_s16le | 44100/16x2 | 7.0 | 0.0038 | 0.0036 | -4.0% | 0.0144 | 0.027 | 0.076 | 3.80 | 0.4216 | 0.4283 |
| t04 | alac | 44100/16x2 | 7.0 | 0.0063 | 0.0057 | -9.2% | 0.0136 | 0.043 | 0.059 | 3.87 | 0.4325 | 0.4393 |
| t04 | pcm_s16le | 44100/16x2 | 7.0 | 0.0035 | 0.0031 | -11.3% | 0.0126 | 0.024 | 0.066 | 3.67 | 0.4325 | 0.4393 |
| t05 | alac | 44100/16x2 | 7.0 | 0.0066 | 0.0062 | -6.9% | 0.0137 | 0.046 | 0.063 | 3.83 | 0.4680 | 0.4748 |
| t05 | pcm_s16le | 44100/16x2 | 7.0 | 0.0037 | 0.0032 | -13.1% | 0.0131 | 0.025 | 0.069 | 3.82 | 0.4680 | 0.4748 |
| t06 | alac | 44100/16x2 | 7.0 | 0.0066 | 0.0059 | -9.5% | 0.0133 | 0.045 | 0.060 | 3.77 | 0.4572 | 0.4641 |
| t06 | pcm_s16le | 44100/16x2 | 7.0 | 0.0040 | 0.0032 | -19.1% | 0.0129 | 0.025 | 0.070 | 3.71 | 0.4572 | 0.4641 |
| t07 | alac | 44100/16x2 | 7.0 | 0.0064 | 0.0060 | -5.9% | 0.0143 | 0.045 | 0.061 | 3.76 | 0.4611 | 0.4679 |
| t07 | pcm_s16le | 44100/16x2 | 7.0 | 0.0049 | 0.0033 | -32.4% | 0.0146 | 0.026 | 0.080 | 3.82 | 0.4611 | 0.4679 |
| t08 | alac | 44100/16x2 | 7.0 | 0.0065 | 0.0064 | -1.9% | 0.0142 | 0.049 | 0.058 | 3.83 | 0.4515 | 0.4583 |
| t08 | pcm_s16le | 44100/16x2 | 7.0 | 0.0039 | 0.0032 | -16.8% | 0.0142 | 0.025 | 0.078 | 3.90 | 0.4515 | 0.4583 |
| t09 | alac | 44100/16x2 | 7.0 | 0.0066 | 0.0066 | -0.7% | 0.0137 | 0.050 | 0.062 | 3.80 | 0.4524 | 0.4593 |
| t09 | pcm_s16le | 44100/16x2 | 7.0 | 0.0036 | 0.0032 | -11.5% | 0.0136 | 0.024 | 0.071 | 3.88 | 0.4524 | 0.4593 |
| t10 | alac | 44100/16x2 | 7.0 | 0.0063 | 0.0053 | -16.2% | 0.0124 | 0.041 | 0.058 | 3.77 | 0.3811 | 0.3888 |
| t10 | pcm_s16le | 44100/16x2 | 7.0 | 0.0038 | 0.0035 | -8.7% | 0.0146 | 0.027 | 0.082 | 3.61 | 0.3811 | 0.3888 |
| t11 | alac | 44100/16x2 | 5.5 | 0.0062 | 0.0055 | -11.2% | 0.0134 | 0.033 | 0.047 | 3.75 | 0.5099 | 0.5279 |
| t11 | pcm_s16le | 44100/16x2 | 5.5 | 0.0039 | 0.0034 | -12.5% | 0.0182 | 0.021 | 0.082 | 3.70 | 0.5099 | 0.5279 |
| t12 | alac | 44100/16x2 | 5.0 | 0.0072 | 0.0061 | -14.6% | 0.0157 | 0.033 | 0.052 | 3.79 | 0.5506 | 0.5629 |
| t12 | pcm_s16le | 44100/16x2 | 5.0 | 0.0044 | 0.0040 | -8.5% | 0.0202 | 0.022 | 0.082 | 3.70 | 0.5506 | 0.5629 |
| t13 | alac | 44100/16x2 | 5.0 | 0.0068 | 0.0062 | -8.7% | 0.0153 | 0.035 | 0.050 | 3.81 | 0.5431 | 0.5548 |
| t13 | pcm_s16le | 44100/16x2 | 5.0 | 0.0044 | 0.0035 | -20.0% | 0.0187 | 0.020 | 0.076 | 3.61 | 0.5431 | 0.5548 |
| t14 | alac | 44100/16x2 | 4.9 | 0.0073 | 0.0062 | -15.1% | 0.0170 | 0.033 | 0.054 | 3.83 | 0.3318 | 0.3507 |
| t14 | pcm_s16le | 44100/16x2 | 4.9 | 0.0047 | 0.0041 | -11.2% | 0.0193 | 0.024 | 0.081 | 3.65 | 0.3318 | 0.3507 |
| t15 | alac | 44100/16x2 | 5.0 | 0.0068 | 0.0062 | -9.2% | 0.0150 | 0.034 | 0.052 | 3.78 | 0.5479 | 0.5568 |
| t15 | pcm_s16le | 44100/16x2 | 5.0 | 0.0045 | 0.0040 | -10.2% | 0.0179 | 0.024 | 0.073 | 3.89 | 0.5479 | 0.5568 |
| t16 | alac | 44100/16x2 | 4.7 | 0.0068 | 0.0075 | +10.9% | 0.0145 | 0.038 | 0.047 | 3.87 | 0.5588 | 0.5719 |
| t16 | pcm_s16le | 44100/16x2 | 4.7 | 0.0044 | 0.0037 | -15.2% | 0.0189 | 0.022 | 0.071 | 3.62 | 0.5588 | 0.5719 |
| t17 | alac | 44100/16x2 | 5.3 | 0.0064 | 0.0059 | -7.9% | 0.0137 | 0.034 | 0.047 | 3.78 | 0.5375 | 0.5474 |
| t17 | pcm_s16le | 44100/16x2 | 5.3 | 0.0040 | 0.0035 | -12.4% | 0.0165 | 0.021 | 0.067 | 3.80 | 0.5375 | 0.5474 |
| t18 | alac | 44100/16x2 | 5.0 | 0.0062 | 0.0058 | -6.1% | 0.0140 | 0.031 | 0.047 | 3.78 | 0.5430 | 0.5546 |
| t18 | pcm_s16le | 44100/16x2 | 5.0 | 0.0040 | 0.0035 | -13.0% | 0.0166 | 0.019 | 0.066 | 3.77 | 0.5430 | 0.5546 |
| t24 | alac | 44100/16x2 | 25.0 | 0.0064 | 0.0058 | -9.0% | 0.0112 | 0.151 | 0.159 | 3.77 | 0.5278 | 0.5309 |
| t24 | pcm_s16le | 44100/16x2 | 25.0 | 0.0035 | 0.0030 | -14.5% | 0.0088 | 0.080 | 0.136 | 3.63 | 0.5278 | 0.5309 |
| t25 | alac | 44100/16x2 | 25.0 | 0.0065 | 0.0068 | +3.8% | 0.0129 | 0.175 | 0.169 | 3.77 | 0.5304 | 0.5336 |
| t25 | pcm_s16le | 44100/16x2 | 25.0 | 0.0035 | 0.0030 | -15.7% | 0.0086 | 0.081 | 0.129 | 3.70 | 0.5304 | 0.5336 |
| t26 | alac | 44100/16x2 | 25.0 | 0.0070 | 0.0067 | -4.5% | 0.0129 | 0.173 | 0.171 | 3.76 | 0.5203 | 0.5232 |
| t26 | pcm_s16le | 44100/16x2 | 25.0 | 0.0037 | 0.0038 | +3.1% | 0.0093 | 0.100 | 0.137 | 3.71 | 0.5203 | 0.5232 |
| t28 | alac | 96000/24x2 | 8.7 | 0.0146 | 0.0135 | -7.9% | 0.0263 | 0.125 | 0.122 | 3.95 | 0.6897 | 0.6913 |
| t28 | pcm_s24le | 96000/24x2 | 8.7 | 0.0090 | 0.0094 | +3.6% | 0.0177 | 0.090 | 0.097 | 3.65 | 0.6897 | 0.6913 |
| t29 | alac | 96000/24x2 | 8.0 | 0.0192 | 0.0129 | -32.7% | 0.0253 | 0.109 | 0.110 | 3.82 | 0.7074 | 0.7098 |
| t29 | pcm_s24le | 96000/24x2 | 8.0 | 0.0091 | 0.0095 | +4.2% | 0.0198 | 0.082 | 0.101 | 3.64 | 0.7074 | 0.7098 |
| t30 | alac | 96000/24x2 | 8.0 | 0.0149 | 0.0133 | -10.4% | 0.0251 | 0.115 | 0.109 | 3.92 | 0.7032 | 0.7053 |
| t30 | pcm_s24le | 96000/24x2 | 8.0 | 0.0091 | 0.0084 | -7.8% | 0.0205 | 0.076 | 0.096 | 3.64 | 0.7032 | 0.7053 |
| t31 | alac | 96000/24x2 | 8.0 | 0.0148 | 0.0138 | -7.2% | 0.0259 | 0.116 | 0.121 | 3.78 | 0.7023 | 0.7043 |
| t31 | pcm_s24le | 96000/24x2 | 8.0 | 0.0091 | 0.0077 | -15.6% | 0.0200 | 0.068 | 0.096 | 3.64 | 0.7023 | 0.7043 |
| t32 | alac | 96000/24x2 | 8.0 | 0.0151 | 0.0149 | -0.7% | 0.0274 | 0.127 | 0.131 | 3.89 | 0.7067 | 0.7087 |
| t32 | pcm_s24le | 96000/24x2 | 8.0 | 0.0090 | 0.0081 | -9.7% | 0.0200 | 0.071 | 0.095 | 3.64 | 0.7067 | 0.7087 |
| t33 | alac | 192000/24x2 | 8.0 | 0.0297 | 0.0262 | -11.6% | 0.0466 | 0.224 | 0.198 | 3.87 | 0.7178 | 0.7184 |
| t33 | pcm_s24le | 192000/24x2 | 8.0 | 0.0173 | 0.0155 | -10.3% | 0.0293 | 0.136 | 0.136 | 3.59 | 0.7178 | 0.7184 |
| t34 | alac | 192000/24x2 | 8.0 | 0.0283 | 0.0277 | -2.1% | 0.0455 | 0.238 | 0.189 | 3.80 | 0.7205 | 0.7214 |
| t34 | pcm_s24le | 192000/24x2 | 8.0 | 0.0175 | 0.0158 | -9.9% | 0.0304 | 0.145 | 0.136 | 3.74 | 0.7205 | 0.7214 |
| t35 | alac | 134560/24x2 | 8.0 | 0.0206 | 0.0192 | -6.6% | 0.0354 | 0.172 | 0.160 | 3.76 | 0.7124 | 0.7135 |
| t35 | pcm_s24le | 134560/24x2 | 8.0 | 0.0131 | 0.0110 | -16.1% | 0.0239 | 0.098 | 0.116 | 3.70 | 0.7124 | 0.7135 |
| t46 | alac | 48000/16x2 | 5.9 | 0.0076 | 0.0067 | -12.1% | 0.0162 | 0.042 | 0.063 | 3.82 | 0.3744 | 0.3821 |
| t46 | pcm_s16le | 48000/16x2 | 5.9 | 0.0043 | 0.0036 | -14.8% | 0.0153 | 0.024 | 0.069 | 3.70 | 0.3744 | 0.3821 |
| t47 | alac | 48000/16x2 | 4.8 | 0.0077 | 0.0070 | -8.3% | 0.0199 | 0.037 | 0.063 | 3.80 | 0.3593 | 0.3684 |
| t47 | pcm_s16le | 48000/16x2 | 4.8 | 0.0042 | 0.0037 | -10.7% | 0.0189 | 0.020 | 0.074 | 3.71 | 0.3593 | 0.3684 |
| t48 | alac | 48000/16x2 | 5.4 | 0.0076 | 0.0067 | -11.7% | 0.0164 | 0.038 | 0.059 | 3.78 | 0.3787 | 0.3870 |
| t48 | pcm_s16le | 48000/16x2 | 5.4 | 0.0042 | 0.0040 | -4.2% | 0.0172 | 0.024 | 0.074 | 3.65 | 0.3787 | 0.3870 |
| t49 | alac | 48000/16x2 | 5.4 | 0.0073 | 0.0072 | -2.2% | 0.0162 | 0.042 | 0.058 | 3.76 | 0.3649 | 0.3734 |
| t49 | pcm_s16le | 48000/16x2 | 5.4 | 0.0044 | 0.0036 | -18.3% | 0.0163 | 0.021 | 0.070 | 3.68 | 0.3649 | 0.3734 |
| t50 | alac | 48000/16x2 | 5.5 | 0.0072 | 0.0065 | -9.7% | 0.0154 | 0.038 | 0.058 | 3.72 | 0.3574 | 0.3666 |
| t50 | pcm_s16le | 48000/16x2 | 5.5 | 0.0042 | 0.0036 | -14.6% | 0.0163 | 0.022 | 0.070 | 3.68 | 0.3574 | 0.3666 |
| t51 | alac | 48000/16x2 | 6.0 | 0.0073 | 0.0066 | -8.6% | 0.0145 | 0.043 | 0.055 | 3.80 | 0.3868 | 0.3943 |
| t51 | pcm_s16le | 48000/16x2 | 6.0 | 0.0042 | 0.0035 | -15.3% | 0.0185 | 0.024 | 0.084 | 3.75 | 0.3868 | 0.3943 |
| t52 | alac | 48000/16x2 | 6.6 | 0.0070 | 0.0068 | -2.3% | 0.0146 | 0.048 | 0.063 | 3.77 | 0.3724 | 0.3793 |
| t52 | pcm_s16le | 48000/16x2 | 6.6 | 0.0043 | 0.0037 | -14.1% | 0.0145 | 0.027 | 0.071 | 3.72 | 0.3724 | 0.3793 |
| t53 | alac | 48000/16x2 | 60.6 | 0.0070 | 0.0067 | -4.6% | 0.0130 | 0.420 | 0.407 | 3.78 | 0.3764 | 0.3775 |
| t53 | pcm_s16le | 48000/16x2 | 60.6 | 0.0038 | 0.0033 | -12.2% | 0.0083 | 0.212 | 0.270 | 3.71 | 0.3764 | 0.3775 |
| t54 | alac | 48000/16x2 | 9.0 | 0.0071 | 0.0066 | -6.0% | 0.0159 | 0.063 | 0.085 | 3.66 | 0.3534 | 0.3589 |
| t54 | pcm_s16le | 48000/16x2 | 9.0 | 0.0041 | 0.0044 | +8.4% | 0.0135 | 0.042 | 0.093 | 3.82 | 0.3534 | 0.3589 |
| t56 | alac | 44100/16x2 | 5.0 | 0.0085 | 0.0057 | -32.5% | 0.0158 | 0.031 | 0.051 | 3.82 | 0.2848 | 0.2975 |
| t56 | pcm_s16le | 44100/16x2 | 5.0 | 0.0044 | 0.0036 | -16.8% | 0.0178 | 0.021 | 0.076 | 3.68 | 0.2848 | 0.2975 |
| t57 | alac | 44100/16x2 | 5.0 | 0.0065 | 0.0058 | -10.5% | 0.0157 | 0.031 | 0.055 | 3.76 | 0.2789 | 0.2927 |
| t57 | pcm_s16le | 44100/16x2 | 5.0 | 0.0051 | 0.0037 | -26.9% | 0.0199 | 0.021 | 0.086 | 3.64 | 0.2789 | 0.2927 |
| t58 | alac | 44100/16x2 | 5.0 | 0.0068 | 0.0076 | +10.7% | 0.0166 | 0.040 | 0.058 | 3.76 | 0.2660 | 0.2790 |
| t58 | pcm_s16le | 44100/16x2 | 5.0 | 0.0039 | 0.0037 | -6.5% | 0.0193 | 0.020 | 0.080 | 3.71 | 0.2660 | 0.2790 |
| t59 | alac | 44100/16x2 | 5.0 | 0.0070 | 0.0066 | -5.6% | 0.0163 | 0.035 | 0.055 | 3.73 | 0.2738 | 0.2882 |
| t59 | pcm_s16le | 44100/16x2 | 5.0 | 0.0045 | 0.0036 | -20.9% | 0.0176 | 0.021 | 0.077 | 3.68 | 0.2738 | 0.2882 |
| t60 | alac | 44100/16x1 | 5.2 | 0.0019 | 0.0023 | +21.0% | 0.0080 | 0.013 | 0.034 | 3.57 | 0.0921 | 0.1105 |
| t60 | pcm_s16le | 44100/16x1 | 5.2 | 0.0014 | 0.0015 | +5.6% | 0.0114 | 0.010 | 0.050 | 3.47 | 0.0921 | 0.1105 |
| t63 | alac | 44100/24x1 | 5.2 | 0.0030 | 0.0029 | -4.1% | 0.0098 | 0.017 | 0.040 | 3.71 | 0.3598 | 0.3790 |
| t63 | pcm_s24le | 44100/24x1 | 5.2 | 0.0019 | 0.0018 | -0.6% | 0.0075 | 0.011 | 0.030 | 3.57 | 0.3598 | 0.3790 |

Totals over 88 files (780 s audio): baseline 5.45 s CPU, final 4.98 s (-8.5%), FFmpeg 12.16 s (final -59.0%).

