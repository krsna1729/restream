# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-122 deepen AppState and engine metric interfaces](#q-122-architecture-opus-deepen-appstate-and-engine-metric-interfaces)
- [Q-123 break the smallest module cycles](#q-123-architecture-opus-break-the-smallest-module-cycles)
- [Q-124 untangle the engine composition root and triage the remaining deslop findings](#q-124-architecture-opus-untangle-the-engine-composition-root-and-triage-the-remaining-deslop-findings)
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

### Q-122 [architecture] [opus] Deepen AppState and engine metric interfaces
- Goal: remove the 151 `field-leak` findings, starting with `AppState`,
  `ShardMetrics` (14), `IoBuffers` (13), `GeneralizedSinkMetrics` (11),
  `LeafCommon` (8) and `SrtFabricLeaf` (7). Hide fields behind operations or
  snapshot queries; do not replace each field with a mechanical getter.
  The shard loop already counts through `ShardMetrics` operations; the
  remaining `ShardMetrics` writes are the backends' `observe_metrics`
  snapshot fill. `TlsCounters` counts through `record(TlsEvent)`.
- Why open: `deslop-please` 0.1.4 reports code outside each owner reading or
  mutating its fields. `AppState` is the API surface; the engine metrics are
  shared across modules. `IoBuffers` and `SrtFabricLeaf` leak only to their
  owner's own stream/leaf modules. The remaining top owners are listed in
  the audit output, not duplicated here.
- Gates: scoped `deslop-please` has no `field-leak` for those owners; API and
  engine tests pass; any hot metric path keeps its benchmark evidence.

### Q-123 [architecture] [opus] Break the smallest module cycles
- Goal: remove the remaining `module-cycle` findings. The seven smallest are
  removed: `moq_transport` shares close/error/header/HTTP-3 frame primitives
  in `moq_transport::wire`; `api::file_ingest` and `api::ingests` share the GOP
  sanitizer in `api::state`; `restream-mpegts` owns `StreamKind` in `probe`;
  `hls::fmp4` owns `BufferedSample` in `codec`; egress `command` and `leaf`
  share `FeedId`/`OutputId` in `egress::ids`; `metrics` owns the delivery
  floor instead of importing it from `delivery`; and `TcpConnectAttempt`
  belongs to the Compio TCP owner. Later removals: the HLS session owns its
  applied-init state; the push registry takes the pull spawner instead of
  importing `hls_pull`; `config` owns the capacity and retry value types; and
  the recording/stream-check meta keys sit in one leaf module for the
  reconciler and their owners. The last pass removed the db/logging,
  migrations/schema, `restream_sync` reader, xtask qualify/release,
  `restream_mcp` session, `application` service-error, SRT ingress-types,
  RTMP connection and client/session, and egress TLS/Compio TCP cycles.
  The harness `hls_*_duty`, `fault_recovery`, `mixed_runner` and
  `resource_sweep` cycles were broken by moving shared helpers into leaf
  modules, and the 16-module `test_harness` cycle by moving the API waits,
  pipeline/output observation, process stats and output helpers into
  top-level leaf modules (`api_waits`, `observe`, `core/process`,
  `output_helpers`). Left: the 32-module engine cycle, which needs its own
  design, not a mechanical import shuffle.
- Design: the engine modules target this order, low to high: vocabulary and
  configuration (`config`, `codec`, `timing`, `executor`, `ffmpeg*`,
  `preview_ids`, `moq_transport`); runtime state (`ingest_registry`,
  `ingest_lease`, `stage_registry`); media workers (`stage_runtime`,
  `audio_router`, `external_transcoder`, `ingest_publisher`, `stream_qa`,
  `shared_ts_muxer`); `hls` primitives and `egress` (with its protocol
  backends); `engine_registries`; the `engine`/`engine_*` composition modules
  and snapshots; and finally the protocol servers (`rtmp`, `srt`, `moq`,
  `hls_ingest`, `hls_pull`). Lower modules must not import `engine` or `engine_*`. A
  subsystem that needs engine state takes a scoped handle (event log, stage
  manager, egress context) instead of `Arc<MediaEngine>`. The reported
  32-module cycle is the union of those subsystem back-references, so it
  closes only when the last one is gone; each slice removes one back-reference
  and is checked with the same scoped audit.
- Why open: the engine cycle is wider than the removed pairs and needs
  ownership boundaries rather than a move of one shared item.
- Gates: scoped `deslop-please` reports no cycle for each changed area;
  affected lifecycle/protocol tests pass; public API changes are documented.

### Q-124 [architecture] [opus] Untangle the engine composition root and triage the remaining deslop findings
- Goal: reduce the remaining tangle, the `MediaEngine`/`StageRegistry`
  tangle (35 things across 17 modules, first reported at `hls/preview.rs`, now
  `engine_hls_preview.rs`), then
  decide the remaining 18 `shallow-module` and 15 `wide-directory` findings by
  ownership rather than file moves. The `test_harness::resource_sweep` tangle
  is gone: a `PeerPorts` value owns the port-band arithmetic. The
  `hls_put_shard`, EgressTask/FabricRun and SRT `tokio_ingress` tangles are
  gone: fabric runs hold the engine, and SRT publisher tasks hold the engine
  instead of the server.
- Why open: the engine tangle is the union of subsystem back-references into
  `MediaEngine` (see Q-123's design); several shallow `lib.rs` modules may be
  deliberate public façades. No `long-file` finding exists, so file length
  alone is not the problem.
- Design: [Backend module layers](egress-architecture.md#backend-module-layers)
  names the order: leaf/visit and feeds, protocol backends, shard runtime,
  then the engine fabric/factory that constructs a backend with its context.
  The HLS report and `visit_leaf` no longer import `MediaEngine`; the clock
  moved to `timing`. Remaining back-references (progress sink, upload report,
  audio routing) move into a construction context.
- Triage: the shallow findings are cohesive façades, not accidental bags:
  `api::state` and `application::services` expose their owned operations,
  `db` exposes queries, `restream_core`/`restream_domain`/`restream_moq`/
  `restream_rtmp`/`restream_sync` are library surfaces, and the harness
  modules are test-only. Splitting them would add forwarding, so they stay.
  The wide directories (`restream-engine/src`, `egress`, `egress/backends`,
  `srt`, benches, `api`, `runtime_views`, `application`, `db`, `bootstrap`,
  `tests`) match their current owners; no file move is planned without a
  new owner.
- Gates: a design note names the intended layers for the engine tangle; RTMP,
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
