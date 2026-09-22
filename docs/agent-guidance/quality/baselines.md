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
no baseline was recorded.

Scope of that conclusion: it says **this 6-vCPU host cannot carry 100 × 8 Mbps
losslessly when the sink runs on the same box** (the sink competes for the same
cores as restream). It does not say 100 outputs need a 25 GbE path: 100 ×
8 Mbps is ~0.8 Gbit/s of payload, well inside a 10 GbE-class link, and the
roadmap's ≥25 GbE requirement is specifically the 1000-output qualification
environment (§9.1). A separate peer host on a ≥10 GbE path is the right next
step for this rung.

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

### WI3.4A local reference lane — veth + CPU partitioning (measurement tree `a4024c64`; ledger commit `f72387ed`, 2026-09-21)

Measured tree `a4024c64` (clean, bench provenance matching that SHA); the
numbers below were recorded in the ledger by `f72387ed`, which is
documentation-only.

Single-host development lane: sink process in its own network namespace over a
veth pair (`scripts/harness/veth-topology.sh`), restream pinned to CPUs 0-2 and
the sink to 3-5, both masks recorded from the processes themselves. Clean tree,
bench provenance matching HEAD.

| Rung | Common window | Rated samples | Peer delivery | Peer observed | First-DATA | Verdict |
|---|---:|---:|---:|---:|---:|---|
| 50 outputs | 21.3 s | 15/15 | 49.4 MB/s of 50 MB/s expected (98.9 %) | 21.7 s (coverage 1.02) | 769.7 pps/output | `contaminated` (residual drops 2–143/s, retransmits ≤7/s in single samples) |
| 100 outputs | 20.2 s | 14/14 | 56.7 MB/s of 100 MB/s expected (57 %) | 20.6 s (coverage 1.02) | 686 pps/output | `contaminated` (receiver-limited) |

What this establishes: the lane and the contract work end to end — topology and
observed CPU masks land in the artifact, peer delivery is integrated over the
peer's own intervals, and workload conformance passes at 50 outputs while the
100-output rung is flagged as receiver-limited on this 6-vCPU host. The
receiver is the limiting side somewhere between 50 and 100 × 8 Mbps here, which
is a property of this host, not of the 8 Mbps workload or of a 10 GbE path.

