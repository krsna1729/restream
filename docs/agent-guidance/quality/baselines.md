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

### WI3.7 provenance-clean current-host qualification (2026-09-23)

Measured from clean build commit `4acb0148` with the `wi37-shard-bench`
feature, pinned `srt-rs` checkout `86369b0`, and the six-vCPU veth topology.
The durable evidence is
[`test/harness/baselines/wi37-current-host/`](../../../test/harness/baselines/wi37-current-host/):
`summary.json` contains the selected cells and `manifest.json` records the
provenance, binary SHA-256s, selected artifact SHA-256s, classifications, and
exact commands. Raw run logs are intentionally not committed.

The qualification retained 28 plaintext logical cells (`1..4` shards ×
`10,20,30,40,50,60,80` outputs), one matching attempt per cell, and six
matching crypto repeats (AES-128/AES-256, three each). All selected rows were
apparatus-valid and `stable-unclassified`; no boundary-unstable cell was
included. The joint fit is `fixedCorePerShard = 0.103519`,
`secondsPerData = 13.0106 us/DATA`, `R² = 0.90245`, with a provisional
two-shard upper bound. The clean fit is materially different from the prior
local-only `0.0518` / `20.25 us/DATA` result, so this is evidence, not a
frozen current-host coefficient; WI4 is not advanced and Q-025 remains open.
The crypto paired deltas span zero and remain unresolved on this host.

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
so **a lossless saturation lane is not currently re-established**. There is no causal
reason to revert the drain to `30bcbbc9`: the deployed receive hot loop is materially
the same shape as the clean revision, so a revert would not explain the drops. The
skew finding below is the better explanation, and clean rows are to be obtained by
repeated alternating runs with every attempt recorded, not by retrying until one passes.

### WI3.6 Stage B — implemented, fanout-matched, four clean alternating pairs (2026-09-22)

Stage B exists: harness-only feature `wi3-owner-bench = ["srt-transport/bench-internals"]`
(default off; normal builds never see `bench-internals`), a `owner-tx` arm that builds the
caller side with `OwnerCallerSide::new_single`, attaches it with the benchmark-only
`with_caller`, adds 1000 direct caller legs (`add_direct` + in-process connected
`quiet_connected_caller` instances), injects pre-materialized datagrams round-robin with
`bench_push_pending`, drains initial connection events, and drives the production
`service()` -> `wait_for_activity()` rhythm with real monotonic timestamps and a normal
`OwnerServiceBudget`. Thread CPU is split with `CLOCK_THREAD_CPUTIME_ID` into an injection
scope and an Owner-drive scope. The artifact reports all three Stage B CPU quantities:
inclusive sender CPU, injection CPU, and Owner-drive CPU, each as seconds and
microseconds per measured datagram.

The previous ledger heading was wrong: the rows below are **A, B, B, B**, not alternating
A/B pairs. They are initial clean evidence only. The primary matched comparator for the
replacement ledger is `compio-pipeline` at queue depth 16; the boxed `compio` row is a
secondary historical reference.

The previous root-cause wording was also wrong. The predecessor's `connected_caller`
already reached `ConnectionState::Connected`. The quiescence issue was residual protocol
output and timer state after `Connected`, not an unfinished handshake. Do not attribute
it to the application event queue: pinned `Owner::has_pending_work()` does not include
that queue in its pending-work decision, and one `poll_caller_events()` call drains at
most 64 events. The benchmark setup now drains residual output, clears the protocol
deadline, and drains caller events before rated injection.

**Control-traffic resolution**: every datagram submitted in the rated window is an
injected DATA packet (`actions == txSubmitted == txCompletedOk == submitted == completed`,
with `maintenanceActions == 0` and `protocolOutputFailures == 0`). This satisfies the
ladder's exact `received == completed` settlement rule against the UDP drain with zero
overshoot.

**Earlier non-alternating clean rows (veth lane, depth 16, 1000 destinations, exact delivery, zero drops):**

| Arm | Run | pps/core | Inclusive sender us/datagram | Payload Gbit/CPU-s | Verdict | Settlement |
|---|---:|---:|---:|---:|---|---|
| `compio` (Stage A, historical) | 1 | 160 502 | 6.23 | 1.690 | healthy | settled (polls 1, 0.001 s) |
| `owner-tx` (Stage B) | 1 | 118 075 | 8.47 | 1.243 | healthy | settled (polls 1, 0.002 s) |
| `owner-tx` (Stage B) | 2 | 106 350 | 9.40 | 1.120 | healthy | settled (polls 1, 0.003 s) |
| `owner-tx` (Stage B) | 3 | 110 688 | 9.03 | 1.165 | healthy | settled (polls 1, 0.002 s) |

The four replacement pairs are all **healthy**, exact-delivery, zero-receiver-drop rows:

| Pair | Arm | pps/core | Inclusive sender us/datagram | Injection us/datagram | Owner-drive us/datagram | Receiver/kernel drops | Verdict |
|---:|---|---:|---:|---:|---:|---:|---|
| 1 | `compio-pipeline` A | 174 672 | 5.725 | — | — | 0 | healthy |
| 1 | `owner-tx` B | 124 077 | 8.060 | 0.868 | 6.909 | 0 | healthy |
| 2 | `compio-pipeline` A | 162 320 | 6.161 | — | — | 0 | healthy |
| 2 | `owner-tx` B | 102 343 | 9.771 | 1.025 | 8.383 | 0 | healthy |
| 3 | `compio-pipeline` A | 164 267 | 6.088 | — | — | 0 | healthy |
| 3 | `owner-tx` B | 119 643 | 8.358 | 0.891 | 7.157 | 0 | healthy |
| 4 | `compio-pipeline` A | 166 483 | 6.007 | — | — | 0 | healthy |
| 4 | `owner-tx` B | 118 330 | 8.451 | 0.891 | 7.249 | 0 | healthy |

