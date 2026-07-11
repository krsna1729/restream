# Performance & Resource Baselines

Durable measurement ledger for perf-sweep. Criterion's `target/criterion/`
state is scratch; this file is the source of truth for "did we regress".

Rules: measurements are serial (idle host, kill-check first), bench profile
only, recorded with date + commit. Update a row only with fresh numbers from
this machine; never copy numbers you did not measure. Historical sections are
reference points — do not overwrite them, add new dated rows.

## Benchmark ledger (Criterion medians)

| Suite | Metric | Median | Noise ± | Commit | Date | Last verified |
|---|---|---|---|---|---|---|
| ring_buffer | (seed via Q-003) | — | — | — | — | — |
| avio_throughput | (seed via Q-003) | — | — | — | — | — |
| high_performance_data_path | (seed via Q-003) | — | — | — | — | — |
| matrix_throughput | — | — | — | — | — | — |
| srt_ingest_latency | — | — | — | — | — | — |
| transcoder_throughput | — | — | — | — | — | — |
| hls_cost | — | — | — | — | — | — |
| hls_fmp4_cost | — | — | — | — | — | — |
| stage_feeder | — | — | — | — | — | — |
| stage_metrics | — | — | — | — | — | — |
| codec_conversions | — | — | — | — | — | — |
| simd_alternatives | — | — | — | — | — | — |
| alert_tracker | — | — | — | — | — | — |

Default regression threshold: ±5% on throughput suites unless a row notes
otherwise. A regression beyond threshold is filed, not silently absorbed.

## Resource ledger (resource-sweep / scale runs)

| Config | RSS | Ring payload | AVIO peak HWM | Blocked writes | Commit | Date |
|---|---|---|---|---|---|---|
| (seed via Q-006) | — | — | — | — | — | — |

### Historical reference — 2026-06-27 memory-optimization pass

After ring/AVIO/TS sizing cuts (−205 MB RSS total across 15 scale cases,
−175 MB ring payload, zero ring overflows):

| Config | RSS after | Ring payload after |
|---|---|---|
| h265-srt 4M | 205 MB | 47 MB |
| h265-srt-multi 8M | 237 MB | 77 MB |
| h264-srt-multi 8M | 137 MB | 71 MB |
| h264-rtmp 8M | 116 MB | 35 MB |

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

## Standing optimization targets (2026-06-27 CPU profile, task-clock 999 Hz)

| Self % | Symbol | Meaning | Backlog |
|---|---|---|---|
| 3.28% | `__memmove_avx_unaligned_erms` | AVIO buffer → `ts_accum` copy | Q-009 [opus] |
| 2.60% | `pthread_mutex_lock` | SRT internal + MemoryQueue mutex | (unfiled) |
| 1.18% | `__vdso_clock_gettime` | per-packet SRT latency tracking | (unfiled) |
| 0.87% | `_int_malloc` | per-packet `Arc::new(MediaPacket)` | Q-010 [opus] |
| 0.43% | `VecDeque::extend` | AVIO queue write (second copy) | Q-009 [opus] |

## Profiling notes (VPS — hardware counters available)

The Contabo VPS (KVM, AMD EPYC gen1) exposes the AMD vPMU: `cycles`,
`instructions`, `cache-references/misses`, `branches`, `branch-misses`,
`L1-dcache-loads/misses`, and stalled-cycle events all work (with ~50–67%
multiplexing when >6 events). `perf_event_paranoid=4`, so `sudo perf` is
required. This is the designated box for CPU/cache-locality measurements;
WSL2 has no PMU (see below).

### Thread-level counter contrast — 2026-07-11, 1,200-output soak, commit 6fc2f254

Measured with `sudo perf stat -t <tid>` (30 s) during the steady-state soak.

| Thread | CPUs used | IPC | L1d miss | Branch miss | Ctx-switch/s | Migrations/s |
|---|---:|---:|---:|---:|---:|---:|
| SRT ingest epoll waiter (blocking pool) | 0.99 | 2.13 | 0.03% | 0.06% | 27 | 0.8 |
| Idle-ish tokio scheduler worker | 0.002 | 0.45 | 4.18% | 8.33% | 2,169 | 807 |
| restream process-wide | 4.25* | 1.17 | — (38% of cache refs miss) | — | 3,376 | 219 |

\* perf task-clock; pidstat usr+sys showed ~2.05 CPUs over the same window —
the gap is scheduled-but-stalled time on the oversubscribed vCPUs. Process-wide
stalls: 5% frontend, 24% backend.

Interpretation (data for future bin-packing/locality decisions):

- A pinned-hot thread gets excellent locality for free (IPC 2.13, sub-0.1%
  miss rates). Cold, rarely-woken workers pay heavily per wake (IPC 0.45,
  8% branch miss, 800 migrations/s). Fewer, busier workers beat many idle ones.
