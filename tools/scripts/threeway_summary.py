#!/usr/bin/env python3
"""Print compact native / reference / FFmpeg / main medians from benchmark JSON.

Usage: threeway_summary.py SIGNAL FILE...  Each FILE is an audeniq-qc-tools
benchmark report. Rows: CPU s / wall s / peak RSS MiB (and FLAC bytes for
conversions). The full per-run data stays in the JSON reports.
"""
import json
import sys

signal = sys.argv[1]
for path in sys.argv[2:]:
    try:
        report = json.load(open(path))
    except FileNotFoundError:
        continue
    print(f"### {signal} :: {path} :: {report.get('host_cpu')} :: {report.get('seconds')}s x{report.get('repeats')}")
    for result in report["results"]:
        name = result.get("codec") or result.get("case") or "?"
        cells = []
        for who, row in sorted(result["median"].items()):
            cell = f"{who}={row['cpu_s']:.3f}/{row['wall_s']:.3f}/{row['peak_rss_kib'] / 1024:.1f}"
            size = (result.get("output_bytes") or {}).get(who)
            if size:
                cell += f"/{size}B"
            cells.append(cell)
        print(f"  {name:10s} " + "  ".join(cells))
    print("AUDENIQ_THREEWAY_JSON=" + json.dumps({"signal": signal, "file": path, "report": report}, separators=(",", ":")))
