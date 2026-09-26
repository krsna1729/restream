#!/usr/bin/env python3
"""Summarize a scripts/harness/capacity-ramp.sh artifact root.

Writes <root>/summary.csv (one row per protocol and rung) and
<root>/summary.md (per-protocol tables plus the highest rung where every
repeat delivered to every destination at >= 0.95 of the offered rate).
"""

import csv
import glob
import json
import os
import statistics
import sys


def number(value, default=float("nan")):
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def main(root):
    rungs = {}
    for path in glob.glob(os.path.join(root, "*", "*", "rep*", "resource-sweep-results.csv")):
        protocol, outputs = path[len(root) + 1:].split(os.sep)[:2]
        for row in csv.DictReader(open(path)):
            rungs.setdefault((protocol, int(outputs)), []).append(row)

    header = [
        "protocol", "outputs", "repeats", "passed",
        "rx_delivered_min", "rx_ratio_min", "rx_ratio_median_med", "rx_interval_min",
        "rx_jain_min", "restream_delivered_min", "restream_ratio_min",
        "cpu_avg_median", "cpu_peak_max", "rss_peak_mb_max", "threads_peak_max",
    ]
    rows = []
    for (protocol, outputs), runs in sorted(rungs.items()):
        delivered = [int(number(r["delivery_delivered"], 0)) for r in runs]
        rows.append({
            "protocol": protocol,
            "outputs": outputs,
            "repeats": len(runs),
            "passed": sum(d >= outputs for d in delivered),
            "rx_delivered_min": min(delivered),
            "rx_ratio_min": min(number(r["delivery_ratio_min"]) for r in runs),
            "rx_ratio_median_med": statistics.median(number(r["delivery_ratio_median"]) for r in runs),
            "rx_interval_min": min(number(r["delivery_interval_ratio_min"]) for r in runs),
            "rx_jain_min": min(number(r["delivery_jain"]) for r in runs),
            "restream_delivered_min": min(int(number(r.get("restream_delivery_delivered"), 0)) for r in runs),
            "restream_ratio_min": min(number(r.get("restream_delivery_ratio_min")) for r in runs),
            "cpu_avg_median": statistics.median(number(r["restream_cpu_avg_pct"]) for r in runs),
            "cpu_peak_max": max(number(r["restream_cpu_peak_pct"]) for r in runs),
            "rss_peak_mb_max": max(number(r["rss_peak_kb"]) for r in runs) / 1024,
            "threads_peak_max": max(number(r["thread_count_peak"]) for r in runs),
        })

    with open(os.path.join(root, "summary.csv"), "w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=header)
        writer.writeheader()
        writer.writerows(rows)

    provenance = {}
    try:
        provenance = json.load(open(os.path.join(root, "provenance.json")))
    except OSError:
        pass
    lines = ["# Capacity ramp", ""]
    if provenance:
        lines += [
            f"- commit `{provenance.get('commit', '?')}`"
            + (" (dirty)" if provenance.get("dirty") else ""),
            f"- CPU: {provenance.get('cpu_model', '?')}, {provenance.get('online_cpus', '?')} online,"
            f" NUMA nodes {provenance.get('numa_nodes', '?')}, hypervisor {provenance.get('hypervisor', '?')}",
            f"- kernel {provenance.get('kernel', '?')}; {provenance.get('memory', '')}",
            f"- Restream CPUs `{provenance.get('restream_cpus')}`, harness/sink CPUs"
            f" `{provenance.get('harness_cpus')}`, egress shards {provenance.get('egress_shards')},"
            f" SRT sink threads {provenance.get('sink_threads')}",
            f"- one ingest at {provenance.get('bitrate')}, window {provenance.get('window_secs')} s,"
            f" {provenance.get('repeats')} repeats per rung; pass = every destination >= 0.95",
            "",
        ]
    for protocol in sorted({row["protocol"] for row in rows}):
        protocol_rows = [row for row in rows if row["protocol"] == protocol]
        capacity = max(
            (row["outputs"] for row in protocol_rows if row["passed"] == row["repeats"]),
            default=0,
        )
        lines += [
            f"## {protocol.upper()}: capacity {capacity} outputs (all repeats passed)",
            "",
            "| outputs | passed | rx delivered min | rx ratio min | rx interval min | rx Jain min"
            " | Restream delivered min | CPU avg (median) | CPU peak | RSS MB |",
            "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
        for row in protocol_rows:
            lines.append(
                f"| {row['outputs']} | {row['passed']}/{row['repeats']} | {row['rx_delivered_min']}"
                f" | {row['rx_ratio_min']:.3f} | {row['rx_interval_min']:.3f} | {row['rx_jain_min']:.5f}"
                f" | {row['restream_delivered_min']} | {row['cpu_avg_median']:.1f}%"
                f" | {row['cpu_peak_max']:.1f}% | {row['rss_peak_mb_max']:.0f} |"
            )
        lines.append("")
    with open(os.path.join(root, "summary.md"), "w") as handle:
        handle.write("\n".join(lines))
    print("\n".join(lines))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: capacity-ramp-summary.py <artifact root>")
    main(sys.argv[1])