The matched medians are **6.047 inclusive sender us/datagram** for primary
`compio-pipeline` A and **8.405 inclusive sender**, **0.891 injection**, and
**7.203 Owner-drive us/datagram** for Stage B. The inclusive B minus A median
(+2.357 us/datagram) is not the transport increment. The required A -> B
transport subtraction is:

`median(Owner-drive B) - median(raw Stage-A inclusive) = 7.203 - 6.047 = +1.155 us/datagram`

The four pairwise transport increments are **+1.069, +1.184, +1.242, and
+2.222 us/datagram**. Because these are deliberately *paired* runs, the primary
estimate for the increment is the median of the four pairwise deltas, not the
difference of the two medians; both are recorded:

```text
Stage-B transport increment:
  difference of medians:  +1.155 us/datagram
  median paired delta:    +1.213 us/datagram  (1.069-2.222)
```

The spread is retained rather than hidden. On this host the Owner compatibility
execution path therefore adds roughly **~1.2 us/datagram**, not 2.5-3+. Owner-drive is
85.6–85.8% of Stage B sender CPU, injection is 10.5–10.8%, and the residual
inclusive scope is the outer sender/control work. All eight rated rows have
`receiver.udpRcvbufErrors == 0`, `receiver.receiveErrors == 0`, and
`verdict == "healthy"`. Every Stage B row also has
`txSubmitted == txCompletedOk == completionsReaped`, `txFailedSends == 0`, and
`protocolOutputFailures == 0`.

**Attempt provenance:** the first 20-second A attempt failed before the rated
window because the warmup receiver counter rose (`udpRcvbufErrors` 99 693 ->
103 643). After a clean drain restart, the 20-second A retry reached the rated
window but accumulated 25 114 receiver/kernel drops and was `unclassified`.
Neither attempt is silently discarded:

| Attempt | Verdict | Artifact | SHA-256 |
|---|---|---|---|
| A pre-window | loss | `.local/artifacts/wi36-stage-b-pair1-compio-pipeline/attempt.txt` | `b92bcad3ba715b3730ebf04e8ee32b5c634cbe3cebc1aaa7390880c481087ea2` |
| A rated 20 s | unclassified/loss | `.local/artifacts/wi36-stage-b-pair1b-compio-pipeline/substrate-pps.json` | `b411fe78dcd23cb473c7254dbe03be8d57163f6f3e56a9eaef8ab40923ba40af` |

The clean pair artifacts remain at the following stable local paths; hashes
are recorded so an artifact copy can be verified:

| Pair/arm | Artifact | SHA-256 |
|---|---|---|
| 1/A | `.local/artifacts/wi36-stage-b-pair1c-compio-pipeline/substrate-pps.json` | `028d87d470107a0823282486ac8817641171dd60c4b322c0e3840224992a9e9a` |
| 1/B | `.local/artifacts/wi36-stage-b-pair1c-owner-tx/substrate-pps.json` | `d860f90ce51c2f182279398a133b69e4f722328af88a3f48bca932de31f29c84` |
| 2/A | `.local/artifacts/wi36-stage-b-pair2-compio-pipeline/substrate-pps.json` | `6a007c34e2c7fac1a853502aa541038a86bf217a51f8e6b8d6029d9f2d53d3eb` |
| 2/B | `.local/artifacts/wi36-stage-b-pair2-owner-tx/substrate-pps.json` | `132daf5527a10a9dbfab70d5cdbad5982ae20f70cc180e6b1bac7b496a984683` |
| 3/A | `.local/artifacts/wi36-stage-b-pair3-compio-pipeline/substrate-pps.json` | `eef79fee5964c90b0db5860089c9fb5ace55be5ca2d1c1346622eb723f2ba6ba` |
| 3/B | `.local/artifacts/wi36-stage-b-pair3-owner-tx/substrate-pps.json` | `39e2bb615c2821388d6ba99ccadf44889b6623c4e6b681a3838a223663be1424` |
| 4/A | `.local/artifacts/wi36-stage-b-pair4-compio-pipeline/substrate-pps.json` | `450cf96108a37c4ea4983dd08e3696e41258f0945fe6c806dfe53d0332f77c1d` |
| 4/B | `.local/artifacts/wi36-stage-b-pair4-owner-tx/substrate-pps.json` | `4ce93fcfa6f631263cf36a3844c8417f1cd75f88ba2f3efead8e1cb250faed79` |

Exact rated command for every clean row (only artifact directory and
`SUBSTRATE_VARIANT` changed, in order A1, B1, A2, B2, A3, B3, A4, B4):

```sh
source .local/artifacts/wi3-topology/wi3-topology.env
TEST_HARNESS_ARTIFACT_DIR=.local/artifacts/<run> \
WORK_DIR=.local/artifacts/<run> \
SUBSTRATE_VARIANT=<compio-pipeline|owner-tx> \
SUBSTRATE_SENDER_CPUS=0 SUBSTRATE_HARNESS_CPUS=1 WI3_RECEIVER_CPUS=2-5 \
SUBSTRATE_DEST_BASE=10.53.1.1 SUBSTRATE_DEST_PORT=9000 \
SUBSTRATE_DEST_COUNT=1000 SUBSTRATE_QUEUE_DEPTH=16 \
SUBSTRATE_WARMUP_SECS=3 SUBSTRATE_DURATION_SECS=5 \
SUBSTRATE_REPORT_SECS=5 SUBSTRATE_RECEIVER_STATE=http://10.53.0.2:9997/state \
taskset -c 1 target/bench/test_harness substrate-pps --no-netns
```

