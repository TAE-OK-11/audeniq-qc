# AUDENIQ FFmpeg dependency audit

Audited before implementation: `TAE-OK-11/audeniq` main at
`01ecdb3de4719680bf6f026160852bcf50f83550` (2026-10-04).
No AGENTS.md was present. Audit includes production Rust, deployment and tests.

| Production caller | Required behavior | Replacement boundary |
|---|---|---|
| `crates/core/src/qc.rs:probe_with` | Local FFprobe JSON; container, codec, duration, rate, channels, effective bit depth; cover dimensions | Typed native probe, separate image probe |
| `qc.rs:decode_analysis_inner` | One whole-file decode; interleaved f32, EBU integrated LUFS/true peak; per-channel peaks, clipping runs >=3 at 0.999, 50ms silence/energy blocks, zero crossings | Single streaming Rust decode and fused meter |
| `qc.rs:decode_analysis_with_fp_tap` | Continuous mono 11025Hz s16; retain <=3x30s head/middle/tail windows via pipe (latest main already removed full-track temp-file retention) | Anti-aliased streaming resampler with bounded window retention; versioned compatibility |
| `fingerprint.rs:decode_window` | Input seek and duration bounds; same mono/s16/11025Hz resampling; fallback fingerprint path | Native window extraction; existing FFT/Philips hash belongs to AUDENIQ, not FFmpeg |
| `lossless.rs:to_flac` | WAV, ALAC in M4A, AIFF, integer non-hybrid WavPack, TTA and FLAC -> 16/24-bit FLAC; 1-2 channels; 44100-192000Hz; no resample/downconversion; metadata removal | Lossless-only native codecs and streaming FLAC writer |
| `lossless.rs:pcm_sha256` | SHA256 of interleaved signed **left-aligned** s32le; compare decoded source/output; reject shortened/malformed inputs | Canonical PCM hash + verified atomic conversion |
| `provenance.rs:inspect_audio` | FFprobe fallback for format tags (encoder/software/creator_tool/comment/description), bounded output and parser sandbox | Native local tag extraction; never infer rights from tags |
| `deploy/Dockerfile`, `deploy.sh`, Actions/tests | Runtime tools installed and smoke-tested; tests generate audio/images using FFmpeg | Keep FFmpeg only as development differential oracle until every production path qualifies |

## Scope exclusions

No video transcoding, playback, network protocols, streaming services, arbitrary
filter graph, lossy audio encoders, or general FFmpeg CLI compatibility.
Cover-image probing is in scope because it is an actual FFprobe use.
Fingerprint FFT, policy decisions, database, upload ACLs, and sandbox enforcement
remain the caller's responsibilities. Never equate an unsupported format with PASS.

## Invariants

* Preserve sample rate, channels, bit depth, integer sample values and decoded count.
* Strictly detect truncated containers, frame CRC failures, nonfinite PCM and invalid lengths.
* Distinguish invalid input, unsupported input, I/O, deadline and resource limits.
* No automatic production replacement or QC cache reuse until oracle tests pass.
* Resampling/metric changes require new algorithm/cache versions and an explicit migration.
* Performance must compare equivalent work, with CPU seconds, wall time, peak RSS,
  codec, sample rate, bit depth, duration, command, CPU and repetitions recorded.
* This development host is Intel Xeon Platinum 8573C, **not EPYC Zen3 or Arm**.
  Hardware-specific performance must be measured on those machines.

## FFmpeg references

Pinned upstream `FFmpeg/FFmpeg` at `12c589a37d093cc55618f8377b092dc21416806f`:
`libavformat/wavdec.c`, `aiffdec.c`, `mov.c`, `flacdec.c`, `ttadec.c`, `wvdec.c`;
`libavcodec/pcm.c`, `alac.c`, `flacdec.c`, `flacenc.c`, `tta.c`, `ttadata.c`,
`ttadsp.c`, `wavpack.c`, `wavpack.h`; `libavfilter/ebur128.c`, `f_ebur128.c`;
`libswresample/resample.c`, `resample_template.c`, `rematrix.c`.

Referenced/translated LGPL code retains attribution and LGPL-2.1-or-later.
Rust dependencies have their own licenses; see THIRD_PARTY.md as modules land.
Rewriting or optimizing a translation does not erase upstream license obligations.
