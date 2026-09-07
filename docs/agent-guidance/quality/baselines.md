# Performance & Resource Baselines

Durable measurement ledger for perf-sweep. Criterion's `target/criterion/`
state is scratch; this file is the source of truth for "did we regress".

Rules: measurements are serial (idle host, kill-check first), bench profile
only, recorded with date + commit. Update a row only with fresh numbers from
this machine; never copy numbers you did not measure. Historical sections are
reference points — do not overwrite them, add new dated rows.

## Contents

- [Benchmark ledger (Criterion medians)](#benchmark-ledger-criterion-medians)
- [Resource ledger (resource-sweep / scale runs)](#resource-ledger-resource-sweep-scale-runs)
- [Standing optimization targets (2026-06-27 CPU profile, task-clock 999 Hz)](#standing-optimization-targets-2026-06-27-cpu-profile-task-clock-999-hz)
- [Archived campaign and profiling notes](#archived-campaign-and-profiling-notes)

## Benchmark ledger (Criterion medians)

| Suite | Metric | Median | Noise ± | Commit | Date | Last verified |
|---|---|---|---|---|---|---|
| ring_buffer | `ring_buffer/consumer/pull_burst/8` | 1.951 µs (4.10 Melem/s) | ±0.4% | 52428c2b | 2026-07-18 | 2026-07-18 |
| avio_throughput | `memory_queue/write_batch/with_len` | 467 ns (2.62 GiB/s) | ±2% | 52428c2b | 2026-07-18 | 2026-07-18 |
| high_performance_data_path | `data_path/mpegts_demux_drain/reuse_then_consume` | 672 µs (9.26 GiB/s) | ±7% (see note) | 52428c2b | 2026-07-18 | 2026-07-18 |
| high_performance_data_path | `data_path/burst_mux_write/batch_mux_into_write` (Q-009 after) | 10.06 µs (3.18 Melem/s) | ±1% | Q-009 | 2026-07-18 | 2026-07-18 |
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

Seeded 2026-07-18 (Q-003): each of `ring_buffer`, `avio_throughput`, and
`high_performance_data_path` runs dozens of Criterion groups per suite; the
row above records one representative low-variance headline group per suite
rather than every group, matching this table's one-row-per-suite shape.
Three clean serial `scripts/build/resource-limit.sh cargo bench --profile
bench --bench <name>` runs per suite on an idle host (`pgrep -x
restream/mediamtx/ffmpeg` all empty); Median is the median of the three
per-run medians, Noise ± is the spread across those three runs.
`high_performance_data_path`'s `mpegts_demux_drain/reuse_then_consume`
showed a monotonic warm-to-fast drift across the three runs (8.39 → 9.26 →
9.66 GiB/s, ~15% top-to-bottom) rather than random jitter, likely CPU
frequency/cache ramp-up across repeated process invocations on this WSL2
host — noted here rather than silently averaged away, since it exceeds the
±5% default threshold and a future perf-sweep comparison against this row
should expect that much run-to-run spread on this suite specifically.

## Resource ledger (resource-sweep / scale runs)

| Config | RSS | Ring payload | AVIO peak HWM | Blocked writes | Commit | Date |
|---|---|---|---|---|---|---|
| baseline-empty | 75,960 KB | 0 KB | 0 KB | not measured by this harness mode | 39685ea3 | 2026-07-18 |
| ingest-only h264-rtmp | 78,892 KB | 2,470 KB | 0 KB | not measured by this harness mode | 39685ea3 | 2026-07-18 |
| ingest-only h265-srt | 80,752 KB | 2,190 KB | 0 KB | not measured by this harness mode | 39685ea3 | 2026-07-18 |
| egress-growth-source-srt 10-per-group | 104,948 KB | 6,444 KB | 2,125 KB | not measured by this harness mode | 39685ea3 | 2026-07-18 |
| egress-growth-transcode-dual-mixed 10-per-group | 700,156 KB | 6,390 KB | 35,728 KB | not measured by this harness mode | 39685ea3 | 2026-07-18 |

Full 42-case breakdown and dated MSR/resource campaigns live in
[archive/quality/baselines-campaigns-2026-07.md](../../archive/quality/baselines-campaigns-2026-07.md).
VPS/WSL2 profiling dumps live in
[archive/quality/baselines-profiling-2026-07.md](../../archive/quality/baselines-profiling-2026-07.md).

## Standing optimization targets (2026-06-27 CPU profile, task-clock 999 Hz)

| Self % | Symbol | Meaning | Backlog |
|---|---|---|---|
| 3.28% | `__memmove_avx_unaligned_erms` | AVIO buffer → `ts_accum` copy | Q-009 [opus] — addressed 2026-07-18 (see note) |
| 2.60% | `pthread_mutex_lock` | SRT internal + MemoryQueue mutex | (unfiled) |
| 1.18% | `__vdso_clock_gettime` | per-packet SRT latency tracking | (unfiled) |
| 0.87% | `_int_malloc` | per-packet `Arc::new(MediaPacket)` | Q-010 [opus] — rejected 2026-07-18 (see note) |
| 0.43% | `VecDeque::extend` | AVIO queue write (second copy) | Q-009 [opus] — addressed 2026-07-18 (see note) |

### Q-009 result — AVIO→TsMux copy elimination (2026-07-18)

The 2026-06-27 profile predates the pure-Rust `TsMuxer` rewrite; the two-copy
shape it named (FFmpeg AVIO output buffer → `ts_accum`) now lives as the
muxer's internal `output: Vec<u8>` scratch → per-packet `extend_from_slice`
into the SRT egress burst accumulator (`srt_egress.rs::start_shared_ts_muxer`).
Q-009 removes that copy: `TsMuxer::mux_packet_into` / `mux_packet_by_stream_idx_into`
append TS packets directly into the caller's accumulator, and the egress feeder
freezes the accumulator with an O(1) `Bytes::from(Vec)` ownership transfer
instead of `BytesMut::freeze()`. The standalone `mux_packet` API is preserved
(via `mem::take` of the internal scratch) for the ~30 remaining single-packet
callers, so no correctness contract moved.

Microbenchmark (`high_performance_data_path`, WSL2, idle host, 100 samples;
contabo unavailable — a ~2-day external MSR workload was live, kill-check
non-empty, not this session's to kill):

| Variant | Median | Throughput |
|---|---|---|
| `batch_accumulate_write` (before: `mux_packet` + `extend_from_slice`) | 10.767 µs | 2.972 Melem/s |
| `batch_mux_into_write` (after: `mux_packet_into`) | 10.062 µs | 3.180 Melem/s |

−6.5% latency / +7.0% throughput per burst, non-overlapping 95% CIs
([10.66, 10.89] vs [9.97, 10.16] µs). Core mux path unchanged within noise
(`data_path/mpegts_mux/mux_all_packets` 478 µs, 12.6 GiB/s). `cargo test --lib
mpegts` (74) and `--lib srt` (91) green.

### Q-010 result — per-packet `Arc<MediaPacket>` pooling REJECTED (2026-07-18)

Decision: do not add a slab/pool allocator for `MediaPacket`; the
`Arc::new(MediaPacket)` allocation stays. No runtime code changed. Evidence:

Bench shape — `RingBuffer` slots hold `ArcSwapOption<MediaPacket>`; `push` /
`push_batch` do `Arc::new(packet)` then `.store()` (`ring_buffer.rs:489,519`),
and readers `load_full()` obtain an `Arc<MediaPacket>` with an unbounded
lifetime. `MediaPacket` is 56 B (`repr(C)`); with the 16-B Arc control block
each allocation is a 72-B request, served from glibc's per-thread `tcache`
(lock-free, O(1) fast path). The payload is a separately ref-counted `Bytes`,
untouched by any packet pool.

Microbenchmark (`ring_buffer/producer`, WSL2, idle host, kill-check clean,
100 samples; the whole-`push` time below includes the `Arc::new` allocation
plus the `ArcSwap` store):

| Variant | Median (burst) | Per-element | Throughput |
|---|---|---|---|
| `push_one_at_a_time/1` | 142.05 ns | 142 ns | 7.04 Melem/s |
| `push_batch/1` | 145.71 ns | 146 ns | 6.86 Melem/s |
| `push_one_at_a_time/4` | 541.34 ns | 135 ns | 7.39 Melem/s |
| `push_batch/4` | 519.09 ns | 130 ns | 7.71 Melem/s |
| `push_one_at_a_time/8` | 1.086 µs | 136 ns | 7.37 Melem/s |

Per-element push cost is flat (~130–145 ns) from burst 1→8 and `push` ≈
`push_batch` within noise, so batching already amortizes the path and there is
no per-burst allocation win left for a pool to capture. Reasons to reject:

1. Magnitude — the profile put `_int_malloc` at 0.87% self-time; the request
   is a tcache-served 72-B size class, already a lock-free O(1) fast path. A
   pool's best case only replaces an already-fast path.
2. Ownership — reclamation is intrinsically last-reader-drop: readers hold the
   `Arc` across await points, threads, and arbitrary time. `Arc` + the global
   allocator already implement exactly that (last `Arc` drop frees to tcache).
   A custom slab must replicate the same last-drop hook *plus* a synchronized
   cross-thread freelist, because producers (SRT/RTMP ingest threads) and
   consumers (egress/HLS/recording tasks) run on different threads — every
   reclaim becomes a cross-thread free contending on the pool lock, versus
   tcache handling the common same-thread free lock-free. It trades a
   lock-free path for a contended one on a path the profile says is cold.
3. Safety — a slot-reusing pool risks use-after-free / ABA if any reader
   outlives the intended lifetime, violating the "no failure path may crash the
   engine" invariant. `Arc` makes that impossible by construction.

A documented rejection is a valid completion for this item (backlog Q-010).

### Q-012 decision — CPU affinity is a process/cgroup concern, not a runtime feature (2026-07-18)

Decision: do not add in-process thread-family CPU pinning. CPU partitioning is
supported at the process/cgroup layer only (systemd `CPUAffinity`, Docker
`--cpuset-cpus`, Kubernetes CPU manager); the operator guidance lives in
`docs/configuration.md` § Linux Service Placement. No runtime affinity code
exists or is added. This closes Q-012, which the 2026-07-12 series had narrowed
to "an opt-in runtime affinity design."

Evidence already on record (2026-07-12 VPS/local MSR series, detailed in the
Profiling-notes sections below and the journal Q-012 entries):

| Config | CPU (cores) | IPC | Cache misses | Ctx switches | Migrations |
|---|---|---|---|---|---|
| Default runtime (clean MSR) | 2.321 | 0.336 | 20.80% | 7.663 K/s | 920.3/s |
| External `taskset` partition (SRT→CPU 0-1, other→2-5) | 2.051 | 0.420 | 16.25% | 4.330 K/s | 288.5/s |
| In-process scanner, same masks proven applied | 2.45 / 2.42 | — | ~20.6–20.9% | ~7.7–8.0 K/s | — |

The external partition is a real win; the in-process scanner did not reproduce
it *despite a thread census proving the masks were applied*. Related runtime
knobs were also probed and rejected: Tokio blocking cap-32 (worse CPU/RSS),
`restream-tokio` thread-name label (kept, neutral), and a Tokio keepalive knob
(no thread-family shrink, worse CPU/RSS).

Why the process/cgroup layer is correct and in-process pinning is not:

1. Robustness — the scanner is a one-shot `/proc/self/task` pass, but the Tokio
   runtime continuously spawns replacement/blocking threads (census showed a
   `restream-tokio` family in the 60s over a run with only two hot scheduler
   worker identities). New threads inherit their creator's mask at clone time,
   so a one-shot partition erodes as the thread population turns over. A
   process-level cpuset is enforced by the kernel on every present and future
   thread for the whole process lifetime — no scanner can match that.
2. Layering / container-awareness — a cpuset derives from the effective CPU
   mask/cgroup quota automatically and is honored inside containers; in-process
   host-CPU masks are not container-aware and would fight orchestration.
3. Cost/benefit — the win depends on a clean default run (no allocator cap, no
   worker override) and holding the partition for the whole window; that is
   exactly what a launch-time cpuset provides for free, and exactly what
   fragile in-process re-pinning cannot guarantee. Adding thread-lifecycle
   placement code (with its concurrency-proof burden) to chase an effect the
   supported layer already captures is negative-value.

No new measurement was run for this decision: WSL2 has no PMU, and the Contabo
VPS carried a ~2-day external MSR workload (kill-check non-empty, not this
session's to kill). The decision rests on the recorded in-process-scanner
negative plus the robustness/layering argument; re-running the scanner would
only re-confirm the negative. A documented rejection is a valid completion.


## Archived campaign and profiling notes

Dated MSR/resource campaign write-ups:
[baselines-campaigns-2026-07.md](../../archive/quality/baselines-campaigns-2026-07.md).

VPS and WSL2 profiling dumps:
[baselines-profiling-2026-07.md](../../archive/quality/baselines-profiling-2026-07.md).