Neither rung is a contractual baseline: the 50-output rung is off-ladder (the
contract's ladder is 100/300/500/1000) and the 100-output rung misses the
workload; both also carry residual no-loss violations. A `healthy`,
ladder-eligible artifact still needs either a host with more CPU headroom or a
cheaper receiver (WI3.5's UDP drain), or the external-host lane (WI3.4B).

### WI3.5 substrate measurement — veth lane, TX-only lane, and the sender-side profile (2026-09-21)

Harness mode `substrate-pps`. All arms: one sender CPU pinned exclusively, harness
on disjoint CPUs, 1000 destinations, preconstructed 1316-byte payload, queue
depth 64, warmup then a measured window whose start and end are taken while the
sender is **parked** (explicit pause barrier), so sender completions and peer
counters describe exactly the same interval. `pps/core` is completed datagrams
divided by the sender thread's own CPU seconds, split into user and system time.
Three arms: `compio` (the completion path the SRT egress uses), `io-uring` (native
ring, `SendMsg` with preconstructed slots; `SUBSTRATE_REAP_MODE=sliding|window`),
and `sendto` (blocking `libc::sendto` in-process — same payload, same
preconstructed destination array, same socket options, same machinery).

**veth lane (end-to-end, `udp-drain` peer on CPUs 3-5).** Preserved as
end-to-end veth lane evidence. veth receive processing materially participates in
this ceiling: with `rps_cpus` clear the sender's core absorbs the peer's receive
path, and with it set the ceiling moves to the receiver instead.

| Arm | pps/core | payload Gbit/s | user / system | Verdict |
|---|---:|---:|---|---|
| compio | 139 853 | 1.472 | — | healthy |
| io-uring (sliding) | 114 061 | 1.201 | — | healthy |
| bare blocking `sendto` (Python convenience check) | 111 467 | 1.174 | — | — |
| compio, `rps_cpus` set on the peer queue | 172 554 | 1.817 | — | receiver-limited |
| io-uring, `rps_cpus` set | 146 420 | 1.541 | — | receiver-limited |

**TX-only lane (`scripts/harness/dummy-lane.sh`, disposable dummy netdev, no peer,
no receiver process).** The netdev's own `tx_packets`/`tx_bytes` reconcile with the
sender's completions one-for-one (window slack allowed), which is what makes this
lane usable for attribution at all; a device whose counters cannot account for the
datagrams is rejected rather than trusted.

| Arm | pps/core | payload Gbit/s | user / system (s) | TX | Verdict |
|---|---:|---:|---|---|---|
| `sendto` | 262 777 | 2.766 | 0.24 / 4.76 (5 s window) | 1.000 packets/completion | healthy |
| compio | 256 896 | 2.704 | 3.20 / 16.80 | 1.000 (20 s) | healthy |
| io-uring sliding | 224 283 | 2.359 | 1.02 / 18.96 | 1.000 | healthy |
| **io-uring window (64 SQEs/enter)** | **322 918** | 3.398 | 0.14 / 19.86 | 1.000 | healthy |

Reading: the veth lane's ceiling is roughly half the TX-only lane's, so veth's
receive path materially participates in it — but even with no peer at all the
sender tops out at ~0.26-0.32 Mpps/core. The submission mechanism is worth ~44 %
between the sliding and batched native arms and ~23 % against a bare `sendto`
loop, while userspace cost collapses to 0.14 s of 20 s for the batched ring: at
this rate the ceiling is the stock IPv4/UDP transmit path, not the submission API.

**Sender-side profile (perf, 997 Hz, CPU-pinned, 8795 samples, folded stacks in
`.local/artifacts/wi3-dummy-prof/`).** On-CPU: the sender is CPU-saturated
(20.00 s of CPU in a 20.00 s window) with 5-13 context switches per window, so
there is no off-CPU story to chase. 88.97 % of samples are inside the `sendto`
syscall path, and the cost is spread rather than concentrated: skb allocation and
header build 19.44 %, neighbour plus device transmit 19.42 %, route lookup 9.72 %,
IP ID selection 5.91 %, dst refcount release 5.41 %, netfilter hooks 0.74 %, qdisc
0.00 %. No single hotspot to fix, no firewall or qdisc tax to remove.

Cross-reference: pinned `srt-rs` `crates/srt-bench/benches/udp_datapath_floor.rs`
(recorded in its `docs/results/scaling-1000/floor-single-core.txt`) measured the
same host class in-process on loopback: a null syscall at 303 ns, Compio with 64
operations in flight at 8.898 us CPU/datagram (166 343 pps), `sendmmsg` batch=16
at 7.611 us (207 574 pps). Those include the loopback receive path on the same
core, which is why they sit above the TX-only figures here; the shape agrees —
per-datagram cost in the microseconds, dominated by the kernel stack, with the
submission API worth tens of percent rather than multiples.

**What this does and does not establish.** It establishes a *sender-side kernel
stack* bound on **the measured host** (6-vCPU Zen-class VM): ~3.1-3.8 us of sender
CPU per datagram, ~0.26-0.32 Mpps/core, with the cost distributed across the
generic IPv4 transmit path. This is deliberately scoped to that host class: **the
measured current host is limited to ~0.3 Mpps/core** — not "Linux UDP is limited
to ~0.3 Mpps/core", which is a cross-host claim deferred to WI3.5B (roadmap
§11.1). It does **not** measure a physical NIC: no DMA, no IRQ/completion
placement, no offloads, no line-rate backpressure, and no claim is made here about
NIC-path cost per datagram. It also does not yet test `sendmmsg`, SQPOLL,
`SEND_ZC` or AF_XDP.

Reference commit for this current-host characterization: `d6145413`. Relative
conclusions (where our own overhead lives, how the architecture parallelizes)
transfer to other hosts; absolute capacity numbers do not, which is why the
modern-P-core rerun is deferred rather than borrowed.

Implication for the roadmap's 2 Mpps/core target: 2 Mpps/core is 0.5 us per
datagram, while the measured kernel transmit path alone costs ~3.1 us on this host
class. A faster submission API cannot close a 6x gap that lives in the packet path
itself; that target needs either a fundamentally cheaper transmit mechanism
(AF_XDP/XDP TX or equivalent kernel bypass) or a different host/kernel
configuration, and the SRT protocol work of WI3.6 adds on top of whichever figure
the substrate finally provides.

### WI3.6 ladder start — common-topology rungs on the veth lane (2026-09-21)

Same topology for every rung: veth lane (`wi3-sink` namespace), one pinned sender
CPU (CPU 0), harness on 1-2, receiver on 3-5, 1316-byte payload, 8 Mbps-equivalent
shape. Stage A is unpaced (substrate instrument); stage D is the product path at
its contract rate.

| Stage | Fanout | Result | Receiver | Verdict |
|---|---:|---|---|---|
| A: raw Compio UDP (veth, unpaced) | 10 dests | **150 350 pps/core** (6.65 us/datagram sender CPU), 1.583 Gbit/s, user 1.68 s / sys 18.32 s | 0 drops, loss 7e-6 | healthy, attributable |
| A: raw Compio UDP (veth, unpaced) | 1000 dests | 139 853 pps/core | 0 drops | healthy |
| D: full Restream SRT egress | 10 outputs | DATA-first 760.85 pps/output, total SRT datagrams 956.29 pps/output, `cpuMicrosPerSrtPacket` mean 55.13 | 14.0 / 14.7 rcvbuf drops per second in 2 of 12 samples, delivery 97.5 % | contaminated |

Two findings that shape the rest of the ladder:

1. **The same-host SRT sink is not provably lossless at any probed fanout.** At 20
   outputs (8 MiB sink buffers) drops ran 93/s and 27/s with 98.5 % delivery; with
   32 MiB buffers, 17/s and 2.2/s with 97.3 %; at 10 outputs, 14.0/s and 14.7/s
   with 97.5 %, plus one retransmission blip (5.68/s). Residual drops of ~0.15 %
   persist even at ten outputs, so "the receiver is provably not the limiter" is
   not demonstrable on this host as configured: it needs more receiver CPU or
   buffers, an external receiver (WI3.4B), or an explicitly documented residual
   threshold. The receiver is the reason the ladder cannot simply be run at the
   product's larger fanouts.
2. **Load regime matters more than the ladder's ordering.** Stage A saturates at
   6.65 us/datagram of sender CPU, while stage D at 10 outputs reports 55 us per
   SRT packet — a paced, wakeup-dominated regime far below saturation. Comparing
   those two numbers directly would attribute pacing overhead to the SRT protocol.
   The ladder must therefore compare stages at matched load (pace stage A to the
   product rate, or measure the SRT stages at saturation on a receiver that can
   absorb it), and report the regime alongside every figure.

Amplification is measurable today and already recorded: at 10 outputs the product
path sends 1.257 total SRT datagrams per first-transmission DATA packet
(956.29 / 760.85), which is the denominator that keeps control and retransmission
traffic from reading as CPU inefficiency.

### WI3.6 receiver apparatus — RPS-partitioned lane, effective buffers, fanout probe (2026-09-21)

Lane amended for sender attribution: sender CPU 0, harness/control CPU 1, receiver
plus peer-side RX processing on CPUs 2-5, with the peer veth's `rps_cpus` set to
the receiver mask (requested `3c`, observed `3c`, recorded in the topology env
file) so veth's peer RX work no longer executes on the measured sender core. The
plain-veth Stage A/D numbers recorded earlier stay exploratory evidence, not
subtraction anchors.

Effective socket buffers are now read back instead of assumed: the sink serves
`requestedRcvbufBytes` plus an `effectiveSocketBuffers` block whose granted values
come from `getsockopt` on a probe socket carrying the same request in the same
namespace. With 32 MiB requested the kernel granted **50 MiB** receive (and 16 MiB
send). Simple socket-buffer undersizing is therefore **not supported by the probe
evidence**; the sink drain path remains the suspected apparatus failure. The probe
is still indirect — it does not read the listener sockets themselves — so the
granted values on the actual listeners remain to be proven from pinned srt-rs
`socket_buffer_stats()` (which records OS-granted buffers from the real sockets)
before that question is closed.

Fanout probe under that lane (8 Mbps per output, 15 s samples, sink on CPUs 2-5):

| Fanout | Rated | Peer delivery | Coverage | Drops / retransmits in the rated window |
|---:|---:|---|---|---|
| 1 | 11/11 | 0.97 of 1 MB/s | 1.02 | rcvbuf errors 48, 50, 88, 45, 64, 347 per second across 6 samples |
| 2 | 12/12 | 1.95 of 2 MB/s | 1.02 | 131, 7.5 per second |
| 4 | 11/11 | 3.94 of 4 MB/s | 1.05 | 41, 18 per second |
| 8 | 11/11 | 7.86 of 8 MB/s | 1.02 | 18, 113 per second |
| 10 | 11/11 | 9.99 of 10 MB/s | 1.02 | 113 per second plus 2.91 retransmissions/s |

**No fanout is lossless, including fanout 1.** At a single 8 Mbps output (~956 SRT
datagrams/s) the receiver overflows its receive buffer in most seconds, so the
apparatus — not capacity — is the limiter: the same lane's cheap UDP drain absorbed
150 000 pps with zero drops and zero receive errors. Delivery still reads 97-99 %
because SRT retransmits the drops, which is precisely why a residual-loss allowance
is not acceptable here: loss recovery changes the protocol cost being attributed.

Classification: the harness `srt-sink` is **not a valid measurement receiver** for
WI3.6. Before stages C and D can price SRT, the receiver must be either the pinned
srt-rs two-process qualification receiver (`compio_shared_owner_qual`, pinned to the
receiver CPUs inside the same namespace) or a fixed harness sink. Stage B
(pre-materialized Owner TX against the cheap UDP drain) does not need SRT receiver
semantics and can proceed immediately.

### WI3.6 Stage A controls — boxed vs no-box Compio (2026-09-21)

RPS-partitioned lane (sender CPU 0, harness CPU 1, receiver plus peer RX on CPUs
2-5), 10 destinations, 1316-byte payload, 20 s window, unpaced (saturation regime).

| Arm | pps/core | us/datagram | payload Gbit/s | user / system (s) | Receiver |
|---|---:|---:|---:|---|---|
| `compio` (frozen WI3.5 arm: `Pin<Box<dyn Future>>` per datagram) | 164 342 | 6.08 | 1.730 | 1.73 / 18.27 | 0 UDP drops, 2 195 NIC rx drops, loss 2.1e-4 |
| `compio-pipeline` (WI3.6 control: homogeneous futures, no per-datagram boxing) | 171 872 | 5.82 | 1.809 | 1.88 / 18.12 | 582 UDP drops, 2 993 NIC rx drops, loss 1.0e-3 |
| `sendto` (blocking `libc::sendto`) | 172 089 | 5.81 | 1.811 | 0.64 / 19.36 | 18 659 UDP drops, 33 712 NIC rx drops, loss 1.5e-2 |

Stage A's own harness allocation is therefore worth **~4 %** (6.08 -> 5.82
us/datagram), not more: an A->B difference larger than that is Owner-side, and the
boxed arm stays frozen as the WI3.5 historical reference.

