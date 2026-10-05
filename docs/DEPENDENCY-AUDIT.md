# Encoder and dependency internalization audit — 2026-10-05 KST

Audited repository revision: `519093c86010b2c8ebea711e6fef208dec0ef172`. Engine code is the accepted
`b8918279`; subsequent commits contain reports/documentation only.
This is an implementation/dependency audit, not a new optimization patch or a
claim of measured speedup from dependency removal. Backend integration is deferred.

## What is already in the repository

| Component | Implementation / location | External codec library required by default? |
| --- | --- | --- |
| FLAC encoding, levels 0..8 | `src/flac.rs`: constant/verbatim/fixed/LPC, Rice coding, stereo model selection, STREAMINFO, frame writing | No |
| WAV / raw s32le output | `src/pcm.rs`: PCM export, WAV header, output re-read/hash verification | No |
| FLAC / ALAC / TTA / WavPack lossless decoding | Native Rust modules; FFmpeg-derived provenance retained | No |
| WAV / AIFF / M4A / FLAC containers and metadata | `audio.rs`, `m4a.rs`, `mp4.rs`, `probe.rs`, native FLAC decoder | No |
| LUFS / true peak / peak / clipping / silence | `meter.rs`, `resample.rs`, `kernels.rs` | No |
| Fingerprint mono downmix / sinc resampling | `resample.rs`, `kernels.rs` | No |
| FLAC CRC8 / CRC16 | `bits.rs`: native tables, CRC16 slicing-by-eight | No |
| SHA-256, MD5, IEEE CRC32 | `sha256.rs`, `md5.rs`, `crc32.rs` (in-repository since round four) | No |
| JPEG / PNG cover decode | `jpeg.rs`, `png.rs`, `inflate.rs` (in-repository since round seven; image 0.24.9 kept as test oracle) | No |
| JSON reports | `json.rs` (in-repository since round eight; serde_json 1.0.140 kept as test oracle) | No |

FLAC encoding calls no libFLAC, FFmpeg process or native codec FFI. ALAC,
WavPack and TTA encoders are not implemented: they are input formats that can
be normalized to FLAC. Adding those encoders would be new output functionality,
not removal of an existing external encoder.

Symphonia is optional behind `reference-codecs`, absent from the default build.
Both compared codec builds share our FLAC encoder and meters, so a
native/reference WAV conversion timing difference does not prove that there are
two different external/native encoders.

## Remaining direct dependencies and candidates

Status after round eight: the default build has **no** third-party crates
(`cargo tree --locked -e normal` lists only audeniq-qc and the tools crate);
all of the following are now dev-dependency test oracles only. At the time of
this audit the default direct dependency list had six crates: serde, serde_json, sha2,
md-5, crc32fast and image. Cargo.lock also contains optional reference/build
dependencies; presence in the lockfile does not establish runtime linkage.
The successful default `cargo tree --locked -e normal` from the fresh x86 CI
job is preserved in [dependency-audit-evidence.json](dependency-audit-evidence.json).
Its proc-macro subtree is compile-time tooling, not a set of runtime parsers.

| Candidate | Current pinned dependency | Use and potential internalization | Priority / expected scope |
| --- | --- | --- | --- |
| FLAC PCM MD5 | md-5 0.10.6 | Specialized bounded streaming MD5, shared encoder/decoder primitive; process canonical 16/24-bit PCM through reusable chunk scratch rather than a full second block buffer where beneficial | First external primitive candidate for audio; time packing and compression separately |
| PCM SHA-256 | sha2 0.11.0 | Keep only SHA-256 streaming state, software fallback, AArch64 SHA2 and x86 SHA-NI dispatch | Second; retain existing hardware acceleration and streaming/unaligned correctness |
| IEEE CRC32 | Done: `src/crc32.rs` (crc32fast 1.5.2 kept as test oracle; still linked through PNG) | AVX-512/AVX2 VPCLMUL, PCLMUL, AArch64 PMULL folding with tree lane reduction; slicing-by-8 fallback | Measured, see BENCHMARK-VERIFIED-PIPELINE.md round five |
| PNG cover validation | Done: `src/png.rs` + `src/inflate.rs` | Chunks, CRC, zlib/DEFLATE and per-row filter checks with the previous decoder's exact acceptance; 32 KiB window, no image buffer | See BENCHMARK-VERIFIED-PIPELINE.md round seven |
| JPEG cover validation | Done: `src/jpeg.rs` | Markers, tables and full Huffman decoding of baseline, progressive and lossless scans with the previous decoder's exact acceptance; coefficients kept only for progressive | See BENCHMARK-VERIFIED-PIPELINE.md round seven |
| Fixed JSON output | Done: `src/json.rs` | Direct writer for the fixed reports (byte-identical to serde_json, ryu float digits), `Value`, `json!` and a correctly rounded parser for the tools | See BENCHMARK-VERIFIED-PIPELINE.md round eight |