- Tokio is **not** bin-packed today: `src/main.rs` defaults
  `worker_threads = num_cpus` (6 here), no affinity. Override knob exists:
  `RESTREAM_TOKIO_WORKER_THREADS` (and `RESTREAM_TOKIO_MAX_BLOCKING_THREADS`,
  default 512). Async work at 1,200 outputs is light enough that 2–3 workers
  would likely improve locality; untested.

### Thread-class CPU attribution — same soak (pidstat, 10 s)

| Thread class | Threads | Total CPU | Hottest single |
|---|---:|---:|---:|
| tokio-rt-worker (6 sched + ~62 blocking) | 68 | 100.8% | 99.9% (SRT ingest epoll waiter) |
| SRT:RcvQ:w (one per libsrt multiplexer) | 61 | 104.1% | 2.3% |
| SRT:SndQ:w | 61 | ~0% | 0% |
| everything else | ~4 | ~0.1% | — |

Two structural costs found:

1. **SRT ingest epoll waiter busy-spin** (`src/media/srt.rs:1536`): the
   long-lived `spawn_blocking` loop calls `srt_epoll_wait(200ms)` and re-enters
   immediately after notifying — no re-arm handshake with the consumer. With
   the ingest socket nearly always read-ready, the loop spins at full speed
   inside libsrt `CEPoll::wait` (std::set churn, global `uglobal` mutex,
   `steady_clock::now`, malloc/free) ≈ **1 core burned per SRT ingest**,
   independent of output count. Call graph: `/tmp/hotworker.perf` on the VPS.
2. **Per-SRT-egress multiplexer threads**: 60 SRT egress ⇒ 61 muxers ⇒ 122
   libsrt worker threads; RcvQ workers alone total ~1 core (mostly system
   time + scheduler churn). ≈ 1.7% CPU + 2 threads per SRT egress.

### libsrt 1.5.5 lock/thread topology (source audit, 2026-07-11)

Audited `.local/build/static/src/srt` against the profile above.

- **Threads are not tunable.** libsrt spawns one RcvQ + SndQ worker pair per
  UDP multiplexer, one TsbPd thread per *receiving* socket, one global GC.
  No socket option changes this; topology is controlled only by muxer
  sharing.
- **Muxer sharing rule** (`CUDTUnited::updateMux`, api.cpp:3155): a caller
  socket reuses an existing muxer only if it is *bound to the same local
  port first* and `CSrtMuxerConfig` matches exactly — IPTTL, IPTOS,
  REUSEADDR, UDP_SNDBUF, UDP_RCVBUF (+BINDTODEVICE). Unbound `srt_connect`
  auto-selects an ephemeral port (api.cpp:2025) ⇒ fresh muxer per egress.
  Our egress never pre-binds (`srt_bind` is used only by the ingest
  listener, src/media/srt.rs:1153); all sockets already use uniform
  8 MB UDP buffers, so a single fixed local bind port would collapse
  60 egress muxers → 1 (−118 threads, −~1 core RcvQ churn, and one shared
  kernel UDP socket instead of 61 × 8 MB buffer requests). A settings
  mismatch on a shared port is a hard bind error, so option uniformity must
  be enforced at the call site. Accepted ingest sockets already share the
  listener muxer — sharing is libsrt's normal server topology.
- **Cross-socket locks — ingest can stall egress and vice versa.** Three
  process-globals couple all SRT sockets: (1) `CEPoll::m_EPollLock` — held
  by `CEPoll::wait` for every readiness scan (epoll.cpp:565–723) and taken
  *unconditionally* by `update_events` (epoll.cpp:874) from per-packet paths
  (TsbPd delivery core.cpp:5729, egress ACK processing core.cpp:8642,
  send-buffer-full core.cpp:7148) even for sockets subscribed to no epoll.
  The spinning ingest waiter hammers this lock (9.3% mutex_lock + 5.4%
  unlock of its core); the epoll-spin fix removes most of the pressure.
  (2) `m_GlobControlLock` — shared-read on every `srt_send`/`srt_recv`
  (`locateSocket`, api.cpp:2681), exclusive during connect/close/updateMux:
  a 60-output SRT reconnect storm briefly serializes all SRT I/O including
  ingest. (3) `CGlobEvent` — one global condvar; any socket's event wakes
  every `CEPoll::wait` sleeper. At current scale measured contention is
  modest (egress SndQ threads ~0% CPU, no visible stall); it grows with SRT
  egress count, ACK rate, and additional ingests.

## Profiling notes (WSL2)

Hardware PMU counters are unavailable under Hyper-V. Use
`perf record -e task-clock` (software sampling), `perf_event_paranoid=-1`,
with the distro `linux-tools-generic` perf binary.