Lane finding: with RPS redirecting receive processing to CPUs 2-5, the peer's
netdev rx path starts dropping (`nicRxDropped` 2 195-33 712) above ~160 000 pps
even when UDP drops are zero — the RPS backlog, not the socket buffer, is the first
receiver-side ceiling on this lane. Saturation-regime runs must therefore either
raise `net.core.netdev_max_backlog` on the receiver (recording it as lane
configuration) or run below that rate; all three arms above are `unclassified`
under the zero-loss rule for exactly this reason.

### WI3.6 measurement hardening — quiescence barrier, strict delivery, softnet/backlog (2026-09-21)

Boundary correctness: a pause request now means *stop refilling, drain every
already-submitted operation to in-flight zero, then acknowledge*. Previously the
arms acknowledged at the top of the loop with up to `queue_depth - 1` sends still in
flight, so up to 63 datagrams could cross the peer snapshot boundary and be counted
on one side of the window only. The Compio arms drain `FuturesUnordered`, the native
ring reaps until `in_flight == 0`, and the blocking arm is already quiescent when its
last `sendto` returns.

Delivery rule for the ladder is now exact: `SUBSTRATE_DELIVERY_TOLERANCE` defaults to
`0`, requiring `received == completed` plus zero UDP, NIC, application and softnet
drops. The old `<= 0.001` tolerance survives only as an explicitly-set value for
reproducing historical WI3.5 rows.

