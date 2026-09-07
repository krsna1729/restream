# Performance baselines — profiling notes (archived)

> Archived from `docs/agent-guidance/quality/baselines.md`. Live Criterion and resource ledger tables remain there.

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

### MSR full live alert and one-sink recovery proof — 2026-07-12 (VPS)

Commit under test: local tree after the audio-router lifecycle fix and
egress-failure log-level correction. Artifacts:
`.local/artifacts/msr-dashboard-live-3030-20260712T114859Z/`.

Setup:

```sh
RESTREAM_HTTP=3030 RESTREAM_INITIAL_ADMIN_PASSWORD=restream-local-harness-password \
  MSR_OUTPUT_COUNTS=1200 MSR_SAMPLE_SECS=3600 MSR_SAMPLE_INTERVAL_MS=5000 \
  MSR_SINK_SAMPLE_SECS=3 MSR_NO_CLEANUP=1 BENCH_BUILD=never \
  scripts/harness/run.sh msr -- --no-netns
```

Pre-fault receiver and UI-status proof:

- MediaMTX API `/v3/paths/list` returned `1200/1200` ready paths with
  `bytesReceived=1,114,198,451`.
- `/api/v1/alerts` returned `0` alerts.
- Full `/api/v1/engine/health` returned `1200` outputs, `1200` running,
  and `0` `blockedBy` fields.

Fault slice: kicked one MediaMTX RTMP publisher connection with
`POST /v3/rtmpconns/kick/{id}` for `live/msr-rank01-rtmp-0001`, then sampled
summary health, alerts, and paginated MediaMTX path health for 61 seconds.
Raw evidence:
`.local/artifacts/msr-dashboard-live-3030-20260712T114859Z/rtmp-kick-proof.jsonl`.

| Metric | Result |
|---|---|
| Kicked connection | `6f8a4846-5961-4cfb-aa17-0cd90ba65b3f` |
| Kick API status | `200` |
| Health failures | `0/61` |
| Summary health p95 | `56.8 ms` |
| Summary health max | `201.8 ms` |
| Alert count max | `1` |
| MediaMTX ready min | `1199/1200` |
| MediaMTX ready final | `1200/1200` |
| Final MediaMTX bytes | `8,702,695,480` |

Log proof: the recovered egress failure was emitted as `WARN`
(`event_type="egress.failed"`, `phase=send`, `error=remote closed connection`)
instead of `ERROR`; Restream had no `ERROR`/`panic` lines for the run.

Steady-state process-mode perf on the same fixed run, after enabling PMU
access with `sudo sysctl kernel.perf_event_paranoid=-1`, kept MediaMTX at
`1200/1200` ready paths while bytes advanced. Raw artifacts:

- `.local/artifacts/msr-dashboard-live-3030-20260712T114859Z/perf-stat-restream-20260712T115411Z.csv`
- `.local/artifacts/msr-dashboard-live-3030-20260712T114859Z/pidstat-threads-restream-20260712T115450Z.txt`
- `.local/artifacts/msr-dashboard-live-3030-20260712T114859Z/perf-record-restream-20260712T115450Z.txt`

Process counters over a 15 s `perf stat -p <restream-pid>` attach:

| Metric | Result |
|---|---:|
| CPU utilized | `4.243` CPUs |
| Instructions per cycle | `0.34` |
| Cache misses | `20.76%` of cache refs |
| Branch misses | `10.63%` of branches |
| Context switches | `5.334 K/sec` |
| CPU migrations | `136.610/sec` |

Thread-family CPU from 10 s `pidstat -t` averages:

| Thread family | Threads | CPU | User | System |
|---|---:|---:|---:|---:|
| Tokio runtime workers | 68 | `137.35%` | `45.86%` | `91.49%` |
| SRT RcvQ workers | 61 | `125.00%` | `19.10%` | `106.36%` |
| SRT SndQ workers | 61 | `29.07%` | `3.50%` | `25.59%` |
| SRT TsbPd | 1 | `0.79%` | `0.10%` | `0.69%` |
| sqlx workers | 10 | `0.40%` | `0.20%` | `0.20%` |

`perf record -F 99 -g -p <restream-pid>` showed the largest single sampled
bucket in scheduler/futex wakeups (`__pv_queued_spin_lock_slowpath` via
`try_to_wake_up`/`futex_wake`, 2.60%), followed by APIC timer interrupts
(2.01%), RTMP egress work (1.50%), and `__memmove_avx_unaligned_erms`
(1.02%). The actionable ordering did not change after the alert/log fixes:

1. Collapse SRT egress UDP muxers where destination parameters are compatible
   (same local bind port + identical muxer config) to remove most of the
   122 SRT worker threads and roughly `1.5` CPUs of RcvQ/SndQ churn at this
   60-SRT-output shape.
2. Reduce Tokio wakeup/migration pressure in RTMP egress fan-out. The 68
   Tokio worker/blocking threads only use ~1.37 CPUs but drive high system
   time and migrations; any worker heuristic should use effective CPU mask,
   ingest/output mix, and shared-stage shape rather than MSR alone.
3. Treat `memmove` as a secondary copy target. It is visible but far below
   scheduler/libsrt costs in this workload, so AVIO/TsMux copy removal should
   stay behind correctness proof and targeted bench evidence.

### MSR SRT egress muxer reuse proof — 2026-07-12 (VPS)

Same host, bench-profile binaries, 1 SRT ingest, 30 audio tracks, 1,200 active
egress outputs (`rtmp:1140`, `srt:60`). This run validates the runtime SRT
egress change that sets `SRTO_REUSEADDR` for non-bonded egress sockets and
reuses the first successful local UDP muxer port for later compatible egresses.
Bonded SRT egress remains on its existing group path.

