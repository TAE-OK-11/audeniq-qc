#!/usr/bin/env python3
"""Workload-matched native / reference / FFmpeg verified FLAC pipeline benchmark.

Every engine performs the same end-to-end job on the same input:

  read source -> decode -> canonical PCM SHA-256 (left-aligned s32le)
  -> FLAC encode (compression level 5, STREAMINFO MD5)
  -> durable write (fsync) -> verify the published output decodes to the
  exact source PCM -> publish.

* native / reference: one `audeniq-qc convert` process (it fsyncs, verifies
  and publishes internally; reference re-decodes the whole output file).
* ffmpeg: process 1 decodes once and fans out to the FLAC encoder and the
  SHA-256 hash muxer; `sync FILE` fsyncs; process 2 re-decodes the output from
  disk with frame CRC checking (-err_detect crccheck+explode -xerror) and
  hashes it; the harness requires both hashes to be equal before publishing
  with link(). CPU and wall are summed over the processes, peak RSS is the
  largest child.

`--qc` adds QC to the same single source decode (native `--analyze`, FFmpeg
`ebur128=peak=true`). Pure-codec rows (`--pure`) are clearly separate: they
time FFmpeg encode-only and decode/hash-only, and native `pcm-hash`.

Every timed output is also hashed independently with FFmpeg outside timing,
and output FLAC sizes are recorded. Runs are interleaved across engines.
"""
import argparse
import hashlib
import json
import os
import statistics
import subprocess
import sys
import tempfile
import threading
import time


class Usage:
    """Accumulate CPU/RSS of children with os.wait4 (exact per child)."""

    def __init__(self):
        self.cpu = 0.0
        self.rss = 0

    def run(self, cmd, capture=True):
        # Exec inherits the spawning mm's RSS high-water mark into ru_maxrss,
        # so the harness's own RSS would leak into it. GNU time (a ~1 MiB
        # parent) reports the child's own peak RSS; CPU comes from wait4 at
        # microsecond resolution (time's own CPU is negligible).
        with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err, \
                tempfile.NamedTemporaryFile("r") as rss:
            wrapped = ["/usr/bin/time", "-f", "%M", "-o", rss.name, *cmd]
            actions = [(os.POSIX_SPAWN_DUP2, out.fileno(), 1), (os.POSIX_SPAWN_DUP2, err.fileno(), 2)]
            pid = os.posix_spawn(wrapped[0], wrapped, os.environ, file_actions=actions)
            _, status, ru = os.wait4(pid, 0)
            rc = os.waitstatus_to_exitcode(status)
            self.cpu += ru.ru_utime + ru.ru_stime
            self.rss = max(self.rss, int(rss.read().split()[-1]))
            out.seek(0)
            err.seek(0)
            if rc != 0:
                raise RuntimeError(f"{cmd[0]} failed rc={rc}: {err.read()[-400:]!r}")
            return out.read() if capture else b""


def hash_line(out):
    text = out.decode().strip()
    if not text.startswith("SHA256="):
        raise RuntimeError(f"unexpected hash output {text!r}")
    return text.split("=", 1)[1]


def ffmpeg_pipeline(ff, src, dst, qc):
    u = Usage()
    t0 = time.perf_counter()
    partial = dst + ".partial.flac"
    cmd = [ff, "-nostdin", "-v", "error", "-xerror", "-threads", "1", "-i", src,
           "-map", "0:a:0", "-map_metadata", "-1", "-c:a", "flac", "-threads", "1",
           "-compression_level", "5", partial,
           "-map", "0:a:0", "-c:a", "pcm_s32le", "-f", "hash", "-hash", "sha256", "-"]
    if qc:
        cmd += ["-map", "0:a:0", "-af", "ebur128=peak=true:framelog=quiet", "-f", "null", "-"]
    source_hash = hash_line(u.run(cmd))
    u.run(["sync", partial], capture=False)
    verify = [ff, "-nostdin", "-v", "error", "-xerror", "-err_detect", "crccheck+explode",
              "-threads", "1", "-i", partial, "-map", "0:a:0", "-c:a", "pcm_s32le",
              "-f", "hash", "-hash", "sha256", "-"]
    if hash_line(u.run(verify)) != source_hash:
        raise RuntimeError("FFmpeg verification mismatch")
    os.link(partial, dst)
    os.unlink(partial)
    wall = time.perf_counter() - t0
    return {"cpu": u.cpu, "wall": wall, "rss_kib": u.rss, "pcm_sha256": source_hash}