Receiver-side drop diagnosis, rather than inference from `nicRxDropped`:

- The drain peer now serves a `softnet` block (`processed`, `dropped`,
  `timeSqueeze`, `receivedRps`, `flowLimit`) from `/proc/net/softnet_stat`, and the
  substrate artifact records its deltas; a non-zero `dropped` or `flowLimit` now
  blocks attribution.
- `netdev_max_backlog` is **not namespaced** on this kernel (the peer namespace
  exposes only per-net `net.core` entries), so the lane knob is host-wide:
  `veth-topology.sh` saves the original, sets the requested value, reads it back,
  records all three in the env file, and `down` restores the original.

Measured effect (10 destinations, 20 s, cheap drain, quiescence barrier active):

| Arm | backlog | pps/core | us/datagram | NIC rx drops | UDP rcvbuf drops |
|---|---:|---:|---:|---:|---:|
| `compio-pipeline` | 1 000 000 | 185 429 | 5.39 | 0 | 24 991 (0.68 %) |
| `sendto` | 1 000 000 | 171 549 | 5.83 | 0 | 8 209 (0.24 %) |

So the backlog hypothesis holds for the *netdev* drops — with 1 000 000 the
`nicRxDropped` counter is zero — and the lane's ceiling moved to the **drain socket**
instead (185 kpps at 2 drain threads sharing CPUs 2-5 with the RPS softirq). Neither
row is lossless yet, so no A/B attribution is drawn from them; the earlier
"boxing = ~4 %" reading stays exploratory, as does "the RPS backlog is the first
ceiling" — the counters now say which stage dropped, and the answer depends on the
configuration.