Raw artifacts:

- `.local/artifacts/msr-srt-muxer-reuse-3030-20260712T121114Z/redeploy.log`
- `.local/artifacts/msr-srt-muxer-reuse-3030-20260712T121114Z/restream.log`
- `.local/artifacts/msr-srt-muxer-reuse-3030-20260712T121114Z/perf-stat-restream-srt-muxer-reuse.csv`
- `.local/artifacts/msr-srt-muxer-reuse-3030-20260712T121114Z/pidstat-threads-restream-srt-muxer-reuse.txt`

Correctness proof:

| Check | Result |
|---|---:|
| Restream health | `ready` |
| Runtime graph active egress | `rtmp:1140`, `srt:60` |
| MediaMTX paths ready | `1200/1200` |
| MediaMTX bytes received | `6,511,473,976` and advancing |
| Runtime alerts | `0` |
| Logged reusable muxer port | `51702` |

Process counters over a 20 s `perf stat -p <restream-pid>` attach:

| Metric | Before | After |
|---|---:|---:|
| CPU utilized | `4.243` CPUs | `2.992` CPUs |
| Instructions per cycle | `0.34` | `0.34` |
| Cache misses | `20.76%` | `19.38%` |
| Branch misses | `10.63%` | `8.53%` |
| Context switches | `5.334 K/sec` | `2.215 K/sec` |
| CPU migrations | `136.610/sec` | `201.210/sec` |

Thread-family CPU from `pidstat -t` averages:

| Thread family | Before threads / CPU | After threads / CPU |
|---|---:|---:|
| Tokio runtime workers | `68 / 137.35%` | `68 / 108.95%` |
| SRT RcvQ workers | `61 / 125.00%` | `2 / 17.74%` |
| SRT SndQ workers | `61 / 29.07%` | `2 / 8.65%` |
| SRT TsbPd | `1 / 0.79%` | `1 / 0.30%` |
| sqlx workers | `10 / 0.40%` | `10 / 0.30%` |

Interpretation: muxer reuse removed 118 libsrt worker threads and about
1.25 cores of steady CPU from this MSR shape while preserving all MediaMTX
receiver health. The next optimization target is Tokio/system scheduling:
worker count is still 68, the six scheduler workers are hot, and CPU migration
rate did not improve with SRT thread collapse. Any Tokio heuristic should be
driven by effective CPU mask plus workload shape (ingests, outputs, SRT count,
and stage sharing), not MSR alone.

### MSR Tokio worker-count quick sweep after SRT muxer reuse — 2026-07-12 (VPS)

Same 1,200-output MSR shape and same bench-profile binary as the muxer-reuse
proof. Each point first proved MediaMTX receiver health via paginated
`/v3/paths/list` (`1200/1200` ready and `bytesReceived > 0`), then attached
`perf stat -p <restream-pid>` for 20 seconds.

Raw artifacts:

- `.local/artifacts/msr-srt-muxer-reuse-3030-20260712T121114Z/perf-stat-restream-srt-muxer-reuse.csv`
- `.local/artifacts/msr-tokio-workers3-3030-20260712T121730Z/perf-stat-restream-workers3.csv`
- `.local/artifacts/msr-tokio-workers2-3030-20260712T121914Z/perf-stat-restream-workers2.csv`
- `.local/artifacts/msr-tokio-default-3030-20260712T122557Z/perf-stat-restream-default-workers.csv`

| Tokio workers | MediaMTX ready | CPU utilized | IPC | Cache misses | Branch misses | Context switches | CPU migrations |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 6 | `1200/1200` | `2.992` CPUs | `0.34` | `19.38%` | `8.53%` | `2.215 K/sec` | `201.210/sec` |
| 3 | `1200/1200` | `2.813` CPUs | `0.35` | `19.55%` | `8.65%` | `2.386 K/sec` | `217.783/sec` |
| 2 | `1200/1200` | `2.458` CPUs | `0.40` | `17.49%` | `8.40%` | `1.912 K/sec` | `231.536/sec` |

Interpretation: after the SRT muxer/thread fix, two Tokio scheduler workers
gave the best CPU/cache/context-switch result for this 6-vCPU high-fanout I/O
shape. CPU migrations remained high, so a future affinity/bin-packing pass
needs separate proof. The default runtime policy was changed to derive effective
CPUs from Rust available parallelism, process CPU mask, and cgroup v2 quota,
then use roughly one Tokio worker per three effective CPUs (rounded up, clamped
to `1..8`) while preserving `RESTREAM_TOKIO_WORKER_THREADS` as an override.
The rebuilt default-policy binary selected the same two-worker shape on this
host (`64` `tokio-rt-worker` threads total after blocking tasks), kept MediaMTX
at `1200/1200` ready paths with bytes advancing to `5,152,906,267`, and measured
`2.476` CPUs over the attached 20-second perf sample.

### Negative result: RTMP burst write coalescing — 2026-07-12 (VPS)

Hypothesis: coalescing already-pulled RTMP media packets into one reused
`Vec<u8>` per ring burst would reduce `send`/wakeup pressure enough to offset
the extra userspace copy. The experiment preserved correctness (`1200/1200`
MediaMTX paths ready, bytes advancing, zero runtime alerts, zero Restream
warn/error/panic lines), but did not improve the process counters.

Raw artifacts:

