#!/usr/bin/env python3
"""Analyze scripts/harness/srt-loss-latency-matrix.sh output and produce
per-backend spider/radar charts for docs/srt-pure-rust-plan.md Phase 4's
driver-framework bake-off (mio/tokio/smol/monoio/glommio/compio, libsrt as
reference baseline).

Reads the TSV's caller_stats/listener_stats "STATS ..." key=value lines
(format documented in test/native/srt-loss-listener.c and
crates/srt-interop/src/bin/loss_listener_*.rs's doc comments) and computes,
per backend, aggregate scores across three axes explicitly asked for:
throughput achieved (vs the requested bitrate), latency introduced (RTT
above the netem-injected floor), and CPU cost -- plus memory as a bonus
fourth axis and loss-recovery quality as a fifth. Each axis is normalized
0-1 with 1 = best observed, so the radar chart is directly readable ("closer
to the edge is better on every axis") without needing to remember which
raw units are good or bad.

Usage:
    python3 scripts/harness/srt-bakeoff-analyze.py <matrix.tsv> [output_dir]
"""

import sys
import csv
import re
from collections import defaultdict
from pathlib import Path

def parse_stats(s: str) -> dict:
    d = {}
    for kv in s.split():
        if "=" not in kv:
            continue
        k, v = kv.split("=", 1)
        try:
            d[k] = float(v) if re.match(r"^-?\d+\.\d+$", v) else int(v)
        except ValueError:
            d[k] = v
    return d


