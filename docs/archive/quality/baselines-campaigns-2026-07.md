# Performance baselines — dated campaigns (archived)

> Archived from `docs/agent-guidance/quality/baselines.md`. Live Criterion and resource ledger tables remain there.

## Contents

- [Campaign notes](#campaign-notes)

## Campaign notes

### Historical reference — 2026-06-27 memory-optimization pass

After ring/AVIO/TS sizing cuts (−205 MB RSS total across 15 scale cases,
−175 MB ring payload, zero ring overflows):

| Config | RSS after | Ring payload after |
|---|---|---|
| h265-srt 4M | 205 MB | 47 MB |
| h265-srt-multi 8M | 237 MB | 77 MB |
| h264-srt-multi 8M | 137 MB | 71 MB |
| h264-rtmp 8M | 116 MB | 35 MB |

### Resource-sweep baseline — 2026-07-18

First `resource-sweep` harness run recorded in this ledger (Q-006). Idle host,
`scripts/build/bench-harness.sh` then
`scripts/build/resource-limit.sh target/bench/test_harness resource-sweep`,
serial, isolated lifecycle. Values are per-scenario peaks/averages across
`sampleCount` samples; "Blocked writes" is not a field this harness mode
measures (AVIO/ring blocked-write counters are covered separately by the
`avio_throughput`/`ring_buffer` benches, seeded under Q-003).

| Scenario | Label | Ingests | Outputs | RSS peak KB | RSS avg KB | Source ring peak KB | Transcoder ring peak KB | AVIO HWM peak KB | Total CPU avg % | Total CPU peak % |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| baseline-empty | empty | 0 | 0 | 75,960 | 75,764.67 | 0 | 0 | 0 | 0.83 | 2.00 |
| ingest-only | h264-rtmp | 1 | 0 | 78,892 | 76,979.33 | 2,470 | 0 | 0 | 1.16 | 1.99 |
| ingest-only | h264-srt | 1 | 0 | 81,280 | 80,633.33 | 2,407 | 0 | 0 | 1.99 | 1.99 |
| ingest-only | h265-srt | 1 | 0 | 80,752 | 80,221.33 | 2,190 | 0 | 0 | 1.99 | 2.98 |
| ingest-only | mixed.live.srt.h264.a2.bf2 | 1 | 0 | 81,216 | 80,531.33 | 2,499 | 0 | 0 | 1.82 | 2.00 |
| ingest-only | mixed.live.srt.h265.a2.bf2 | 1 | 0 | 80,916 | 80,304.67 | 2,277 | 0 | 0 | 1.82 | 2.00 |
| ingest-growth-same | 1-pipelines | 1 | 0 | 81,280 | 80,708.67 | 2,407 | 0 | 0 | 1.66 | 1.99 |
| ingest-growth-same | 3-pipelines | 3 | 0 | 91,316 | 89,784.67 | 9,894 | 0 | 0 | 3.47 | 3.97 |
| ingest-growth-same | 5-pipelines | 5 | 0 | 106,380 | 103,914.67 | 21,296 | 0 | 0 | 5.29 | 5.94 |
| ingest-growth-mixed | 1-pipelines | 1 | 0 | 78,888 | 78,112.67 | 2,470 | 0 | 0 | 1.32 | 1.99 |
| ingest-growth-mixed | 3-pipelines | 3 | 0 | 86,048 | 85,025.33 | 7,467 | 0 | 0 | 3.14 | 3.99 |
| ingest-growth-mixed | 5-pipelines | 5 | 0 | 97,416 | 95,434.00 | 17,466 | 0 | 0 | 4.30 | 4.96 |
| egress-growth-source-same | 1-per-group | 1 | 1 | 81,992 | 81,399.33 | 2,520 | 0 | 0 | 1.99 | 2.99 |
| egress-growth-source-same | 5-per-group | 1 | 5 | 87,100 | 85,862.67 | 4,757 | 0 | 0 | 2.81 | 2.97 |
| egress-growth-source-same | 10-per-group | 1 | 10 | 90,488 | 89,846.67 | 6,386 | 0 | 0 | 3.14 | 3.96 |
| egress-growth-source-srt | 1-per-group | 1 | 1 | 85,904 | 85,064.00 | 2,520 | 0 | 481 | 2.81 | 3.98 |
| egress-growth-source-srt | 5-per-group | 1 | 5 | 94,732 | 93,877.33 | 4,757 | 0 | 951 | 4.96 | 5.95 |
| egress-growth-source-srt | 10-per-group | 1 | 10 | 104,948 | 104,145.33 | 6,444 | 0 | 2,125 | 6.60 | 6.93 |
| egress-growth-source-mixed | 1-per-group | 1 | 2 | 87,048 | 86,114.00 | 2,520 | 0 | 481 | 3.31 | 3.97 |
| egress-growth-source-mixed | 5-per-group | 1 | 10 | 97,592 | 96,378.67 | 4,757 | 0 | 859 | 5.61 | 5.94 |
| egress-growth-source-mixed | 10-per-group | 1 | 20 | 111,124 | 109,938.67 | 6,444 | 0 | 2,125 | 7.25 | 8.89 |
| egress-growth-transcode-same | 1-per-group | 1 | 1 | 104,832 | 102,729.33 | 3,183 | 13,980 | 0 | 89.35 | 111.07 |
| egress-growth-transcode-same | 5-per-group | 1 | 5 | 117,476 | 115,421.33 | 5,421 | 14,083 | 0 | 95.18 | 131.56 |
| egress-growth-transcode-same | 10-per-group | 1 | 10 | 121,844 | 121,712.67 | 6,444 | 14,343 | 0 | 97.45 | 132.63 |
| egress-growth-transcode-srt | 1-per-group | 1 | 1 | 121,568 | 117,742.00 | 3,183 | 13,980 | 501 | 89.39 | 112.03 |
| egress-growth-transcode-srt | 5-per-group | 1 | 5 | 165,252 | 163,227.33 | 5,605 | 14,083 | 6,098 | 95.53 | 121.64 |
| egress-growth-transcode-srt | 10-per-group | 1 | 10 | 196,476 | 194,098.67 | 6,401 | 14,343 | 12,260 | 121.07 | 143.28 |
| egress-growth-transcode-mixed | 1-per-group | 1 | 2 | 121,416 | 118,409.33 | 3,183 | 13,980 | 429 | 89.51 | 110.04 |
| egress-growth-transcode-mixed | 5-per-group | 1 | 10 | 159,956 | 158,027.33 | 5,421 | 14,083 | 6,015 | 103.57 | 132.54 |
| egress-growth-transcode-mixed | 10-per-group | 1 | 20 | 206,220 | 205,658.00 | 6,401 | 14,343 | 13,260 | 126.06 | 165.60 |
| egress-growth-source-plus-transcode-mixed | 1-per-group | 1 | 4 | 131,776 | 128,274.00 | 3,183 | 13,980 | 1,096 | 93.23 | 113.76 |
| egress-growth-source-plus-transcode-mixed | 5-per-group | 1 | 20 | 167,592 | 166,189.33 | 5,605 | 14,083 | 6,577 | 105.76 | 137.91 |
| egress-growth-source-plus-transcode-mixed | 10-per-group | 1 | 40 | 207,684 | 205,679.33 | 6,401 | 14,343 | 14,966 | 128.21 | 152.87 |
| egress-growth-transcode-dual-mixed | 1-per-group | 1 | 4 | 213,868 | 201,726.67 | 3,183 | 43,099 | 1,027 | 229.15 | 303.62 |
| egress-growth-transcode-dual-mixed | 5-per-group | 1 | 20 | 509,416 | 491,116.67 | 5,555 | 43,627 | 16,712 | 295.53 | 363.26 |
| egress-growth-transcode-dual-mixed | 10-per-group | 1 | 40 | 700,156 | 684,176.67 | 6,390 | 43,965 | 35,728 | 347.47 | 444.26 |
| egress-growth-source-plus-transcode-dual-mixed | 1-per-group | 1 | 6 | 211,236 | 204,526.00 | 3,183 | 43,099 | 1,150 | 237.18 | 300.81 |
| egress-growth-source-plus-transcode-dual-mixed | 5-per-group | 1 | 30 | 521,144 | 505,920.67 | 6,103 | 43,627 | 16,472 | 301.78 | 358.17 |
| egress-growth-source-plus-transcode-dual-mixed | 10-per-group | 1 | 60 | 766,752 | 743,404.00 | 6,394 | 43,308 | 32,087 | 344.22 | 412.62 |
| egress-growth-hevc-bridge | 1-per-group | 1 | 1 | 110,816 | 109,116.67 | 2,723 | 21,816 | 0 | 143.96 | 180.10 |
| egress-growth-hevc-bridge | 5-per-group | 1 | 5 | 132,020 | 129,086.67 | 4,972 | 21,747 | 0 | 150.49 | 186.52 |
| egress-growth-hevc-bridge | 10-per-group | 1 | 10 | 136,100 | 135,663.33 | 6,296 | 21,874 | 0 | 154.50 | 202.32 |

No negative results (ring overflows, AVIO stalls beyond expected transcode
back-pressure, or unbounded growth) surfaced in this pass — RSS scales
roughly linearly with output count within each scenario family, and the two
highest-RSS scenarios (`egress-growth-transcode-dual-mixed`,
`egress-growth-source-plus-transcode-dual-mixed`) are the dual-transcode
cases, consistent with running two independent transcoder stages per group.
Commit: 39685ea3. Artifacts:

- `.local/artifacts/resource-sweep/resource-sweep-results.json`
- `.local/artifacts/resource-sweep/resource-sweep-results.csv`
- `.local/artifacts/resource-sweep/resource-sweep-samples.jsonl`
- `.local/artifacts/resource-sweep/mediamtx.log`
- `.local/artifacts/resource-sweep/restream.log`
- `.local/artifacts/latest/resource-sweep.json`

### Internal video-preset rollout RSS baseline — 2026-07-10

Command:

```sh
RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
ONLY_CHECKS=load,ffprobe,decode-scan \
scripts/harness/rollouts/internal-video-presets.sh
```

RSS guard baseline: `test/harness/baselines/internal-video-presets-rss.csv`.
Regression threshold: `20%` per-output RSS for this rollout guard, allowing
host/process jitter while catching large memory regressions.

| Scenario | Outputs | Restream RSS delta | Per-output RSS | External FFmpeg RSS | Commit |
|---|---:|---:|---:|---:|---|
| `mixed.live.srt.h264.a1.bf0` | 12 | 113,388 KB | 9,449 KB | 0 KB | this commit |
| `mixed.live.srt.h264.a1.bf2` | 12 | 134,004 KB | 11,167 KB | 0 KB | this commit |
| `mixed.live.srt.h264.a2.bf0` | 30 | 128,576 KB | 4,285 KB | 0 KB | this commit |
| `mixed.live.srt.h264.a2.bf2` | 30 | 150,024 KB | 5,000 KB | 0 KB | this commit |

Jitter headroom by design (defaults; env-overridable):

| Ring | Default slots | Typical rate | Headroom |
|---|---|---|---|
| Source (SRT ingest) | 1024 | 80 pkt/s | 12.8 s |
| Source (2v16a adaptive) | 4980 | 830 pkt/s | 6.0 s |
| Transcoder output | 512 | 80 pkt/s | 6.4 s |
| TS mux ring | 256 | ~400 chunks/s | 0.64 s (SRT 12 MB send buffer absorbs the rest) |
| AVIO queue | 512 KB | 1 MB/s @ 8 Mbps | 0.5 s |

### External capacity rollout proof — 2026-07-10

Command:

```sh
scripts/harness/rollouts/external-capacity.sh
```

The guard runs `mixed.live.srt.h264.a2.bf0` twice: first with enough external
FFmpeg permits for a passing capacity smoke, then with one permit and default
checks enabled. The constrained leg must fail causally with
`blockedByPhase=waitingForCapacity`, `backend=externalFfmpeg`, nonzero `waitMs`,
and at least one persisted `ready` recording row with both `temp_path` and
`final_path`.

| Scenario | Capacity-ok permits | Constrained permits | Required constrained evidence | Commit |
|---|---:|---:|---|---|
| `mixed.live.srt.h264.a2.bf0` | 2 | 1 | `waitingForCapacity`, `externalFfmpeg`, `waitMs>0`, ready recording metadata row | this commit |

### Mahashivratri msr full-scale ramp — 2026-07-11 (VPS, not WSL2)

Host: dedicated Contabo VPS (6 vCPU AMD EPYC gen1, 11 GiB RAM, 2 GiB swap),
idle. **Not comparable to WSL2 rows in this file.** Commit `6fc2f254`
(includes msr sink tuning: MediaMTX `writeQueueSize: 512`).

```sh
scripts/build/resource-limit.sh target/bench/test_harness msr        # smoke
MSR_FULL=1 scripts/build/resource-limit.sh target/bench/test_harness msr
```

Status: **PASS at every checkpoint including 1,200 outputs** (1 SRT ingest,
30 audio tracks, Zipf fan-out, 95% RTMP / 5% SRT, 1080p30 H.264 passthrough,
loopback MediaMTX sink). Zero warn/error/panic lines in restream logs.

| Outputs | Egress mix | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |
|---:|---|---:|---:|---:|---:|---:|
| 30 | rtmp:29,srt:1 | 32.1 | 42.4 | 90 MB | 92 KB | 6 |
| 120 | rtmp:114,srt:6 | 102.9 | 128.8 | 126 MB | 362 KB | 6 |
| 300 | rtmp:285,srt:15 | 147.0 | 171.2 | 180 MB | 808 KB | 5 |
| 600 | rtmp:570,srt:30 | 196.0 | 232.7 | 276 MB | 1.78 MB | 5 |
| 900 | rtmp:855,srt:45 | 209.9 | 230.1 | 365 MB | 3.02 MB | 4 |
| 1200 | rtmp:1140,srt:60 | 244.4 | 280.6 | 447 MB | 4.10 MB | 3 |

CPU % is of a single core (600% available on this host). No capacity knee on
this box: 1,200 outputs ran at ~2.4 cores avg / 2.8 peak with ~55% CPU
headroom. CPU scales strongly sublinearly (40× outputs → 7.6× CPU; marginal
cost ≈ 0.18%/output above the 30-output base). RSS ≈ 90 MB + ~0.3 MB/output.
Caveats: loopback sink (MSR-01 link certification still open), moderate
fixture bitrate (Phase 2 connection-scale, not the bitrate envelope), no
external transcoders active. Raw artifacts retained off-repo
(`.local/artifacts/msr-vps/` on the dev box; `~/msr-artifacts-smoke30` +
`.local/artifacts/msr/` on the VPS).

### Mahashivratri msr full-scale ramp with MediaMTX API proof — 2026-07-12 (VPS)

Host: `vmi3423592`, dedicated Contabo VPS (6 vCPU AMD EPYC gen1, 11 GiB RAM,
2 GiB swap), idle. Commit `0e4774e` (SRT plain stream IDs, generic MediaMTX
path-health verifier, paginated `/v3/paths/list` reads).

```sh
MSR_FULL=1 WORK_DIR=.local/artifacts/msr-full-baseline-20260712-paged \
  scripts/harness/run.sh msr
```

Status: **PASS at every checkpoint including 1,200 outputs**. Each checkpoint
proved MediaMTX receiver health through `/v3/paths/list`: every expected path
was `ready=true` and aggregate `bytesReceived` grew across the sample window.
The first full attempt after adding receiver proof failed at 120 outputs because
MediaMTX paginates path listings at 100 items by default; commit `0e4774e`
walks all pages and the rerun passed. Zero warn/error/panic lines in restream,
MediaMTX, and publisher logs for this clean baseline run.

| Outputs | Egress mix | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 30 | rtmp:29,srt:1 | 30/30 | 4.1 MB | 18.1 | 20.4 | 91 MB | 76 KB | 6 |
| 120 | rtmp:114,srt:6 | 120/120 | 17.2 MB | 49.6 | 72.3 | 124 MB | 388 KB | 6 |
| 300 | rtmp:285,srt:15 | 300/300 | 42.4 MB | 76.7 | 111.0 | 177 MB | 736 KB | 6 |
| 600 | rtmp:570,srt:30 | 600/600 | 89.3 MB | 121.8 | 126.8 | 292 MB | 1.69 MB | 5 |
| 900 | rtmp:855,srt:45 | 900/900 | 142.3 MB | 231.1 | 239.1 | 363 MB | 2.55 MB | 4 |
| 1200 | rtmp:1140,srt:60 | 1200/1200 | 192.9 MB | 202.7 | 210.3 | 459 MB | 3.69 MB | 4 |

Artifacts: `.local/artifacts/msr-full-baseline-20260712-paged/`
(`msr-results.json`, `msr-samples.jsonl`, `msr-report.md`, logs, SQLite DB).
This is the local/VPS connection-scale baseline for future MSR comparisons.

### Mahashivratri msr process-mode perf counters — 2026-07-12 (VPS)

Same host and commit as the API-proved ramp above. This run attached
`perf stat` to the `restream` process only after the bench-profile process
started, excluding the harness wrapper, MediaMTX, shell, and build overhead.

```sh
sudo perf stat -x, -p <restream-pid> \
  -o .local/artifacts/msr-full-perf-process-20260712/perf-restream-process.csv \
  -e cycles,instructions,cache-references,cache-misses,branches,branch-misses,\
context-switches,cpu-migrations,page-faults,minor-faults,major-faults
```

The attached run also passed all MediaMTX receiver-health checkpoints
(`1200/1200`, `bytesReceivedDelta=229,620,986` at the final checkpoint).
It is **not** the log-noise baseline: MediaMTX emitted one SRT TS decode warning
near shutdown/load, while Restream and publisher logs were quiet.

| Counter | Value | Note |
|---|---:|---|
| cycles | 199,778,946,277 | 67% multiplexed |
| instructions | 56,913,663,158 | IPC 0.28 |
| cache references | 7,566,409,593 | 66% multiplexed |
| cache misses | 2,039,585,276 | 26.96% of cache refs |
| branches | 12,904,759,218 | 65% multiplexed |
| branch misses | 1,215,289,024 | 9.42% of branches |
| context switches | 75,912 | process lifetime during attached run |
| CPU migrations | 10,143 | high enough to keep worker/thread affinity on the optimization list |
| page faults | 12,187 | all minor |
| major faults | 0 | no disk-backed fault pressure |

The process-mode run's resource checkpoint was noisier/heavier than the clean
baseline, likely because of perf overhead and host variance:

| Outputs | MediaMTX ready | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak |
|---:|---:|---:|---:|---:|---:|
| 30 | 30/30 | 24.2 | 27.8 | 91 MB | 32 KB |
| 120 | 120/120 | 63.1 | 72.1 | 121 MB | 276 KB |
| 300 | 300/300 | 233.5 | 308.9 | 180 MB | 960 KB |
| 600 | 600/600 | 244.8 | 262.3 | 277 MB | 1.71 MB |
| 900 | 900/900 | 275.4 | 296.3 | 385 MB | 2.72 MB |
| 1200 | 1200/1200 | 356.3 | 360.3 | 487 MB | 3.82 MB |

Interpretation for the next optimization pass:

- IPC 0.28 and 26.96% cache-miss rate point at memory/cache locality and
  scheduler/thread movement, not raw compute, as the next bottleneck class.
- 10,143 migrations during the attached run support testing worker/thread
  bin-packing or affinity before touching packet code.
- AVIO HWM remains bounded (<4 MB at 1,200 outputs) and no major faults were
  observed, so memory growth is currently acceptable for the connection-scale
  baseline.

### Mahashivratri msr Tokio worker sweep — 2026-07-12 (VPS)

Host: `vmi3423592`, commit `6f7f28e`. Single-variable sweep of
`RESTREAM_TOKIO_WORKER_THREADS` at one 300-output checkpoint:

```sh
RESTREAM_TOKIO_WORKER_THREADS=<n> MSR_OUTPUT_COUNTS=300 \
  WORK_DIR=.local/artifacts/msr-worker-sweep-20260712/w<n>-300 \
  BENCH_BUILD=never scripts/harness/run.sh msr
```

Each run attached `perf stat -p <restream-pid>` to the Restream process only.
Every run passed with `300/300` MediaMTX paths ready, `bytesReceived` growing
through `/v3/paths/list`, and zero warn/error/panic lines in the run logs.

| Tokio workers | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | IPC | Cache miss % | Branch miss % | Context switches/s | Migrations/s |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | 300/300 | 38.2 MB | 288.9 | 339.5 | 143 MB | 902 KB | 0.26 | 31.82 | 8.66 | 2,205 | 560 |
| 3 | 300/300 | 40.8 MB | 239.4 | 283.8 | 150 MB | 862 KB | 0.24 | 33.45 | 10.03 | 3,403 | 1,029 |
| 4 | 300/300 | 39.0 MB | 252.8 | 302.2 | 158 MB | 795 KB | 0.27 | 33.38 | 9.43 | 3,385 | 1,070 |
| 6 | 300/300 | 47.8 MB | 278.8 | 331.5 | 161 MB | 633 KB | 0.23 | 32.46 | 9.93 | 2,676 | 775 |

Interpretation:

- 3 workers was the best CPU result in this short 300-output pass, using about
  5% less average CPU than 4 workers and about 14% less than 6 workers.
- 2 workers is too constrained for this shape: it had the worst CPU and the
  longest attached perf duration despite passing liveness.
- The worker count did not fix cache locality by itself. IPC stayed below 0.3
  and cache misses stayed above 31% in every run, so changing the default worker
  count alone is not enough evidence for a production default change.
- 3 workers is the best candidate for the next full 1,200-output confirmation
  run, but the default should not be changed until that full run also wins.

A follow-up thread census at `RESTREAM_TOKIO_WORKER_THREADS=3` and 300 outputs
peaked at 66 Restream threads. The SRT threads were already proportional to SRT
socket count: 16 `SRT:RcvQ:*` plus 16 `SRT:SndQ:*` for 15 SRT egresses plus the
SRT ingest, with `SRT:TsbPd` and `SRT:GC` also present. This confirms that the
full MSR shape's 60 SRT egresses will keep carrying roughly one RcvQ/SndQ pair
per SRT socket unless sockets share libsrt muxers. The next structural
optimization target is therefore SRT muxer/thread sharing, before hot/cold
member layout work.

#### Full-scale confirmation of 3-worker candidate

The 3-worker candidate was promoted to a full `MSR_FULL=1` ramp with
process-mode perf:

```sh
RESTREAM_TOKIO_WORKER_THREADS=3 MSR_FULL=1 \
  WORK_DIR=.local/artifacts/msr-worker-sweep-20260712/w3-full-confirm \
  BENCH_BUILD=never scripts/harness/run.sh msr
```

Status: **PASS for receiver liveness but rejected as a clean/default
candidate**. MediaMTX reported `1200/1200` ready paths with bytes growing, but
Restream emitted many `sqlx` slow query/pool-acquire warnings during the
full-scale lifecycle burst and MediaMTX emitted SRT TS decode warnings. The run
is retained as negative sizing evidence, not as a log-clean baseline.

| Outputs | Egress mix | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 30 | rtmp:29,srt:1 | 30/30 | 4.5 MB | 40.5 | 58.1 | 90 MB | 34 KB | 6 |
| 120 | rtmp:114,srt:6 | 120/120 | 17.9 MB | 129.1 | 194.8 | 115 MB | 280 KB | 6 |
| 300 | rtmp:285,srt:15 | 300/300 | 39.2 MB | 218.2 | 233.7 | 160 MB | 937 KB | 6 |
| 600 | rtmp:570,srt:30 | 600/600 | 85.2 MB | 388.4 | 395.6 | 231 MB | 2.00 MB | 3 |
| 900 | rtmp:855,srt:45 | 900/900 | 173.6 MB | 429.1 | 434.7 | 316 MB | 3.00 MB | 3 |
| 1200 | rtmp:1140,srt:60 | 1200/1200 | 282.4 MB | 449.5 | 450.8 | 404 MB | 14.00 MB | 2 |

Process-mode perf counters:

| Counter | Value | Note |
|---|---:|---|
| cycles | 2,084,070,807,729 | 67% multiplexed |
| instructions | 652,302,689,467 | IPC 0.31 |
| cache references | 97,217,081,244 | 66% multiplexed |
| cache misses | 21,725,964,040 | 22.35% of cache refs |
| branches | 148,525,840,849 | 65% multiplexed |
| branch misses | 15,562,068,188 | 10.48% of branches |
| context switches | 9,994,043 | full attached run |
| CPU migrations | 542,817 | full attached run |
| page faults | 112,665 | mostly minor |
| major faults | 4 | unexpected; another reason not to promote this run |

Conclusion: fewer Tokio workers improved some perf counters relative to the
default process-mode perf run, but full-scale MSR startup/reconcile pressure
needs more async/control-plane headroom. Keep the production default unchanged
for now. Any future heuristic should size from the process's effective CPU
quota/mask and workload shape (ingests, output count, SRT egress count, stage
sharing, and external transcoders), not from MSR alone.

### Mahashivratri msr dashboard run after health snapshot lock fix — 2026-07-12 (local)

Host: local development box, commit `844a7c3` plus prior MSR fixes. Full MSR
shape left running for dashboard inspection on `127.0.0.1:3030`:

```sh
RESTREAM_HTTP=3030 MSR_OUTPUT_COUNTS=1200 MSR_SAMPLE_SECS=3600 \
  MSR_SAMPLE_INTERVAL_MS=5000 MSR_SINK_SAMPLE_SECS=3 MSR_NO_CLEANUP=1 \
  WORK_DIR=.local/artifacts/msr-dashboard-live-3030-20260712T110026 \
  BENCH_BUILD=never scripts/harness/run.sh msr -- --no-netns
```

MediaMTX receiver proof stayed green during the live observation window:
`1200/1200` paths ready through paginated `/v3/paths/list`, with aggregate
`bytesReceived` growing from `14,033,112,477` to `14,201,173,521` over a
3-second spot check. Artifacts:
`.local/artifacts/msr-dashboard-live-3030-20260712T110026/`.

Process-mode `perf stat` attached to the live Restream pid only:

| Counter | Value | Note |
|---|---:|---|
| task-clock | 64,991.62 ms | 4.276 CPUs utilized over 15 s |
| cycles | 71,242,974,987 | 68% multiplexed |
| instructions | 26,126,232,306 | IPC 0.37 |
| cache references | 3,638,096,570 | 64% multiplexed |
| cache misses | 725,116,472 | 19.93% of cache refs |
| branches | 5,362,681,943 | 67% multiplexed |
| branch misses | 547,496,712 | 10.21% of branches |
| context switches | 334,252 | 5.143 K/sec |
| CPU migrations | 8,498 | 130.755/sec |
| page faults | 232 | all minor in this short sample |

Authenticated health/status latency under the same live load:

| Endpoint | Samples | Response size | p50 | p95 | Max | Notes |
|---|---:|---:|---:|---:|---:|---|
| `/api/v1/engine/health` | 30 | 3.95 MB | 392 ms | 934 ms | 1,768 ms | Full per-output payload; bounded but heavy |
| `/api/v1/engine/health?view=summary` | 20 | 174-175 KB | 28 ms | 44 ms | 46 ms | Broad dashboard health shape |
| `/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary` | 20 | 175 KB | 362 ms | 444 ms | 460 ms | Dominated by metrics network sampler |
| `/api/v1/pipelines/<id>/graph` | 5 | 9.33 MB | 401 ms | — | 594 ms | Full raw MSR topology: 1,259 nodes, 1,258 edges |

The dashboard runtime path stayed bounded, but the latency split matters:
health summary itself is not the bottleneck. `build_system_metrics_snapshot`
does a deliberate 250 ms network delta sample even for `view=summary`, so
runtime dashboard refreshes pay that wall-clock cost regardless of the health
snapshot lock fix.

The raw processing graph endpoint returned the full MSR topology: 1 ingest,
1 demux, 1 source ring, 30 audio-filter stages, 26 packetizers, and all 1,200
egress leaves. Frontend rendering now folds repeated egress leaves by count,
but the API payload remains a measured control-plane cost. A future server-side
graph view could preserve full topology while returning repeated homogeneous
leaf groups directly.

Thread census over the same live shape found 210 Restream threads. The six hot
Tokio scheduler workers carried roughly 17-21% CPU each, with one additional
Tokio worker at ~2%. The 60 SRT egresses plus one SRT ingest again created one
`SRT:RcvQ:*` worker per socket, each around 0.9-1.4% CPU, for about another
core of aggregate scheduler/system overhead. `SRT:SndQ:*` threads were mostly
near-idle but still present one-per-muxer.

RSS rose during the live dashboard window even though named media buffers were
flat. Across 215 5-second samples at the 1,200-output shape, RSS grew from
323,884 KiB to 1,088,120 KiB while AVIO HWM stayed at 3.2-4.5 MiB, source rings
stayed around 16-17 MiB, transcoder rings around 20-22 MiB, TSMux rings around
9.5-10.9 MiB, and retained payload stayed below 50 MiB. `/proc/<pid>/smaps_rollup`
later reported 1,036,576 KiB private anonymous memory out of 1,091,340 KiB RSS;
`pmap -x` showed multiple nearly-full 64 MiB anonymous regions plus a 36 MiB
heap. That shape is consistent with allocator arena retention, native-library
buffers, or thread churn rather than bounded media-ring growth.

A follow-up 2-minute observation at the same 1,200-output shape showed summary
health remained responsive (`p50=33 ms`, `p95=77 ms`, max `387 ms`) while
MediaMTX bytes advanced by 7.09 GB. RSS/anonymous memory held essentially flat
around 1.09 GB during that window, so the current evidence is allocator/native
memory growth to a plateau, not an unbounded ring-buffer leak.

Interpretation:

- The health snapshot lock fix did not introduce an obvious control-plane
  wedge: `/healthz` stayed responsive while MediaMTX continued to receive all
  1,200 paths, and authenticated `/api/v1/engine/health?view=summary` stayed
  under 50 ms in the live sample.
- IPC, cache miss rate, branch miss rate, and migration rate continue to point
  at locality/scheduler pressure before data-structure field layout as the
  next optimization class.
- The next control-plane latency win is to avoid doing a synchronous 250 ms
  network-rate sample on every dashboard runtime summary refresh, for example
  by caching/updating network counters out-of-band.
- The processing graph is now visually usable at MSR scale, but the raw
  9.33 MB graph response is large enough to justify a future grouped-leaf API
  view if graph refresh becomes part of regular operations.
- The structural SRT opportunity remains muxer/socket sharing or otherwise
  reducing the per-SRT-egress native thread footprint. Worker-count heuristics
  should use effective CPU quota/mask plus workload shape, not MSR alone.
- The memory follow-up should test `MALLOC_ARENA_MAX`/allocator choices as a
  single-variable MSR run before changing hot-path data structures. The evidence
  points first at allocator arena retention or native per-thread buffers, not a
  named ring buffer leak.

