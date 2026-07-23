# Phase 0 egress baseline workloads

This directory records the deterministic workload manifest and captured
baseline artifacts required by the egress fabric migration plan
([egress-implementation](../../../../docs/egress-implementation.md), Phase 0).

## Contents

- [Manifest](#manifest)
- [Recorded artifacts](#recorded-artifacts)
- [Reproduction](#reproduction)

## Manifest

`manifest.json` defines five workload shapes:

| Id | Shape | Target scale |
|---|---|---|
| `w1-healthy-rtmp` | healthy RTMP fan-out | 1,000 |
| `w2-healthy-srt` | healthy SRT fan-out | 1,000 (legacy fails ≥512 by design) |
| `w3-mixed` | mixed RTMP + SRT fan-out | 1,200 |
| `w4-bad-neighbor` | healthy siblings beside a stalled output | 998 + 1 + 1 |
| `w5-reconnect-storm` | ≥25% of outputs lose their destination together | 25% of fleet |

The legacy SRT sender cap (512 concurrent sender threads,
`sender_semaphore` in `src/media/engine_registries.rs`) is recorded as a
known architectural failure at target scale, not a host limitation. Removing
it is a Phase 4 exit criterion.

## Recorded artifacts

Artifacts live in per-host-class subdirectories, e.g. `wsl-6cpu-12gb/`.
Each capture stores the harness `scale.csv` (columns: `config, step, label,
cpu_pct, rss_kb, ffmpeg_n, ffmpeg_rss_kb, total_rss_kb`), `summary.txt`
(per-config `rss_delta_kb` and `per_output_kb`), and a `capture.json` noting
the git commit, date, scale used, and deviations.

A host-class capture at reduced scale is a valid comparison baseline for that
host class only. Full-target-scale captures require a host provisioned for
1,000+ concurrent outputs and matching receivers; run the same manifest rows
there and store them under a new host-class directory.

## Reproduction

```sh
scripts/build/bench-harness.sh
RAMP_FAMILY_CONFIGS=srt-rtmp-src N_OUTPUTS=100 \
  WORK_DIR=.local/artifacts/egress-phase0/w1 \
  scripts/build/resource-limit.sh target/bench/test_harness ramp-family
```

Substitute the mode and env rows from `manifest.json` for the other shapes.
Copy `scale.csv` (committed as `steps.csv`) and `summary.txt` into the host-class directory and fill in
`capture.json`.
