# AUDENIQ integration contract

The audited AUDENIQ source is pinned in AUDIT.md. Integration must run this binary
as a child through its existing `parser_sandbox::restrict`, preserving Landlock,
seccomp, environment clearing, output limits, timeout/kill/reap and RLIMIT policy.
Never load an uploaded media parser directly into the API/worker process.

| Existing call | Native command | Caller behavior |
|---|---|---|
| qc::probe / image probe | `probe PATH` | Same consumed `streams`/`format` fields; metadata alone is not a decode PASS |
| qc::decode_analysis | `analyze PATH [--fingerprint]` | Map measured fields; validate returned spec/count; convert i16 windows to f32 /32768 |
| fingerprint fallback | `fingerprint PATH` | Retain continuous-filter head/mid/tail windows; existing AUDENIQ Philips FFT stays in backend |
| lossless normalization | `convert INPUT OUTPUT.flac` | Use a private writable staging directory; check declared expansion/file limit; publish only verified result |
| normalization + review QC | `convert INPUT OUTPUT.flac --analyze [--fingerprint]` | Read nested analysis after verified output; source metadata is analysis.spec, output metadata is top-level spec; do not run another PCM QC scan |
| PCM comparison | `pcm-hash PATH` | Canonical interleaved left-aligned s32le SHA256 |
| PCM export | `decode INPUT OUTPUT --format wav\|s32le` | Source/output specs are separate; WAV preserves effective depth, raw s32le stores it left-aligned in 32 bits |
| provenance fallback | `tags PATH` | Map only needed tags; unsupported/missing tags do not establish rights or AI absence |

Default library limits: 4GiB file, 1,382,400,000 decoded frames, 16MiB packet,
600s deadline. AUDENIQ has stricter upload policy; enforce those caller limits
before launch and retain the child's RLIMIT limits. A deadline checked by a Rust
parser cannot interrupt an arbitrary dependency decode in progress, so the parent
must also enforce a wall deadline and kill/reap.

The converter creates a temporary sibling and verifies it before a no-clobber
hard link. Its writable grant must cover only a new **private staging directory**,
not an upload parent. Move/rename the finished FLAC from that directory in the
parent. Do not pass an existing pre-created empty output file: no-clobber rejects it.

PCM export has the same staging/no-clobber contract. Explicit
`convert --compression-level 0..8` uses this engine's presets and forces FLAC
re-encoding. Without that option, FLAC frames are preserved after full validation;
other inputs use profile 5. These levels are not FFmpeg/libFLAC-equivalent numbers.

Version boundaries:

Fused conversion computes QC from source PCM and retains independent output
decode/hash/spec/count verification. It does not weaken validation or normalize
loudness. The parent must deserialize the bounded report and enforce its own
rule/cache versions and existing parser sandbox. Backend wiring is still separate.

* Metrics `native-bs1770-v1` require a new AUDENIQ QC rule/cache version.
* Resampler `native-sinc64-v1` requires fingerprint algorithm version 3, separate
  from existing version 2. Queries must compare only equal versions.
* Rebuild external reference fingerprints in the new version before expecting
  comparisons; retain original masters and version 2 records for rollback.
* This engine never modifies PostgreSQL or assigns policy PASS/FAIL.

Tags are a bounded purpose-specific subset, not a full FFprobe report. JPEG/PNG
covers are fully decoded to validate them. M4A must have one ALAC audio track;
embedded cover metadata is allowed, additional tracks are rejected. Unknown
duration cannot feed segmented fingerprints. Unsupported inputs must fail closed.

Production qualification still needs the complete official EBU corpus, real
accepted/rejected upload corpus, hostile-parser fuzzing, sandbox smoke tests,
deployment-host concurrency and the actual requested Arm machine. An earlier CI
run verified AVX2 on AMD EPYC 7763 (Zen3); the latest Arm-priority engine was
validated again on AMD EPYC 7763 and a Neoverse N2 NEON host. See GRAVITON4.md
and VALIDATION.md for the exact source commits
and CPU identities. Those runs do not substitute for qualification on the user's
deployment hosts. Synthetic/oracle results in this
repository are evidence for the tested cases, not a universal performance or
accuracy guarantee.
