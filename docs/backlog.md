# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-120 split the last complexity baselines](#q-120-performance-opus-split-the-last-two-complexity-baselines)
- [Q-121 converge EgressTask fabric paths](#q-121-lifecycle-opus-converge-the-fabric-run-paths-in-egresstask)
- [Q-122 deepen AppState and engine metric interfaces](#q-122-architecture-opus-deepen-appstate-and-engine-metric-interfaces)
- [Q-123 break the smallest module cycles](#q-123-architecture-opus-break-the-smallest-module-cycles)
- [Q-124 untangle HLS egress and triage the remaining deslop findings](#q-124-architecture-opus-untangle-hls-egress-and-triage-the-remaining-deslop-findings)
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

### Q-120 [performance] [opus] Split the last two complexity baselines
- Goal: `SrtOwners::drain_events` (22) and `KtlsConnection::read_records` (21)
  leave `crates/xtask/reportcard-baseline.json`, so the reportcard ratchet has
  no engine entries left.
- Why still open: both are hot paths, so a split needs before/after numbers
  from a benchmark that drives the real path. `drain_events` needs a live SRT
  owner set; `read_records` needs a kernel-TLS socket and the engine's private
  connection type. No existing Criterion suite covers either.
- Gates: a production-path benchmark for each, then the split with numbers
  showing no regression on a named host, then delete the entry with
  `cargo xtask reportcard --update-baseline`.

### Q-121 [lifecycle] [opus] Converge the fabric run paths in EgressTask
- Goal: `run_srt_fabric`, `run_rtmp_fabric`, and `run_hls_put_fabric` share
  one sequence for retain, dispatch placement, stop or leaf wait, Remove or
  Abandon, runtime release, and status forwarding. A protocol supplies only
  its hooks and phase.
- Why open: #697 fenced retry settlement by attempt and
  `EgressManager::dispatch_remove` routes Remove and Abandon through one
  path, but the three run methods still repeat the sequence and can drift.
  HLS uses Abandon when a stage ends under the output; SRT and RTMP always
  Remove. No known correctness gap; this is duplication risk.
- Gates: RTMP, SRT, and HLS fabric tests cover stop, leaf failure, dispatch
  failure, and stage end; every status and retry write stays `_if_current`;
  shutdown ordering does not change.

### Q-122 [architecture] [opus] Deepen AppState and engine metric interfaces
- Goal: remove the 189 `field-leak` findings, starting with `AppState` (23),
  `ShardMetrics` (21), `IoBuffers` (13), `GeneralizedSinkMetrics` (11),
  `LeafCommon` (8) and `MediaEngine` (7). Hide fields behind operations or
  snapshot queries; do not replace each field with a mechanical getter.
- Why open: `deslop-please` 0.1.4 reports code outside each owner reading or
  mutating its fields. `AppState` is the API surface; the engine metrics are
  shared across modules. The remaining top owners are listed in the audit
  output, not duplicated here.
- Gates: scoped `deslop-please` has no `field-leak` for those owners; API and
  engine tests pass; any hot metric path keeps its benchmark evidence.

### Q-123 [architecture] [opus] Break the smallest module cycles
- Goal: remove the remaining `module-cycle` findings. The seven smallest are
  removed: `moq::transport` shares close/error/header/HTTP-3 frame primitives
  in `transport::wire`; `api::file_ingest` and `api::ingests` share the GOP
  sanitizer in `api::state`; `restream-mpegts` owns `StreamKind` in `probe`;
  `hls::fmp4` owns `BufferedSample` in `codec`; egress `command` and `leaf`
  share `FeedId`/`OutputId` in `egress::ids`; `metrics` owns the delivery
  floor instead of importing it from `delivery`; and `TcpConnectAttempt`
  belongs to the Compio TCP owner. The 32-module engine cycle and the
  16-module `test_harness` cycle still need their own design, not a mechanical
  import shuffle.
- Why open: the remaining cycles are wider than the seven removed pairs, and
  the engine and harness cycles need an ownership boundary rather than a
  move of one shared item.
- Gates: scoped `deslop-please` reports no cycle for each changed area;
  affected lifecycle/protocol tests pass; public API changes are documented.

### Q-124 [architecture] [opus] Untangle HLS egress and triage the remaining deslop findings
- Goal: reduce the `hls_put_shard` tangle (58 things across 37 modules) and
  the `hls_ingest::registry` tangle (6 across 3), then decide the remaining
  17 `shallow-module` and 15 `wide-directory` findings by ownership rather
  than file moves. Treat the 69 predominantly test-only findings in
  `test_harness` as lower priority.
- Why open: `hls_put_shard` crosses most of the egress module graph; several
  shallow `lib.rs` modules may be deliberate public façades. No `long-file`
  finding exists, so file length alone is not the problem.
- Gates: a design note names the intended layers for the HLS tangle; RTMP,
  SRT, HLS, and harness tests pass; scoped `deslop-please` shows the intended
  reduction; no wide-directory move without an owning module.

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