### WI3.6 measurement fence + clean Stage-A repeats (2026-09-21)

Two-sided fence, now proven end to end:

- **Sender quiescence** (previous change) drains in-flight operations to zero before
  acknowledging a pause.
- **Receiver settlement** (new): after each sender quiescence boundary the harness
  polls the peer until its datagram delta equals the sender's completed count —
  warmup delta against `warm_completed`, window delta against `completedInWindow` —
  and returns immediately as loss if any UDP/NIC/application/softnet drop counter
  increments. The rated clock and the sender CPU sample stop at sender quiescence,
  so settlement time is never charged to the sender. `settlement.window.outcome`
  must be `settled` for a run to be attributable.
- The live `/state` now publishes the `softnet` block the gate reads (it previously
  appeared only in the drain's exit JSON, so a clean run could never become
  attributable).

Diagnostic receiver hardened so it cannot become the benchmark bottleneck: per-thread
cache-line-separated counters with per-thread datagram counts and granted
`SO_RCVBUF` per socket in `/state`, `recvmmsg` batches of 32 (`UDP_DRAIN_BATCH`),
`recv` in the single-datagram path, four drain threads on CPUs 2-5, and the interval
log now divides each delta by the interval since the previous tick rather than by
total uptime. `netdev_max_backlog` is restored on every exit path, independently of
whether the namespace still exists.

**Clean Stage-A repeats** (10 destinations, 20 s, RPS lane, backlog 1 000 000,
4 drain threads, exact `received == completed`, zero UDP/NIC/softnet drops):

