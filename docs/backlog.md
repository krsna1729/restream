# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-117 drop vendored hang and moq-msf](#q-117-platform-sonnet-drop-the-vendored-hang-and-moq-msf)
- [Q-118 concrete control-plane composition](#q-118-architecture-opus-one-concrete-control-plane-composition-root)
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

### Q-117 [platform] [sonnet] Drop the vendored hang and moq-msf
- Goal: `hang` and `moq-msf` come from crates.io again, so Dependabot and
  the advisory checks see their real versions.
- Gates, per crate: an upstream release passes the vendored regression
  cases, ported to Restream tests against the public API. Then delete
  `vendor/<crate>` and its `[patch.crates-io]` entries in the root and
  `fuzz/` manifests, and the `moq_ingest` fuzz smoke stays clean.

### Q-118 [architecture] [opus] One concrete control-plane composition root
- Goal: HTTP handlers reach `db::*`, `MediaEngine` and config through one
  concrete application root, not `AppServices` (`src/api/mod.rs`) built by
  `SqliteServiceFactory` (`src/infrastructure/service_wiring.rs`).
  Every catalog, input and admission service already calls `crate::db`
  through `DeadlineDb`.
- Remaining: replace the factory with `AppServices` fields built directly
  from one root (API database, lifecycle database, `CatalogAuth`, recording
  reporter), and migrate the tests that call factory methods.
- Keep `NotifyTransport` (scripted HTTP sends) and the services that hold
  policy (stream-key lookup, settings workflows, file ingest, media library,
  agent mutations).
- Gates: one vertical slice per change with its handler tests unchanged;
  API contract tests stay green; the noun checks in
  `tests/architecture/service_wiring.rs` go with the types they name; net
  production lines go down.

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
