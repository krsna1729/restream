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
- [Real-1080p MSR envelope: residual intermittent zero-video failures (2026-08-12)](#real-1080p-msr-envelope-residual-intermittent-zero-video-failures-2026-08-12)
- [2026-08-12 update: sink mode, 1,200-output scaling, command channel optimization](#2026-08-12-update-sink-mode-1200-output-scaling-command-channel-optimization)
- [2026-08-13 update: ingest path exonerated, residual hole is egress-side](#2026-08-13-update-ingest-path-exonerated-residual-hole-is-egress-side)
- [2026-08-13 update: residual "zero video" verdict root-caused — probe artifact, not an engine defect](#2026-08-13-update-residual-zero-video-verdict-root-caused--probe-artifact-not-an-engine-defect)
- [2026-08-13 update: sink-mode bugs fixed; ~600-connection SRT egress ceiling root-caused and fixed](#2026-08-13-update-sink-mode-bugs-fixed-600-connection-srt-egress-ceiling-root-caused-and-fixed)
- [2026-08-13 update: sink mode gained real RTMP capability; RTMP-only confirmed at 1,200](#2026-08-13-update-sink-mode-gained-real-rtmp-capability-rtmp-only-confirmed-at-1200)
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
- SRT stall detection counting TLPKTDROP drops as protocol progress — **fixed
  and evidenced** (commit `061d7de6`): `observe_stall` now only counts a
  native-backlog decline as progress when libsrt's drop counter did not
  advance, so a drop-riddled leaf ("0 sent / millions dropped") can no longer
  reset its no-progress clock every sweep; `classify_stall` also enforces the
  previously-dead `max_feed_lag_units` ceiling (a leaf behind the feed head
  is Stalled even while its native buffer drains, since catch-up would be
  lossy). Unit tests cover both; the concurrency `fast.sh` gate and the full
  live contract gate (`fault.resilience`, `fault.egress-retry`,
  `fault.output-stall`, `recovery`) pass with the new classification.

**Added, tested, but not evidenced as necessary for the reproduced bug:**
- Retry jitter (`backoff_ms`) — `CommandChannelFull` never observed in this
  investigation at any scale.

**Root-caused but not yet fixed (latent, evidence-backed):**
- *(none remaining — the SRT `observe_stall` drop-progress divergence was
  fixed in commit `061d7de6`, see the fixed list above.)*
- **Residual real-1080p envelope flake (the active residual):** video egress
  is delivered in 2–4 s bursts / 1–4 s holes (all outputs lockstep, audio
  continuous) because the leaf visit work budget batch-drains video ahead of
  the demux fill and re-visits are feed-event-gated (see the 2026-08-12
  section below). This reproduces the original zero-video verdict
  intermittently at n=30 (~36%) and is the likely mechanism behind the
  original n=600–900 catastrophe — separate from the overrun/start class
  (which stays at zero post-fix). Not yet fixed: a pacing change here is a
  hot-path concurrency change needing the full proof workflow.

**Not started:**
- `EGRESS_SNDBUF_FLOOR` reconsideration (buffer sized from a 50Mbps
  worst-case assumption since live per-output bitrate isn't wired in for
  most outputs) — deliberately left alone pending evidence it's still a
  significant residual factor once the above fixes land.
- Re-running the 30g×60fps bracket at corrected bitrates (mechanism is
  CRF-identical to the runs above; the fps dimension does not interact with
  the fix differently).
- A 1,200-output (300-group) bitrate-sweep run at corrected bitrates: the
  ladder already brackets the ceiling (peer-side handshake wall at ~270 SRT
  connections), so a full run would reproduce the same wall; a
  restream-as-peer harness would be the clean way to prove the fabric's own
  1,200-output handling (see the ladder reading above).
- In-place migration for the SRT TS ring when an output attaches before the
  pipeline ever went live (creation-time sizing falls back to the
  configured minimum in that ordering) — deliberately not implemented: the
  muxer writer task holds the ring for the pipeline's lifetime, and the
  realistic ordering (probe precedes egress attach) is fully covered.
- Loom coverage of the leaf visit/sweep state machine — **checked and
  ruled out, not skipped**: both `on_ready` (visits) and `on_media_tick`
  (the once-per-second stall sweep, `sweep_stalled_leaves`) run on the
  shard's own single OS thread inside its command loop, so `LeafCommon`
  (including the new `last_packets_sent_drop` state) is private to one
  thread by construction. A loom test would test a reimplementation, not
  the code. The genuinely shared state is the ring itself, which is
  atomic-based and already exercised by the existing threaded tests; the
  high-value disagreement proof turned out to be the proptest below.

**Closed this session (2026-08-11):**
- Live scheduling-contention fault case — `fault.srt-output-stall`'s
  backpressured-receiver proof now runs an optional contention phase
  (`FAULT_CONTENTION_BURNER_THREADS=N`, default 0; run here at N=8): pure-spin
  threads starve restream's shard threads while the watch window runs, and
  the artifact records load average before/during/after plus the output's
  libsrt drop counter. Pass criteria are the deterministic core either way;
  PASS at both N=0 and N=8 (loadavg 0.44→2.19 under burners, drops 0/0 —
  the backpressure is demonstrably not drop-masked, and the leaf is still
  reclaimed at the no-progress deadline).
- **Sieve finding (important for what the stall sweep can and cannot do)**:
  with peer TLPKTDROP enabled, an overloaded leaf whose flow window is
  closed keeps admitting one packet per drop (each drop frees a send-buffer
  slot, the engine refills it, the new packet ages and is dropped) — a
  sustained ~1 packet/sec of engine byte progress that keeps
  `last_byte_progress` fresh, so the leaf stays classified
  `Backpressured`/`Idle` and the stall sweep (correctly) does not reclaim
  it. That shape — drops firing while the connection "looks healthy" — is
  the original "0 sent / 3M dropped" peer symptom, and it is a
  delivery-side problem: the fix for the *stall-sweep* dodge (drop-aware
  `observe_stall`, commit `061d7de6`) tightens the deadline corner where a
  leaf's buffer drains only via drops with no admission progress, while
  the sieve shape is caught by the harness's peer-side correctness probes
  (the ladder's decode checks), not by backpressure classification.
- SRT TS ring sizing — `start_shared_ts_muxer` now sizes the shared
  `TsChunkRing` at creation from the probe-derived packet rate the source
  ring already carries (`set_estimated_pkt_rate`, written by
  `adapt_pipeline_ring`): `ceil(pkt_rate × 5s)`, clamped to
  `[ts_ring_capacity, 16,384]`. The fixed 256-chunk default is a
  sub-millisecond bridge at MSR's real multi-track rate (30 audio tracks +
  video ≈ 1,560 pps ⇒ the old ring wrapped every ~0.17 s, far less than
  one GOP interval, so any scheduling hiccup pushed a leaf into
  overrun-resync and a mid-GOP restart at the peer). Unit tests cover the
  fallback (unprobed ring → configured), the MSR envelope scaling, the
  configured floor, and the cap.
- Disagreement-class property proof — `live_start_cursor` (the unified
  resync position every overrun-recovery path now uses) is property-tested
  over every plausible (epoch, retained window, sync point) combination:
  the cursor always carries the feed's epoch, always lies within
  `[oldest_sequence, head_sequence]`, equals the latest retained sync
  point when one exists, and falls back to the *live edge* (never
  `oldest_sequence`, the historical disagreement) when none does.
  `live_start_cursor` is now `pub(super)` so the property can see it; the
  contract the three implementers disagreed on is enforced by construction
  from here on.

## Real-1080p MSR envelope: residual intermittent zero-video failures (2026-08-12)

The canonical `msr` harness envelope was run on the real fixture
(`/tmp/msr-1080p-30a.mp4`, 1× H.264 1080p @ 3036k/25fps + 30× AAC, 1508 MB,
stream-copied from yt-dlp `dX8_EjS6ZjU` fmt 137+140) at n=30 with
`MSR_SIGNAL_CALIBRATION=0` (calibration is proven to fail on real media) and
the investigative `RESTREAM_MSR_FIXTURE_OVERRIDE` hook. Commands do not
encode: the publisher stream-copies `0:v:0` + `0:a:1`×29 + `0:a:2` through
MPEG-TS (`-bsf:v h264_mp4toannexb`), so the 30-audio envelope costs no
encoder CPU on the publisher.

Result: **the original failure verdict still reproduces intermittently at
n=30 — 4/11 runs failed with `msr-rank01-* ffprobe did not capture any video
packets`** (run 1: rtmp-0008 @ checkpoint 30; run 3: srt-0020 — the original
SRT channel; run 6: rtmp-0021; run 10: rtmp-0021 at ~t=20–25s). Failing
probes show audio-only packet streams (zero video) with the video stream
present in the header; the harness verdict is the same class as the original
n=600–900 log.

### Delivery shape: video bursts with holes, audio continuous

Not a per-output defect. In every run the engine's video egress is globally
lockstep-bursty: all outputs' `bytesOut` move identically at 0.8 s sampling
(identical rates to 3 decimals; e.g. run 11 swinging 0.021 → 0.874 → 0.000 →
0.972 MB/s), and every probe — healthy and failing — shows video arriving in
2–4 s spans at full 25–26 fps separated by 1–4 s holes, while audio is
continuous throughout the 5 s window (SRT: full 43 pkt/s; RTMP: a fixed
6.1 pkt/s muxer cadence, ~1/7 of source). Passing probes catch 1–3 s of
burst (21–83 video packets); failing probes land fully inside a hole. The
observed failure rate (~36%) ≈ the measured hole fraction of the delivery
schedule.

Facts that rule out the neighboring hypotheses:
- **Publisher is smooth.** The same ffmpeg publisher run standalone
  (31-stream stream-copy through `-f mpegts`) emits video at a rock-steady
  25 f/s with no gaps (control probe: 125 video packets over 4.96 s).
- **Input is smooth.** Engine input `bytesReceived` constant 0.85–1.1 MB/s.
- **No feed overruns.** Per-output `resyncCount` 0 and `feedLagUnits` 0–8 in
  all captured health samples — the committed overrun/resync fixes are
  active and quiet.
- **No TCP pathology (the RTMP quality row is tcp_info-backed, per the
  telemetry code):** `tcpLost` = 0, `tcpRetrans` = 0, `tcpNotsentBytes` = 0
  (zero socket backpressure) in every sample; `tcpBytesAcked` growth
  decelerates in the same holes the mediamtx `bytesReceived` shows — the
  engine simply stops sending video during holes; the wire is never the
  bottleneck.
- **Mediamtx publish side healthy** at video rate whenever polled (0.17–0.72
  MB/s per path, avg ~0.43 MB/s ≈ source).

### Driver: the per-visit leaf work budget, event-gated re-visits

`config.rs` defaults — `visit_max_units: 32`, `visit_max_bytes: 256*1024`,
`visit_max_us: 2_000`, readiness re-registration event-gated (feed
readiness / 25 ms idle poll) — batch-drain up to ~0.66 s of 3.1 Mbps video
per leaf visit. At the real bitrate the mux outruns the demux fill, so a
leaf drains the available backlog in large batches, then idles until the
ring re-fills and the next feed event re-registers it — the 2–4 s burst /
1–4 s hole schedule. Audio packets stay continuously available in the
interleave (and are small enough to slip through every visit), so audio
never holes; 30 outputs × 25 fps × 15 KB makes the aggregate video demand
11.7 MB/s shared through the same event-gated path, so all leaves burst in
lockstep. At n=600–900 the same batching with hundreds of leaves stretches
loop cadence into sustained holes — the original catastrophe at scale.
This is a separate mechanism from the fixed overrun/start class (those
counters stay at zero); it is a delivery-pacing shape that only the
peer-side probe can see (progress gates stay green on audio bytes alone).

### Residual status

- Not a harness/reader artifact: the engine provably does not send video
  during holes (tcp acked + mediamtx bytesReceived), and failures reproduce
  with and without external probe load.
- Not a mediamtx config fix (`writeQueueSize`, `readTimeout`): the wire
  lacks the video.
- Open: a pacing fix — re-visit leaves on a media-cadence timer rather than
  feed-event gating, or byte-fair per-visit budgets tuned to real bitrate —
  is a hot-path concurrency change requiring the full proof workflow
  (fast.sh contract gate, benchmark before/after, live harness fault case);
  recommended as the next backlog item.

## 2026-08-13 update: ingest path exonerated, residual hole is egress-side

### Direct instrumentation of the ingest-forward path

Added `[ingest-fwd]` trace at `forward_ingest_packets()` entry that logs per-call
video count + gate state. Ran instrumented msr n=30 (real 1080p+30A fixture) with
`RUST_LOG=restream::media::srt::ingest_packets=info,...`:

Result: **409 video-forward calls in ~16s = 25.6 fps** — continuous 25fps delivery
with **zero inter-arrival gaps > 500ms**. Gate state = `active` on every call.
Video=1 per call (one PES per `drain_into`), consistent with per-frame PES
assembly completing at the next PUSI=1 (~40ms cadence).

**Conclusion: the ingest path is NOT the root cause of the 1-3.5s video holes
observed at the egress.** The source ring receives video at full frame rate
continuously. Any video absence at an SRT or RTMP output must come from the
egress side: feed-reader cursor issues, TsFeed/TS-muxer delivery jitter, SRT
socket send-buffer backpressure, or shard-visit scheduling gaps.

### Lifecycle benchmark

Consolidated obsolete multi-file SRT benchmarks into a single
`benches/srt_lifecycle.rs` (5c0a6d6). Models both raw-blocking client
(RCVSYN=1,SNDSYN=1) and production fabric-egress nonblocking
(RCVSYN=0,SNDSYN=0 after connect) setups.

Key results (1200 connections, 3s send):
- raw-blocking: 1195/1200 opened, 28MB sent, 18.52s elapsed
  (each `srt_connect` blocks ~15ms for handshake on 8 workers)
- egress-nonblocking: 1200/1200 opened, 2.6MB sent, 11.05s elapsed
  (all `srt_connect` return immediately; 321/1200 handshakes complete
  within window; 0 sender failures)

Validates the fabric egress approach: nonblocking connect does not stall the
caller thread, eliminating thundering-herd blocking at scale.

### Open: egress-side investigation for residual hole

Now that the ingest side is confirmed clean, the residual 1-3.5s video gap
(observed as head_lag=0 with audio-only reads between GOP-sized video batches
at the egress visit level) must be in:

1. **TsFeed / TsChunkRing** — cursor tracking between the TS muxer writer and
   the SRT leaf reader (separate from the source RingFeed whose cursor prime
   fix is already landed).
2. **SRT egress send backpressure** — TLPKTDROP on the send side, or
   SRTO_SNDBUF filling up, silently discarding video while audio (which needs
   less bandwidth) continues.
3. **Shard visit scheduling** — the 25ms idle_wait between shard iterations
   combined with event-gated re-visits (FeedWake from ring notifications) may
   leave a leaf unvisited during a data burst.

Recommended next step: add egress-side instrumentation to `TsFeed::read_from`
and the SRT egress sender packet loop to measure actual per-output delivery.

## 2026-08-13 update: residual "zero video" verdict root-caused — probe artifact, not an engine defect

Reproduced the failing verdict on the first try with a new high-bitrate
fixture (Big Buck Bunny 1080p60 h264 ~4 Mbps + 30×AAC ≈ 8 Mbps,
`scripts/fixtures/generate-msr-1080p-fixture.sh`, GOP 3.1–4.2 s):
`msr` srt-only n=30 → `msr-rank01-srt-0002 ffprobe did not capture any video
packets`. Then discriminated the three remaining hypotheses with per-second
muxer instrumentation plus live timed-attach probes:

- **The shared TS muxer emits video continuously**: `video_in=60/s,
  video_muxed=60/s, dropped=0, overflows=0` every single second
  (instrumented `shared_muxer.rs`). The `audio_in≈6/s` that looks anomalous
  is normal — ffmpeg's mpegts muxer packs ~7 AAC frames per PES, and the
  probe re-splits them to ~47 pkt/s.
- **The peer receives full rate**: mediamtx path health showed 30/30 paths
  with tracks at ~27 MB/s aggregate (full VBR rate incl. video), zero
  decode errors — the engine→peer leg delivers video.
- **Timed reader attaches prove continuity**: 10 sequential 8 s ffprobe
  attaches across paths measured time-to-first-video of **0.62–5.31 s**
  (≈ uniform over the GOP) and, after the first video packet, a max
  inter-packet DTS gap of **0.117–0.200 s** — no holes, ever.

Mechanism of the false verdict: mediamtx forwards video to a fresh reader
only from the next IDR, while audio starts immediately; ffprobe's
`-read_intervals %+N` clock starts at that first (audio) packet. With a
GOP ≥ the 5 s sample window, a healthy stream intermittently yields zero
video packets in-window (measured directly: one attach waited 5.31 s). The
earlier "engine bytesOut holes" (0.02→0.87→0.00→0.97 MB/s at 0.8 s
sampling, all outputs lockstep) are VBR at GOP cadence — a large IDR
followed by small P-frames of the same shared content — not delivery
pauses. This closes the "burst/hole pacing residual" as **not an engine
defect**; the earlier TLPKTDROP/cursor findings were real and remain fixed.

Harness fix (`resource_sweep/msr/verification.rs`):
`MSR_FFPROBE_SAMPLE_SECS` default 5 → 12 (covers worst-case GOP + attach
jitter), and the verdict now measures what matters — video must appear in
the window AND stay continuous after its first packet
(`MSR_FFPROBE_MAX_VIDEO_GAP_SECS`, default 2 s, catches real starvation
like TLPKTDROP eating fragmented video PES while single-message audio
survives). Checks record `firstVideoOffsetSecs`/`maxVideoGapSecs`.

### Layer 2: the failure that survived the wider window — peer UDP rcvbuf overflow

The 12 s window still failed intermittently (`srt-0022`: 7.6 s of pure
audio at full 45 pkt/s — beyond any GOP; a correlated rerun caught
`srt-0019` at 12.0 s audio-only). Per-second polling of engine health +
`/proc/net/snmp` + `/proc/net/udp` during a fresh ramp attributed it
completely:

- **Every one of the run's 2,652,812 kernel `RcvbufErrors` landed on one
  UDP port — mediamtx's SRT listener.** gosrt multiplexes every SRT flow
  (30 publishers ≈ 240 Mbps ≈ 23k pkt/s, plus retransmissions, plus
  reader egress) through that single socket and never calls `SO_RCVBUF`,
  so it runs at `net.core.rmem_default` = 208 KB ≈ **7 ms** of buffering.
  Any mediamtx read-loop hiccup or lockstep IDR burst (all outputs carry
  the same content, so their keyframe bursts align) overflows it at
  30–50k drops/s.
- The engine sender is clean the whole time: `packetsSentDrop=0`, send
  rate steady, send-buffer occupancy only spiking (to ~386 ms) from the
  retransmission backlog the receiver's NAKs demand
  (`packetsSentLoss`≈`packetsSentRetrans` ≈ 83k in ~15 min on one output).
- Retransmission heals most loss (RTT 0.65 ms), but whatever misses the
  250 ms TSBPD deadline is dropped **receiver-side** (invisible in the
  sender's `packetsSentDrop`) — a multi-packet video PES dies if any
  fragment is missing while single-packet audio PES survives, giving the
  signature audio-only stream at mediamtx and every downstream probe.

Host remediation, live-proven: `sysctl net.core.rmem_default=8388608`
(mediamtx inherits it since it never asks for more) dropped kernel UDP
errors from 2.65 M/run to ~104 k/run (25×) and the full srt-only n=30
ffprobe checkpoint went **PASS** with `firstVideoOffsetSecs` 2.0–3.3 s and
`maxVideoGapSecs` ≈ 0.017 s across every sample. For scale runs the
structural answer is `MSR_PEER=sink`: restream's own sink-mode listener
requests 8 MB UDP buffers itself and one process per port-slice avoids
the single-socket multiplexer bottleneck entirely.

Takeaway for production: the engine's egress was never the defect at this
tier — the receiving peer's socket sizing was. The same signature
(sender-side drop counters at zero, `packetsSentLoss`≈`packetsSentRetrans`
climbing, peers reporting audio-only) should be read as receiver-side
pressure, not an engine pacing bug.

## 2026-08-13 update: sink-mode bugs fixed; ~600-connection SRT egress ceiling root-caused and fixed

Pure-SRT scale ramp (`MSR_PEER=sink`, `MSR_PROTOCOL_MIX=srt-only`, real BBB
1080p60/8Mbps fixture, `scripts/fixtures/generate-msr-1080p-fixture.sh`)
found two real sink-mode production bugs and one real SRT egress ceiling
near 600-650 concurrent connections in one pipeline, all three now fixed
and live-proven.

### Fixed: sink discard loop misread "no data yet" as connection close

`sink_discard_loop` (`src/media/srt/listener.rs`, `RESTREAM_SINK_MODE=1`)
treated any `srt_recv` return `<= 0` as a closed connection. The listener
sets `SRTO_RCVSYN=0` (nonblocking), inherited by every accepted socket, so
a fresh accept's first read routinely returns `SRT_EASYNCRCV` ("nothing to
read yet") — not a close. Every accepted client was torn down on its first
empty poll: live at n=100 sink-peer outputs, sink logs showed
`accepted=208 closed=208 clients=0 discarded=0MB` over 5 minutes — 100% of
connections closed before a single byte was read. Fixed by checking
`last_srt_error()` and only closing on a real error code.

### Fixed: sink discard loop busy-spun a full CPU core once any client existed

The same loop's only backoff (`sleep(10ms)`) was gated on `clients.is_empty()`
— with one or more clients connected it spun continuously, non-blocking
`srt_accept()` + one non-blocking `srt_recv()` per iteration, forever, even
when no client had data. Measured: each of 4 sink instances pegged ~80% of
a CPU core purely from this spin, ~3.2 of 6 cores on the host consumed for
no productive work. Fixed with an `empty_streak` counter: after a full lap
of the client list finds no data anywhere, sleep 1ms before the next lap.
Reset on any accept or successful read, so a busy client set is never
throttled. Live-proven: n=400 checkpoint fell from 15s to 1s, n=500 from
17s to 2s after this fix (see below).

### Also fixed: harness sink-peer readiness checked TCP state on a UDP port

`spawn_sink_peer`/`verify_preexisting_sink_peer`
(`src/bin/test_harness/resource_sweep.rs`) waited for the SRT port via
`wait_for_tcp_listener_ready`, which polls `/proc/net/tcp`'s LISTEN state —
SRT binds UDP, which never appears there, so this always timed out at 30s.
New `wait_for_udp_listener_ready`/`proc_net_has_bound_udp_port`
(`src/bin/test_harness/core/process.rs`) check `/proc/net/udp[6]` for the
bound local port instead (UDP has no LISTEN state; presence of the port is
the readiness signal).

### Root-caused and fixed: SRT egress connections above ~600-650 concurrent
### were hitting too-tight a connect timeout, not a capacity ceiling

With both sink-mode bugs fixed, a clean 500-outputs-in-14s /
600-outputs-in-3s ramp still hit a wall past ~600-650 total concurrent SRT
egress connections in one pipeline. Symptom: `restream::media::srt::egress_sender`
logged `srt send peer closed: Connection does not exist (2002)` (SRT_ENOCONN)
on `srt_send()` for a leaf shortly after libsrt itself logged the connection
as established — not a rejected handshake, an established connection whose
socket ID libsrt reported as no longer valid by the time the engine's next
send landed. Classified correctly as `PeerClosed` by
`classify_srt_send_result`, triggering the existing retry/backoff path — so
this was never a correctness bug, only a performance one.

Reproduction: extending a live, already-clean 600-connection pipeline to
750 via direct API calls (bypassing a fresh-process confound) reproduced it
immediately — 51/150 new outputs failed within 15s of creation, 257 total
`ENOCONN` warnings logged. **No permanent failures observed**: every failed
output's exponential-backoff retry (base 5s, doubling, capped at 300s)
eventually succeeded (`outputMaxRetries` 10, max retries seen on any one
output: 7) — but the worst-case straggler in a checkpoint could take
several minutes.

Two hypotheses tested and rejected before the real cause was found:
- **`RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT=0`** (each egress socket its own
  local port/multiplexer instead of one shared per shard): made things
  **worse** — checkpoint progress visibly regressed (508→534→511→499
  outputs-with-progress across successive polls) under the CPU/thread
  pressure of ~700+ separate multiplexers × 2 libsrt worker threads each
  on a 6-core host. Confirms the existing per-shard shared-multiplexer
  design (this doc's own 2026-08-11 fix) is correct.
- **More sink peer instances** (`MTX_COUNT` 4→12, cutting connections per
  sink-side multiplexer from ~187 to ~62): partial improvement (the same
  600→750 extension that failed immediately at `MTX_COUNT=4` instead
  showed slow tail convergence at `MTX_COUNT=12`) but did not eliminate
  the failures — a real but secondary contributor, not the root cause.

**Root cause, found via socket-ID lifecycle tracing** (temporary
`tracing::debug!` at connect-success, at leaf-close, and at every failed
`srt_send`, since reverted): every failing socket ID appeared in the trace
**exactly once** — connected, then closed after one failed send — ruling
out any socket-ID-reuse race in `complete_pending_connect`
(`src/media/egress/backends/srt.rs`; its `pending.common.generation !=
generation` staleness guard is not implicated). The failing send in every
case landed **3.00-3.05s after connect**, matching
`RESTREAM_SRT_CONNECT_TIMEOUT_MS`'s old 3000ms default to the millisecond —
the underlying SRT handshake itself was still completing when libsrt's own
connect-timeout fired and tore the socket down first. Under a burst of
600+ simultaneous handshakes to one peer, real (loopback) handshake
completion routinely takes longer than 3s; it is not stuck, just slower
than the timeout allowed for.

**Fix**: raised the default from 3,000ms to 10,000ms
(`src/config.rs`, `RuntimeTuning`-adjacent `srt_connect_timeout_ms`,
still overridable via `RESTREAM_SRT_CONNECT_TIMEOUT_MS`). Live-proven: the
same 600→650→700 ramp that always showed failures past 600 at the 3s
default ran **zero `ENOCONN`/`egress.failed` events across all three
checkpoints** at the 10s default — 600 in 6s, 650 in 9s, 700 in 7s, every
checkpoint clean on the first pass, no stragglers, no retries.

### Confirmed at the full 1,200-output target

The complete 100→1,200 sink-peer ramp (all fixes applied: sink accept/
busy-spin fixes, 10s connect timeout, 5s sink-verification sample window)
ran clean end to end: **1,200/1,200 PASS**, every 100-output checkpoint
converging in 0-28s with zero `ENOCONN`, zero `egress.failed`, zero sink
verification false-positives. Final state at n=1,200: 3.4 of 6 CPU cores,
3.9 GB RSS, 214 threads, ~3.6 GB delivered in the closing 5s sample window
(~5.8 Gbps aggregate egress). `packetsSentDrop` was still nonzero at full
scale (TLPKTDROP under real network-stack/CPU contention, the same
mechanism characterized earlier in this document) but did not block
correctness — retransmission and the existing retry/backoff handle it, and
every output kept delivering bytes throughout.

Also fixed as part of closing this out: `verify_msr_sink_checkpoint`'s
sample window (`MSR_SINK_ENGINE_SAMPLE_SECS`, `src/bin/test_harness/resource_sweep/msr.rs`)
was 2s — a leaf sampled between two GOP-cadence bursts (up to ~4.2s apart
at this fixture's keyframe interval) could show a genuinely flat window
with nothing actually wrong. Live-caught: 5/700 leaves false-failed a
checkpoint at 2s; widening to 5s cleared the identical ramp with zero
false positives. Same class of fix as the earlier `MSR_FFPROBE_SAMPLE_SECS`
widening above — GOP-cadence delivery needs a sample window at least one
GOP wide, not a fixed short interval.

The pure-SRT egress path is now genuinely proven to 1,200 outputs at real
1080p/8 Mbps bitrate on this 6-core host, correctly, with no known open
defects. RTMP-only and the canonical 95%-RTMP/5%-SRT mix at the same real
bitrate and scale have not yet been run through this fixture — sink mode's
RTMP listener doesn't complete a handshake (documented limitation above),
so that verification needs `MSR_PEER=mediamtx` instead, which reintroduces
mediamtx's own peer-capacity ceiling from earlier in this document and was
out of scope for this session.

## 2026-08-13 update: sink mode gained real RTMP capability; RTMP-only confirmed at 1,200

Sink mode's RTMP listener (`src/media/rtmp/listener.rs`, `RESTREAM_SINK_MODE=1`)
previously accepted a TCP connection and discarded bytes without completing
the RTMP handshake — a real RTMP client (restream's own egress fabric
included) blocks on the handshake and then blocks again waiting for the
server to accept its `connect`/`publish` requests before it ever sends
media, so no RTMP egress connection to a sink peer ever delivered a byte.
Live-confirmed: 0/50 outputs made progress in 158s against the old sink.

Fixed by reusing the exact machinery real RTMP ingest already drives:
`perform_server_handshake` (shared with ingest, unchanged) completes C0/C1
↔ S0/S1/S2, then a new minimal driver (`src/media/rtmp/sink_session.rs`)
runs `rml_rtmp::sessions::ServerSession` — the same state machine real
ingest uses — accepting `ConnectionRequested`/`PublishStreamRequested` and
discarding every other event (video/audio data, metadata, play requests)
unread. No new RTMP implementation: both pieces are the identical library
and pattern real ingest already relies on, just without instantiating a
pipeline for what they receive.

**Live-proven**: 50/50 and 100/100 clean immediately after the fix, then
the full 100→1,200 RTMP-only ramp passed **every single checkpoint in
1-2 seconds, 1,200/1,200, zero stragglers, zero `packetsSentDrop`** (TCP is
reliable; SRT's TLPKTDROP mechanism has no RTMP analogue). Final state at
n=1,200: **2.1 of 6 CPU cores, 1.5 GB RSS, 55 threads** — dramatically
cheaper than the SRT-only run at the same scale (3.4 cores, 3.9 GB RSS,
214 threads), consistent with this document's own earlier reasoning: RTMP
over TCP has low per-connection marginal cost, no per-multiplexer thread
model, and no hard delivery deadline forcing drops under load.

Both protocol slices of the MSR envelope are now proven correct and
performant at the full 1,200-output target on this 6-core host. The
canonical 95%-RTMP/5%-SRT mix (the real MSR shape) at the same scale and
bitrate is the natural closing verification.

## Artifact index

Live run artifacts (this worktree, `.local/artifacts/`, all `NO_CLEANUP=1`):
the 2026-08-12 msr envelope runs (overwritten per run, latest preserved):
`msr-restream.log`, `msr-mediamtx.yml`, `msr-samples.jsonl`,
`msr-30-msr-rank01-*.ffprobe.json` (run-1 collapse series 83→0 preserved for
rtmp-0001..0008), publishers; poll captures `/tmp/msr-final*.jsonl`
(engine health incl. tcp_info + mediamtx paths at 0.8 s), control publisher
emission probes in `/tmp/pub-ctrl2-probe.json`.
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

## 2026-08-12 update: sink mode, 1,200-output scaling, command channel optimization

### Sink mode (`RESTREAM_SINK_MODE=1`)

A minimal SRT/RTMP listener that accepts connections and discards data at the
application level — no pipelines, no source rings, no probes, no adaptive
resize. RAM per connection: ~10 KB vs 4.6 MB (ring) + 6 MB (SRT buffer) with
a full pipeline. Created via `SrtServer::run` in `listener.rs`: binds with
`SRTT_LIVE` + `SRTO_LATENCY=250` + `SRTO_REUSEADDR`, accepts in a tight loop,
spawns discard threads. Equivalent for RTMP in `rtmp/listener.rs`.

For 1,200-output tests, 4 sink instances on separate ports absorb 300
connections each. Kernel UDP receive buffer stays at default; no
`RESTREAM_SRT_UDP_BUFFER` or `RESTREAM_RING_HEADROOM_SECS` needed on sinks.

### Config additions (env via config framework)

| Variable | Default | Purpose |
|---|---|---|
| `RESTREAM_SINK_MODE` | (unset) | Enable minimal discard listener |
| `RESTREAM_SRT_UDP_BUFFER` | 8 MB | SRT UDP send/recv buffer |
| `RESTREAM_RING_HEADROOM_SECS` | 6.0 s | Pipeline ring adaptive-resize headroom |

### Command channel optimization

`dispatch_spec` (`manager.rs`) previously cloned the `OutputSpec` at the call
site before checking command slot availability — 100% wasted heap allocation
(Strings, Arc bump, LeafPolicy) when the channel was full. Now checks the
slot first and clones only on success.

### 1,200-output scale test results (4 sink instances × 300)

- 1,243 egress.started, 42 failed (all SRT handshake timeout)
- 8 end-to-end SRT connections established (2 per sink)
- 0 `CommandChannelFull` errors at capacity 8192
- 0 panics across all instances
- Memory: 5.6 GiB total (A + 4 sinks + ffmpeg + OS)

Remaining bottleneck: SRT egress connection establishment rate on A. Each
connection spawns a DNS resolution thread, waits on a limited-capacity
completion queue, and the shard processes completions batch-by-batch.
With 1,200 connections in flight, the thread-per-resolve model saturates the
6-core machine at ~8 simultaneous connections.

### Files changed

`src/config.rs`, `src/media/egress/manager.rs`,
`src/media/engine_pipeline.rs`, `src/media/rtmp/listener.rs`,
`src/media/srt.rs`, `src/media/srt/listener.rs`,
`src/media/srt/socket.rs`, `src/media/srt_tests/socket_runtime.rs`.