### WI3.6 Stage C — plaintext production-attach Owner qualification (2026-09-22)

Stage C used the pinned upstream production qualification path rather than
duplicating it in the Restream synthetic harness:
`/home/dev/.cargo/git/checkouts/srt-rs-2a4e85d4a1e5ceb4/86369b0/crates/srt-bench/benches/compio_shared_owner_qual.rs`
at srt-rs revision `86369b0815c09333547c965652250e17f580e834`. The sender uses
real `Owner::connect`, `SocketOwnership::Shared`, the production Compio runtime,
fixed `tx_lanes=16`, `connect_cc=16`, payload 1316 bytes, plaintext SRT, and
8 Mbps per destination. The receiver is an independent pinned
`srt-bench runtime=compio mode=receiver` process. Receiver TSV counters, not
sender TX completion alone, decide loss.

The 1000-destination attempt did **not** qualify: the receiver recorded
184 133 `udp_rcvbuf_err`, the sender never submitted DATA, and its
`pre_window_drained=false`. The required loss criterion was not weakened.
Descending fanout kept the same payload, rate, TX lanes, connect concurrency,
receiver process, and 30-second sender window:

| Fanout | Sender `data_retx` | Sender `pre_window_drained` / `drain_ok` | Receiver `pkt_sent` | Receiver `sec_a` / `sec_b` | Kernel / datapath drops | Result |
|---:|---:|---|---:|---:|---:|---|
| 1000 | 0 | false / true | 0 | 0 / 0 | 184 133 / 0 | failed |
| 600 | 0 | false / true | 0 | 0 / 0 | 38 141 / 0 | failed |
| 300 | 0 | false / true | 0 | 12 232 / 0 | 0 / 0 | failed |
| 100 | 0 | true / false | 74 217 | 175 469 / 0 | 0 / 9 615 | failed |
| 50 | 0 | true / false | 104 290 | 192 045 / 0 | 0 / 5 351 | failed |
| 25 | 0 | true / false | 104 978 | 141 138 / 1 | 0 / 15 086 | failed |
| 15 | 0 | true / false | 177 919 | 121 473 / 0 | 0 / 9 054 | failed |
| 14 | 0 | true / false | 308 639 | 5 766 / 4 | 0 / 9 307 | failed |
| 13 | 0 | true / false | 208 160 | 84 209 / 0 | 0 / 15 231 | failed |
| 12 | 0 | true / false | 273 540 | 0 / 0 | 0 / 0 | zero-loss counters, sender drain incomplete |
| 11 | 0 | true / true | 250 756 | 0 / 0 | 0 / 0 | **highest fully clean tested** |
| 10 | 0 | true / true | 227 950 | 0 / 0 | 0 / 0 | clean, below 11 |

Here `sec_a` is receiver protocol loss, `sec_b` receiver duplicates,
`udp_rcvbuf_err` is kernel receive-buffer loss, and `datapath_q_dropped` is
benchmark receiver queue loss. Fanout 12 met the two hard zero-loss counters
and zero DATA-retransmission conditions but did not complete the sender's
drain fence; it is therefore not called fully clean. Fanout 11 is the
conservative highest clean fanout.

Every Stage C attempt is preserved as `sender.log` plus `receiver.tsv` under
`.local/artifacts/wi36-stage-c-<fanout>/`; the stable SHA-256 pairs are:

| Fanout | Sender log SHA-256 | Receiver TSV SHA-256 |
|---:|---|---|
| 1000 | `760240c6c363cf74a6e4f6e57d63508cc34c126bebb10b4cc18ef8c76a99fc92` | `1a204a1f02432b7e1bd7383f18b8180175e4b4e992be64742b48e20ad3ace80e` |
| 600 | `c4968b10a9b260a0d4c101865fd0795721289e76208ce035279a8d82ccfdcf8b` | `d17ebc9ac006c52069537f4ef72107db7d1e9b57cb63015fde6e21f9bc775823` |
| 300 | `0bc2a37ee0a7f7d3d0a6d4f15ea201910f74d131230a46ad91a8409e58aed23a` | `6dbe655efacd33015a6459630eec1d88a0879bb72b37132996d34cc77b6b15e2` |
| 100 | `7f468a7280d5a5b9ded8fe079a6bbca1f3ec8ce18084b7be90c879da48dbff8a` | `4fe7d605aaee5e24672d3e0c63dd06526f436001dd52889ffdc5971487ab1ce9` |
| 50 | `ba3d5333667e33d092f9962a0966dc6a62d86f200fb75f6026d6e936a1b0302b` | `602d8c0207855863c47313435402d23cf135e3cfac37a83c72726b39e3e90bab` |
| 25 | `d83424ed2599b6ab1a377e0edd3eab4335d698fbffc6678004983b83dc295b16` | `1a8a8c6c34dd9b844be803b1d094ecf882244d877a41b899949fa835ad8b238e` |
| 15 | `9671ee1b39b316989e2f8b6b50d3658620a6fcf12934b5461094090c4f10884f` | `38c40da16c1ad8bc23f07e90573e71324da36c5ffa62762aa16b31040a105352` |
| 14 | `40d1144e8068a8c6e6f1a9efd3e0628c6d3a570e25c58eb44a53277fb82e5297` | `ec55b80099aff05e36d2f6040172ae0631a243181abb3ce5b2711c15076a2c36` |
| 13 | `2b7170891a95e065dc1878ef7c55c6a4bab461bbeb18297576195fe25ab05b29` | `1dbb50e5a6b14ee0b079f288db15a0412317bb80d939dae96e17c198725837b4` |
| 12 | `f79ea006147b8cf53acae492ae903584b947dc4d1a8135d4ac99f7e77ea3c5b0` | `9dc0fda4825a397f57c2277db394ebe33846c3496f0c309c0c050a26f3457218` |
| 11 | `646efee12e644696177d88be8c940ac65c3b5d67fc7dee5de579e3df6d3efd49` | `b06fd6bcbaf4333808a33f739774e09d0b3ff9696673fcc54479e3807ed434f9` |
| 10 | `a0ae9e9dd278b891957cb232040b23a49a78bdcc63c19709647337d71f49cc0c` | `d46eb0e51e13cafb52906ce80de1f8c2e56f29145f96f4db659672a320302fc8` |

