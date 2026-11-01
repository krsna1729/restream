# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-070 MoQ ingest: IETF MOQT profiles](#q-070-protocol-opus-moq-ingest-ietf-moqt-profiles)
- [Q-074 bounded HLS ingest poll](#q-074-performance-opus-a-rare-multi-millisecond-hls-ingest-poll-is-bounded-whatever-its-cause)
- [Q-102 fuzzy command palette](#q-102-frontend-sonnet-fuzzy-command-palette)
- [Q-110 chrono to jiff](#q-110-platform-sonnet-move-from-chrono-to-jiff-after-jiff-10)
- [Q-111 receiver TLPKTDROP floor](#q-111-protocol-opus-an-unrecovered-loss-stalls-srt-rs-delivery-at-least-one-second)
- [Not planned](#not-planned)
  - [Q-033 host-level shard pool](#q-033-host-level-shard-pool)
  - [Q-071 HLS-to-HLS passthrough](#q-071-hls-to-hls-passthrough)
  - [Q-097 MediaMTX MoQ dialect interoperability](#q-097-mediamtx-moq-dialect-interoperability)

### Q-025 [performance] [opus] Re-qualify the SRT shard law after transport convergence
- Goal: qualify the SRT cold prior (64 outputs per shard at 4.8 Mbit/s,
  `crates/restream-engine/src/egress/sizing.rs`, [SRT shard capacity](runtime-crossings.md#srt-shard-capacity))
  and `srt_egress_connect_concurrency` (`crates/restream-engine/src/config.rs`) on a second host.
- Gates: the single-shard SRT matrix on another host; no production policy
  change without measured evidence.

### Q-070 [protocol] [opus] MoQ ingest: IETF MOQT profiles
- Goal: an IETF MOQT publisher (MSF catalog, LOC objects) feeds a pipeline
  input through the existing MoQ listener. The stream key comes from the
  connection path, else the track namespace; the input registers as
  protocol `"moq"`.
- Publishers: GStreamer `moqsink` with `container=loc`, and the `moq` CLI
  through moq-relay.
- Gates: one harness mode ingesting from each publisher, with frame counts
  and monotonic timestamps checked on read-back.

### Q-074 [performance] [opus] A rare multi-millisecond HLS ingest poll is bounded whatever its cause
- Goal: a `hlsIngest` poll on the MEDIA executor never spends multiple
  milliseconds of thread CPU.
- Remaining work: run the live gate on a host whose thread CPU clock is
  trustworthy. The application bounds are in place: a box walk counts
  nested boxes, a poll converts at most 256 fMP4 samples, and a whole
  `moof` decode and plan takes about 0.5 ms at the 16384-sample bound on a
  Zen 1 VM, so a full incremental parser is not needed. On that VM the gate
  cannot pass or fail the code: TS and fMP4 both showed 19–40 ms maxima,
  about half of it system time, while the `--host-probe` thread, which does
  no work, saw thread-CPU gaps up to 97 ms (hypervisor time charged to the
  thread). Allocator or kernel tuning showed no repeatable tail improvement.
- Gates: release `hls-push-duty -- --no-netns --hls-publishers 8
  --duration-secs 60 --poll-usage --host-probe`, with `--segment-type ts`
  and `--segment-type fmp4`, on bare metal over five runs each, with
  `hlsIngest` max CPU below 4 ms and fewer than one poll above 4 ms per
  minute per session, and a probe whose own thread-CPU gaps stay below
  1 ms. A mean or p99 passing does not waive the maximum.

### Q-102 [frontend] [sonnet] Fuzzy command palette
- Goal: the ⌘K palette matches loosely worded requests to the existing
  commands (fuzzy matching, no language model) and shows the resulting plan
  before anything runs.
- Files: `web/ts/control-room/shell/command-palette.tsx`, `palette-model.ts`.
- Gates: misspelt and reordered requests reach the same command the buttons
  would; nothing applies without the approval step.

### Q-110 [platform] [sonnet] Move from chrono to jiff after jiff 1.0
- Goal: no crate depends on chrono. Its maintainer has soft-deprecated it
  ([chrono#1768](https://github.com/chronotope/chrono/issues/1768)), and a
  RustSec "unmaintained" advisory waits for jiff 1.0; with no ignore in
  `deny.toml`, that advisory would fail the Cargo deny job.
- Scope: 5 crates and about 40 files, mostly `Utc::now`, RFC 3339 output
  and `from_timestamp` conversions; `serde_with`'s chrono feature also
  pulls it in.
- Gates: contract tests pin today's serialized timestamps (API JSON and
  SQLite rows) before the switch and pass unchanged after it;
  `cargo tree -i chrono` is empty.

### Q-111 [protocol] [opus] An unrecovered loss stalls srt-rs delivery at least one second
- Goal: a packet that is never retransmitted holds back delivery no longer
  than libsrt's receiver would. The srt-rs receiver drops it only
  `max(1.25 × latency, 1 s)` past its play time
  (`SrtReceiver::tlpktdrop_threshold_us`, srt-rs `e86aa6f`), so at the
  product's latencies every such loss stalls the output for over a second.
  Reading libsrt, that one-second floor belongs to its sender's drop and its
  receiver skips once the following packet is due; confirm that in
  `srtcore` first. Not measured live.
- Gates: a deterministic srt-proto test in which a lost, never-resent packet
  lets the next one out at its own play time; a libsrt interop run that
  compares the stall.

## Not planned

### Q-033 host-level shard pool
A pooled prototype cut shard CPU at 16 feeds × 2 outputs by more than half,
but an injected Owner fault failed both feeds in a shared pool. Production
keeps per-feed shard groups
([per-feed shard isolation](runtime-crossings.md#per-feed-shard-isolation));
reopen only if that failure-domain contract changes.

### Q-071 HLS-to-HLS passthrough
Relaying pushed HLS segments past the ring would need separate capacity
accounting, preview and health for ring-less outputs, and segment-level
failover. No telemetry shows HLS demux plus segmenting as a meaningful cost.

### Q-097 MediaMTX MoQ dialect interoperability
Blocked upstream: MediaMTX v1.21.2 has no PUBLISH_NAMESPACE dispatch, waits
for the server's SETUP, puts one frame per group with codec-tick timestamps
and no timescale, and emits a non-MSF catalog. Restream carries no MediaMTX
profile; `moq-relay-interop` stays diagnostic-only.