- `.local/artifacts/msr-rtmp-batch-3030-20260712T123503Z/perf-stat-restream-rtmp-batch.csv`
- `.local/artifacts/msr-rtmp-batch-3030-20260712T123503Z/pidstat-threads-restream-rtmp-batch.txt`

| Variant | MediaMTX ready | CPU utilized | IPC | Cache misses | Branch misses | Context switches | CPU migrations |
|---|---:|---:|---:|---:|---:|---:|---:|
| Default 2-worker policy | `1200/1200` | `2.476` CPUs | `0.37` | `19.38%` | `8.39%` | `2.728 K/sec` | `322.886/sec` |
| RTMP media burst write coalescing | `1200/1200` | `2.530` CPUs | `0.35` | `20.68%` | `7.79%` | `2.325 K/sec` | `265.887/sec` |

Conclusion: the extra userspace copy and retained per-egress buffer outweigh
the syscall/context-switch reduction in this workload. The code was reverted;
future RTMP work should target packet construction/allocation in
`rml_rtmp::ChunkSerializer` or payload ownership before adding another batching
copy.

### Current MSR RTMP allocator/serializer profile — 2026-07-12 (VPS)

Same post-fix full MSR dashboard run as the MediaMTX env-hardening redeploy
(`abc8ee0`, bench-profile binaries, 1,200 outputs, `1200/1200` MediaMTX paths
ready). A fresh 20-second `perf stat -p <restream-pid>` attach measured:

| Metric | Current |
|---|---:|
| CPU utilized | `2.304` CPUs |
| IPC | `0.36` |
| Cache misses | `18.04%` |
| Context switches | `3.050 K/sec` |
| CPU migrations | `337.348/sec` |

Thread sampling still showed the SRT muxer reuse win holding (`2` RcvQ and `2`
SndQ workers), while RTMP/Tokio work dominated the remaining CPU. A 15-second
`perf record -g -p <restream-pid>` had these top user-space symbols:

| Symbol/family | Overhead |
|---|---:|
| `restream::media::rtmp::start_rtmp_egress` | `2.56%` |
| `libc::_int_malloc` | `1.50%` |
| `rml_rtmp::chunk_io::serializer::ChunkSerializer::serialize` | `1.17%` |
| `libc::__memmove_avx_unaligned_erms` | `1.15%` |
| `libc::_int_free` | `1.14%` |
| `libc::malloc` | `0.97%` |

Interpretation: production RTMP egress already uses reusable conversion
buffers for Raw H.264/AAC, but it must hand owned `Bytes` to `rml_rtmp` and then
write the serialized outbound packet. The next RTMP optimization should isolate
packet-construction/allocation first (for example, a benchmark that compares the
current `Bytes::copy_from_slice` + `ChunkSerializer` path with any reusable or
ownership-transfer alternative). Repeating burst write coalescing is not
justified by the current evidence.

Raw artifacts:

- `.local/artifacts/msr-redeploy-3030-20260712T125823Z/perf-stat-restream-current-20260712T130031Z.csv`
- `.local/artifacts/msr-redeploy-3030-20260712T125823Z/pidstat-threads-restream-current-20260712T130107Z.txt`
- `.local/artifacts/msr-redeploy-3030-20260712T125823Z/perf-report-restream-current-20260712T130204Z.txt`

### RTMP payload ownership micro-benchmark — 2026-07-12 (VPS)

Follow-up to the current MSR RTMP profile and negative RTMP burst-coalescing
result. This benchmark isolates only the Raw-to-RTMP payload handoff below
`rml_rtmp::ChunkSerializer`: current production shape reuses a conversion `Vec`
then copies into `Bytes`; alternatives avoid that final copy by moving a fresh
or replaced `Vec` into `Bytes`.

Command:

```sh
scripts/build/resource-limit.sh cargo bench --bench codec_conversions -- \
  'codec/rtmp_payload_ownership' --warm-up-time 1 --measurement-time 2 \
  --sample-size 20
```

Raw output:
`.local/artifacts/bench-rtmp-payload-ownership-20260712/output.txt`

| Payload | Current reuse+copy | Fresh Vec→Bytes | Replace reused Vec→Bytes | Best observed delta |
|---|---:|---:|---:|---:|
| 8 KiB P-frame | `527.46 ns` | `503.44 ns` | `472.51 ns` | `-10.4%` |
| 30 KiB / 3-NALU P-frame | `2.4251 us` | `1.7831 us` | `1.7556 us` | `-27.6%` |
| 80 KiB IDR | `6.1042 us` | `4.3466 us` | `4.4666 us` | `-28.8%` |
| 207 B AAC | `35.333 ns` | `35.529 ns` | — | no signal |

Interpretation: the copy into `Bytes` is expensive enough for video payloads to
justify a scoped runtime experiment, but not for audio. The promising runtime
shape is not burst write coalescing; it is converting into a buffer whose
ownership can be moved into `Bytes` before `rml_rtmp` serialization. Correctness
risk remains: each RTMP egress must keep independent buffers and must not reuse
a buffer after handing ownership to `Bytes`.

### Negative result: RTMP Raw video payload ownership transfer — 2026-07-12 (VPS)

Hypothesis: the micro-benchmark win above would carry into full MSR if Raw
video conversion moved the converted `Vec<u8>` into `Bytes` instead of copying
the reusable conversion buffer. The runtime experiment changed only the Raw
video RTMP egress handoff; audio stayed on the existing reusable-buffer +
`Bytes::copy_from_slice` path.

Correctness proof:

| Check | Result |
|---|---:|
| Scoped RTMP tests | `82 passed` |
| MediaMTX paths ready | `1200/1200` |
| MediaMTX bytes delta | `202,990,188` over 3 s |
| Startup Restream/MediaMTX errors | `0` |