These are existing Rust libraries. Internalization usually means taking only
the required Rust implementation into the repository, retaining licenses and
provenance, then specializing/testing it; it is not necessarily C-to-Rust porting.
Vendoring unchanged code transfers maintenance but does not establish speed/RAM
improvement. Each candidate needs an isolated reference implementation during
qualification and same-host end-to-end benchmarking before replacing defaults.

### Dependency details that affect removal

* md-5 uses digest 0.10.7, block-buffer 0.10.4, crypto-common 0.1.7 and
  generic-array 0.14.7; sha2 uses the separate digest 0.11.3, block-buffer 0.12.1,
  crypto-common 0.2.2 and hybrid-array 0.4.15. Replacing MD5 can retire its older
  branch if no other selected feature needs it; shared cfg-if/typenum remain.
  A md-5 0.11 comparison is also useful as an external-control experiment
  before attributing gains specifically to self-development.
* sha2 already selects AArch64 SHA2 or x86 SHA-NI at runtime, with software
  fallback. AVX2 is not a replacement for those SHA-256 instructions. MD5 has
  different arithmetic and cannot be replaced by SHA-256 or sped up with SHA2
  instructions while retaining its required FLAC digest.
* crc32fast is shared by TTA, PNG and flate2. Replacing only the call in
  `bits.rs` removes a direct dependency edge but does not eliminate the crate
  from a build that still uses the current PNG stack. x86 SSE4.2 CRC32 computes
  CRC32C, not the IEEE polynomial needed here. Preserve initialization,
  reflected polynomial, final XOR and incremental composition.
* The default PNG tree contains **two miniz_oxide versions, 0.8.9 and 0.9.1**,
  plus fdeflate. This is source/build duplication; no binary-size or runtime-RAM
  penalty has been measured. Do not claim all implementations execute per image.
* No Rayon dependency appears in the audited default tree. Do not blame
  unsolicited JPEG thread pools for the audio results.
* Image decoding never runs in audio conversion/QC commands. Feature-gating
  covers out of an audio-only build can reduce dependency/build scope, but
  cannot be described as removing full-image allocations from the current
  audio hot path.

## Encoder work worth doing before larger ports

The encoder is already native, but source inspection identifies targeted work:

1. **Bounded LPC candidates and coefficients.** `Plan::new` constructs
   `Vec::with_capacity(3)` per channel/model plan and collects 2/4/8 coefficient
   vectors. Replace these small bounded heaps with fixed arrays plus length;
   store selected coefficients inline in `Mode::Lpc`. Preserve sorting/ties,
   rounding, cost calculation and encoded output decisions.
2. **Residual generation and cost passes.** Fixed-predictor planning materializes
   the difference buffer, zigzag residuals, a mean pass and neighboring Rice
   cost passes. The current residual pool already reuses large buffers.
   Fuse reductions with generation where exact overflow handling and cost
   decisions can be preserved; do not claim that the pool currently allocates
   one new residual vector per block.
3. **PCM plane preparation.** Deinterleave/right-shift, covariance and optional
   mid/side generation traverse channel data separately. Evaluate a combined
   preparation kernel, while preserving floating reduction choices or reporting
   any compression-model change. Full input blocks are already borrowed directly
   when possible; tails reuse pending storage.
4. **Hash packing.** SHA-256's canonical s32le input already borrows PCM bytes
   on little-endian ARM/x86 (`audio::pcm_bytes`), so there is no extra SHA PCM
   allocation there to remove. MD5 requires packed original-width PCM; use
   bounded streaming scratch or a specialized update path, not a change of
   digest input convention. Preserve independent output redecoding/verification.
5. **Measured ISA candidates.** Rice costs have an explicit NEON kernel and
   scalar x86 code; test an exact AVX2 reduction against compiler-generated
   scalar code rather than assuming it wins. The block Rice writer already
   packs via a word accumulator. CRC16 is already slicing-by-eight; any
   PMULL/PCLMUL CRC16 experiment must preserve its separate polynomial and
   justify additional dispatch/code size.

The smaller candidate allocations and unfused passes are **source findings**.
Their attributable CPU/RAM cost has not been isolated; implementation and
before/after evidence are required before presenting percentages.

## Existing profiling evidence