The one failed receiver-launch attempt at fanout 12 is also retained at
`.local/artifacts/wi36-stage-c-12/attempt.txt` (SHA-256
`ffccd87ef462872fc6518c5d121e7f2b29c8831e7fe37416fb066948dc54829c`);
the corrected fanout-12 sender/receiver run is the row above.

Exact Stage C build commands:

```sh
cd /home/dev/.cargo/git/checkouts/srt-rs-2a4e85d4a1e5ceb4/86369b0
/home/dev/restream/scripts/build/resource-limit.sh cargo build --release -p srt-bench --bin srt-bench
/home/dev/restream/scripts/build/resource-limit.sh cargo bench -p srt-bench --bench compio_shared_owner_qual --no-run
```

Exact receiver command template:

```sh
taskset -c 2-5 /home/dev/.cargo/git/checkouts/srt-rs-2a4e85d4a1e5ceb4/86369b0/target/release/srt-bench \
  runtime=compio mode=receiver 12000 90 120 --connections <fanout> \
  --ingress per-port --egress per-connection --encryption plain --workers 1 \
  --cpus 2-5 --pin on --out /home/dev/restream/.local/artifacts/wi36-stage-c-<fanout>/receiver.tsv
```

Exact sender command template:

```sh
taskset -c 0 /home/dev/.cargo/git/checkouts/srt-rs-2a4e85d4a1e5ceb4/86369b0/target/release/deps/compio_shared_owner_qual-aa6a3035a9ac534d \
  --fanout <fanout> --duration-ms 30000 --base-port 12000 \
  --tx-lanes 16 --connect-cc 16 --payload-bytes 1316 \
  --rate-mbps-per-dest 8 --send-shards 1
```

At the highest fully clean fanout, 11, the matched Restream A/B rerun stayed
healthy with zero receiver drops: `compio-pipeline` A measured **168 227
pps/core** and **5.944 inclusive sender us/datagram**; `owner-tx` B measured
**134 778 pps/core**, **7.420 inclusive**, **0.259 injection**, and
**6.874 Owner-drive us/datagram**. The transport subtraction is therefore
`6.874 - 5.944 = +0.930 us/datagram` (15.6%). Raw artifacts:

| Arm | Artifact | SHA-256 |
|---|---|---|
| A | `.local/artifacts/wi36-stage-b-clean-11-compio-pipeline/substrate-pps.json` | `03493648f0135471f4d5765f21917825f797a0310fae48795f9db5b3b1086741` |
| B | `.local/artifacts/wi36-stage-b-clean-11-owner-tx/substrate-pps.json` | `cc173b157fe5db9f0311022090e614ebb97e47595becc85b5734e369a5050aa5` |

### WI3.6 Stage C — clean F=11 window row, derived costs, and repeats (2026-09-22)

Fanout 11 was rerun four more times with the identical shape (plaintext, payload
1316 B, interval 1316 us, 8 Mbps per destination, `--tx-lanes 16`,
`--connect-cc 16`, 30 s sender window,
`srt-bench runtime=compio mode=receiver` in the `wi3-sink` namespace on CPUs
2-5). The three fully clean rows below are the ones eligible for WI3.6: zero
missed source ticks, `tx_class_data_first == 250756` (exactly 11 x 22796),
zero DATA retransmission, zero receiver protocol/kernel/datapath loss, and
`drain_ok`.

```text
expected_ticks 22796   generated_ticks 22796   missed_source_ticks 0
data_offered 250756    data_accepted 250756     tx_class_data_first 250756
window cpu_ms         7623.7 / 7032.8 / 6918.6   (cpu_ms 7740.1 / 7245.0 / 7620.2)
drain cpu_ms          116.4 / 212.2 / 701.6
```

| Fanout-11 run | `window_cpu_ms` | us / first-transmission DATA | us / total wire datagram | wire / DATA-first | service visits / tick | first-submit lateness p50 / p99 / max (us) | in-flight at window end | drain_ok |
|---|---:|---:|---:|---:|---:|---|---:|---|
| base | 7 623.7 | 30.403 | 24.938 | 1.2191 | 0.9557 | 100 / 95 000 / 123 704 | 15 | true |
| r3 | 7 032.8 | 28.046 | 22.787 | 1.2308 | 0.9788 | 100 / 22 000 / 47 376 | 14 | true |
| r13 | 6 918.6 | 27.591 | 22.402 | 1.2316 | 0.9824 | 100 / 40 000 / 64 484 | 11 | true |
| **median** | **7 032.8** | **28.046** | **22.787** | **1.2308** | **0.9788** | — | — | — |
| range | 6 918.6-7 623.7 | 27.591-30.403 | 22.402-24.938 | 1.2191-1.2316 | 0.9557-0.9824 | — | 11-15 | — |