Process counters over a 20 s `perf stat -p <restream-pid>` attach:

| Metric | Current baseline | Ownership transfer experiment |
|---|---:|---:|
| CPU utilized | `2.304` CPUs | `2.468` CPUs |
| IPC | `0.36` | `0.37` |
| Cache misses | `18.04%` | `18.29%` |
| Context switches | `3.050 K/sec` | `2.598 K/sec` |
| CPU migrations | `337.348/sec` | `301.908/sec` |
| Page faults | `1.834/sec` | `60.491/sec` |

Conclusion: despite the micro-benchmark win, the full MSR workload got more
expensive. The extra allocation/page-fault pressure outweighed the removed copy,
while context-switch and migration reductions were not enough to compensate.
The runtime code was reverted. Future RTMP work should look below this handoff
at `rml_rtmp::ChunkSerializer` allocation behavior or a true reusable outbound
serialization buffer, not per-packet `Vec` ownership transfer.

Raw artifacts:

- `.local/artifacts/msr-rtmp-ownership-3030-20260712T132924Z/perf-stat-restream-ownership-20260712T133014Z.csv`
- `.local/artifacts/msr-rtmp-ownership-3030-20260712T132924Z/pidstat-threads-restream-ownership-20260712T133046Z.txt`

### Restored MSR dashboard sample after RTMP ownership rejection - 2026-07-12 (local)

After reverting the runtime RTMP ownership-transfer experiment, rebuilt the
bench-profile harness binaries and left a restored 1,200-output MSR dashboard
run alive on port 3030 for inspection. This is a live observation sample, not a
zero-warning certification run: Restream emitted one startup slow-SQL warning
while enabling outputs.

MediaMTX receiver proof stayed green before and after the process-mode perf
attach:

| Sample | MediaMTX ready | MediaMTX bytes delta |
|---|---:|---:|
| Before perf | `1200/1200` | `164,832,352` over 3 s |
| After perf | `1200/1200` | `166,214,003` over 3 s |

Process counters over a 15 s `perf stat -p <restream-pid>` attach:

| Metric | Restored runtime |
|---|---:|
| CPU utilized | `2.527` CPUs |
| IPC | `0.367` |
| Cache misses | `18.70%` |
| Branch misses | `8.44%` |
| Context switches | `6.118 K/sec` |
| CPU migrations | `676.333/sec` |
| Page faults | `0.067/sec` |
| RSS / PSS | `333,840 KiB` / `323,384 KiB` |
| Private anonymous | `288,640 KiB` |

Thread sampling still showed work split between two hot Tokio workers and the
shared SRT muxer threads: hottest `tokio-rt-worker` samples were `43.11%` and
`42.32%` CPU, while the hottest SRT threads were `SRT:RcvQ:w2` at `14.57%` and
`SRT:SndQ:w2` at `7.98%`. The SRT thread explosion remains fixed; the open
question is CPU placement/migration policy, tracked as Q-012, and allocator
arena sizing for RSS/PSS, tracked as Q-013.

Raw artifacts:

- `.local/artifacts/msr-redeploy-3030-20260712T133546Z/mediamtx-proof-before-final-perf.json`
- `.local/artifacts/msr-redeploy-3030-20260712T133546Z/mediamtx-proof-after-final-perf.json`
- `.local/artifacts/msr-redeploy-3030-20260712T133546Z/perf-stat-restream-restored-final.csv`
- `.local/artifacts/msr-redeploy-3030-20260712T133546Z/pidstat-threads-restream-restored-final.txt`
- `.local/artifacts/msr-redeploy-3030-20260712T133546Z/restream-smaps-rollup-before-final-perf.txt`

### Negative result: MSR `MALLOC_ARENA_MAX=2` arena cap - 2026-07-12 (local)

Single-variable follow-up to the restored MSR dashboard sample above. The run
kept the same bench-profile binaries and 1,200-output loopback MediaMTX sink,
changing only the process environment with `MALLOC_ARENA_MAX=2`.

Correctness and sink proof:

| Check | Result |
|---|---:|
| MediaMTX ready before perf | `1200/1200` |
| MediaMTX bytes delta before perf | `162,241,256` over 3 s |
| MediaMTX ready after perf | `1200/1200` |
| MediaMTX bytes delta after perf | `169,653,485` over 3 s |
| Later MediaMTX bytes delta | `183,699,931` over 3 s |
| Restream warn/error/panic lines | `0` |

Resource/counter comparison against the immediately preceding restored runtime
sample:

| Metric | Restored runtime | `MALLOC_ARENA_MAX=2` |
|---|---:|---:|
| CPU utilized | `2.527` CPUs | `2.600` CPUs |
| IPC | `0.367` | `0.374` |
| Cache misses | `18.70%` | `18.37%` |
| Branch misses | `8.44%` | `8.50%` |
| Context switches | `6.118 K/sec` | `5.495 K/sec` |
| CPU migrations | `676.333/sec` | `712.867/sec` |
| Page faults | `0.067/sec` | `76.467/sec` |
| RSS / PSS at first sample | `333,840 / 323,384 KiB` | `310,848 / 292,509 KiB` |
| RSS / PSS after settle | `333,840 / 323,384 KiB` | `317,444 / 299,104 KiB` |
| Private anonymous after settle | `288,640 KiB` | `265,324 KiB` |
| AnonHugePages | `0 KiB` | `0 KiB` |

