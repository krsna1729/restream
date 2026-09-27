#!/usr/bin/env python3
"""Measure host scheduling jitter on one CPU: spin on a monotonic clock and
count gaps between consecutive reads longer than a threshold.

On a virtual machine, gaps with no competing work on the CPU come from the
hypervisor descheduling the vCPU; the guest often reports zero steal time for
them. Every Restream thread on that CPU loses the same wall time, so capacity
and tail-latency numbers must be read against this. Usage:

    taskset -c <cpu> scripts/harness/host-jitter.py [seconds] [threshold_ms]

Prints one JSON object.
"""
import json
import sys
import time


def main() -> None:
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 20.0
    threshold = (float(sys.argv[2]) if len(sys.argv) > 2 else 5.0) / 1000.0
    clock = time.perf_counter
    start = prev = clock()
    gaps = 0
    worst = 0.0
    lost = 0.0
    while True:
        now = clock()
        gap = now - prev
        if gap > threshold:
            gaps += 1
            lost += gap
            worst = max(worst, gap)
        prev = now
        if now - start >= seconds:
            break
    print(json.dumps({
        "seconds": seconds,
        "threshold_ms": threshold * 1000,
        "gaps": gaps,
        "worst_ms": round(worst * 1000, 1),
        "lost_ms": round(lost * 1000, 1),
        "lost_pct": round(100 * lost / seconds, 2),
    }))


if __name__ == "__main__":
    main()