Control classes per first-transmission DATA are identical across the three rows
(only ACK and ACKACK are non-zero): ACK 0.0964-0.1074, ACKACK 0.1227-0.1243,
NAK / keepalive / handshake / DROPREQ / KM / shutdown / other-control all 0.
`tx_class_total = first + ACK + ACKACK` exactly in every row, so protocol
amplification is fully accounted for by the peer's ACK/ACKACK cadence.

Invariants across the three rows: `owner_faulted = false`,
`rx_dropped = rx_truncated = rx_lost = rx_duplicates = 0`,
`pending_after_drain = 0`, `tx_pool_free = tx_pool_capacity = 16`,
`tx_pool_high_water = 16`, receiver `pkt_sent = core_total = 250756` with
`sec_a = sec_b = 0`, driver `IoUring`, compio 0.19.2, host contention
`contended`.

Artifacts (sender log / receiver TSV SHA-256):

| Run | Sender log | Receiver TSV |
|---|---|---|
| base | `646efee12e644696177d88be8c940ac65c3b5d67fc7dee5de579e3df6d3efd49` | `b06fd6bcbaf4333808a33f739774e09d0b3ff9696673fcc54479e3807ed434f9` |
| r3 | `3746531f38a3c6594f099ab17e4ba3d303d80758ec12c6b23edcd4181519fe7d` | `bdb55fa40a7bdbf6ee34fa051bb10d67ab1a55ffedb9add199bfc0ff35e53ab3` |
| r13 | `4539988ce2c2b1b8ccff2091ac8c4a7337c115b29c8b6443427147d13acf1f88` | `555d09e9ca00a9411a633f8a007a5ad55a6cd071264a165aa841a550f9b1d86f` |

Four further attempts were **not** fully clean and are retained rather than
discarded; each is usable only as a bound, never as the rated row:

| Attempt | Failure | us / DATA-first |
|---|---|---|
| `wi36-stage-c-11-r2` | 42 missed source ticks (11 x 42 fewer DATA offered) | 32.569 |
| `wi36-stage-c-11-r5` | 4 missed source ticks | 33.575 |
| `wi36-stage-c-11-r10` | clean ticks, but 11 DATA first-transmitted after the window close | 29.041 |
| `wi36-stage-c-11-r12` | clean ticks, but 165 DATA first-transmitted after the window close | 30.554 |
| `wi36-stage-c-11-r9` | 106 missed ticks, 275 DATA retransmissions, receiver `sec_a` 238 / `sec_b` 36 | (not rating-eligible) |
| `wi36-stage-c-11-r11` | zero loss but 4 retransmissions and `drain_ok = false` | (not rating-eligible) |
| `wi36-stage-c-11-r8` | 28 missed ticks and `drain_ok = false` | (not rating-eligible) |
| `wi36-stage-c-11-r4`, `-r6`, `-r7` | aborted launches (malformed sender argv); `sender.log` empty, receiver recorded partial capture | (no row) |

**F=11 is a lossless *attribution* fanout, not a real-time capacity point.**
Rating eligibility required zero loss and a complete sender fence, and that
selection is itself evidence about the lane: of the **10 completed
non-malformed F=11 launches, only 3 were rating-eligible**. The clean rows also
carry real first-submit lateness despite zero missed source ticks — p99
**22 000-95 000 us** and maxima **47 376-123 704 us** — so F=11 is not evidence
that the sink is robust under host contention, only that it can be lossless in
the rated window when it is. Carry both figures forward as qualification risk
into WI3.7 (capacity) and WI10 (portability); WI3.6 is not gated on fixing them,
because attribution needs a lossless point, not a real-time guarantee.

