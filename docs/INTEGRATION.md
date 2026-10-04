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
| PCM comparison | `pcm-hash PATH` | Canonical interleaved left-aligned s32le SHA256 |
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

Version boundaries:

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
EPYC Zen3 and the actual requested Arm machine. Synthetic/oracle results in this
repository are evidence for the tested cases, not a universal performance or
accuracy guarantee.
