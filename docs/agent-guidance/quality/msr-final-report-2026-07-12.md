# MSR Full Performance Report - 2026-07-12

Status: **PASS at every checkpoint** including **1,200 outputs**: 1 SRT
ingest, 30 audio tracks, Zipf fan-out, 95% RTMP / 5% SRT, 1080p30 H.264
passthrough, loopback MediaMTX sink. MediaMTX API proof was collected at every
checkpoint: all expected paths were `ready=true` and aggregate `bytesReceived`
grew during the sink sample window. Restream and harness logs had zero
warn/error/panic lines.

Artifact root:
`.local/artifacts/msr-final-full-20260712T165925Z`

## Contents

- [Release Read](#release-read)
- [Memory Scaling](#memory-scaling)
- [RTMP vs SRT Read](#rtmp-vs-srt-read)
- [1,200-Output Perf Snapshot](#1200-output-perf-snapshot)
- [Perf Investigation Summary](#perf-investigation-summary)
- [Thread Shape](#thread-shape)
- [Mixed-Matrix Thread Shape](#mixed-matrix-thread-shape)
- [Capacity Read](#capacity-read)
- [Sampling Confidence](#sampling-confidence)

## Release Read

Recommendation: **release candidate / controlled deployment is reasonable for
this connection-scale MSR profile; broad release should wait for the long-soak
fault-recovery gate.**

What is strong enough:

- The full 30 -> 1,200 output ramp passed.
- MediaMTX receiver liveness was part of the pass condition, not a separate
  visual check: every expected path had to be ready and `bytesReceived` had to
  grow at every checkpoint.
- Restream and harness logs were clean: zero warn/error/panic lines.
- CPU, RSS, AVIO HWM, and ring telemetry stayed bounded through the ramp.
- Process-mode `perf` was collected against the Restream process while
  MediaMTX receiver proof stayed green before and after the perf window.

What is still not proven by this report:

- A 12-hour soak with fault injection and recovery.
- The full bitrate envelope beyond this 1080p30 H.264 passthrough fixture.
- Protocol-isolated marginal costs for RTMP-only and SRT-only fan-out.
- Long-duration control-plane behavior after egress failures.

So my release call is: **yes for an RC or staged release with this report as
the connection-scale baseline; no for declaring the MSR launch fully certified
until the soak/fault-recovery run passes.**

| Outputs | Egress mix | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 30 | `rtmp:29,srt:1` | `30/30` | `4,665,974` | 17.27 | 23.54 | 92.0 MB | 32 KB | 4 |
| 120 | `rtmp:114,srt:6` | `120/120` | `15,540,301` | 43.28 | 53.09 | 119.4 MB | 256 KB | 4 |
| 300 | `rtmp:285,srt:15` | `300/300` | `46,554,165` | 63.78 | 80.36 | 152.6 MB | 864 KB | 4 |
| 600 | `rtmp:570,srt:30` | `600/600` | `76,622,802` | 90.06 | 98.45 | 207.1 MB | 1.5 MB | 4 |
| 900 | `rtmp:855,srt:45` | `900/900` | `155,214,230` | 114.87 | 141.06 | 263.1 MB | 2.3 MB | 4 |
| 1200 | `rtmp:1140,srt:60` | `1200/1200` | `208,072,764` | 126.87 | 131.90 | 329.5 MB | 3.3 MB | 4 |

## Memory Scaling

Peak memory grew from **92.0 MB RSS** at 30 outputs to **329.5 MB RSS** at
1,200 outputs. Across that span, the marginal RSS cost was about **208
KiB/output**. PSS grew from **88.1 MB** to **311.2 MB**, about **195
KiB/output**, so most of the growth was private process memory rather than
shared file mappings.

| Outputs | Egress mix | RSS peak | PSS peak | Anonymous peak | Private dirty peak | Retained peak | Source ring peak | Stage buffer peak | TSMux ring peak | AVIO HWM peak | Stages |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 30 | `rtmp:29,srt:1` | 92.0 MB | 88.1 MB | 55.4 MB | 55.4 MB | 11.0 MB | 9.9 MB | 0.7 MB | 0.4 MB | 32 KB | 1 |
| 120 | `rtmp:114,srt:6` | 119.4 MB | 113.3 MB | 80.4 MB | 80.4 MB | 17.3 MB | 16.2 MB | 0.7 MB | 0.4 MB | 256 KB | 1 |
| 300 | `rtmp:285,srt:15` | 152.6 MB | 144.7 MB | 111.3 MB | 111.3 MB | 17.8 MB | 16.7 MB | 0.7 MB | 0.4 MB | 864 KB | 1 |
| 600 | `rtmp:570,srt:30` | 207.1 MB | 193.9 MB | 160.2 MB | 160.2 MB | 20.9 MB | 16.7 MB | 2.8 MB | 1.6 MB | 1,486 KB | 4 |
| 900 | `rtmp:855,srt:45` | 263.1 MB | 247.6 MB | 213.7 MB | 213.7 MB | 28.0 MB | 16.5 MB | 7.5 MB | 4.3 MB | 2,361 KB | 11 |
| 1200 | `rtmp:1140,srt:60` | 329.5 MB | 311.2 MB | 276.9 MB | 276.9 MB | 48.1 MB | 16.7 MB | 21.1 MB | 10.3 MB | 3,359 KB | 30 |

What this suggests:

- The source ring reaches its plateau early, around **16-17 MB**, and does not
  scale with output count after 120 outputs.
- Per-output memory is mostly private anonymous memory: connection state,
  protocol/session state, queues, and runtime allocations.
- The larger step-ups after 600 outputs correlate with the stage count rising
  from `1` to `4`, `11`, then `30`. Stage-associated buffers, not the source
  ring, explain much of the late-ramp retained/ring growth.
- AVIO HWM remains small relative to RSS, peaking at **3.3 MB** at 1,200
  outputs. That is healthy for this loopback sink profile.
- `AnonHugePages`, hugetlb, and swap were all **0** in the final
  `smaps_rollup`, so this run did not use huge pages and did not page out.

## RTMP vs SRT Read

This MSR profile intentionally keeps the egress mix fixed at **95% RTMP / 5%
SRT**. At 1,200 outputs that means **1,140 RTMP outputs** and **60 SRT
outputs**. The run proves this exact mixed profile, but it does not isolate a
clean per-output cost for RTMP versus SRT because both protocols ramp together.

The final thread census is still useful:

- RTMP dominates output count, so most per-output connection/session memory is
  from RTMP outputs.
- SRT egress does not create one visible helper-thread pair per destination in
  this run. At 60 SRT outputs the process had **2 SRT receive queue workers**
  and **2 SRT send queue workers**, which indicates shared libsrt queue workers.
- SRT helper threads were a meaningful CPU contributor at the final checkpoint:
  **21.0%** CPU in receive queue workers and **12.2%** in send queue workers
  over the 5-second thread census window.
- The two hot Tokio runtime workers consumed most CPU, so the remaining
  optimization question is not simply "reduce SRT threads"; it is how much work
  the Tokio workers are doing for fan-out, muxing, socket I/O, and scheduling.

To separate protocol costs, the next measurement should run three short
calibration profiles at the same total output counts: RTMP-only, SRT-only, and
the 95/5 MSR mix. That would let us estimate marginal CPU/RSS/thread cost by
protocol instead of inferring it from a coupled workload.

## 1,200-Output Perf Snapshot

Independent process-mode `perf stat -p <restream-pid>` was attached for 15 s at
the 1,200-output checkpoint. MediaMTX receiver proof was collected before and
after the perf window.

| Metric | Result |
|---|---:|
| MediaMTX ready before perf | `1200/1200` |
| MediaMTX bytes delta before perf | `203,110,184` over 3 s |
| MediaMTX ready after perf | `1200/1200` |
| MediaMTX bytes delta after perf | `171,669,716` over 3 s |
| CPU utilized | `2.339` CPUs |
| IPC | `0.307` |
| Cache misses | `20.41%` |
| Branch misses | `9.62%` |
| Context switches | `3.209 K/sec` |
| CPU migrations | `388.668/sec` |
| Page faults | `0/sec` |
| RSS / PSS | `339,136 / 320,408 KiB` |
| Private anonymous | `285,268 KiB` |

Perf interpretation:

- **IPC 0.307** is low, which usually means the process is not limited by pure
  arithmetic throughput. It is consistent with pointer-heavy fan-out, protocol
  framing, queues, syscalls, scheduler wakeups, and cache misses.
- **20.41% cache-miss rate** is high enough that data locality and allocation
  layout remain real opportunities. The best candidates are hot/cold splitting
  of per-egress state, tighter queue metadata, and reducing cross-thread
  handoff churn.
- **9.62% branch misses** is also high. Some of that is expected in protocol
  state machines and mixed output handling, but it makes branch-heavy hot paths
  worth inspecting with sampled stacks before refactoring.
- **3.209 K context switches/sec** and **388.668 migrations/sec** are not
  catastrophic for 1,200 loopback outputs, but they explain why external
  affinity/placement showed promise. Any in-process pinning still needs better
  proof because the first runtime scanner did not reproduce the external win.
- **0 page faults/sec** during the perf window and **0 swap** in smaps are good:
  the run was not memory-pressure limited at 1,200 outputs.

## Perf Investigation Summary

The performance work produced a useful shape, plus several rejected tuning
paths:

| Investigation | Result | Decision |
|---|---|---|
| SRT epoll busy-spin | Earlier perf identified a level-triggered epoll waiter burning CPU when data stayed readable. Demand-gating removed the pathological spin in later runs. | Keep fix |
| SRT muxer/thread sharing | Mixed MSR at 60 SRT egresses used only 2 receive and 2 send queue workers, not one helper pair per SRT destination. | Good shape for this workload |
| Tokio worker count | The final default run used 2 hot Tokio scheduler workers; worker-count tuning alone was not enough to define a universal heuristic. | Derive future heuristic from ingest/output/stage-sharing shape, not MSR alone |
| Tokio blocking cap | `RESTREAM_TOKIO_MAX_BLOCKING_THREADS=32` still showed roughly the same Tokio-named thread family and worsened short-run CPU/RSS. | Rejected |
| Tokio blocking keepalive | `100 ms` keepalive did not shrink the 64 Tokio-named threads and worsened CPU/RSS. | Rejected |
| External CPU placement | External SRT-vs-other partitioning improved some counters in a clean A/B, but the in-process scanner did not reproduce the win. | Prefer systemd/operator placement guidance for now |
| Allocator arena cap | Lowered RSS/PSS in one local run but worsened CPU/cache/branch counters. | Emergency memory-pressure knob only, not default |
| RTMP ownership/burst experiments | Micro-benchmark wins did not carry cleanly into full MSR. | Rejected until full-workload perf supports it |

Current optimization opportunities from the evidence:

- Continue with sampled-stack attribution before changing hot-path layouts. The
  low IPC and high cache-miss rate make layout work tempting, but the exact
  owning structures should come from `perf record`/flamegraph evidence.
- Investigate hot/cold splitting for per-egress state only if sampled stacks
  show repeated cache pressure in egress metadata or queue traversal.
- Keep SRT helper-thread count under observation in SRT-heavy calibration runs;
  this 95/5 mixed run is not enough to prove SRT-only scaling.
- Consider service-level CPU/NUMA placement guidance before in-process pinning.
  Runtime pinning affects lifecycle and needs stronger concurrency proof.

## Thread Shape

Final census at 1,200 outputs:

| Thread group | Count | Notes |
|---|---:|---|
| Restream total | 82 | Full process thread count during final MSR snapshot |
| `restream-tokio` | 64 | Tokio-owned runtime/blocking-pool-named family; only two were hot |
| SRT receive queue | 2 | Shared libsrt receive workers |
| SRT send queue | 2 | Shared libsrt send workers |
| SQLite workers | 10 | Background SQLite worker threads |
| Main / SRT timestamp / SRT GC / tracing appender | 4 | Single-purpose support threads |

The current evidence says the 64 Tokio-named threads are Tokio-owned, but not
simply reclaimable idle `spawn_blocking` threads: a cap-32 experiment and a
100 ms keepalive prototype both failed to reduce the family and worsened the
short-run MSR numbers.

## Mixed-Matrix Thread Shape

At the final 1,200-output checkpoint, the mixed matrix was:

- 1 SRT ingest
- 30 audio tracks
- 1,140 RTMP egresses
- 60 SRT egresses
- 30 active stage buffers
- 82 total Restream threads

Thread count did **not** scale linearly with output count. Most outputs are
handled by a small number of hot runtime/SRT workers plus many sleeping
connection/runtime-owned threads:

- Two Tokio workers carried the majority of runtime CPU in the 5-second thread
  census: **56.2%** and **52.2%**.
- The shared SRT queue workers were the next visible protocol CPU cost:
  receive workers at **21.0%** combined and send workers at **12.2%** combined.
- SQLite, tracing, SRT GC, and timestamp/playback threads were effectively cold
  in this run.
- Of the 64 Tokio-named threads, 63 were sleeping at the census point and only
  one was runnable. That count is real, but it is not the same as 64 hot CPU
  workers.

The mixed matrix therefore looks CPU-concentrated, not thread-count
concentrated: the primary hot path is the runtime fan-out/scheduling work plus
shared SRT queue work. The next proof should classify Tokio thread stacks during
the final plateau so we can distinguish async scheduler work, blocking-pool
work, socket I/O, mux/framing, and wakeup overhead.

## Capacity Read

CPU percentage is single-core based. On this 6-vCPU host, the final 1,200-output
checkpoint used about **1.27 cores average** by harness sampling and **2.339
CPUs** during the independent perf window. CPU scaling was sublinear across the
ramp, RSS stayed under **330 MB** at 1,200 outputs, and AVIO high-water mark
stayed at **3.3 MB**.

## Sampling Confidence

This run is high confidence for **functional pass/fail and receiver liveness**:
each checkpoint required MediaMTX `/v3/paths/list` to report every expected path
ready and aggregate `bytesReceived` increasing.

It is medium confidence for **capacity sizing**: each checkpoint used four
resource samples, which is enough to catch the broad CPU/RSS/queue shape but not
enough to claim small percentage wins. Treat differences below about 5-10% as
noise unless repeated across multiple runs or supported by `perf`.

| Outputs | Samples | CPU mean % | CPU stddev % | CPU min-max % |
|---:|---:|---:|---:|---:|
| 30 | 4 | 17.27 | 3.88 | 13.98-23.54 |
| 120 | 4 | 43.28 | 7.38 | 34.31-53.09 |
| 300 | 4 | 63.78 | 9.98 | 55.08-80.36 |
| 600 | 4 | 90.06 | 6.97 | 79.35-98.45 |
| 900 | 4 | 114.87 | 18.61 | 90.27-141.06 |
| 1200 | 4 | 126.87 | 4.23 | 120.58-131.90 |

Recommended improvement: keep the current short checkpoint ramp for fast
regression detection, then add a calibration mode that samples each plateau for
60-120 seconds with 1-second CPU/RSS/queue samples, plus a 15-second
`perf stat -p` window at the final plateau. For publication-quality numbers,
repeat the full ramp three times from a cold harness start and report median,
p95, min/max, and coefficient of variation.

Caveats: this is a loopback MediaMTX sink run, not the 12-hour soak. The test
certifies connection-scale and receiver liveness for this MSR profile; bitrate
envelope and long-duration fault recovery remain separate proof points.