Fresh ARM rerun, four-minute ALAC conversion+QC, diagnostic snapshot 3:

| Inclusive instrumented stage | Calls | Wall milliseconds |
| --- | ---: | ---: |
| Encoder, including its MD5/packing and model work | 2,813 | 486.2 |
| Encoder planning, nested inside encoder | 5,626 | 281.6 |
| Output FLAC PCM packing + MD5 check, nested inside output verify | 2,813 | 102.2 |
| Source PCM SHA-256 | 2,813 | 49.5 |
| Output redecoding and verification, including decode/MD5/hash | 1 | 359.0 |

These stages overlap and include instrumentation overhead; **do not sum them**.
The FLAC MD5 timer includes compact-PCM packing plus MD5 and describes the
output decoder, not a standalone measurement of the encoder's MD5.
Encoder MD5, CRC16 and Rice writing do not yet have independent stage timers.
The evidence points to encoder/model/MD5 work as useful places to investigate;
it does not justify a promised gain from rewriting a cryptographic primitive.

## Cover memory and compatibility

Since round seven `probe::cover` validates JPEG/PNG in this repository
without decoding pixels (see BENCHMARK-VERIFIED-PIPELINE.md); the text below
describes the earlier image-based decoder.

`probe::cover` decoded a complete image, retained it only to read width/height,
then dropped it. Its limits are 64 MiB encoded input (also bounded by file limit),
8192×8192 dimensions and a configured 128 MiB image allocation limit.
A 3000×3000 RGBA8 pixel array alone is about **34.3 MiB**; this is an arithmetic
example, not measured RSS, and actual decoded formats/buffers vary.

A lower-risk baseline is removing the general image wrapper while keeping
format-specific decoders temporarily: PNG supports line/row decoding, and JPEG
exposes decode buffer limits. That is **partial dependency reduction**, not fully
native JPEG/PNG. An audio-only feature/build can additionally omit the cover
stack while retaining existing default cover support.

Full native validation must check more than dimensions/signatures: PNG
IDAT/zlib/DEFLATE, filters, palette/bit-depth rules, CRC/Adler, scanline sizes and
interlaced passes; JPEG tables, entropy runs, restarts, scans/truncation and
supported progressive modes. Metadata-only probing must never claim
`fully_decoded:true`. Unsupported modes must be explicit; dropping existing
cover compatibility is a scope choice, not an invisible performance patch.
Incremental limits/deadlines also need enforcement inside the new decoder.

## Proposed sequence and acceptance

For the current WAV/ALAC → FLAC + QC priority:
**profile and optimize the existing FLAC encoder → specialized MD5 experiment →
SHA-256 minimal/hardware-preserving implementation → IEEE CRC32**.
Cover validation is a separate PNG-then-JPEG track; JSON is last.

For each replacement, keep reference comparison in development only, use
published hash/checksum vectors and incremental boundary tests, require exact
PCM/hash/frame counts, retain corruption/deadline checks, and measure default
uninstrumented binaries on native ARM/x86. Encoder storage-only changes should
also preserve compressed bytes/model choices. Cover paths need their own valid,
truncated, malformed and compatibility corpus plus peak-RSS measurements.
Actual Graviton4 and Zen3 runs remain separate from N2/9V74 evidence.

No runtime dependency, encoder model, QC schema or backend integration was
changed by this audit. Existing licenses and FFmpeg port attribution remain
applicable to future internalization/rewrites.

## Evidence and upstream documentation

* [Cargo.toml](../Cargo.toml), [Cargo.lock](../Cargo.lock), [THIRD_PARTY.md](../THIRD_PARTY.md).
* [Fresh rerun](BENCHMARK-THREEWAY-RERUN.md); [raw ARM](benchmark-threeway-rerun-arm.json).
* [Default dependency tree job, attempt 3](https://github.com/TAE-OK-11/audeniq-qc/actions/runs/37209634333/attempts/3), job 111467079517.
* [sha2 0.11.0](https://docs.rs/crate/sha2/0.11.0): runtime SHA2/SHA-NI and fallback.
* [md-5 0.10.6](https://docs.rs/crate/md-5/0.10.6): current MD5/digest dependency branch.
* [crc32fast 1.5.2](https://docs.rs/crate/crc32fast/1.5.2): IEEE hardware/fallback paths.
* [png 0.17.16](https://docs.rs/png/0.17.16/png/): line/frame decoding.
* [jpeg-decoder 0.3.2](https://docs.rs/jpeg-decoder/0.3.2/jpeg_decoder/struct.Decoder.html): buffer limits and full decode API.