WI3.6 must use `window_cpu_ms`, never `cpu_ms`: the latter spans
window + post-window drain, and the drain is real (116-702 ms here, and ~1.2x
the window's traffic on the upstream F=200 shard).

**Correction — `rx_mode` is sender-side.** `compio_shared_owner_qual` prints
`owner.rx_mode()` of the *sender's* Owner caller socket, and `managed_rx` is the
comparison with `OwnerRxMode::ManagedMultishot` for that same sender-side
endpoint. The value recorded above, `Some(RawReadiness)`, therefore means the
**sender-side Owner RX path fell back to the raw reader** (`ManagedPreferred`
could not register the provided-buffer ring on this host). It is not a statement
about the independent `srt-bench` receiver process, which is a separate program
with its own receive path.

### WI3.6 paced regime (Regime P) — controls, and why paced B->C is not subtractable (2026-09-22)

Stage C is a product-paced run: 11 destinations, one 1316-byte payload per
destination every ~1316 us (8 Mbps each, 88 Mbps aggregate, ~8360 datagrams/s).
The Stage-B fanout-11 rows above are *saturation* rows (168-135k datagrams/s) and
**cannot be subtracted from Stage C**. A product-paced mode was therefore added
to the Restream substrate harness (`SUBSTRATE_PACE_US=<interval>`), which emits
one datagram per destination per tick as a single burst and resets its schedule
at every pause boundary; `sourceBurstsInWindow` / `partialSourceBurstDatagrams`
in the artifact prove the burst shape (0 partial bursts means the window's
datagram count is an exact multiple of the fanout).

Paced rows, six alternating repetitions, same lane/placement, K = 16, 5 s
windows, zero receiver drops, exact settlement, `healthy`:

| Rep | Arm | datagrams | source ticks | us / datagram | us / source tick | Owner-drive us/dg | injection us/dg | service visits |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| A1 | `compio-pipeline` | 41 822 | 3 802 | 27.976 | 307.73 | — | — | — |
| B1 | `owner-tx` | 41 811 | 3 801 | 38.507 | 423.57 | 11.584 | 2.540 | 3 801 |
| A2 | `compio-pipeline` | 41 822 | 3 802 | 25.585 | 281.43 | — | — | — |
| B2 | `owner-tx` | 41 822 | 3 802 | 32.758 | 360.34 | 8.327 | 2.004 | 3 802 |
| A3 | `compio-pipeline` | 41 811 | 3 801 | 28.461 | 313.08 | — | — | — |
| B3 | `owner-tx` | 41 789 | 3 799 | 36.613 | 402.74 | 10.618 | 2.413 | 3 799 |
| **median A** | | | | **27.976** | **307.73** | — | — | — |
| **median B** | | | | **36.613** | **402.74** | **10.618** | **2.413** | — |

Paired paced A->B deltas (inclusive sender CPU): **+10.531, +7.173, +8.152
us/datagram**, median **+8.152 us/datagram** (+95 us per source tick). Removing
the harness-only injection scope: median **+5.739 us/datagram**.

Stage C at the same pacing and fanout, on the same scope (whole sending
thread/process CPU per window; `production_runtime_builder` builds compio's
single-threaded runtime, so the process CPU is the sender thread's CPU):

```text
paced A (raw Compio UDP)      median 307.73 us / source tick   (281.43-313.08)
paced C (full plaintext SRT)  median 308.51 us / source tick   (303.50-334.43)
paced B (Owner compat path)   median 402.74 us / source tick   (360.34-423.57)
```

Paced A and paced C are close within this lane's run-to-run spread: at the
product's load shape the sending thread costs ~300-310 us of CPU per 1316 us
tick (~23% of one core for 88 Mbps). Read that as a **whole-path** number only:
the net product-paced `A -> C` CPU difference is unresolved around zero, and the
protocol term inside it is not isolable because A and C submit and materialise
through different machinery (raw `FuturesUnordered` sends versus Owner
scheduler + direct final-slot materialisation + SRT protocol + 1.2308 wire
datagrams per DATA-first). Full SRT as a whole does not create a large positive
CPU gap over the raw paced control; that is a bound on the *aggregate*, not a
measurement of protocol CPU. **Paced B is 90-120 us
per tick *above* both**, so the compatibility control as a whole is unsuitable
for a paced `B -> C` subtraction — not because any one term of its cost was
identified, but because its aggregate harness/compatibility execution dominates
the differential (see below).

That is a measured apparatus result, not a transport result, and it is why the
prescribed paced `B -> C` subtraction is not valid:

- B's `Owner-drive` scope covers only the `service()` span (10.6 us/datagram);
  C's cost is whole-thread, so `C - B_drive` would attribute B's per-tick
  parking and injection to "protocol work".
- The like-for-like scope is whole sending thread per tick, and there
  `C - B` is **negative** (-94 us/tick, -8.6 us/datagram).
- Paced B's largest component is **outside** the measured scopes: 36.613
  inclusive, 10.618 Owner-drive, 2.413 injection, leaving **23.582
  us/datagram** of pacing/runtime/outer-loop CPU. Aggregate
  harness/compatibility execution dominates the differential, so the
  domination cannot be assigned to the per-datagram `Vec`
  clone, the 1316-byte copy into the reserved TxPool slot, or its deallocation
  individually.
- B's final TxPool is the binding resource in every paced row
  (`txPoolFreeMin = 0`, `inFlightHwm = 16` at K = 16), while C runs the same
  offer through `send_shared` and materialises directly into the final slot.

So the paced regime supports: **A -> B = +8.152 us/datagram inclusive (+5.739
excluding harness injection)**, and an absolute product-paced cost for the real
SRT path (**28.05 us per first-transmission DATA**, 308.51 us per source tick,
1.2308 wire datagrams per DATA).

**`A ≈ C` is a whole-path statement, not a protocol-cost measurement.** The
net product-paced `A -> C` whole-path CPU difference is **unresolved around
zero**; protocol CPU cannot be isolated from this experiment because A and C use
different submission and materialisation machinery:

```text
A: FuturesUnordered raw sends
C: Owner scheduler + direct final-slot materialisation + SRT protocol
   + 1.2308 wire datagrams per DATA-first
```

C can therefore carry positive protocol cost that is offset by a cheaper or
better-batched TX path than A, and no term of that difference is attributable
from these rows.

**Paced `B -> C`: UNRESOLVED / NONBLOCKING.** The compatibility control as a
whole is unsuitable for `B -> C` subtraction. A direct-slot `B'` control (one
that materialises into the final slot with no per-datagram `Vec`) would resolve
it, but it is an upstream `srt-rs` bench-internals capability and is
**deliberately not built now**: it reopens the measurement loop for a number
that no current optimization decision needs. Revisit only if a Stage-D result
leaves protocol-versus-integration attribution genuinely decision-critical.

Paced artifacts (SHA-256 of `substrate-pps.json`):

| Rep | Artifact | SHA-256 |
|---:|---|---|
| A1 | `.local/artifacts/wi36-stage-p4-a1/substrate-pps.json` | `9aac267d0ef3e1fcb9b373b990778453db21673ffb3e102b8b2564aa4bbc5f37` |
| B1 | `.local/artifacts/wi36-stage-p4-b1/substrate-pps.json` | `adf598a2b28132ec01d78a43d805cafcebf565e939dcad59a587d5afe8aef921` |
| A2 | `.local/artifacts/wi36-stage-p4-a2/substrate-pps.json` | `680ececc54a9f0409cf37bf304a2112f003df8f1595bba5be181a842e1d7f160` |
| B2 | `.local/artifacts/wi36-stage-p4-b2/substrate-pps.json` | `d0b7236107f1ee0e49ebeb29e233f0ab9f2f112119556c5af2f1535468c80f6a` |
| A3 | `.local/artifacts/wi36-stage-p4-a3/substrate-pps.json` | `4d3e0ad3ac793f2e3036ebbae1ce6420d3646f3abffd197ca2893c5f3178a274` |
| B3 | `.local/artifacts/wi36-stage-p4-b3/substrate-pps.json` | `47f8611eb6517f431f64969bf45ac0bfb112819eb53b8a55f6e2d8df99faed40` |

Superseded paced calibration attempts (retained, not used for any number):
`wi36-stage-p2-*` (harness pacer dropped the ticks a late burst had passed
instead of catching up) and `wi36-stage-p3-*` (pacer phase was not re-anchored
after the warmup pause, and the `owner-tx` burst waited for free TxPool slots,
which added a second `service()` visit per tick: 2.1 visits/tick against the
production path's 0.98).

Paced-mode harness changes landed with this evidence:

- `SUBSTRATE_PACE_US` selects product pacing; per-tick bursts are one datagram
  per destination, and skipped ticks are counted from the epoch-anchored
  schedule rather than silently stretching the interval.
- The pacer is re-anchored at each pause boundary, so a warmup pause cannot make
  the sender fire a catch-up burst into the rated window.
- Stage-B service totals are now **window deltas**: `ServiceTotals` is
  snapshot at the warmup boundary and subtracted at the end, so
  `serviceVisits` / `actions` / TX counters are comparable with the upstream
  Stage-C window figures instead of being whole-run cumulative totals.
- `tx_pool_free_min` is `Option<u64>` and therefore survives a real zero: the
  previous `0`-as-uninitialized sentinel erased a genuine empty pool (visible
  immediately in the paced rows, all of which report `txPoolFreeMin = 0`).

### WI3.6 Stage D — full Restream SRT egress at F=11 (2026-09-22)

Stage D is the whole product path: one Restream process publishing 11 plaintext
SRT outputs at the 8 Mbps shape, sourced from the canonical
`bench-h264-8m.ts` SRT ingest fixture, against the same independent pinned
`compio` receiver as Stage C. Lane and placement: egress shard threads pinned to
CPU 0, the rest of Restream on CPU 1, harness on CPU 1, receiver on CPUs 2-5,
ports 12000-12010 (`per-port`), 30 s rated window.

Two apparatus facts, both recorded in the artifact:

- The receiver's **default 250 ms datapath horizon is smaller than the
  product's per-visit burst**. Restream drains up to
  `RESTREAM_EGRESS_VISIT_MAX_BYTES` = 262 144 B = **199 datagrams** per shard
  visit at 1316 B, against a derived per-connection queue capacity of **189**
  packets: the receiver's own application queue overflowed (8 628 dropped, peak
  depth exactly 189/189) and every retransmission followed from it. The rated
  rows therefore run the receiver with `--datapath-queue-horizon-ms 4000`
  (3 036 packets per connection, 33 396 total) and the TSBPD latency the Stage C
  rows used (`120` ms; the third positional is `latency_ms`, not a connect
  timeout). Peak queue depth in the rated rows is 439-879 of 3 036, so the
  receiver is provably not the limiter.
- The product's SRT egress runs **two** shard threads at this shape
  (`EgressShardProfile::SrtCpuParallel` clamps to `clamp(effective_cpus, 2, 8)`,
  so one CPU of Restream affinity still yields two shards). Each shard index
  also has a second thread carrying the inherited `comm` at 0.00 s CPU. All are
  pinned to CPU 0 and their observed affinity is `[0]`.

**Result: no rating-eligible Stage D row at F=11.** Five rated attempts were
rejected by both required recovery fences: every row failed
`zero_data_retransmission`, and every row failed `receiver_protocol_loss` with
`sec_b > 0` (while `sec_a = 0`). These are two views of the same recovery
phenomenon, but both checks are independently required. All other fence
conditions passed in all five, including exact whole-run reconciliation.

| Rep | window DATA-first | window retx | receiver `sec_a` / `sec_b` | queue peak / cap | kernel + queue drops | reconciliation residual | verdict |
|---|---:|---:|---|---:|---|---:|---|
| r1 | 246 752 | 11 | 0 / 22 | 439 / 3 036 | 0 | 0 | rejected |
| r2 | 246 752 | 33 | 0 / 33 | 879 / 3 036 | 0 | 0 | rejected |
| r3 | 246 752 | 42 | 0 / 42 | 663 / 3 036 | 0 | 0 | rejected |
| r4 | 246 752 | 47 | 0 / 58 | 676 / 3 036 | 0 | 0 | rejected |
| r6 | 246 752 | 3 | 0 / 3 | 815 / 3 036 | 0 | 0 | rejected |

Every attempt: 11/11 connections established, all outputs `running`/`sending`
across four mid-window samples, no owner fault, `txFailedSends` 0,
`txExhaustions` 0, `queueOverflows` 0, `resyncCount` 0, and engine
first-transmission DATA (280 192, whole lifetime) equal to receiver `pkt_sent`
exactly.

The retransmissions are **not lost data**. For the lane-probed attempt (r6) the
host veth and the namespace veth moved identical packet counts
(36 420 175 -> 36 767 616, +347 441) with `tx_dropped`/`tx_errors`/`rx_dropped`/
`rx_errors` all zero, host-wide softnet `dropped` did not move (182 414 before
and after), `flow_limit` 0, and the receiver's socket held a 32 MiB effective
receive buffer with zero overflow. The receiver declared gaps, NAKed, and the
repairs arrived as duplicates (`sec_b`); the loss counters that would show
dropped data never moved.

Three bounded mechanism probes are now recorded:

- **RPS is not a necessary cause.** One repetition with the peer veth's
  `rps_cpus` set to `0` (single-CPU RX) still showed 30 window
  retransmissions and 30 receiver duplicates. This rules out multi-CPU RPS as
  a necessary cause; it does not establish that RPS or network scheduling is
  irrelevant. Lane restored to `3c` on exit.
- **The prior 64 KiB visit probe is inconclusive.** One repetition with
  `RESTREAM_EGRESS_VISIT_MAX_BYTES=65536` still showed 28 window
  retransmissions, but the old artifact's burst-bound read was `null`, so the
  override's effective value was unconfirmed.
- **The bounded visit-floor probe is rejected.** With F=11 and
  `RESTREAM_EGRESS_VISIT_MAX_BYTES=1316`, the corrected artifact reads back
  exactly `1316` (`datagramsPerVisit=1`), yet reports 131 window
  retransmissions and `sec_b=149`. The visit-fragment hypothesis is therefore
  unsupported under the probe rule.
- **Fanout is not necessary for the floor.** With F=1 and the normal
  `262144`-byte effective bound, the artifact reads back `262144`
  (`datagramsPerVisit=199`) but still reports one window retransmission and
  `sec_b=1`. That is consistent with same-peer burst/TX ordering, not proof of
  ownership by either side. Since the F=11/1316 probe retained retransmission,
  no pinned `srt-rs` `TxEngine` experiment is opened; Stage D closes with no
  valid row.

A **paced control on the same lane and the same receiver settings** shows that
receiver duplicate events can occur without sender DATA retransmission: the
upstream paced sender (Stage C's shape, F=11, 8 Mbps/destination) ran with
**0 window retransmissions** but the receiver still counted 10 duplicates.
That control had its own defect (40 missed source ticks, so it is not a rating
row). It does not assign the D retransmissions to the receiver; it only shows
that the receiver duplicate counter can move without a sender retransmission.

**Indicative CPU numbers (rejected rows — not rating-eligible).** Reported
because the fence failure is a protocol-timing artefact of order 1e-5..1e-4 of
DATA, and because the decision they inform is aggregate:

| Scope | median us / window DATA-first | range |
|---|---:|---|
| SRT egress shard threads (sum over both shards, CPU 0) | **26.018** | 22.857-30.111 |
| whole Restream process | **33.718** | 30.395-38.379 |
| non-egress Restream CPU (process − egress; ingest SRT + demux/mux/media/control/etc.) | **~7.9** | 7.5-8.3 |

Egress threads carry 5.64-7.43 s of CPU per ~30.34 s window (21-24% of one
core); the process carries 7.50-9.47 s. The egress threads hold ~77% of the
process CPU at this shape.

Against Stage C (28.046 us per first-transmission DATA, one sender thread,
27.591-30.403), **no positive C→D egress CPU increment is resolved**:
rejected D egress-thread measurements overlap the clean C range. These are
indicative only, not a rating result. The process-minus-egress figure is
non-egress Restream CPU — ingest SRT plus demux/mux/media/control/etc. — and
is not a protocol attribution. Two caveats must travel with the comparison:
D's egress is carried by two shard threads (C by one), so per-thread
efficiency differs even though the reported D number is summed; and C must
never be subtracted from D's whole-process CPU.

Offered rate differs slightly and is recorded for comparability: D's source
delivers 739.35 DATA/s per output (7.78 Mbps at 1316 B) against C's 759.9
(8.00 Mbps), i.e. the product row is ~2.7% below the upstream offer.

Artifacts: `.local/artifacts/wi36-stage-d-{r1,r2,r3,r4,r6-laneprobe}/egress-duty.json`,
the RPS probe at `wi36-stage-d-r5-rps0/`, the old burst probe at
`wi36-stage-d-r7-burst64k/`, the corrected bounded probes at
`wi36-stage-d-probe-f11-visit1316/` and
`wi36-stage-d-probe-f1-default/`, and the paced control at
`wi36-stage-d-control-paced/{sender.log,receiver.tsv}`. Every rejected attempt
kept its full artifact (receiver TSV, both stdout/stderr logs, Restream log,
publisher log) next to it.

Carry **no valid Stage D row** into WI3.7 (capacity) or WI10 (portability).
F=11 remains the lossless *attribution* fanout for Stage C, but the full
Restream path did not produce a valid D measurement: its recovery floor
persists at the corrected 1316-byte visit bound and even at F=1. The D CPU
figures above remain indicative only; do not use them as a positive
protocol/integration increment.
