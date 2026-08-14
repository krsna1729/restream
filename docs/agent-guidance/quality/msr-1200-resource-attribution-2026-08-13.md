# MSR 1,200-Output Resource Attribution — 2026-08-13

Where thread count, memory, and CPU at n=1,200 actually come from, for each
of the three canonical MSR protocol mixes, measured on the same 6-core host
with the same real 1080p60/8 Mbps fixture (30 audio tracks, `source+atrack:N`
per output) used throughout
[the SRT egress scale investigation](srt-egress-scale-investigation-2026-08-10.md).
That document root-causes and fixes the correctness/performance bugs found
along the way; this one explains the resulting steady-state footprint.

## Contents

- [Method](#method)
- [Headline numbers](#headline-numbers)
- [Thread count: exact, verified accounting](#thread-count-exact-verified-accounting)
- [Memory: dominant cost is per-connection, not per-feed](#memory-dominant-cost-is-per-connection-not-per-feed)
- [CPU: proportional to active egress-shard thread count, not output count](#cpu-proportional-to-active-egress-shard-thread-count-not-output-count)
- [Summary](#summary)
- [Efficiency evaluation](#efficiency-evaluation)

## Method

Each mix was run via `msr` (`MSR_PEER=sink`, single checkpoint at
`MSR_OUTPUT_COUNTS=1200`) so the numbers below are one clean n=1,200
snapshot per mix, not an average across a ramp. Sink peers (the receiving
side) are excluded from all figures — every number here is the main
restream engine process only, identified among the sink processes (all
named `restream`) by the absence of `RESTREAM_SINK_MODE` in its environment.
Threads were counted by reading `/proc/<pid>/task/*/comm` directly and
grouping by name with trailing digits stripped (e.g. `egress-shard-17` and
`egress-shard-142` both count as `egress-shard`); memory is `VmRSS` from
`/proc/<pid>/status` cross-checked against the harness's own categorized
sample (`sourceRingKb`/`transcoderRingKb`/`tsmuxRingKb`/`retainedKb`/
`unattributedKb` in `msr-samples.jsonl`). Per-feed output distributions came
from `/api/v1/engine/health` while each stack was live.

## Headline numbers

| Mix | CPU (of 6 cores) | RSS | Threads |
|---|---:|---:|---:|
| `srt-only` (1,200 SRT / 0 RTMP) | 2.9 (293-294%) | 4.11 GB | 214 |
| `rtmp-only` (0 SRT / 1,200 RTMP) | ~2.4 (241%) | 1.51 GB | 55 |
| canonical 95/5 (1,140 RTMP / 60 SRT) | ~3.2 (323%) | 1.97 GB | 223 |

## Thread count: exact, verified accounting

Every count below was read directly from the live process; the totals sum
to exactly the measured `Threads:` value in `/proc/<pid>/status` for all
three mixes with zero unaccounted threads.

### srt-only — 214 threads

| Source | Count |
|---|---:|
| `egress-shard-*` | 180 |
| `SRT:SndQ:w*` | 7 |
| `SRT:RcvQ:w*` | 7 |
| `sqlx-sqlite-wor*` | 10 |
| `restream-tokio-*` | 5 |
| `SRT:TsbPd` | 2 |
| `restream` (main), `tracing-appende*`, `SRT:GC` | 1 each |
| **Total** | **214** |

`egress-shard-*` (180): every SRT output selects one audio track
(`source+atrack:N`, N = 0-29), and an SRT egress `FeedId` is keyed by
`(protocol, pipeline, encoding)` — so each distinct track selection is its
own feed, each with its own independent shard pool. `srt-only` assigns
every ordinal to SRT unconditionally, so all 30 tracks are populated: 30
feeds. Every SRT feed's shard pool uses the `SrtCpuParallel` profile
(`src/config.rs`, `target_egress_fabric_shards`), which **always** claims
`cpu_max` shards (6 on this host) regardless of how many outputs land on
that feed — the design rationale (`docs/egress-architecture.md`) is that
SRT shard count is a libsrt-multiplexer-parallelism budget bounded by CPU,
not a per-output cost like RTMP's. 30 feeds × 6 shards/feed = 180 dedicated
OS threads (`EgressShardHandle::spawn`, one thread per shard,
unconditionally, independent of protocol).

`SRT:SndQ`/`SRT:RcvQ` (7 + 7 = 14): libsrt spawns exactly one send-queue and
one receive-queue worker thread per **multiplexer** (one bound local UDP
endpoint), never per connection or per feed
(`CUDTUnited::updateMux`/`CSndQueue::init`/`CRcvQueue::init` in the vendored
libsrt source — see the parent investigation doc's "The feed / shard /
thread / multiplexer relationship" section). Local-port reuse
(`SrtEgressMuxerPorts`, keyed by `ShardId` alone) means shard *N* of every
one of the 30 feeds shares **one** multiplexer, so egress needs only 6
multiplexers total (one per shard ID) — plus 1 more for the SRT ingest
listener's own multiplexer, which is entirely separate. 7 multiplexers × 2
threads = 14.

`sqlx-sqlite-wor*` (10): SQLite connection pool worker threads. Fixed pool
size, independent of output count or protocol mix — this is baseline
process overhead, not something that scales with the workload.

`restream-tokio-*` (5): the async runtime's worker threads
(`default_tokio_worker_threads(6)` = 2) plus a small number of on-demand
blocking-pool threads (`tokio::task::spawn_blocking` callers — e.g. the SRT
ingest listener's `sink_discard_loop`-style blocking work). Not scale-driven
by output count in this range.

`SRT:TsbPd` (2): time-stamped packet delivery threads, one per **ingest**
connection needing live-mode delivery timing. Egress leaves do not each get
one of these (confirmed by `rtmp-only`, which has the same ingest and the
same 2 `TsbPd` threads with zero egress SRT connections at all) — this cost
is ingest-side only and invariant across all three mixes.

### rtmp-only — 55 threads

| Source | Count |
|---|---:|
| `egress-shard-*` | 33 |
| `sqlx-sqlite-wor*` | 10 |
| `restream-tokio-*` | 5 |
| `SRT:TsbPd` | 2 |
| `SRT:SndQ:w*`, `SRT:RcvQ:w*` (ingest multiplexer only) | 1 each |
| `restream`, `tracing-appende*`, `SRT:GC` | 1 each |
| **Total** | **55** |

`egress-shard-*` (33): RTMP egress uses the `OutputCount` shard profile
(`shard_count = ceil(outputs_on_feed / 128)`, capped at `cpu_max`=6), the
opposite design choice from SRT — RTMP's marginal per-connection cost is
low (plain TCP, no per-multiplexer thread model, no hard delivery
deadline), so shards exist only to spread `epoll_wait` overhead, not to buy
parallelism. 30 distinct RTMP feeds (same one-per-audio-track structure as
SRT) with live per-feed output counts (largest to smallest, from
`/api/v1/engine/health`):

```
285, 143, 95, 71, 57, 48, 41, 36, 31, 29,
25, 24, 22, 20, 19, 18, 17, 16, 15, 14,
14, 13, 13, 12, 12, 11, 10, 10, 10, 9
```

Only the top two feeds exceed the 128-output-per-shard threshold: `ceil(285/128)=3`,
`ceil(143/128)=2`; every other feed (≤95 outputs) gets exactly 1 shard.
3 + 2 + 28×1 = 33, matching the measured count exactly.

`SRT:SndQ`/`SRT:RcvQ` (1 + 1): zero SRT egress connections exist in this
mix, so the only SRT multiplexer at all is the ingest listener's — the SRT
multiplexer/thread cost is purely a function of egress SRT connection
count and disappears entirely with `rtmp-only`.

Everything else (sqlx, tokio, TsbPd, misc) matches `srt-only` exactly —
confirming these are fixed baseline costs, not scale- or protocol-driven.

### canonical 95/5 — 223 threads

| Source | Count |
|---|---:|
| `egress-shard-*` | 189 |
| `SRT:SndQ:w*` | 7 |
| `SRT:RcvQ:w*` | 7 |
| `sqlx-sqlite-wor*` | 10 |
| `restream-tokio-*` | 5 |
| `SRT:TsbPd` | 2 |
| `restream`, `tracing-appende*`, `SRT:GC` | 1 each |
| **Total** | **223** |

`egress-shard-*` (189 = 156 SRT + 33 RTMP): the canonical mix assigns every
20th ordinal to SRT (`MsrProtocolMix::Canonical`), independent of rank/track
boundary. Live query confirmed only **26 of 30** tracks actually received
an SRT output — the four smallest ranks (10-11 outputs each) happened not
to contain a multiple of 20 in their ordinal range. 26 SRT feeds × 6 shards
= 156. The RTMP side reproduces the same 30-feed, `[285,143,95,...]`-shaped
distribution as `rtmp-only` (95% of the same Zipf population), giving the
same 33 RTMP shards. 156 + 33 = 189.

The libsrt multiplexer count (7+7=14) is identical to `srt-only`'s, **not**
scaled down for having only 26 populated feeds instead of 30 — because
multiplexers are keyed by `ShardId` alone (shared across every feed on that
shard), the cost is `cpu_max` (6) plus 1 for ingest, regardless of how many
distinct SRT feeds exist, as long as at least one does.

**Architectural takeaway**: SRT egress thread cost scales with *distinct
audio-track selections* (feed count) × a fixed CPU-sized shard budget,
completely decoupled from how many outputs share a track. RTMP thread cost
scales with *output concentration per track* (`ceil(count/128)` summed
across feeds) and is far cheaper in aggregate for the same total output
count, because most tracks stay under the 128-output-per-shard threshold
and cost exactly one shard each regardless of exact count.

## Memory: dominant cost is per-connection, not per-feed

The harness's own categorized sample (`sourceRingKb` + `transcoderRingKb` +
`tsmuxRingKb` = `retainedKb`, confirmed to sum exactly in all three
samples) accounts for the **shared, per-feed** ring buffers — small in
absolute terms because there are only 26-30 of them, not 1,200:

| Mix | `retainedKb` (rings) | `rssKb` | Rings as % of RSS |
|---|---:|---:|---:|
| srt-only | 118,971 KB (~116 MB) | 4,109,888 KB (~4.11 GB) | 2.9% |
| rtmp-only | 107,014 KB (~105 MB) | 1,514,460 KB (~1.51 GB) | 6.9% |
| canonical | 215,814 KB (~211 MB) | 1,965,872 KB (~1.97 GB) | 10.7% |

The other 89-97% (`unattributedKb`, `rssKb` minus the categorized rings
minus the small clean/shared portions) is **per-connection** overhead that
scales with the 1,200 output count, not the ~30 feed count. Dividing by
output count gives a rough per-connection cost:

| Mix | `unattributedKb` | Outputs | Per-output |
|---|---:|---:|---:|
| srt-only | 3,990,917 KB | 1,200 (all SRT) | ~3.33 MB |
| rtmp-only | 1,407,446 KB | 1,200 (all RTMP) | ~1.17 MB |
| canonical | 1,750,058 KB | 1,140 RTMP + 60 SRT | ~1.46 MB blended |

SRT costs roughly **2.8x** more resident memory per connection than RTMP.
The live egress-socket config log line explains most of the gap directly:

```
[srt] egress config: latency=250ms lossmaxttl=256 UDP snd=8192KB rcv=1024KB,
SRT snd=6102KB rcv=1023KB, FC=32768
```

`SRT snd=6102KB` is libsrt's own userspace send buffer
(`CSndBuffer`, sized from `DESIRED_SRT_BUF`/`SRTO_SNDBUF`,
`src/media/srt/socket.rs`) allocated per socket — a real per-connection
heap allocation that counts toward process RSS. `UDP snd=8192KB` is a
kernel-level `SO_SNDBUF` socket buffer and does **not** count toward
process RSS (kernel-owned, not mapped into the process's own page tables),
so it does not appear in these numbers despite being requested per socket
too. The measured ~3.33 MB/connection average sits below the 6.1 MB
configured ceiling because live buffer occupancy fluctuates rather than
staying pegged at capacity (matches `quality.msSendBuf` telemetry observed
elsewhere in the parent investigation running well under the configured
buffer most of the time). RTMP egress has no equivalent large per-socket
userspace buffer — it streams through the OS's own TCP send buffer
(kernel-owned, same non-RSS accounting) plus per-leaf application-level
bookkeeping (`LeafCommon`, AVIO queue headroom) an order of magnitude
smaller.

canonical's blended ~1.46 MB/output lands close to but above the
output-weighted prediction from the two pure mixes
(`(1140×1.17 + 60×3.33) / 1200 ≈ 1.28 MB`) — the ~14% gap is consistent
with, but not conclusively isolated to, the extra per-shard/per-feed fixed
overhead from having both SRT and RTMP shard pools live simultaneously
(189 vs. either pure mix's shard count) rather than a clean linear
per-connection sum. Not further decomposed here; the two pure-mix numbers
above are the reliable per-protocol figures.

## CPU: proportional to active egress-shard thread count, not output count

CPU percentage (of 6 cores, from the harness's own `restreamCpuPct`, a
snapshot rather than a sustained average) tracks total thread count more
than output count:

| Mix | Threads | CPU (of 6 cores) | CPU per 100 threads |
|---|---:|---:|---:|
| rtmp-only | 55 | ~2.4 | ~4.4 |
| srt-only | 214 | 2.9 (293-294%, harness aggregate) | ~1.4 |
| canonical | 223 | ~3.2 | ~1.4 |

This is expected, not a red flag: most of the 180-189 SRT/RTMP shard
threads in the SRT-heavy mixes spend most of their time idle-waiting
(`recv_timeout` on an empty command channel, per `EgressShardRuntime::run`
in `src/media/egress/shard.rs`) rather than burning CPU — shard count is
sized for libsrt-multiplexer parallelism and epoll-fan-out headroom, not
because every shard is continuously busy. `rtmp-only`'s higher CPU-per-
thread ratio reflects that its far smaller 33-shard pool is working
harder per thread to push the same 1,200-output aggregate bitrate through
TCP's own flow control, versus SRT's larger, more idle-headroom shard
pool per connection.

## Summary

| Cost center | Scales with | srt-only | rtmp-only | canonical |
|---|---|---:|---:|---:|
| `egress-shard` threads | distinct feeds × per-feed shard formula | 180 | 33 | 189 |
| libsrt multiplexer threads | `cpu_max` + 1, only if any SRT egress exists | 14 | 2 | 14 |
| Fixed baseline threads (DB pool, tokio, ingest TsbPd, misc) | host config, not output count | 20 | 20 | 20 |
| Ring buffer memory | feed count (26-30) | ~116 MB | ~105 MB | ~211 MB |
| Per-connection memory | output count × protocol | ~3.33 MB/SRT | ~1.17 MB/RTMP | blended |

The practical implication for capacity planning: **SRT egress cost in this
architecture is driven primarily by how many distinct audio/quality-tier
selections (feeds) exist, not by total SRT output count** — 26-30 feeds
cost the same 180-ish shard threads whether each feed carries 1 output or
50. RTMP cost is driven by *concentration* per feed and stays cheap as
long as no single feed's output count crosses the 128-per-shard threshold.
For real Mahashivratri-shaped traffic (a Zipf-distributed audience across
30 languages, 95% RTMP), this reorganized run measured 223 threads and
1.97 GB RSS at the full 1,200-output target — both well within a
single 6-core, 12 GB host's headroom, but a much larger track catalog
(hundreds of distinct SRT selections rather than 30) would scale SRT's
thread cost linearly in feed count even at low per-feed output counts,
independent of total connection count.

## Efficiency evaluation

Is this footprint minimal, or is there slack worth removing? Two separate
findings, with different confidence and different recommended action.

### Not minimal: per-feed SRT shard pools over-provision small feeds

The dominant thread cost in every mix that includes SRT is
`egress-shard` threads for SRT feeds: 180 of 214 threads in `srt-only`, 156
of 189 egress-shard threads in canonical. Each of the 26-30 distinct SRT
feeds gets its own independent shard pool sized at the full CPU-derived
ceiling (`EgressShardProfile::SrtCpuParallel`,
`target_egress_fabric_shards` in `src/config.rs`) — including feeds with as
few as 9-11 outputs (the smallest canonical-mix ranks), which get exactly
the same 6 dedicated shard threads as a 285-output feed. This is genuine
over-provisioning: the CPU-per-100-threads figures above already show most
SRT shard threads sitting mostly idle, and a 9-output feed cannot plausibly
need 6 dedicated OS threads' worth of libsrt-multiplexer parallelism.

The size of the win is real but bounded by what's actually safe to remove.
Each idle Tokio-adjacent OS thread costs a kernel thread-stack reservation
plus scheduler bookkeeping (single-digit KB to low hundreds of KB of
resident stack depending on actual usage, not the full 2-8 MB reserved
address-space default) and one more `epoll_wait` participant — real but
small next to the ~4-6 MB CSndBuffer per SRT *connection* that dominates
measured RSS. The likely payoff of fixing this is a meaningfully smaller
thread count (better process/scheduler hygiene, clearer `htop`/telemetry
signal, lower baseline CPU from fewer idle-wake cycles) more than a large
RSS reduction.

**Why this was not changed in this session**: `EgressShardProfile::SrtCpuParallel`
being output-count-*independent* is not an oversight — it is the fix,
proptested (`srt_cpu_parallel_target_is_cpu_ceiling_regardless_of_outputs`,
`src/config/tests/configuration_behavior.rs`), for a real, previously-shipped
bug: an earlier output-count-scaled SRT shard formula capped a ~60-output
SRT feed at 1 shard / 1 libsrt multiplexer, and that one multiplexer's
`CSndQueue` thread became a hard bottleneck past roughly 120 concurrent SRT
egress connections, causing continuous `TLPKTDROP` (see
[the SRT egress scale investigation](srt-egress-scale-investigation-2026-08-10.md)).
Two directions could reduce the current footprint without reopening that
bug, and both are real hot-path/lifecycle changes requiring the full proof
ladder (deterministic unit tests, the existing proptests re-verified against
the new formula, a live harness run at real bitrate re-proving zero
`TLPKTDROP`/overrun events, and a benchmark) before being trustworthy:

1. **Give `SrtCpuParallel` its own, much smaller output-per-shard threshold**
   than RTMP's 128 (informed by the ~120-connection failure point observed
   above — a threshold well under that, with margin, applied per shard
   rather than per feed) instead of being fully output-count-independent.
   Lower risk to implement (one pure function, already unit- and
   proptest-covered), but directly touches the exact formula whose previous
   version caused the original incident, so the new threshold and its
   safety margin would need explicit justification, not just "smaller
   than 128."
2. **Share one shard pool per protocol across all feeds** (matching how the
   libsrt egress multiplexer layer already shares per shard ID across
   feeds, `src/media/egress/backends/srt/muxer_ports.rs`) instead of
   instantiating a new pool per feed. This is architecturally consistent
   with the multiplexer layer's own design and would cut SRT egress shard
   threads from `feed_count × cpu_max` to just `cpu_max` regardless of how
   many distinct feeds exist — a much larger win at high feed-catalog
   counts — but is a materially larger change (each `EgressFabricRuntime`
   is currently instantiated per `(protocol, feed)`; sharing a pool means a
   shard must be able to service leaves from multiple feeds, which the
   `EgressShard`'s `feeds: FeedSubscriptions` field already models, but the
   manager/runtime instantiation call sites do not currently exercise).

Neither direction is implemented here. This session's mandate and budget
went to proving correctness and real-bitrate scale for all three mixes and
documenting the resulting footprint truthfully; picking and shipping one of
the above requires its own scoped session with the full concurrency proof
ladder, not a follow-on edit appended to this one.

### Already minimal: everything else measured

The remaining cost centers do not show comparable slack:

- **Fixed baseline threads (~20)**: sqlx pool size, Tokio worker count, and
  ingest `TsbPd` threads are either fixed pool sizes tuned elsewhere or
  scale with live connection count, not with output count — nothing here
  is MSR-output-driven bloat.
- **RTMP egress-shard threads (33)**: `OutputCount` profile already
  amortizes shard cost across up to 128 outputs per shard; the measured
  33 threads for 1,200 RTMP outputs (or 1,140 in canonical) is close to
  the theoretical minimum `sum(ceil(feed_outputs / 128))` for this
  population's actual shape (two feeds exceed 128, the rest sit at exactly
  1 shard each — there is no unused per-feed headroom to remove).
- **Per-connection SRT memory (~6 MB `CSndBuffer`/socket)**: already
  configurable per-destination via the `sndbuf=` URL parameter
  (`docs/configuration.md` § "Recognized SRT egress URL parameters") for
  operators who know their real bitrate needs less headroom; the default
  is a worst-case-bitrate formula, not a fixed overallocation independent
  of configured link parameters.
- **Ring buffers (`source_ring`/`output_ring`/`TsChunkRing`)**: shared once
  per pipeline/stage regardless of destination count, and already the
  smallest measured contributor (2.9-10.7% of RSS across all three mixes).

Net assessment: the current design's per-connection and per-feed-population
costs are close to minimal for what each destination protocol structurally
requires. The one substantiated inefficiency — SRT egress-shard thread
count scaling with distinct feed count rather than actual load — is real,
bounded in expected payoff, and deliberately left unfixed here because a
correct fix requires the same proof rigor that the two bugs this
investigation already found and fixed both required, and this session's
scope was documentation, not another hot-path change.