def load_rows(path: str):
    rows = []
    with open(path, newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for r in reader:
            if r["caller_rc"] != "0" or r["listener_rc"] != "0":
                continue  # skip failed cells -- connect timeouts etc, not a
                          # data point for any backend's steady-state cost
            c = parse_stats(r["caller_stats"])
            l = parse_stats(r["listener_stats"])
            if "pkt_sent" not in c or "pkt_recv" not in l:
                continue
            backend = c.get("backend", r["impl"])
            rows.append(
                dict(
                    backend=backend,
                    loss_pct=float(r["loss_pct"]),
                    delay_ms=int(r["delay_ms"]),
                    bitrate_bps=int(r["bitrate_bps"]),
                    duration_s=float(r["duration_s"]),
                    c=c,
                    l=l,
                )
            )
    return rows


def per_cell_metrics(row: dict) -> dict:
    c, l = row["c"], row["l"]
    duration = row["duration_s"]
    bitrate = row["bitrate_bps"]

    sent = c.get("pkt_sent_total", c.get("pkt_sent", 0))
    payload_bytes = 1316
    achieved_bps = sent * payload_bytes * 8 / duration if duration > 0 else 0
    throughput_ratio = min(achieved_bps / bitrate, 1.0) if bitrate > 0 else 0.0

    rtt_ms = l.get("rtt_ms", 0.0)
    expected_rtt_ms = row["delay_ms"] * 2
    # Latency *introduced* by the driver itself, not by netem: the excess
    # over the theoretical 2x one-way-delay floor. Floored at a small
    # constant to avoid divide-by-zero at 0ms delay.
    latency_overhead_ms = max(rtt_ms - expected_rtt_ms, 0.0)

    cpu_user_ms = c.get("cpu_user_ms", 0.0) + l.get("cpu_user_ms", 0.0)
    cpu_sys_ms = c.get("cpu_sys_ms", 0.0) + l.get("cpu_sys_ms", 0.0)
    cpu_total_ms = cpu_user_ms + cpu_sys_ms
    # CPU cost per packet actually sent -- comparable across cells with
    # different durations/bitrates, unlike raw cpu_total_ms.
    cpu_ms_per_1k_pkts = (cpu_total_ms / sent * 1000) if sent > 0 else None

    peak_rss_kb = max(c.get("peak_rss_kb", 0), l.get("peak_rss_kb", 0))

    # Loss-recovery quality: fraction of detected loss events NOT
    # reflected in the listener's final loss count once retransmission
    # has had a chance to work -- both backend families report a
    # cumulative loss-event counter (rcv_loss_total / equivalent), so a
    # LOWER value relative to packets sent is better recovery.
    rcv_loss = l.get("pkt_rcv_loss_total", 0)
    loss_rate = (rcv_loss / sent) if sent > 0 else 0.0

    return dict(
        throughput_ratio=throughput_ratio,
        latency_overhead_ms=latency_overhead_ms,
        cpu_ms_per_1k_pkts=cpu_ms_per_1k_pkts,
        peak_rss_kb=peak_rss_kb,
        loss_rate=loss_rate,
    )


def aggregate_by_backend(rows: list) -> dict:
    by_backend = defaultdict(list)
    for row in rows:
        by_backend[row["backend"]].append(per_cell_metrics(row))

    agg = {}
    for backend, cells in by_backend.items():
        n = len(cells)
        cpu_vals = [c["cpu_ms_per_1k_pkts"] for c in cells if c["cpu_ms_per_1k_pkts"] is not None]
        agg[backend] = dict(
            n_cells=n,
            throughput_ratio=sum(c["throughput_ratio"] for c in cells) / n,
            latency_overhead_ms=sum(c["latency_overhead_ms"] for c in cells) / n,
            cpu_ms_per_1k_pkts=(sum(cpu_vals) / len(cpu_vals)) if cpu_vals else None,
            peak_rss_kb=sum(c["peak_rss_kb"] for c in cells) / n,
            loss_rate=sum(c["loss_rate"] for c in cells) / n,
        )
    return agg


def normalize_scores(agg: dict) -> dict:
    """0-1 per axis, 1 = best observed among the backends present. Axes
    where a *lower* raw value is better (latency, CPU, memory, loss rate)
    are inverted so "further from center = better" holds for every axis."""
    axes_lower_better = ["latency_overhead_ms", "cpu_ms_per_1k_pkts", "peak_rss_kb", "loss_rate"]
    axes_higher_better = ["throughput_ratio"]
    all_axes = axes_higher_better + axes_lower_better

    scores = {b: {} for b in agg}
    for axis in all_axes:
        vals = {b: v[axis] for b, v in agg.items() if v.get(axis) is not None}
        if not vals:
            continue
        lo, hi = min(vals.values()), max(vals.values())
        spread = hi - lo if hi > lo else 1.0
        for b, v in vals.items():
            norm = (v - lo) / spread
            scores[b][axis] = norm if axis in axes_higher_better else 1.0 - norm
    return scores


AXIS_LABELS = {
    "throughput_ratio": "Throughput",
    "latency_overhead_ms": "Low latency\noverhead",
    "cpu_ms_per_1k_pkts": "CPU\nefficiency",
    "peak_rss_kb": "Memory\nefficiency",
    "loss_rate": "Loss\nrecovery",
}
AXIS_ORDER = ["throughput_ratio", "latency_overhead_ms", "cpu_ms_per_1k_pkts", "peak_rss_kb", "loss_rate"]


def draw_radar(scores: dict, agg: dict, out_path: Path):
    import numpy as np
    import matplotlib.pyplot as plt

    axes = [a for a in AXIS_ORDER if any(a in s for s in scores.values())]
    n = len(axes)
    angles = [i / n * 2 * np.pi for i in range(n)]
    angles += angles[:1]

    fig, ax = plt.subplots(figsize=(9, 9), subplot_kw=dict(polar=True))
    colors = plt.cm.tab10.colors

    for i, backend in enumerate(sorted(scores.keys())):
        vals = [scores[backend].get(a, 0.0) for a in axes]
        vals += vals[:1]
        ax.plot(angles, vals, linewidth=2, label=backend, color=colors[i % len(colors)])
        ax.fill(angles, vals, alpha=0.08, color=colors[i % len(colors)])

    ax.set_xticks(angles[:-1])
    ax.set_xticklabels([AXIS_LABELS.get(a, a) for a in axes], fontsize=11)
    ax.set_ylim(0, 1)
    ax.set_yticks([0.25, 0.5, 0.75, 1.0])
    ax.set_yticklabels(["0.25", "0.5", "0.75", "1.0"], fontsize=8)
    ax.set_title(
        "SRT driver-framework bake-off\n(further from center = better; each axis normalized to the best backend observed)",
        fontsize=12,
    )
    ax.legend(loc="upper right", bbox_to_anchor=(1.35, 1.1), fontsize=10)
    fig.tight_layout()
    fig.savefig(out_path, dpi=150, bbox_inches="tight")
    print(f"wrote {out_path}")


def print_table(agg: dict):
    print(f"\n{'backend':12} {'cells':>6} {'throughput':>11} {'lat_overhead_ms':>16} {'cpu_ms/1k_pkt':>14} {'peak_rss_kb':>12} {'loss_rate':>10}")
    for backend, v in sorted(agg.items()):
        cpu = f"{v['cpu_ms_per_1k_pkts']:.2f}" if v["cpu_ms_per_1k_pkts"] is not None else "n/a"
        print(
            f"{backend:12} {v['n_cells']:6d} {v['throughput_ratio']*100:10.1f}% "
            f"{v['latency_overhead_ms']:16.2f} {cpu:>14} {v['peak_rss_kb']:12.0f} {v['loss_rate']*100:9.2f}%"
        )


def main():
    if len(sys.argv) < 2:
        print(f"usage: {sys.argv[0]} <matrix.tsv> [output_dir]", file=sys.stderr)
        sys.exit(2)
    tsv_path = sys.argv[1]
    out_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("/tmp")
    out_dir.mkdir(parents=True, exist_ok=True)

    rows = load_rows(tsv_path)
    if not rows:
        print("no successful cells found in input TSV", file=sys.stderr)
        sys.exit(1)
    print(f"loaded {len(rows)} successful cells from {tsv_path}")

    agg = aggregate_by_backend(rows)
    print_table(agg)

    scores = normalize_scores(agg)
    draw_radar(scores, agg, out_dir / "srt-bakeoff-radar-all.png")

    # Rust-only variant (drop libsrt) -- the direct 6-way comparison the
    # user asked for, without libsrt's absolute-scale dominance on
    # throughput compressing the visual differences between the six.
    rust_only_scores = {b: s for b, s in scores.items() if b != "libsrt"}
    rust_only_agg = {b: a for b, a in agg.items() if b != "libsrt"}
    if rust_only_scores:
        rust_only_norm = normalize_scores(rust_only_agg)
        draw_radar(rust_only_norm, rust_only_agg, out_dir / "srt-bakeoff-radar-rust-only.png")


if __name__ == "__main__":
    main()
