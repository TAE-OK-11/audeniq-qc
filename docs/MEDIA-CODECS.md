# Native media codecs and reference build

Default builds use AUDENIQ's Rust WAV/AIFF readers, ALAC/FLAC/TTA/WavPack decoders, FLAC encoder and bounded M4A/FLAC containers. No Symphonia is linked in the default dependency graph. The ALAC/FLAC algorithms were ported and reworked from the pinned FFmpeg sources in THIRD_PARTY.md; upstream attribution is retained. General JSON, hash/CRC and JPEG/PNG libraries remain, as requested for the codec/container-first scope.

`reference-codecs` selects the existing Symphonia ALAC/FLAC codecs/demuxers; it keeps the same encoder, QC, resampler, checksum packing and publication checks. It is a comparison build, not a production fallback silently used by the native build.

```sh
cargo build --workspace --release --locked
cargo build --release --features reference-codecs --locked --target-dir reference-target
target/release/audeniq-qc capabilities
reference-target/release/audeniq-qc capabilities
cargo tree --locked -e normal
target/release/audeniq-qc-tools benchmark-convert --wav-alac-only --seconds 240 --repeats 7 --reference-binary reference-target/release/audeniq-qc --output normalization-benchmark.json
target/release/audeniq-qc-tools benchmark --wav-alac-only --seconds 240 --repeats 7 --reference-binary reference-target/release/audeniq-qc --output analysis-benchmark.json
target/release/audeniq-qc-tools benchmark-review --seconds 120 --repeats 5 --reference-binary reference-target/release/audeniq-qc --fingerprint --output review-fingerprint-benchmark.json
```

The media-codecs workflow runs these three-way comparisons against FFmpeg on native Arm and x86 runners, after qualifying both codec paths. It records repeated CPU/wall/RSS samples, actual CPU identity, binary/input hashes and output sizes. Native/reference QC values and fingerprints must match exactly; every converted output must independently decode to the expected PCM SHA-256 through FFmpeg. Both builds use fused conversion+QC when benchmarking review work. Generic dependencies are retained in both builds.

The first comparison (`c9809254`, workflow 37194355514) passed on EPYC 7763 and Neoverse N2, but exposed slower native conversions, especially ALAC on Arm. Raw measurements are preserved in `benchmark-media-before-arm.json` and `benchmark-media-before-x86.json`. Follow-up optimization decodes whole FLAC/ALAC Rice values directly from the word cache, retains bounded cross-word fallbacks, avoids partial-frame retries using validated STREAMINFO size bounds, and vectorizes stereo reconstruction while preserving sample-range checks. Independent boundary tests cover normal/escaped Rice values across every cache offset. Later results must identify their tested source/CPU rather than presenting these initial results as the optimized engine.

The fused-Rice checkpoint `f88d3820` passed workflow 37200979457 on EPYC 7763 and Neoverse N2; its raw results are preserved in `benchmark-media-rice-arm.json` and `benchmark-media-rice-x86.json`. Arm's four-minute ALAC conversion reached 1.51s (FFmpeg 1.51s, reference 1.48s), using 1.42 CPU seconds (FFmpeg 2.03s) and 3040KiB peak RSS (FFmpeg 54600KiB). It tied FFmpeg in wall time and was still slightly slower than reference. The next candidate targets decoder LPC 4/8 with NEON widening products and rolling predictor vectors; source/CRC/MD5/count/range checks remain. An AVX2 decoder candidate was rejected after local measurements showed a regression; x86 keeps scalar sequential restoration and the existing faster AVX2 encoder/QC kernels. `--scalar` also controls the new native decoder DSP. A decode/hash-only benchmark isolates decoder effects from encoder/meter acceleration; it is additional to the three requested conversion/QC comparisons.

## Bounds and integrity

* M4A: single ALAC track and description, non-fragmented sample tables, bounded moov/packet/index storage, monotonic non-overlapping chunks inside mdat, exact stts/stsz/chunk counts and ALAC packet frame counts. Audio timescale must match the ALAC configuration. Version-0 audio entries are supported; additional tracks, compact/fragmented tables and other entry versions are rejected.
* ALAC: mono/stereo, 16/24-bit, escaped and compressed packets, extra bits, adaptive Rice/zero runs, LPC 0..31 and mode-15 first pass. Bounded MSB word cache/CLZ scans, reusable in-place planes and specialized common predictor orders. Explicit end marker and zero padding are required.
* FLAC: constant/verbatim, fixed 0..4 and LPC 1..32, Rice methods 0/1 and escape partitions, wasted bits and all mono/stereo channel assignments. Header/frame CRC, exact frame/sample sequence/count, sample range, trailing-data rejection and complete STREAMINFO PCM MD5 verification (when present). Bounded streaming windows retain only the current unread region; verified frame-copy is preserved.

ALAC has no per-packet PCM checksum, so a structurally valid altered ALAC packet cannot always be identified as corrupted without an independent trusted content hash. Neither codec can infer the intended music from arbitrary valid replacement PCM. Conversion verifies preservation of the decoded source.

Local initial validation passed the 596-check qualification, 288-check codec stress and 20 synthesized loudness/true-peak cases. Tests additionally exercise MSB cache boundaries, escaped ALAC extrema/every-byte truncation, malformed packets, FLAC predictor orders including 32/every-byte truncation and malformed atom lengths. This is not a full official EBU certification, a production music corpus, or a Graviton4 measurement. Performance results must identify their tested source/host; self-development alone is not proof of better speed.