def native_pipeline(binary, src, dst, qc, level=True):
    u = Usage()
    t0 = time.perf_counter()
    # Level 5 is the production default for non-FLAC input. Without an
    # explicit level, FLAC input is copied as verified frames (not re-encoded).
    cmd = [binary, "convert", src, dst, *(["--compression-level", "5"] if level else [])]
    if qc:
        cmd.append("--analyze")
    report = json.loads(u.run(cmd))
    wall = time.perf_counter() - t0
    return {"cpu": u.cpu, "wall": wall, "rss_kib": u.rss, "pcm_sha256": report["pcm_sha256"]}


def pure_ffmpeg_encode(ff, src, dst):
    u = Usage()
    t0 = time.perf_counter()
    u.run([ff, "-nostdin", "-v", "error", "-threads", "1", "-i", src, "-map", "0:a:0",
           "-map_metadata", "-1", "-c:a", "flac", "-threads", "1", "-compression_level", "5", dst],
          capture=False)
    return {"cpu": u.cpu, "wall": time.perf_counter() - t0, "rss_kib": u.rss}


def pure_ffmpeg_decode(ff, src):
    u = Usage()
    t0 = time.perf_counter()
    h = hash_line(u.run([ff, "-nostdin", "-v", "error", "-threads", "1", "-i", src, "-map", "0:a:0",
                         "-c:a", "pcm_s32le", "-f", "hash", "-hash", "sha256", "-"]))
    return {"cpu": u.cpu, "wall": time.perf_counter() - t0, "rss_kib": u.rss, "pcm_sha256": h}


def pure_native_decode(binary, src):
    u = Usage()
    t0 = time.perf_counter()
    r = json.loads(u.run([binary, "pcm-hash", src]))
    return {"cpu": u.cpu, "wall": time.perf_counter() - t0, "rss_kib": u.rss, "pcm_sha256": r["pcm_sha256"]}


def oracle_hash(ff, path):
    out = subprocess.run([ff, "-nostdin", "-v", "error", "-xerror", "-err_detect", "crccheck+explode",
                          "-i", path, "-map", "0:a:0", "-c:a", "pcm_s32le", "-f", "hash", "-hash",
                          "sha256", "-"], check=True, capture_output=True).stdout
    return hash_line(out)


def probe(ff, path):
    ffprobe = os.path.join(os.path.dirname(ff), "ffprobe")
    out = subprocess.run([ffprobe, "-v", "error", "-select_streams", "a:0", "-show_entries",
                          "stream=codec_name,sample_rate,channels,bits_per_raw_sample,bits_per_sample:format=duration",
                          "-of", "json", path], check=True, capture_output=True).stdout
    d = json.loads(out)
    s = d["streams"][0]
    bits = int(s.get("bits_per_raw_sample") or 0) or int(s.get("bits_per_sample") or 0)
    return {"codec": s["codec_name"], "sample_rate": int(s["sample_rate"]), "channels": int(s["channels"]),
            "bits": bits, "duration": float(d["format"]["duration"])}


def med(values):
    return statistics.median(values) if values else None


def summarize(runs):
    return {k: med([r[k] for r in runs]) for k in ("cpu", "wall", "rss_kib")}


