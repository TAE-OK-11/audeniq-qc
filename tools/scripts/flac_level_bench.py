#!/usr/bin/env python3
"""FLAC compression-level comparison on a WAV corpus (development only).

Usage: flac_level_bench.py CORPUS_DIR OUT.json NAME=BINARY... [--levels 0,5]
       [--repeats 3] [--ffmpeg]

For every level, each engine converts every WAV in CORPUS_DIR. Runs are
interleaved per file (engine order alternates by repeat). Reported per level
and engine: total output bytes, CPU seconds (user+system, sum over files of
the per-file minimum over repeats) and the largest peak RSS of one process,
all from GNU time. Every output is decoded by FFmpeg with CRC checking and
must reproduce the source PCM (MD5 of the decoded samples).
"""
import glob
import json
import os
import platform
import subprocess
import sys
import tempfile

TIME = os.environ.get("AUDENIQ_GNU_TIME", "/usr/bin/time")


def run(cmd):
    with tempfile.NamedTemporaryFile("r", suffix=".time") as t:
        r = subprocess.run([TIME, "-f", "%U %S %M", "-o", t.name] + cmd,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        if r.returncode != 0:
            sys.exit(f"failed: {' '.join(cmd)}\n{r.stderr.decode()[-2000:]}")
        user, system, rss = t.read().split()[-3:]
    return float(user) + float(system), int(rss)


def md5(path):
    return subprocess.run(["ffmpeg", "-v", "error", "-err_detect", "crccheck+explode", "-xerror",
                           "-i", path, "-f", "md5", "-"], check=True, capture_output=True,
                          text=True).stdout.strip()


def main():
    args = sys.argv[1:]
    corpus, out = args[0], args[1]
    levels = [0, 1, 2, 3, 4, 5, 6, 7, 8]
    repeats = 3
    use_ffmpeg = "--ffmpeg" in args
    engines = []
    i = 2
    while i < len(args):
        if args[i] == "--levels":
            levels = [int(x) for x in args[i + 1].split(",")]
            i += 2
        elif args[i] == "--repeats":
            repeats = int(args[i + 1])
            i += 2
        elif args[i] == "--ffmpeg":
            i += 1
        else:
            name, binary = args[i].split("=", 1)
            engines.append((name, binary))
            i += 1
    if use_ffmpeg:
        engines.append(("ffmpeg", None))
    files = sorted(glob.glob(os.path.join(corpus, "*.wav")))
    sources = {f: md5(f) for f in files}
    work = tempfile.mkdtemp()
    result = {"arch": platform.machine(), "files": len(files), "repeats": repeats, "levels": {}}
    for level in levels:
        totals = {name: {"bytes": 0, "cpu_s": 0.0, "peak_rss_kib": 0} for name, _ in engines}
        for f in files:
            best = {name: None for name, _ in engines}
            for r in range(repeats):
                order = engines if r % 2 == 0 else engines[::-1]
                for name, binary in order:
                    o = os.path.join(work, f"{name}.flac")
                    if os.path.exists(o):
                        os.remove(o)
                    if binary is None:
                        cmd = ["ffmpeg", "-nostdin", "-v", "error", "-threads", "1", "-i", f,
                               "-map_metadata", "-1", "-c:a", "flac",
                               "-compression_level", str(level), o]
                    else:
                        cmd = [binary, "convert", f, o, "--compression-level", str(level)]
                    cpu, rss = run(cmd)
                    t = totals[name]
                    t["peak_rss_kib"] = max(t["peak_rss_kib"], rss)
                    best[name] = cpu if best[name] is None else min(best[name], cpu)
                    if r == 0:
                        t["bytes"] += os.path.getsize(o)
                        if md5(o) != sources[f]:
                            sys.exit(f"{name} level {level} {f}: decoded PCM differs")
            for name in best:
                totals[name]["cpu_s"] += best[name]
        for t in totals.values():
            t["cpu_s"] = round(t["cpu_s"], 3)
        result["levels"][str(level)] = totals
        print(level, json.dumps(totals), flush=True)
    with open(out, "w") as fh:
        json.dump(result, fh, indent=1)
    names = [n for n, _ in engines]
    print("\n| Level | " + " | ".join(f"{n} bytes / CPU s / RSS KiB" for n in names) + " |")
    print("|---|" + "---:|" * len(names))
    for level, totals in result["levels"].items():
        cells = [f"{totals[n]['bytes']:,} / {totals[n]['cpu_s']} / {totals[n]['peak_rss_kib']}"
                 for n in names]
        print(f"| {level} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main()
