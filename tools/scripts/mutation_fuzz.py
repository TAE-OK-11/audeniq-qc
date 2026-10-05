#!/usr/bin/env python3
"""Deterministic mutation fuzz of the release binary on real media files.

Each seed file is mutated (bit flips, byte runs, truncation, insertion,
deletion, header-region damage) and run through `pcm-hash`, `analyze` and
`convert` with a deadline. Allowed outcomes: success, or exit code 2 with a
JSON error on stderr. Signals (abort/segfault), other exit codes and timeouts
are failures. Every successful conversion must also re-decode in FFmpeg (with
CRC checking) to the PCM SHA-256 the engine reported.
"""
import argparse
import json
import os
import random
import subprocess
import sys
import tempfile


def mutate(data: bytes, rng: random.Random) -> bytes:
    b = bytearray(data)
    kind = rng.randrange(7)
    head = min(len(b), 4096)
    if kind == 0:  # scattered bit flips
        for _ in range(rng.randint(1, 16)):
            i = rng.randrange(len(b))
            b[i] ^= 1 << rng.randrange(8)
    elif kind == 1:  # header/metadata region damage
        for _ in range(rng.randint(1, 8)):
            i = rng.randrange(head)
            b[i] = rng.randrange(256)
    elif kind == 2:  # overwrite a run
        i = rng.randrange(len(b))
        n = rng.randint(1, 512)
        b[i:i + n] = bytes(rng.randrange(256) for _ in range(min(n, len(b) - i)))
    elif kind == 3:  # truncate
        del b[rng.randrange(1, len(b)):]
    elif kind == 4:  # insert bytes
        i = rng.randrange(len(b))
        b[i:i] = bytes(rng.randrange(256) for _ in range(rng.randint(1, 64)))
    elif kind == 5:  # delete bytes
        i = rng.randrange(len(b))
        del b[i:i + rng.randint(1, 64)]
    else:  # extreme field values in the header region
        i = rng.randrange(max(1, head - 4))
        b[i:i + 4] = rng.choice([b"\xff\xff\xff\xff", b"\x00\x00\x00\x00", b"\x7f\xff\xff\xff"])
    return bytes(b)


def run(cmd, timeout):
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return "timeout", b"", b""
    if p.returncode == 0:
        return "ok", p.stdout, p.stderr
    if p.returncode == 2:
        try:
            json.loads(p.stderr.decode().strip().splitlines()[-1])["error"]
            return "rejected", p.stdout, p.stderr
        except (ValueError, KeyError, IndexError):
            return "bad-error", p.stdout, p.stderr
    return f"rc={p.returncode}", p.stdout, p.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--ffmpeg", required=True)
    ap.add_argument("--cases", type=int, default=1600)
    ap.add_argument("--seed", type=int, default=1729)
    ap.add_argument("--timeout", type=int, default=60)
    ap.add_argument("--output", required=True)
    ap.add_argument("seeds", nargs="+")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    seeds = [(p, open(p, "rb").read()) for p in args.seeds]
    counts, failures = {}, []
    with tempfile.TemporaryDirectory() as tmp:
        for case in range(args.cases):
            name, data = seeds[case % len(seeds)]
            ext = os.path.splitext(name)[1]
            src = os.path.join(tmp, f"m{case}{ext}")
            with open(src, "wb") as f:
                f.write(mutate(data, rng))
            dst = os.path.join(tmp, f"m{case}.flac")
            for cmd in (["pcm-hash", src], ["analyze", src], ["convert", src, dst]):
                outcome, out, err = run([args.binary, *cmd, "--timeout-secs", str(args.timeout)],
                                        args.timeout + 30)
                counts[f"{cmd[0]}:{outcome}"] = counts.get(f"{cmd[0]}:{outcome}", 0) + 1
                if outcome not in ("ok", "rejected"):
                    keep = os.path.join(os.path.dirname(args.output), f"fuzz-fail-{case}{ext}")
                    os.replace(src, keep) if os.path.exists(src) else None
                    failures.append({"case": case, "seed_file": os.path.basename(name), "cmd": cmd[0],
                                     "outcome": outcome, "stderr": err[-300:].decode(errors="replace"),
                                     "kept": keep})
                    break
                if cmd[0] == "convert" and outcome == "ok":
                    reported = json.loads(out)["pcm_sha256"]
                    oracle = subprocess.run(
                        [args.ffmpeg, "-v", "error", "-xerror", "-err_detect", "crccheck+explode", "-i", dst,
                         "-map", "0:a:0", "-c:a", "pcm_s32le", "-f", "hash", "-hash", "sha256", "-"],
                        capture_output=True)
                    got = oracle.stdout.decode().strip().split("=", 1)[-1]
                    if oracle.returncode != 0 or got != reported:
                        failures.append({"case": case, "seed_file": os.path.basename(name),
                                         "cmd": "convert-crossdecode", "outcome": "mismatch"})
                    counts["convert:crossdecode-checked"] = counts.get("convert:crossdecode-checked", 0) + 1
                    os.unlink(dst)
            if os.path.exists(src):
                os.unlink(src)
            if (case + 1) % 200 == 0:
                print(f"{case + 1} cases, {len(failures)} failures", file=sys.stderr, flush=True)
    report = {"cases": args.cases, "seed": args.seed, "seed_files": [os.path.basename(p) for p, _ in seeds],
              "counts": counts, "failures": failures, "status": "passed" if not failures else "failed"}
    with open(args.output, "w") as f:
        json.dump(report, f, indent=1)
    print(json.dumps({k: v for k, v in report.items() if k != "failures"}), flush=True)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