Conclusion: the arena cap reduced local MSR RSS/PSS by roughly `16-24 MiB` at
this point in the run, but it increased CPU, CPU migrations, and especially
minor page faults. The memory win is too small relative to the CPU/latency-risk
signals to recommend `MALLOC_ARENA_MAX=2` as a default operator setting for
MSR. Keep allocator tuning as an emergency memory-pressure knob only, and do
not wire it into runtime defaults without a longer soak and p99 latency proof.

Hugepage note from the same run: the host was in THP `madvise` mode and the
Restream process had `AnonHugePages: 0 KiB`. A 10 s dTLB sample showed
`19,429,143` dTLB load misses (`12.95%` of dTLB load accesses), so targeted
large-buffer hugepage work is a plausible later experiment, but global THP
`always` or explicit hugetlb reservation is not justified by this evidence.

Raw artifacts:

- `.local/artifacts/msr-arena2-3031-20260712T161905Z/mediamtx-proof-before-perf.json`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/mediamtx-proof-after-perf.json`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/mediamtx-proof-plus2m.json`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/perf-stat-restream-arena2.csv`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/perf-stat-restream-arena2-dtlb.csv`

### Negative result: mimalloc global allocator A/B - 2026-08-07 (local)

Follow-up to the SRT egress buffer right-sizing work
(`docs/configuration.md`'s "Recognized SRT egress URL parameters"). A live Go
heap profile pulled from MediaMTX 1.20 under equivalent RTMP egress load
showed its runtime actively scavenging freed heap pages back to the OS
(`HeapReleased` non-zero mid-run), while restream uses glibc's default
allocator with no custom global allocator configured anywhere. Combined with
the `MALLOC_ARENA_MAX=2` result above (RSS win, real CPU/fault cost from the
blunt arena-count restriction), the hypothesis was that a purpose-built
allocator — per-thread/per-CPU segment caches with decay-based page return
instead of blunt arena-count restriction — might recover the same RSS
without that cost. Tried mimalloc (`tikv-jemallocator` was the other
documented candidate, not tried).

Setup: `mimalloc` crate as `#[global_allocator]` behind an opt-in Cargo
feature, A/B against the stock build, pure-SRT scenario-1 egress fan-out
(same harness as the buffer right-sizing work), N=640, same VPS class,
n=2 runs per allocator.

| Run | glibc (stock) | mimalloc |
|---|---:|---:|
| 1 | 592.7 MB | 615.4 MB |
| 2 | 688.2 MB | 603.3 MB |
| range | 95.5 MB | 12.1 MB |
| mean | 640.4 MB | 609.4 MB |

Conclusion: **not a clear win.** The two ranges overlap; mimalloc's mean was
~5% lower but sits inside glibc's own run-to-run noise band at this sample
size. mimalloc's variance was visibly tighter (12 MB vs 95 MB spread) — a
more *consistent* footprint, not a confidently smaller one. Reverted rather
than merged: an unproven ~5% maybe isn't worth a new dependency and a global
allocator swap. If revisited, run a real resource-sweep/MSR soak (not a
2-sample spot check) before deciding either way, and consider jemalloc as
the still-untried alternative — it's the more established candidate for this
exact glibc-arena-retention symptom in Rust server workloads.
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/pidstat-threads-restream-arena2.txt`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/restream-smaps-rollup-before-perf.txt`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/restream-smaps-rollup-plus2m.txt`

### Exploratory MSR thread-affinity partition probe - 2026-07-12 (local)

External, reversible `taskset` probe on the live `MALLOC_ARENA_MAX=2` MSR run
above. This did not change runtime code. The original thread masks were saved,
SRT threads were temporarily pinned to CPUs `0-1`, and all other Restream
threads were pinned to CPUs `2-5`; masks were restored to `0-5` after the
sample. This is not a clean Q-012 completion run because it sits on top of the
arena-cap experiment, but it is useful triage for whether internal affinity is
worth designing.

Current code already sizes Tokio workers from effective CPU capacity:
Rust available parallelism, process `Cpus_allowed_list`, and cgroup v2
`cpu.max`, using `effective_cpus / 3` rounded up and clamped to `1..8`. The
live process had mask `0-5` and `82` threads. The `tokio-rt-worker` Linux
thread name is overloaded: it includes idle blocking-pool threads, not just
busy async scheduler workers. The hot sample showed only two busy Tokio
workers, so raw thread count alone is not a capacity problem.

Thread census before partitioning:

| Thread group | Threads | 5 s CPU sample | Notes |
|---|---:|---:|---|
| Tokio runtime/blocking-pool | `64` | `96.60%` | only two hot; most sleeping |
| SRT receive queue | `2` | `19.00%` | `SRT:RcvQ:w2` hot |
| SRT send queue | `2` | `8.60%` | `SRT:SndQ:w2` hot |
| SQLite workers | `10` | `1.00%` | mostly idle |
| Main/restream thread | `1` | `1.80%` | low-duty |
| SRT timestamp/playback | `1` | `0.40%` | low-duty |
| SRT garbage collector | `1` | `0.00%` | idle |
| Tracing file appender | `1` | `0.00%` | idle |

Thread CPU was concentrated in two Tokio workers plus the shared SRT muxer
threads:

| Thread | Hot sample |
|---|---:|
| `tokio-rt-worker` tid `611213` | `43.80%` CPU |
| `tokio-rt-worker` tid `611212` | `43.80%` CPU |
| `SRT:RcvQ:w2` | `16.80%` CPU |
| `SRT:SndQ:w2` | `8.60%` CPU |
| `SRT:RcvQ:w1` | `2.20%` CPU |

Affinity-partitioned process counters over 15 s:

| Metric | Arena-cap unpinned | Arena-cap partition probe |
|---|---:|---:|
| MediaMTX ready | `1200/1200` | `1200/1200` |
| MediaMTX bytes delta | `169,653,485` over 3 s | `220,513,390` over 3 s |
| CPU utilized | `2.600` CPUs | `2.458` CPUs |
| IPC | `0.374` | `0.350` |
| Cache misses | `18.37%` | `19.00%` |
| Branch misses | `8.50%` | `8.88%` |
| Context switches | `5.495 K/sec` | `6.292 K/sec` |
| CPU migrations | `712.867/sec` | `553.133/sec` |
| Page faults | `76.467/sec` | `0.067/sec` |

Conclusion: coarse partitioning is plausible but not proven. It improved CPU
and migrations in this short sample while worsening IPC, cache misses, branch
misses, and context switches. Do not add internal affinity yet. If Q-012 moves
to code, it should first run a clean default-runtime A/B and then design
ownership-aware placement: derive partitions from the effective CPU mask and
pin only long-lived, high-duty thread families (Tokio workers and shared SRT
muxers), with an explicit opt-in and concurrency proof gates.

Raw artifacts:

- `.local/artifacts/msr-arena2-3031-20260712T161905Z/affinity-probe/original-affinity.txt`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/affinity-probe/mediamtx-proof-before-perf.json`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/affinity-probe/mediamtx-proof-after-perf.json`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/affinity-probe/perf-stat-partitioned.csv`
- `.local/artifacts/msr-arena2-3031-20260712T161905Z/affinity-probe/pidstat-partitioned.txt`

### Clean MSR thread-affinity partition probe - 2026-07-12 (local)

Follow-up to remove the allocator-cap caveat from the exploratory probe above.
This run used the normal restored runtime environment: no `MALLOC_ARENA_MAX`,
no `RESTREAM_TOKIO_WORKER_THREADS`, bench-profile binaries, and 1,200-output
loopback MediaMTX sink. The only changed variable during the second sample was
external, reversible `taskset` placement: SRT helper threads on CPUs `0-1`,
all other Restream threads on CPUs `2-5`, then restored to the original masks.
No runtime code changed.

MediaMTX receiver proof and log hygiene:

| Check | Default scheduler | Partition probe |
|---|---:|---:|
| MediaMTX ready before perf | `1200/1200` | `1200/1200` |
| MediaMTX bytes delta before perf | `183,179,690` over 3 s | `176,024,437` over 3 s |
| MediaMTX ready after perf | `1200/1200` | `1200/1200` |
| MediaMTX bytes delta after perf | `201,126,121` over 3 s | `152,000,730` over 3 s |
| Restream warn/error/panic lines | `0` | `0` |

Thread census on the clean default sample:

| Thread group | Threads | 5 s CPU sample | What it does |
|---|---:|---:|---|
| Tokio runtime/blocking-pool | `64` | `93.20%` | async runtime plus mostly idle blocking-pool threads; two hot workers |
| SRT receive queue | `2` | `16.40%` | libsrt receive workers for shared SRT sockets/muxers |
| SRT send queue | `2` | `7.80%` | libsrt send workers for SRT egress traffic |
| SQLite workers | `10` | `1.00%` | SQLx SQLite worker pool, mostly idle |
| Main/restream thread | `1` | `1.20%` | process main thread, low-duty after startup |
| SRT timestamp/playback | `1` | `0.60%` | libsrt timestamp/playback delivery worker |
| SRT garbage collector | `1` | `0.00%` | libsrt helper |
| Tracing file appender | `1` | `0.00%` | async log/file appender |

Hot threads on the default sample were two Tokio workers (`42.20%`, `41.80%`),
one SRT receive worker (`14.40%`), one SRT send worker (`7.80%`), one mild
Tokio worker (`2.20%`), and one mild SRT receive worker (`2.00%`). The
important shape is not 64 busy Tokio workers; it is two hot Tokio workers plus
two hot shared SRT queue workers and many idle helper threads.

Process counters over matching 15 s `perf stat -p` windows:

| Metric | Default scheduler | Partition probe |
|---|---:|---:|
| CPU utilized | `2.321` CPUs | `2.051` CPUs |
| IPC | `0.336` | `0.420` |
| Cache misses | `20.80%` | `16.25%` |
| Branch misses | `8.73%` | `8.53%` |
| Context switches | `7.663 K/sec` | `4.330 K/sec` |
| CPU migrations | `920.333/sec` | `288.533/sec` |
| Page faults | `7.200/sec` | `58.733/sec` |

Conclusion: ownership-aware CPU partitioning is the best remaining performance
lead from the MSR thread work. On a clean default run it reduced CPU by about
`0.27` cores, cut migrations by about `69%`, reduced context switches by about
`43%`, and improved IPC/cache-miss rate while MediaMTX still received all
1,200 streams. It should still not become unconditional runtime behavior from
this probe alone: internal affinity would change thread lifecycle/concurrency
semantics, must derive masks from the effective CPU set/cgroup quota, needs
clear ownership of which code creates each long-lived thread family, and needs
an opt-in plus concurrency proof gates. The next Q-012 step should design that
opt-in placement boundary; it is no longer a blind investigation.

Raw artifacts:

- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/mediamtx-proof-default-before-perf.json`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/mediamtx-proof-default-after-perf.json`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/perf-stat-default.csv`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/pidstat-default.txt`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/thread-census-default.txt`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/affinity-probe/original-affinity.txt`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/affinity-probe/mediamtx-proof-partition-before-perf.json`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/affinity-probe/mediamtx-proof-partition-after-perf.json`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/affinity-probe/perf-stat-partitioned.csv`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/affinity-probe/pidstat-partitioned.txt`
- `.local/artifacts/msr-affinity-clean-3030-20260712T163159Z/affinity-probe/thread-census-partitioned.txt`

### Negative result: in-process runtime affinity scanner - 2026-07-12 (local)

Follow-up to the clean external `taskset` result above. A Linux-only,
environment-gated prototype scanned `/proc/self/task` once per second and
applied the same partition internally: `SRT:*` threads on CPUs `0-1`, all other
Restream threads on CPUs `2-5`. The prototype was tested, then reverted before
commit because it did not reproduce the clean external A/B win.

Correctness and mask proof:

| Check | Result |
|---|---:|
| MediaMTX ready before perf | `1200/1200` |
| MediaMTX bytes delta before perf | `202,797,567` over 3 s |
| MediaMTX ready after perf | `1200/1200` |
| MediaMTX bytes delta after perf | `168,496,569` over 3 s |
| Runtime log | affinity enabled with `allowed=0,1,2,3,4,5`, `srt=0,1`, `other=2,3,4,5` |
| Thread masks | `SRT:*` on `0-1`; Tokio/SQLite/main/tracing on `2-5` |

Process counters:

| Metric | Clean default scheduler | External partition probe | In-process scanner |
|---|---:|---:|---:|
| CPU utilized | `2.321` CPUs | `2.051` CPUs | `2.450` / `2.419` CPUs |
| IPC | `0.336` | `0.420` | `0.352` / `0.343` |
| Cache misses | `20.80%` | `16.25%` | `20.60%` / `20.89%` |
| Context switches | `7.663 K/sec` | `4.330 K/sec` | `7.749` / `7.978 K/sec` |
| CPU migrations | `920.333/sec` | `288.533/sec` | `643.333` / `669.467/sec` |
| Page faults | `7.200/sec` | `58.733/sec` | `0.200` / `0.267/sec` |

Conclusion: correct masks are not enough. The first runtime scanner preserved
receiver health and applied the desired placement, but its process counters
were closer to the default scheduler than to the external partition win. The
code was reverted. Keep systemd/service-level placement guidance, but do not
land in-process pinning until the implementation explains and reproduces the
external A/B result.

Raw artifacts:

- `.local/artifacts/msr-runtime-affinity-3030-20260712T164945Z/mediamtx-proof-before-perf.json`
- `.local/artifacts/msr-runtime-affinity-3030-20260712T164945Z/mediamtx-proof-after-perf.json`
- `.local/artifacts/msr-runtime-affinity-3030-20260712T164945Z/thread-census.txt`
- `.local/artifacts/msr-runtime-affinity-3030-20260712T164945Z/perf-stat-runtime-affinity.csv`
- `.local/artifacts/msr-runtime-affinity-3030-20260712T164945Z/perf-stat-runtime-affinity-second.csv`
- `.local/artifacts/msr-runtime-affinity-3030-20260712T164945Z/pidstat-runtime-affinity.txt`

### Final MSR full ramp after performance series - 2026-07-12 (local)

Bench-profile binaries rebuilt from committed source at `7587fdb`, no allocator
override, no runtime affinity override. Command shape:

```sh
MSR_FULL=1 MSR_SAMPLE_SECS=20 MSR_SAMPLE_INTERVAL_MS=5000 \
  MSR_SINK_SAMPLE_SECS=3 MSR_NO_CLEANUP=1 BENCH_BUILD=never \
  WORK_DIR=.local/artifacts/msr-final-full-20260712T165925Z \
  scripts/harness/run.sh msr -- --no-netns
```

Status: `PASS`. Every checkpoint included paginated MediaMTX `/v3/paths/list`
proof that all expected paths were `ready=true` and aggregate
`bytesReceived` grew across the sink sample window. Restream and harness logs
had zero warn/error/panic lines.

| Outputs | Egress mix | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 30 | `rtmp:29,srt:1` | `30/30` | `4,665,974` | 17.27 | 23.54 | 92.0 MB | 32 KB | 4 |
| 120 | `rtmp:114,srt:6` | `120/120` | `15,540,301` | 43.28 | 53.09 | 119.4 MB | 256 KB | 4 |
| 300 | `rtmp:285,srt:15` | `300/300` | `46,554,165` | 63.78 | 80.36 | 152.6 MB | 864 KB | 4 |
| 600 | `rtmp:570,srt:30` | `600/600` | `76,622,802` | 90.06 | 98.45 | 207.1 MB | 1.5 MB | 4 |
| 900 | `rtmp:855,srt:45` | `900/900` | `155,214,230` | 114.87 | 141.06 | 263.1 MB | 2.3 MB | 4 |
| 1200 | `rtmp:1140,srt:60` | `1200/1200` | `208,072,764` | 126.87 | 131.90 | 329.5 MB | 3.3 MB | 4 |

Independent 1,200-output receiver proof around a 15 s process-mode
`perf stat -p <restream-pid>` attach:

| Check | Result |
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

Final thread census was still the expected MSR shape: `82` Restream threads,
including `64` Tokio runtime/blocking-pool-named threads where only two were
hot, `2` SRT receive queue workers, `2` SRT send queue workers, `10` SQLite
workers, and single main/SRT timestamp/SRT GC/tracing appender threads.

Raw artifacts:

- `.local/artifacts/msr-final-full-20260712T165925Z/msr.json`
- `.local/artifacts/msr-final-full-20260712T165925Z/msr-results.json`
- `.local/artifacts/msr-final-full-20260712T165925Z/msr-samples.jsonl`
- `.local/artifacts/msr-final-full-20260712T165925Z/msr-report.md`
- `.local/artifacts/msr-final-full-20260712T165925Z/mediamtx-proof-final-before-perf.json`
- `.local/artifacts/msr-final-full-20260712T165925Z/mediamtx-proof-final-after-perf.json`
- `.local/artifacts/msr-final-full-20260712T165925Z/perf-stat-final.csv`
- `.local/artifacts/msr-final-full-20260712T165925Z/pidstat-final.txt`
- `.local/artifacts/msr-final-full-20260712T165925Z/thread-census-final.txt`
- `.local/artifacts/msr-final-full-20260712T165925Z/restream-smaps-rollup-final.txt`

### Tokio blocking cap visibility and cap-32 negative result - 2026-07-12 (local)

Follow-up to the MSR thread census. The runtime now exposes resolved Tokio
runtime sizing in the startup summary and `hostSettings`:
`runtime.tokio.worker_threads` and `runtime.tokio.max_blocking_threads`.

Short proof command shape:

```sh
RESTREAM_TOKIO_MAX_BLOCKING_THREADS=32 \
  MSR_OUTPUT_COUNTS=1200 MSR_SAMPLE_SECS=8 MSR_SAMPLE_INTERVAL_MS=4000 \
  MSR_SINK_SAMPLE_SECS=2 BENCH_BUILD=never \
  WORK_DIR=.local/artifacts/msr-blocking-cap32-visible-20260712T152209Z \
  scripts/harness/run.sh msr -- --no-netns
```

Result: `PASS`, with MediaMTX `1200/1200` ready and `139.4 MB`
`bytesReceived` growth. Health and the startup summary both proved the child
process resolved `workerThreads=2` and `maxBlockingThreads=32`, but the live
thread census still reached `85-86` total Restream threads and `66-68`
`tokio-rt-worker`-named threads. The cap-32 run was also worse than the final
uncapped 1,200-output checkpoint (`135.93%` average CPU and `421,892 KiB` RSS
peak versus `126.87%` and `329.5 MB`), so lowering the default blocking cap is
rejected for now.

Raw artifacts:

- `.local/artifacts/msr-blocking-cap32-visible-20260712T152209Z/msr.json`
- `.local/artifacts/msr-blocking-cap32-visible-20260712T152209Z/msr-report.md`
- `.local/artifacts/msr-blocking-cap32-visible-20260712T152209Z/health-tokio.json`
- `.local/artifacts/msr-blocking-cap32-visible-20260712T152209Z/thread-watch.txt`
- `.local/artifacts/msr-blocking-cap32-visible-20260712T152209Z/thread-census-live.txt`

### Tokio thread-name attribution probe - 2026-07-12 (local)

A temporary bench-profile probe renamed Restream's Tokio runtime threads with an
`rs-tokio-*` prefix and reran a short 1,200-output MSR checkpoint. Result:
`PASS`, MediaMTX `1200/1200`, and `133.2 MB` `bytesReceived` growth. The live
census no longer showed default `tokio-rt-worker`; the large idle thread family
belonged to Restream's main Tokio runtime. Tokio reused the two scheduler worker
identities across replacement/blocking threads, so the committed runtime label is
the fixed group name `restream-tokio`, not a misleading unique suffix.

Raw artifacts:

- `.local/artifacts/msr-thread-name-census-20260712T152953Z/msr.json`
- `.local/artifacts/msr-thread-name-census-20260712T152953Z/msr-report.md`
- `.local/artifacts/msr-thread-name-census-20260712T152953Z/thread-watch.txt`
- `.local/artifacts/msr-thread-name-census-20260712T152953Z/thread-census-live.txt`

### Tokio blocking keepalive negative result - 2026-07-12 (local)

A temporary prototype exposed `RESTREAM_TOKIO_THREAD_KEEP_ALIVE_MS` and set the
Tokio runtime blocking-thread keepalive to `100 ms` for a short 1,200-output
MSR checkpoint. The harness reported `PASS`, and MediaMTX had `1200/1200`
paths ready with `141.1 MB` aggregate `bytesReceived` growth, but the thread
family did not shrink and the run was heavier than the final uncapped baseline.

| Metric | Final uncapped baseline | Keepalive `100 ms` prototype |
|---|---:|---:|
| MediaMTX ready | `1200/1200` | `1200/1200` |
| MediaMTX bytes delta | `208,072,764` over 3 s | `141.1 MB` over 2 s |
| CPU average | `126.87%` | `146.9%` |
| CPU peak | `131.90%` | `157.0%` |
| RSS peak | `329.5 MB` | `429 MB` |
| Restream threads | `82` | `82` |
| Tokio-named threads | `64` | `64` |

Conclusion: keepalive tuning is rejected. The 64-ish Tokio-named threads are
Tokio-owned, but this result argues against them being simple idle blocking
threads that can be reclaimed by reducing keepalive. The next useful proof is
per-thread attribution (`tid`, `comm`, `wchan`, CPU deltas, and stack/perf
samples) during a live MSR checkpoint.

Raw artifacts:

- `.local/artifacts/msr-tokio-keepalive100-20260712T153747Z/msr.json`
- `.local/artifacts/msr-tokio-keepalive100-20260712T153747Z/msr-report.md`
- `.local/artifacts/msr-tokio-keepalive100-20260712T153747Z/health-tokio.json`
- `.local/artifacts/msr-tokio-keepalive100-20260712T153747Z/thread-watch.txt`