def bench_file(args, src, engines, workdir):
    info = probe(args.ffmpeg, src)
    pcm_bytes = int(round(info["duration"] * info["sample_rate"])) * info["channels"] * (info["bits"] // 8)
    expected = oracle_hash(args.ffmpeg, src)
    runs = {name: [] for name in engines}
    sizes = {}
    names = list(engines)
    for rep in range(args.repeats + 1):  # first round is warm-up
        for i in range(len(names)):
            name = names[(i + rep) % len(names)]
            dst = os.path.join(workdir, f"{name}.flac")
            if os.path.exists(dst):
                os.unlink(dst)
            r = engines[name](src, dst)
            if r["pcm_sha256"] != expected:
                raise RuntimeError(f"{name} PCM hash mismatch on {src}")
            if rep == 0:
                # Independent cross-decode of the published output, untimed.
                if oracle_hash(args.ffmpeg, dst) != expected:
                    raise RuntimeError(f"{name} output failed FFmpeg cross-decode on {src}")
                sizes[name] = os.path.getsize(dst)
            else:
                runs[name].append(r)
            os.unlink(dst)
    out = {"input": os.path.basename(src), **info, "pcm_bytes": pcm_bytes, "pcm_sha256": expected,
           "flac_bytes": sizes, "flac_ratio": {k: v / pcm_bytes for k, v in sizes.items()},
           "median": {k: summarize(v) for k, v in runs.items()}, "runs": runs}
    for k, m in out["median"].items():
        m["cpu_per_audio_s"] = m["cpu"] / info["duration"]
    return out


def bench_pure(args, src, workdir):
    info = probe(args.ffmpeg, src)
    expected = oracle_hash(args.ffmpeg, src)
    rows = {"ffmpeg_encode_only": [], "ffmpeg_decode_hash": [], "native_decode_hash": []}
    for rep in range(args.repeats + 1):
        dst = os.path.join(workdir, "pure.flac")
        e = pure_ffmpeg_encode(args.ffmpeg, src, dst)
        os.unlink(dst)
        d = pure_ffmpeg_decode(args.ffmpeg, src)
        n = pure_native_decode(args.native, src)
        if d["pcm_sha256"] != expected or n["pcm_sha256"] != expected:
            raise RuntimeError("pure decode hash mismatch")
        if rep:
            rows["ffmpeg_encode_only"].append(e)
            rows["ffmpeg_decode_hash"].append(d)
            rows["native_decode_hash"].append(n)
    return {"input": os.path.basename(src), **info, "label": "PURE CODEC (not end-to-end)",
            "median": {k: summarize(v) for k, v in rows.items()}}


def concurrency(args, engine, inputs, workers, workdir):
    """Run every input once per worker-pool configuration; collect throughput,
    latency distribution and sampled total RSS of live children."""
    import concurrent.futures as cf
    jobs = [inputs[i % len(inputs)] for i in range(args.jobs)]
    stop = threading.Event()
    peak = [0]

    def sampler():
        # Sum VmRSS of every descendant: engines run under `time` (and FFmpeg
        # pipelines under the harness thread), so direct children are not enough.
        me = os.getpid()
        while not stop.is_set():
            parent, rss = {}, {}
            for pid in os.listdir("/proc"):
                if not pid.isdigit():
                    continue
                try:
                    with open(f"/proc/{pid}/stat") as f:
                        parent[int(pid)] = int(f.read().rsplit(")", 1)[1].split()[1])
                    with open(f"/proc/{pid}/status") as f:
                        status = f.read()
                    # The GNU time wrapper is measurement overhead, not engine.
                    if status.startswith("Name:\ttime\n"):
                        continue
                    for line in status.splitlines():
                        if line.startswith("VmRSS:"):
                            rss[int(pid)] = int(line.split()[1])
                except (OSError, IndexError, ValueError):
                    pass
            total = 0
            for pid, kib in rss.items():
                p = parent.get(pid)
                while p and p != me and p != 1:
                    p = parent.get(p)
                if p == me:
                    total += kib
            peak[0] = max(peak[0], total)
            time.sleep(0.005)

    t = threading.Thread(target=sampler, daemon=True)
    t.start()
    results = []
    counter = [0]
    lock = threading.Lock()

    def one(src):
        with lock:
            counter[0] += 1
            n = counter[0]
        dst = os.path.join(workdir, f"c{n}.flac")
        r = engine(src, dst)
        os.unlink(dst)
        return r

    t0 = time.perf_counter()
    failures = 0
    with cf.ThreadPoolExecutor(max_workers=workers) as ex:
        for fut in [ex.submit(one, s) for s in jobs]:
            try:
                results.append(fut.result())
            except Exception:  # noqa: BLE001 - counted and reported
                failures += 1
    elapsed = time.perf_counter() - t0
    stop.set()
    t.join()
    lat = sorted(r["wall"] for r in results)
    audio = sum(probe_cache[s]["duration"] for s in jobs)
    return {"workers": workers, "jobs": len(jobs), "failures": failures, "elapsed_s": elapsed,
            "audio_s_per_wall_s": audio / elapsed, "cpu_total_s": sum(r["cpu"] for r in results),
            "cpu_per_job_s": statistics.mean(r["cpu"] for r in results),
            "p50_wall_s": lat[len(lat) // 2], "p95_wall_s": lat[min(len(lat) - 1, int(0.95 * len(lat)))],
            "peak_total_rss_kib": peak[0], "max_child_rss_kib": max(r["rss_kib"] for r in results)}


probe_cache = {}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--native", required=True)
    p.add_argument("--baseline", help="second native build for A/B")
    p.add_argument("--reference", help="Symphonia reference build")
    p.add_argument("--ffmpeg", required=True)
    p.add_argument("--repeats", type=int, default=5)
    p.add_argument("--qc", action="store_true")
    p.add_argument("--no-ffmpeg", action="store_true")
    p.add_argument("--pure", action="store_true")
    p.add_argument("--concurrency", default="")
    p.add_argument("--jobs", type=int, default=16)
    p.add_argument("--concurrency-engines", default="native,ffmpeg")
    p.add_argument("--frame-copy", action="store_true",
                   help="omit the explicit level for native builds (FLAC input is frame-copied; not FFmpeg-equivalent)")
    p.add_argument("--output", required=True)
    p.add_argument("inputs", nargs="+")
    args = p.parse_args()
    engines = {"native": lambda s, d: native_pipeline(args.native, s, d, args.qc, not args.frame_copy)}
    if args.baseline:
        engines["baseline"] = lambda s, d: native_pipeline(args.baseline, s, d, args.qc, not args.frame_copy)
    if args.reference:
        engines["reference"] = lambda s, d: native_pipeline(args.reference, s, d, args.qc, not args.frame_copy)
    if not args.no_ffmpeg:
        engines["ffmpeg"] = lambda s, d: ffmpeg_pipeline(args.ffmpeg, s, d, args.qc)
    ffv = subprocess.run([args.ffmpeg, "-version"], capture_output=True, text=True).stdout.splitlines()[0]
    report = {"ffmpeg_version": ffv, "qc": args.qc, "repeats": args.repeats,
              "binaries": {k: hashlib.sha256(open(v, "rb").read()).hexdigest()
                           for k, v in (("native", args.native), ("baseline", args.baseline),
                                        ("reference", args.reference)) if v},
              "cpu": open("/proc/cpuinfo").read().split("model name")[1].split("\n")[0].strip(": "),
              "nproc": os.cpu_count(), "results": [], "pure": [], "concurrency": {}}
    with tempfile.TemporaryDirectory(dir=os.environ.get("BENCH_TMP")) as workdir:
        for src in args.inputs:
            probe_cache[src] = probe(args.ffmpeg, src)
            if args.repeats > 0:
                r = bench_file(args, src, engines, workdir)
                report["results"].append(r)
                line = "  ".join(f"{k}: {v['cpu']:.3f}c {v['wall']:.3f}w {v['rss_kib']/1024:.1f}M"
                                 for k, v in r["median"].items())
                sizes = " ".join(f"{k}={v}" for k, v in r["flac_bytes"].items())
                print(f"{r['input']:<18} {line}  | {sizes}", file=sys.stderr, flush=True)
            if args.pure:
                report["pure"].append(bench_pure(args, src, workdir))
        for name in [n for n in args.concurrency_engines.split(",") if n in engines]:
            for w in [int(x) for x in args.concurrency.split(",") if x]:
                c = concurrency(args, engines[name], args.inputs, w, workdir)
                report["concurrency"].setdefault(name, []).append(c)
                print(f"concurrency {name} w={w}: {json.dumps(c)}", file=sys.stderr, flush=True)
    with open(args.output, "w") as f:
        json.dump(report, f, indent=1)


if __name__ == "__main__":
    main()
