# SRT Compio Owner final qualification (Work Item 2.5)

Raw per-run results (`*.json`) and 1 Hz samples (`*.samples.jsonl.gz`) sit next to
this file; `summary.json` is derived from them by
`scripts/harness/srt_final_qual.py summarize`. Large process logs stay under
`.local/artifacts/srt-final/` and are not committed.


## Contents

- [Host contract](#host-contract)
- [Method](#method)
- [Baseline vs candidate (3 repetitions each)](#baseline-vs-candidate-3-repetitions-each)
- [Candidate Owner metrics (median per shard/family, steady window)](#candidate-owner-metrics-median-per-shardfamily-steady-window)
- [Mass connect (100 direct outputs created concurrently, one CPU)](#mass-connect-100-direct-outputs-created-concurrently-one-cpu)
- [Slow application receiver](#slow-application-receiver)
- [Frozen SIGSTOP destination (fault.srt-output-stall)](#frozen-sigstop-destination-faultsrt-output-stall)
- [Thread ownership](#thread-ownership)
- [Carried-forward evidence](#carried-forward-evidence)

## Host contract

AMD EPYC Processor (with IBPB), 6 logical / 6 physical CPUs, no cpuset limit
(affinity 6), cgroup cpu.max `None`, 11.7 GiB,
Linux 6.8.0-139-generic x86_64, governor: not readable (VM). Rust profile: bench (target/bench; scripts/build/bench-harness.sh).
Candidate SHA `c74b98ceadb486de1f105da4020d4ecaec5b84da` (srt-rs `7382e26f80410bef43da149a3550978bcc7a4d49`); baseline `c323e5f5e832a618f71edd873ddcd3dc6c1bde30`
(built in a separate worktree, unmodified). Peer: MSR_PEER=sink (harness-native in-process SRT sink); scenario: egress-growth-source-srt (h264 SRT ingest 1.5M fixture, source-copy SRT outputs);
SRT latency: harness default (HARNESS_SRT_LATENCY_US). No RESTREAM_EGRESS_*/RESTREAM_SRT_* overrides except where a row says so.
No stray restream/mediamtx/ffmpeg/test_harness process existed before any series (the driver aborts otherwise).
Primary-host RX mode: **RawReadiness** (`BufferRingRegistrationFailed(22)`, `srt_managed_rx_available=false`,
`srt_runtime_io_uring=true` on all six shards), from the run samples.

## Method

Each point is a FRESH Restream process, fresh peers and work directory (never cumulative),
`resource-sweep` scenario `egress-growth-source-srt` with `MSR_PEER=sink`, a sampler at 1 Hz,
steady window = 8 s after ALL outputs first progressed, 30 s long. Three repetitions per commit per
point, alternating baseline/candidate order per repetition. The baseline binary is driven by the
current harness unchanged. CPU is process user+system over the window; RSS is the window median.

## Baseline vs candidate (3 repetitions each)

`outputs progressing at end` is per repetition. CPU %, RSS MB: median (min-max). First progress is
output-start request to first positive bytesOut, median across repetitions of p50 / p95 / max (ms).

| build | outputs | outputs progressing at end | runs passing all gates | CPU % | RSS MB | threads | first progress p50/p95/max ms |
|---|---|---|---|---|---|---|---|
| baseline | 10 | [10, 9, 10] | 0/3 | 318 (314-349) | 162 (162-166) | 34 | 234 / 248 / 248 |
| candidate | 10 | [10, 10, 10] | 3/3 | 125 (123-127) | 137 (137-137) | 29 | 140 / 223 / 223 |
| baseline | 30 | [18, 24, 27] | 0/3 | 361 (339-370) | 233 (230-246) | 34 | 188 / 1190 / 1239 |
| candidate | 30 | [30, 30, 30] | 3/3 | 144 (140-156) | 139 (138-139) | 33 | 183 / 1061 / 1149 |
| baseline | 100 | [56, 40, 0] | 0/3 | 331 (115-349) | 258 (255-350) | 34 | 1261 / 1659 / 2183 |
| candidate | 100 | [100, 100, 100] | 3/3 | 173 (168-185) | 146 (146-147) | 34 | 967 / 1119 / 1158 |
| candidate | 300 | [300] | 1/1 | 259 (259-259) | 171 (171-171) | 34 | 1999 / 2867 / 2945 |
| candidate | 500 | [500] | 1/1 | 330 (330-330) | 263 (263-263) | 34 | 2222 / 3595 / 3724 |

`candidate-300`/`-500` are single non-gating frontier runs.

Baseline failures are not averaged away: at 10 outputs only 1-8 outputs kept advancing during the window
(retries and bytesOut resets), at 30 only 0-2 did, at 100 between 0 and 21. Its CPU is 3.2-3.7 cores.
The candidate held 10/30/100 outputs advancing in every repetition. Verdicts (`summary.json`): CPU ratio
0.39 at 10 and 0.40 at 30, RSS ratio 0.84 and 0.60, no first-progress p95 regression, but the baseline
"operated normally" in 0 of 3 runs at both points, so these ratios compare against a degraded baseline.

## Candidate Owner metrics (median per shard/family, steady window)

| outputs | service visits/s | protocol actions/s | maintenance actions/s | TX pkts/s | TX completions/s | TX high-water | TX exhaustions | budget-exhausted/s | RX drop/trunc | avg service |
|---|---|---|---|---|---|---|---|---|---|---|
| 10 | 680 | 1369 | 0.0 | 752 | 753 | 16 | 0 | 0.0 | 0/0 | 17 us (max 17 ms) |
| 30 | 697 | 3271 | 0.0 | 1827 | 1827 | 16 | 0 | 0.0 | 0/0 | 28 us (max 49 ms) |
| 100 | 1102 | 11590 | 0.0 | 6341 | 6342 | 16 | 0 | 0.0 | 0/0 | 37 us (max 23 ms) |
| 300 | 2506 | 31928 | 0.0 | 17628 | 17628 | 16 | 0 | 0.0 | 0/0 | 42 us (max 115 ms) |
| 500 | 2655 | 42666 | 0.0 | 32974 | 32974 | 16 | 0 | 0.0 | 0/0 | 94 us (max 310 ms) |

Every candidate run passed the no-non-idle-spin gate: no family had tx_in_flight == capacity with flat
completions while visiting. Visits/s rises with load together with actions/packets per visit (2 -> 16 actions per
visit), and TX completions equal TX submissions. Maintenance is ~0 in steady state: the pool holds no work
after connection establishment, so there is no future-deadline maintenance accumulation.

## Mass connect (100 direct outputs created concurrently, one CPU)

| capacity | peak in-flight | peak queued | longest queued period | pool-full refusals | expired/failed | progressing | pass |
|---|---|---|---|---|---|---|---|
| default | 13 | 0 | 0.0 ms | 0 | 0/0 | 100/100 | True |
| concurrency-6 | 6 | 6 | 7.9 ms | 12 | 0/0 | 100/100 | False |
| concurrency-16 | 16 | 6 | 3.3 ms | 0 | 0/0 | 100/100 | True |

At the default capacity (64 per Owner) the sink completes handshakes so quickly that in-flight peaked at
13 and the queue never formed; the queue was exercised by lowering
`RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY`. At capacity 16 the queue rose to 6 and drained within 3.3 ms
with 100/100 progressing, no expiry, no failed admission and no refusals. At capacity 6 the queue (bound = capacity)
overflowed and refused 12 requests (outputs retried and connected), which is the documented bounded-queue behavior.
The pool depth high-waters and longest queued period come from new per-batch scalars, because a 1 Hz gauge cannot see a
millisecond-scale burst. NOTE: SRT egress always runs at least 2 shards (the product floors the CPU-derived shard count at 2;
`RESTREAM_EGRESS_SHARDS` does not apply to the SRT profile), so a one-CPU run has ONE CPU but TWO shards/Owners.

## Slow application receiver

`srt.slow-peer` (new harness mode): the receiver (`RawSrtSink`) handshakes, receives media, then PAUSES APPLICATION
DELIVERY while protocol timers and ACK/NAK keep running (not SIGSTOP). Healthy outputs go to the fast sink.

| run | healthy | watch | healthy advancing (min/s) | window stalls | retry/failed | Owner faulted | feed ring payload | sink data events while paused | pass |
|---|---|---|---|---|---|---|---|---|---|
| slow-peer-one-cpu | 10 | 30 s | 10/10 | 0 | 0 | False | 2.6 -> 9.4 MB | 0 | True |
| slow-peer-one-cpu-90s | 10 | 90 s | 10/10 | 0 | 0 | False | 2.5 -> 9.4 MB | 0 | True |
| slow-peer-control-no-pause-90s | 10 | 90 s | 10/10 | 0 | 0 | False | 2.6 -> 9.4 MB | 16756 | True |
| slow-peer-normal-host | 30 | 30 s | 30/30 | 0 | 0 | False | 2.7 -> 9.3 MB | 0 | True |

The pause is real (0 delivered events while paused, vs 16,756 in the no-pause control). In the 90 s run the slow leaf
delivered ~10.7 MB, then its output was closed by the existing stall policy and retried (bytesOut reset); the healthy
siblings never stalled, retried or lost an Owner. Retained feed payload plateaus at the same ~9.4 MB with or without the
slow peer, so the slow leaf does not pin retention. Leaf-to-shard attribution is not exposed reliably by the API, so the
shared-Owner claim is structural (two shards, outputs hashed by rendezvous), not per-leaf.

## Frozen SIGSTOP destination (fault.srt-output-stall)

| build | RSS growth (existing gate: 64 MB) | RSS median by fifth of the run (MB) | max RSS | threads/fds max |
|---|---|---|---|---|
| candidate | 68.0 MB | [139, 181.0, 194.0, 197.0, 197.0] | 198 MB | 29 / 66 |
| baseline | 70.8 MB | [138, 176.0, 176.0, 184.0, 184.0] | 196 MB | 29 / 52 |

**The existing 64 MB gate FAILS for both builds** (candidate 69.7-74.6 MB over four runs, baseline 67.6-72.5 MB over two).
This is the known, previously deferred RSS-under-retry behavior, unchanged by the Compio Owner: RSS climbs during the
first ~40% of the run and then plateaus (candidate 197/197, baseline 184/184), i.e. it is bounded, but it exceeds the gate's
64 MB allowance by 4-10 MB. The srt-rs receiver-delivery half of the mode PASSES.

## Thread ownership

Idle process (no SRT fabric): 15 threads. With outputs: 28-29 (10), 31-33 (30), 32-34 (100), 34 (300 and 500).
Growth from 10 to 500 outputs is +5 threads, not linear in outputs. Source architecture: one OS thread per SRT shard, one
Compio runtime per shard thread, at most two family Owners per shard, one caller UDP socket per Owner.

## Carried-forward evidence

Work Item 2.4: RawReadiness host `BufferRingRegistrationFailed(22)`, 10/30/100 connected, no stall/spin; Docker
Engine 28.0.4 (kernel 6.17.0-1022-azure): substrate Available, `ManagedMultishot`, real SRT egress under the shipped seccomp
profile. No same-host managed-vs-raw benchmark exists and none is claimed.
