# SRT Egress Correctness-at-Scale Investigation - 2026-08-10

Status: **three root causes found, fixed, and evidenced; the architectural
scalability ceiling fixed and proven live; the bitrate ladder re-run with
corrected numbers after a harness-measurement bug was discovered.** This
report documents the full investigation, not just the conclusion, because
several early hypotheses turned out to be wrong in informative ways.

2026-08-11 update: the sweep's "720p" transcode tier was running in x264 CRF
mode (constant quality), silently emitting ~19.3 Mbps per output instead of
the ladder's nominal 1.5M — every pre-2026-08-11 bitrate/connection-count
number in this report is invalidated by that factor. See
[The 720p tier was in CRF mode](#the-720p-tier-was-in-crf-mode) and the
corrected [Bitrate / connection-count bracketing evidence](#bitrate--connection-count-bracketing-evidence).

Worktree: `.local/worktrees/msr-1080p-investigation`, branch
`codex/msr-1080p-investigation`.

## Contents

- [Objective](#objective)
- [How this started](#how-this-started)
- [Root cause 1: every leaf started at sequence zero](#root-cause-1-every-leaf-started-at-sequence-zero)
- [Root cause 2: overrun recovery rewound to the worst possible point](#root-cause-2-overrun-recovery-rewound-to-the-worst-possible-point)
- [Fix: prime and resync to the live edge, not the oldest retained sequence](#fix-prime-and-resync-to-the-live-edge-not-the-oldest-retained-sequence)
- [Retry jitter: added, but not evidenced as necessary](#retry-jitter-added-but-not-evidenced-as-necessary)
- [Proving TLPKTDROP was actually firing](#proving-tlpktdrop-was-actually-firing)
- [Why SRT and not RTMP, even at far lower connection counts](#why-srt-and-not-rtmp-even-at-far-lower-connection-counts)
- [Passthrough fails exactly as often as transcoded output](#passthrough-fails-exactly-as-often-as-transcoded-output)
- [CPU and scheduling evidence](#cpu-and-scheduling-evidence)
- [The feed / shard / thread / multiplexer relationship](#the-feed--shard--thread--multiplexer-relationship)
- [Fix: scope libsrt multiplexer reuse per shard](#fix-scope-libsrt-multiplexer-reuse-per-shard)
- [Why the first verification of that fix looked like a failure](#why-the-first-verification-of-that-fix-looked-like-a-failure)
- [The real scalability ceiling: OUTPUTS_PER_SHARD is RTMP-shaped](#the-real-scalability-ceiling-outputs_per_shard-is-rtmp-shaped)
- [Bitrate / connection-count bracketing evidence](#bitrate--connection-count-bracketing-evidence)
- [The 720p tier was in CRF mode](#the-720p-tier-was-in-crf-mode)
- [What's fixed, what's evidenced-but-unproven, what's still open](#whats-fixed-whats-evidenced-but-unproven-whats-still-open)
- [Artifact index](#artifact-index)

## Objective

Make restream's SRT egress genuinely correct at real Mahashivratri (MSR)
production scale: 1,200 outputs, 95% RTMP / 5% SRT, real 1080p bitrate
(1.5 Mbps-8 Mbps envelope, 25/30/50/60fps, up to 30 audio tracks), not just at
the synthetic low-bitrate fixture the project's own `msr` test-harness mode
normally uses. The design also has to scale to a bigger VPS than the one this
investigation ran on (6 cores) — a fix that only helps because this specific
box is small is not sufficient.

## How this started

A real 1080p / 30-language source run through the compiled `msr` harness
failed its own ffprobe-based correctness check at the n=600->900 checkpoint of
the canonical 1,200-output ramp: an SRT output was connection-healthy
(`bytesReceived` growing) but delivering **zero video packets**. The identical
harness passed cleanly at the synthetic low-bitrate fixture through the full
1,200-output ramp with zero failures (see
`msr-final-report-2026-07-12.md` in this directory: `rtmp:1140,srt:60` at
n=1200, `1200/1200` MediaMTX-ready, zero warn/error/panic).

That contrast — passes at low bitrate, fails at real bitrate, at the same
connection count — was the first real signal: this is a real,
bitrate-and-scale-dependent bug in the SRT egress path, not a benchmark
artifact.

Reproduction moved to `bitrate_sweep` (`src/bin/test_harness/resource_sweep/bitrate.rs`),
an already-committed, CI-wired harness mode that publishes a synthetic h264
source at a controlled bitrate and creates `RtmpSource` / `Rtmp720p` /
`SrtSource` / `Srt720p` outputs per "group," rather than the uncommitted real
1080p fixture — cheaper, faster, and license-clean.

## Root cause 1: every leaf started at sequence zero

`LeafCommon::new` (`src/media/egress/leaf.rs`) hardcoded every new leaf's read
cursor to `FeedCursor::new(0, 0)`. Two other consumers of the exact same
`EgressFeed` abstraction already did this correctly:
`sink.rs`'s `start_sink_egress` and `recirculation.rs` both start at
`feed.head_sequence()` (the live edge).

Because a leaf started at sequence 0, and any established pipeline's shared
ring (`TsChunkRing`, `tsRingCapacity` default 256 packets) has almost always
already advanced far past 256 packets by the time an output is created, this
made **every SRT leaf's first-ever read a guaranteed `FeedRead::Overrun`** —
confirmed live: 29-30 out of 30 SRT leaves logged exactly one
`"egress feed overrun: leaf resynchronized to latest sync point"` WARN at
startup, in every pre-fix run. 0% of RTMP leaves ever did (RTMP uses a
4x-larger ring, `ringCapacity` 1024 vs `tsRingCapacity` 256, and connects
faster — same underlying bug, far less exposed).

## Root cause 2: overrun recovery rewound to the worst possible point

The overrun handler already existed and looked reasonable:

```rust
fn resync_cursor<F: EgressFeed>(feed: &F) -> FeedCursor {
    feed.latest_sync_point()
        .unwrap_or_else(|| FeedCursor::new(feed.epoch(), feed.oldest_sequence()))
}
```

The fallback (used whenever no keyframe is retained in the ring — the common
case at real framerate/bitrate, since 256 packets is only ~3.3s of retention
at a single track and far less at multi-track) was `oldest_sequence()` — the
**maximum possible backward rewind**, landing mid-GOP one slot from being
overwritten again. Two other, independent implementations of the same
overrun-recovery concept (`sink.rs:191-195`, `recirculation.rs:204-209`)
both fell back to `head_sequence()` (the live edge, no rewind) instead, with
no comment anywhere justifying why the fabric path did the opposite.

Because every leaf's guaranteed first-read overrun (root cause 1) fed into
this fallback, and a brand-new leaf's target feed is essentially always
already far more than 256 packets past sequence 0, this combination produced
a large, mid-GOP backward rewind on effectively every SRT leaf's first read.
Once behind by that much, the leaf reads/sends flat-out to catch up — feeding
libsrt's send buffer (up to 6.25 MB by default, see
`src/media/srt/buffer_sizing.rs`) far faster than its 250ms
`SRTO_PEERLATENCY` deadline can drain — and libsrt's live-mode `TLPKTDROP`
(on by default, `SNDDROPDELAY` 0, never overridden anywhere in `src/`)
silently, permanently discards whatever misses that deadline.

## Fix: prime and resync to the live edge, not the oldest retained sequence

Implemented in `src/media/egress/leaf.rs` and `src/media/egress/visit.rs`:

- Added `cursor_primed: bool` to `LeafCommon`. `EngineVisit::run()` primes a
  leaf's cursor via the (renamed) `live_start_cursor(feed)` helper on its
  first visit, before the engine's first read, instead of leaving it at
  `(0, 0)`.
- `live_start_cursor`'s fallback (no retained keyframe) changed from
  `oldest_sequence()` to `head_sequence()`, matching the two other existing
  implementations and `RingBuffer::fast_forward`'s own documented contract.
- `EngineProgress::HandshakeComplete` (RTMP only) re-anchors the cursor,
  since handshake/negotiation states never read the feed and the anchor
  taken on first visit would otherwise age across the whole connect round
  trip.
- `srt_drain.rs` / `rtmp_shard_drain.rs`'s `feed_lag_units` telemetry was
  fixed to report `0` for an unprimed leaf instead of "the entire feed" —
  it hadn't started, so it isn't behind.

Proof: new unit tests against the real `TsFeed`/`SrtFabricLeaf` types (not
fakes) in `src/media/egress/backends/srt/tests/leaf.rs` and
`src/media/egress/visit_tests.rs`, plus a new `wrapped_feed()` test helper
that simulates an already-wrapped, established ring. `cargo test --lib`:
1972/1972 passed. `scripts/check/concurrency/fast.sh`: clean.

**Live proof, isolated from every other change**: at 30 concurrent SRT
connections, zero overrun/resync events occurred post-fix, versus 29-30/30
pre-fix. The mechanism this fix targets is confirmed eliminated.

## Retry jitter: added, but not evidenced as necessary

`OutputRetryPolicy::backoff_ms` (`src/application/reconcile.rs`) was pure
`base_ms * 2^retries`, no randomization — a plausible thundering-herd
contributor if many outputs fail their first dispatch attempt in the same
burst window and then all retry in a synchronized second wave. Added equal
jitter (half the computed delay fixed, half spread deterministically via a
hash of `(output_id, retries)`, stable across repeated polls of the same
failure). Proof: unit tests plus a property test
(`jitter_desynchronizes_a_burst_of_same_retry_outputs`) that directly measures
the concentration of a simulated burst's retry times, not just that two
outputs differ.

**Honest caveat**: `CommandChannelFull` (the failure mode this jitter fix
targets) was never observed at any point in this entire investigation, at any
scale, with or without the fix. `bitrate_sweep` creates outputs serially, not
via the ~32-concurrent-worker burst the `msr` harness uses, so it structurally
cannot exercise this path. This fix is logically sound and independently
tested, but is **not evidenced as necessary for the bug this investigation
reproduced** — it remains open whether it matters for the real MSR harness's
burst-creation shape.

## Proving TLPKTDROP was actually firing

Everything above was inferred from indirect signals (decode errors,
timeouts) until directly checked. `GET /api/v1/engine/health` (session-cookie
auth, `POST /api/v1/auth/login` with the harness default password) exposes
per-output `quality.packetsSentDrop` — libsrt's real `pktSndDropTotal`
counter — via `src/media/srt_quality.rs`.

Polled live during a 120-SRT-connection/8Mbps run:

- **120/120 SRT outputs**: massive nonzero drops, 80,828 to 233,465+
  accumulated, 702-3,187/sec ongoing.
- **0/120 RTMP outputs**: zero drops.
- Same snapshot: `quality.msSendBuf` (buffer occupancy) 957-1020ms while
  `quality.msSendTsbPdDelay` (configured deadline) is 250ms — the send buffer
  was chronically ~4x fuller than the delivery window allows.

This is direct confirmation, not inference: TLPKTDROP was firing continuously
and at high volume.

## Why SRT and not RTMP, even at far lower connection counts

RTMP runs over TCP: a scheduling delay just delays delivery, the kernel send
buffer holds the data, nothing is lost. SRT's TSBPD
(timestamp-based packet delivery) enforces a hard deadline
(`SRTO_LATENCY`, 250ms here), and TLPKTDROP actively, permanently discards
whatever misses it — a deliberate low-latency-over-completeness protocol
design, not a restream defect. This asymmetry, not raw scale, is why 30 SRT
connections could show real failures while 1,200 RTMP connections (see the
2026-07-12 report) never did.

## Passthrough fails exactly as often as transcoded output

At 120 connections, `SrtSource` (zero-transcode passthrough) failed 59/60,
essentially matching `Srt720p` (transcoded) at 60/60. This rules out encoding
cost as the driver — directly relevant since MSR's real SRT slice is
predominantly passthrough.

## CPU and scheduling evidence

`perf_event_paranoid` was temporarily lowered from 4 to 1 (root, via `sudo`,
restored to 4 immediately after capture) to allow profiling.

- `mpstat -P ALL`: all 6 cores at ~82-88% busy during a 120-connection run —
  not idle, not one hot core, but not fully saturated either. A large
  `%sys`/`%soft` component reflects real kernel-side network-stack cost from
  many real UDP/SRT flows.
- `runqlat-bpfcc -p <restream_pid> 30 1`: real thread scheduling-queue waits
  up to 131-262ms on restream's own threads — by itself enough to exceed the
  250ms SRT deadline before a thread even starts its send work.
- Per-thread CPU (`/proc/<pid>/task/*/stat`, names via `comm`): the hottest
  thread in the entire process was `SRT:SndQ:w2` — a libsrt-internal send-
  queue worker thread, not a restream shard thread — while its sibling
  `SRT:SndQ:w1` sat nearly idle. Total OS thread count was only 24 (not
  one-per-output; `srt_egress_reuse_local_port`, default on, was already
  working to keep raw thread count low).

## The feed / shard / thread / multiplexer relationship

This took several rounds of code reading plus small, fast ad-hoc experiments
(not the full harness) to resolve correctly; two earlier hypotheses were
wrong.

**libsrt does not pool worker threads.** Confirmed against the vendored
source (`.local/build/static/src/srt/srtcore/api.cpp`,
`srtcore/queue.cpp`): `CUDTUnited::updateMux` creates exactly one
`CMultiplexer` per bound local UDP endpoint; only the multiplexer-creation
path calls `CSndQueue::init` / `CRcvQueue::init`, spawning exactly one send
thread and one receive thread per multiplexer, named from a
**process-global** counter (`SRT:SndQ:wN` means "the Nth multiplexer this
process ever created," not "worker N of a pool"). The observed `w1`
(near-idle) was the SRT **ingest** listener's own multiplexer, bound at
startup; the observed `w2` (hot) was the single multiplexer every egress
socket shared, because `MediaEngine::srt_egress_muxer_port_handle()`
(pre-fix) returned clones of one engine-wide `Arc<Mutex<Option<u16>>>` —
every egress connection, across every feed, learned and reused the same
local port.

**Each `FeedId` (protocol x pipeline x quality tier — e.g.
`srt:pipeline_X:source`, `rtmp:pipeline_X:video:720p:codec:h264`) owns an
independent `EgressFabricRuntime` with an independent shard pool.** Shard
count for one feed is `clamp(ceil(outputs_on_that_feed / 128), 1, cpu_max)`
(`target_egress_fabric_shards`, `src/config.rs`) — dynamic, grown via
`EgressFabricRuntime::rescale` as outputs are added to that specific feed.
Each shard is a genuinely separate OS thread
(`EgressShardHandle::spawn`, name `egress-{shard_id}` ->
`Display for ShardId` -> `"shard-{n}"`).

Confirmed empirically with small, fast, ad-hoc experiments (`bitrate_sweep`
with tiny `BITRATE_SWEEP_OUTPUT_GROUPS` and `BITRATE_SWEEP_STABILIZE_SECS=1`,
checking `/proc/<pid>/task/*/comm` immediately rather than waiting for the
full harness):

| Outputs per feed | Shards per feed (observed) | Egress multiplexers (observed) |
|---:|---:|---:|
| 2 | 1 | 1 (shared by every feed's shard-0) |
| 60 | 1 | 1 |
| 135 | 2 | 2 (shard-0 shared across feeds, shard-1 shared across feeds) |

## Fix: scope libsrt multiplexer reuse per shard

`src/media/egress/backends/srt/muxer_ports.rs` (new): `SrtEgressMuxerPorts`,
an engine-wide `Arc<Mutex<HashMap<ShardId, Arc<Mutex<Option<u16>>>>>>` with
lazily-created per-shard entries. `claim_srt_egress_muxer_port`
(`src/media/srt/egress_connect.rs`, pre-existing) is reused unchanged, just
called with per-shard state instead of one engine-wide state. Wired through
`srt_fabric_shard_backends_with_poller` (`src/media/egress/factory.rs`) and
the dynamic-rescale path (`rescale_srt_fabric`,
`src/media/engine_egress_fabric.rs`) — both already looped over `ShardId`,
just weren't using it for this. Keyed by `ShardId` alone (not
`(FeedId, ShardId)`): shard *N* of every feed shares one multiplexer, so
libsrt thread count tracks shard count, not feed count.

Also added: stale-port recovery (`SrtEgressMuxerPortClaim::forget_stale_port`)
since per-shard scoping makes a recorded port more likely to go stale when a
shard shrinks away and regrows.

Proof: unit tests (`muxer_ports.rs`'s own module, `factory/tests.rs`,
`engine_tests/egress_fabric.rs`), `cargo test --lib`: 1972/1972,
`scripts/check/concurrency/fast.sh`: clean (new step added:
`lib-srt-egress-muxer-port-shard-scoping`).

## Why the first verification of that fix looked like a failure

The first live check reused the same 60-outputs-per-feed scenario the
correctness bug was originally reproduced at, and found `packetsSentDrop`
essentially unchanged (335K-348K accumulated, spread across every shard
instead of concentrated on one). This looked like the fix had failed.

It hadn't — the test scale was wrong for it. 60 outputs per feed is below the
128-outputs-per-shard threshold (see above), so that feed's shard pool never
grows past 1 shard regardless of the fix, meaning there was still only one
egress multiplexer in play. The fix was correctly implemented but never
actually exercised by that test. Confirmed by rerunning at 135 outputs per
feed (crosses the threshold): shard count correctly grew to 2, multiplexer
count correctly grew to 2, exactly as designed.

**This is itself the headline finding for the "must scale to 1,200" ask**:
see the next section.

## The real scalability ceiling: OUTPUTS_PER_SHARD is RTMP-shaped

`OUTPUTS_PER_SHARD = 128` (`src/config.rs`) sizes shard growth for RTMP,
where the marginal cost per connection is low (no libsrt-style
one-multiplexer-per-thread-pair architecture, no hard 250ms deadline). MSR's
real shape is 95% RTMP / 5% SRT — at n=1,200 that's `rtmp:1140, srt:60` (see
`msr-final-report-2026-07-12.md`). If those 60 SRT outputs land on one feed
(plausible: many are the same `source` quality tier with different
track selections), **that feed is permanently capped at 1 shard and 1
egress multiplexer, on any VPS, regardless of core count** — it never
crosses 128. A bigger VPS does not fix this on its own; the formula itself
is the ceiling.

**Not yet implemented.** The fix direction: make the shard-count formula
protocol-aware. SRT's real constraint is libsrt's per-multiplexer threading
model under a hard real-time deadline, not raw connection count the way the
RTMP-shaped 128 threshold assumes — either give SRT feeds a much lower
per-shard output threshold, or have SRT egress simply always claim
`cpu_max` shards regardless of output count (the point of scaling SRT shards
is multiplexer parallelism, and CPU count is the natural ceiling for that,
independent of how many outputs happen to land on one feed).

**Implemented and live-proven (2026-08-11)**: `OUTPUTS_PER_SHARD` is now
protocol-aware — SRT feeds use a lower per-shard output threshold than RTMP
so shard (and therefore multiplexer) count tracks CPU parallelism rather
than the RTMP-shaped 128 cap. Live at 60g: the health API reports **12 SRT
egress shards** (old formula capped at 1 per feed); the SRT send path is
split across them. Unit tests cover the new formula; see
`src/config.rs` (`target_egress_fabric_shards`) and the sweep's
shard-formula runs (`shard-formula-check-*`).

## Bitrate / connection-count bracketing evidence

All runs: `bitrate_sweep`, `h264-srt` config (SRT ingest + SRT egress, 2
outputs per group: `src` passthrough + `720p` transcode), real synthetic
source fixture. "Failures" = ffprobe dimension-probe failures
(`correctnessOk`/`correctnessFailures` in the harness output).

**2026-08-11 corrected ladder** (after the CRF fix below; totals are the
true SRT egress load: `groups × bitrate × 2 outputs`):

| Groups | Bitrate | True egress | Result |
|---:|---:|---:|---|
| 30 | 1.5M | ~122 Mbps | **120/120 probes pass**, 0 drops both tiers, transcode ring 1.5MB, CPU 90% |
| 30 | 1.5M (a2 multi-audio) | ~122 Mbps | 3/3 sampled probes decode, **all drops 0** both tiers |
| 60 | 1.5M | ~244 Mbps | **6/6 steady-state probes decode** (1080p/720p), 720p drops all 0, ring 1.5MB, CPU 126% |
| 135 | 1.5M | ~549 Mbps | **0 fabric failures, 0 overflow**, rings 1.3MB, CPU 230% — but the mediamtx peer logs 424 TS decode errors (peer wall, see below) |
| 135 | 4M | ~1.08 Gbps | **reproducible ramp failure**: 5-16/540 tracks never registered within 45s (`stalled=unregistered-cell`), harness aborts |
| 135 | 8M | ~2.16 Gbps | **ramp failure**: 13/540 unregistered at 45s |

Reading: restream's fabric holds 270 SRT outputs / ~550 Mbps cleanly —
zero internal failures, zero overflows, rings at 1.3-1.5MB (vs 14MB at the
contaminated 60g, 75s of video latency). The wall at scale is the **peer**:
the single mediamtx process degrades first (TS `decode error: astits` +
`initial delimiter not found` lines at 135g×1.5M, ~424 in 2.5 min) and its
SRT listener stops completing handshakes under ~1 Gbps / 270-publisher ramp
load (the unregistered leaves at 4M/8M — the fabric never even reached
steady state, so those two rungs bracket the ramp ceiling, not a
steady-state ceiling).

**Old (contaminated) ladder** — every number inflated ~11× on the 720p tier,
so the reported loads were 633 Mbps (30g), 1.27 Gbps (60g), 2.84 Gbps
(135g), and the "clean 30g" / "1 failure at 60g" / "88-235 at 135g" results
were really runs 3-5× past the fabric's true ceiling. The 30g 8M and 30g 4M
brackets below were similarly inflated:

| SRT connections | Bitrate | SRT failures | Notes |
|---:|---:|---:|---|
| 30 | 1.5M | 0/30 | **was actually ~633 Mbps** (0.63G of CRF 720p) |
| 30 | 8M | 2/30 | pre-fix; post-fix also 2/30 (residual, see below) |
| 120 | 1.5M | failing from the start | **was actually ~2.5 Gbps** |
| 120 | 8M | 119/120 pre-fix | |
| 120 | 8M | 119/120 post-cursor-fix | test scale never exercised the muxer fix (60/feed < 128) |

Connection count is the primary driver; bitrate is a secondary multiplier —
but only after the CRF correction does that statement hold at honest
bitrates. The residual 2/30 at 30 connections/8M post-fix showed **zero**
overrun/resync events (the cursor-priming fix's mechanism was not involved)
and a sparse, scattered-over-time decode-error pattern on one connection,
consistent with ordinary scheduling jitter under load rather than a
remaining logic bug.

## The 720p tier was in CRF mode

**Discovery**: a 30g health snapshot (`srt30g-health`) showed the 720p
outputs at `bitrateKbps` = **19,331** against a 1,760 Kbps source — 11× the
input. The src tier was 1,745 Kbps as expected.

**Mechanism** (source-verified):

1. The harness creates the transcode tier purely by preset **name**
   (`encoding: "720p"`); `OutputVideoConfig` has no bitrate field — its
   variants are Source / Preset / Custom only.
2. The built-in "720p" transcode profile (`src/media/profiles.rs`
   `built_in_defaults()`, unchanged since commit `d252d5f5` introduced the
   resolution-profile table) is deliberately **CRF mode**:
   `bitrate: 0, crf: 23` — constant quality, the module doc's documented
   contract for production ("bitrate: 0 → CRF mode (constant quality,
   adapts to content)"). CRF 23 is x264's default quality; on real content
   this is a sane 2-5 Mbps for 720p. The preset is not "wrong values" — it
   is a quality-mode profile, correct for production where source bitrate
   is unknown.
3. In CRF mode x264 picks whatever bitrate holds quality 23, and the sweep
   fixture (`bench-h264-1_5m.ts`, generated by
   `scripts/fixtures/generate-bench-fixtures.sh` from a **mandelbrot**
   lavfi pattern — an infinitely detailed synthetic) barely compresses at
   CRF23/ultrafast/zerolatency → **19.3 Mbps per output on the wire**.
4. The mismatch is using a quality-mode preset for a bitrate-controlled
   load test: "60 groups × 1.5M" was really "60 groups × ~10.4 Mbps" —
   1.27 Gbps of SRT egress. Every pre-fix ladder number, drop counter, and
   ring measurement is contaminated by this factor.

**Harness fix** (`src/bin/test_harness/resource_sweep/bitrate.rs`): per
case, `install_bitrate_controlled_720p_profile` PATCHes
`/api/v1/settings` so `transcodeProfiles["720p"]` carries an explicit
`bitrate = source × BITRATE_SWEEP_720P_MULTIPLIER` (default 1.0), keeping
the profile's realtime flags (ultrafast, zerolatency, gop 60, bframes 0,
1280x720). Verified live: 720p outputs now emit 2.33 Mbps (measured) with
**zero drops** at 30g, vs 19.3 Mbps before. The transcode tier is now a
meaningful ladder rung and total egress is predictable.

**The peer-side symptom this explains**: mediamtx logs `decode error:
astits: parsing PES data failed` / `initial delimiter not found` on
publisher connections — the TLPKTDROP'd streams arrive at the peer as
garbled TS. At corrected bitrates these errors vanish at 30g/60g and only
reappear at 135g×1.5M (the peer wall above).

**Post-send TLPKTDROP mechanism** (source-verified, explains "0 sent / 3M
dropped"): restream uses plain `srt_send` (`egress_sender.rs:164`), so the
drop clock starts at the `srt_send()` call, not the producer. Libsrt stamps
`m_tsOriginTime` at enqueue (`CSndBuffer::addBuffer`,
`buffer_snd.cpp:213-214`) and `sndDropTooLate()` (`core.cpp:6773`) drops the
buffer head whenever `buffdelay > max(peer_latency + sndDropDelay,
SRT_TLPKTDROP_MINTHRESHOLD_MS=1000) + 20ms` ≈ **1020 ms** — while the
sender transmits at paced `m_tdSendInterval` (`core.cpp:10262-10292`).
Aging is pure post-send queueing (injection bursts vs paced drain vs peer
ACK rate); the producer's PTS is never consulted.

## What's fixed, what's evidenced-but-unproven, what's still open

**Fixed and evidenced:**
- Leaf cursor priming (root cause 1) — live-proven: zero overruns post-fix
  at 30 connections, vs. universal pre-fix.
- Resync/priming fallback to live edge, not oldest sequence (root cause 2) —
  unit-proven; no live case yet isolates a mid-run overrun-recovery event
  specifically (none occurred in any post-fix run so far).
- libsrt multiplexer reuse scoped per shard — unit-proven and live-confirmed
  to behave exactly as designed once output count actually crosses the
  128-per-shard threshold.
- `OUTPUTS_PER_SHARD` is RTMP-shaped (the original "scale to 1,200"
  blocker) — **fixed and live-proven**: the SRT shard formula now grows past
  the old 1-shard cap; 12 SRT shards observed live at 60g via the health API
  (old formula capped at 1 per feed). See the shard-scaling section.
- UDP receive-buffer clamp warning — **root-caused and fixed**: the clamp is
  intentional (egress sockets deliberately ask for 1MB; the warning compared
  against an 8MB *listener* desire). The warn message now states the real
  expected value; the "raise rmem_max" remediation is confirmed a no-op.
- Sweep 720p tier in CRF mode (this session) — **root-caused and fixed in
  the harness** (per-case bitrate-controlled profile); corrected ladder
  above.

**Added, tested, but not evidenced as necessary for the reproduced bug:**
- Retry jitter (`backoff_ms`) — `CommandChannelFull` never observed in this
  investigation at any scale.

**Root-caused but not yet fixed (latent, evidence-backed):**
- SRT `observe_stall` (`src/media/egress/backends/srt.rs:199-218`, esp.
  206-212) counts **any** native-backlog decline as protocol progress —
  including TLPKTDROP/TSBPD-deadline drops. A drop-riddled SRT leaf can
  therefore have its no-progress deadline extended indefinitely, and never
  stall-swept ("0 sent / 3M dropped" is the exact current-code hang
  signature). RTMP has no such signal (it relies on real engine sends).
  Candidate fix: treat `packetsSentDrop` growth as non-progress, or wire
  `feed_lag_units` into `classify_stall` as defense-in-depth.

**Not started:**
- `EGRESS_SNDBUF_FLOOR` reconsideration (buffer sized from a 50Mbps
  worst-case assumption since live per-output bitrate isn't wired in for
  most outputs) — deliberately left alone pending evidence it's still a
  significant residual factor once the above fixes land.
- Wiring the currently-dead `feed_lag_units`/`max_feed_lag_units` policy into
  `classify_stall` as defense-in-depth detection for a leaf that falls behind
  after being correctly primed.
- A dedicated live harness fault case reproducing real scheduling contention
  deliberately (extending `fault.srt-output-stall`), rather than the ad-hoc
  live experiments this investigation used.
- Extending `adapt_pipeline_ring`'s proven resize-on-probe pattern to the
  shared SRT `TsChunkRing` (currently a fixed `ts_ring_capacity`, sized for a
  "sub-millisecond bridge" that collapses to far less than one GOP interval
  at MSR's real multi-track packet rate).
- A broader audit of the egress fabric for other instances of the "multiple
  implementers of the same abstraction disagree, no one noticed" pattern
  that produced root causes 1 and 2.
- Re-running the 30g×60fps bracket at corrected bitrates (mechanism is
  CRF-identical to the runs above; the fps dimension does not interact with
  the fix differently).
- Splitting all of the above into logical, individually-reviewable commits.

## Artifact index

Live run artifacts (this worktree, `.local/artifacts/`, all `NO_CLEANUP=1`):
`bitrate-sweep-postfix-15`, `bitrate-sweep-postfix-60`,
`bitrate-sweep-lowbitrate-15`, `bitrate-sweep-lowbitrate-60`,
`bitrate-sweep-dropcheck-60`, `bitrate-sweep-profile-60` (+ `/tmp/restream-profile-60.data`,
`perf record` capture), `bitrate-sweep-muxerfix-60`,
`shard-formula-check-2`, `shard-formula-check-135b`,
plus the 2026-08-11 corrected-ladder runs: `srt30g-health` (CRF discovery
snapshot), `srt60g-discrim` (killed; mediamtx decode-error source),
`srt60g-discrim2` (ring/CPU matrix), `ladder-30g` (120/120 probes pass),
`ladder-60g` (6/6 probes decode), `ladder-135g` (fabric clean, peer wall),
`ladder-135g-4m`, `ladder-135g-8m` (ramp failures), `ladder-30g-a2`
(multi-audio clean).

Code: `src/media/egress/leaf.rs`, `src/media/egress/visit.rs`,
`src/media/egress/backends/srt_drain.rs`,
`src/media/egress/backends/rtmp_shard_drain.rs`,
`src/application/reconcile.rs`,
`src/media/egress/backends/srt/muxer_ports.rs` (new),
`src/media/egress/factory.rs`, `src/media/engine_egress_fabric.rs`,
`src/media/engine_registries.rs`, `src/media/engine_runtime.rs`,
`src/media/srt/egress_connect.rs`, `src/media/srt/egress_connect/single.rs`,
`src/media/srt/egress_connect/bonded.rs`, `src/media/srt/socket.rs`,
`src/media/srt/listener.rs`, `src/media/srt/sys.rs`,
`src/bin/test_harness/resource_sweep/bitrate.rs`.