| Arm | Clean runs | pps/core median (min-max) | us/datagram median (min-max) |
|---|---:|---:|---:|
| `compio` (frozen, `Pin<Box<dyn Future>>` per send) | 3/3 | 175 026 (174 332-175 971) | 5.71 (5.68-5.74) |
| `compio-pipeline` (homogeneous futures, no boxing) | 2/3 | 171 973 (168 364-175 581) | 5.82 (5.70-5.94) |
| `sendto` (blocking `libc::sendto`) | 3/3 | 176 569 (156 956-178 215) | 5.66 (5.61-6.37) |

**No reproducible boxing penalty.** With clean rows the three arms span 5.66-5.82
us/datagram (boxed Compio 3/3 clean, unboxed pipeline 2/3, blocking `sendto` 3/3).
The third `compio-pipeline` attempt was rejected by the fence itself — its window
boundary did not reconcile exactly, so it is excluded rather than averaged. The
earlier ~4 % gap did not reproduce under the corrected apparatus and therefore
**cannot be attributed to boxing**; with three attempts per arm this is an
engineering conclusion, not a statistical one. Boxing is not an optimization target,
and Stage A -> B measures Owner/TxEngine execution cost rather than harness
allocation. The clean Stage-A baseline for the ladder is **~5.7 us/datagram of sender CPU
(≈175 000 datagrams/s on one pinned core)** under the fence.

Still open before A -> B attribution is published: allocation counts and bytes per
datagram for A and B, the product-paced regime rows, and the Stage B implementation
behind a harness-only `bench-internals` feature.

### WI3.6 fence corrections + drain telemetry, and an unverified drain revision (2026-09-22)

Fence corrections (landed, unit-tested):

- Settlement succeeds only on `observed == expected`; `observed > expected` returns
  `Settlement::Overshoot` and is contamination, not success, and a warmup boundary
  that lost, overshot or timed out aborts the run before the rated window. Covered by
  three unit tests against a scripted `/state` peer (`equality_settles_and_overshoot_does_not`,
  `a_drop_during_settlement_is_immediate_loss`, `a_short_receiver_times_out_rather_than_settling`),
  plus one live observation earlier where a warmup `udpRcvbufErrors` increment
  refused to start the window. **Correction**: the exact-equality rule was described
  as landed one commit before it was actually in the source; it is in `d81218d1`'s
  successor, not in `d81218d1`.
- `softnet.flowLimit` now gates settlement alongside `softnet.dropped`; `timeSqueeze`
  and `receivedRps` remain diagnostics.
- All three placements must be pairwise disjoint: sender, harness/control and
  receiver (plus its RPS work) are parsed as CPU sets and intersected.
- Artifacts carry a `lane` block (environment provenance) **and** a `laneObserved`
  block with the effective values actually used — e.g. `deliveryToleranceEffective: 0.0`
  when the variable was never exported, plus the observed sender/harness/receiver/RPS
  masks.

Drain telemetry (landed): per-thread counters in `/state` plus granted `SO_RCVBUF`
per socket. The telemetry immediately exposed a real apparatus defect: with
per-socket `SO_REUSEPORT` and ten destination flows the hash sent **all** traffic to
one socket (`perThread` 0/0/0/888 917), so the receiver was effectively
single-threaded.

Drain revision **not yet verified**: replacing per-socket reuseport with one shared
socket across threads balanced the load (658k/727k/668k/708k per thread) but measured
*slower* — 123 124 pps/core and healthy, versus ~175 000 pps/core for the reuseport
configuration. Reverting to per-socket reuseport restored the code path that produced
the nine clean rows, but the first verification run at this revision was
`unclassified` (59 266 receiver rcvbuf drops at 171 967 pps/core, settlement `loss`),
so **a lossless saturation lane is not currently re-established** and no Stage-A/B row
may be trusted until it is. Either the drops are host noise (repeat runs decide) or
this revision regressed the drain; if they persist, revert the drain to `30bcbbc9`.

