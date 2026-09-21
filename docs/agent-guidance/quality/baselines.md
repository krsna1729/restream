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

### WI3.4 packet-rate contract — SRT fanout ladder (2026-09-21)

The contract lives in
[the SRT/Compio roadmap](../../srt-compio-roadmap.md#10-wi34--establish-the-packet-rate-benchmark-contract).
Both 100-output rungs below are **local-host evidence, not a no-loss baseline**:
the sink peer ran in the same 6 vCPU host, and its kernel receive-buffer drops
are visible in each artifact's `validity` verdict.

| Rung | Workload | Verdict | CPU avg/peak | RSS peak | TX datagrams/s | DATA pps/output | retransmits/s | control pps | µs CPU/pkt | ready depth hwm | kernel rcvbuf errors/s |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 outputs (`744602b6`) | 9.32 Mbps fixture (labelled 8M, since regenerated) | contaminated | 191.7% / 220.4% | 176 MB | 106,446 | 882.5 | 2,162 | 16,033 | 18.8 | 182 | 2,001 |
| 100 outputs (`ccc29614` + contract fixes) | 8.01 Mbps fixture | contaminated | 218.6% / 260.2% | 174 MB | 89,109 | 756.6 | 1,004 | 12,445 | 25.4 | 392 | 1,308 |

What these numbers say:

- The fixture fix is visible end to end: with the regenerated 8.0 Mbps fixture
  (`tests/fixtures.rs` asserts the effective rate) the measured
  first-transmission DATA rate is **756.6 packets/s/output** against the
  roadmap's ~760, while the earlier row's 882.5 was the mislabelled 9.32 Mbps
  fixture — which is why that rung is reclassified instead of kept as the
  baseline.
- Neither rung is healthy. Kernel UDP receive-buffer errors (mean 1,308/s, peak
  3,789/s in the corrected rung) plus `ownerServiceBudgetExhausted` make the
  verdict `contaminated`; retransmissions (~2% of first-transmission DATA in the
  corrected rung) follow from the same peer-side pressure. A healthy rung needs
  the sink peer on another host (`srt-sink` +
  `RESOURCE_SWEEP_SRT_PEER_HOSTS`) or the ≥25 GbE multi-host environment §9.1
  requires, so no contractual baseline is recorded yet.
- The Owner TX pool reached full occupancy (16 in flight) in both rungs while
  `txExhaustions` stayed 0: that is "full-pool occupancy observed", not evidence
  that the pool is the first bottleneck.
- The earlier 300-output rung stays **invalid** rather than merely contaminated:
  ~615 k/s kernel receive-buffer errors mean the sink peer was saturated, so its
  packet rates were retransmission-dominated and are not recorded here.
- `cycles/packet` is unmeasurable on this host (no PMU); `cpuMicrosPerSrtPacket`
  is the stand-in. Scheduler wake rate, SQEs/submission and io_uring enters/s
  remain unsourced — each is named in the artifact's `unavailable` block.

Artifacts: `.local/artifacts/wi34-rung-100/` (corrected rung, with
`packet-contract.json` carrying the git SHA, workload/peer environment and the
per-rung validity verdict), `.local/artifacts/wi34-ladder/100-300/` (the
earlier 100/300 rungs and the drop evidence).

### WI3.4 remote-peer shakedown — 100 outputs, one host (`f45e8596`, 2026-09-21)

The first run through the finished contract machinery: sink peer as a separate
process (`srt-sink`, 4 acceptors, 32 MB socket buffers), measuring host
retargeted at it with `RESOURCE_SWEEP_SRT_PEER_HOSTS=127.0.0.1` and pinned
`MTX_SRT`/`MTX_API`, one isolated rung of 100 outputs, 12 s settle + 12 s
sample. 8 rated samples, **12.96 s common rated window**, clean tree, bench
provenance matching HEAD.

| Signal | Value |
|---|---|
| Runtime verdict | `contaminated` (`baselineEligible: false`) |
| Peer delivered payload | 78.3 MB/s for 100 outputs (expected 100 MB/s at 8 Mbps) |
| Peer UDP receive-buffer drops | 68–4 263/s |
| Local kernel UDP receive-buffer errors | up to 5 292/s |
| SRT retransmissions | up to 8 400/s (mean 1 144/s) |
| First-transmission DATA | 778.4 pps/output mean (within the 8 Mbps band) |
| CPU | 22.5 µs per SRT datagram (mean) |

What it shows: the contract now measures what it claims to. Priming gives every
sample a rated interval and a 12.96 s common window; the peer's own delivered
payload (78 % of the workload) is what flagged under-delivery, while the
locally measured send rate (778 pps/output) sat inside the band — the
peer-side check catches exactly what a send-side check cannot. Drops,
retransmissions and the workload shortfall are all in the verdict's reasons, so
no baseline was recorded: one host's loopback sink cannot carry 100 × 8 Mbps
losslessly, which is what the multi-host ≥25 GbE rungs are for.

Operational finding recorded in §10.1: `MTX_SRT`/`MTX_API` must be pinned to the
peer's `SRT_SINK_PORTS`/`SRT_SINK_STATE_PORT`; without them the harness
synthesizes per-process ports and every output dies on
`handshake attempt deadline`. A six-acceptor sink also failed to drain on this
host while one and four acceptors worked — worth re-checking before the
multi-host runs.

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

Evidence already on record (2026-07-12 VPS/local MSR series, detailed in
[archived profiling notes](../../archive/quality/baselines-profiling-2026-07.md)
and the archived journal Q-012 entries in
[journal-2026-07.md](../../archive/quality/journal-2026-07.md)):

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
