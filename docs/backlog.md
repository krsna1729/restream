# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-115 reqwest 0.13](#q-115-platform-sonnet-move-to-reqwest-013-without-a-crypto-provider-panic-or-a-trust-root-change)
- [Q-117 drop vendored hang and moq-msf](#q-117-platform-sonnet-drop-the-vendored-hang-and-moq-msf)
- [Q-118 concrete control-plane composition](#q-118-architecture-opus-one-concrete-control-plane-composition-root)
- [Q-119 egress abstraction review](#q-119-architecture-opus-delete-egress-abstractions-that-only-forward)
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

### Q-115 [platform] [sonnet] Move to reqwest 0.13 without a crypto-provider panic or a trust-root change
- Goal: every crate builds against reqwest 0.13; `.github/dependabot.yml`
  holds it at 0.12 until then. 0.13 renamed `rustls-tls`; its `rustls`
  feature brings aws-lc-rs, a second provider beside ring. With
  `rustls-no-provider`, a `Client` built without preconfigured TLS panics
  unless a process-default provider is installed; its roots come from the
  platform store (`rustls-platform-verifier`), not `webpki-roots` as now.
  `use_preconfigured_tls` is deprecated, and `RequestBuilder::query`
  needs the `query` feature.
- Scope: 6 crates and 16 `Client` construction sites, including tests.
  `ffmpeg_binary.rs` adds PEM roots with `add_root_certificate`, which
  preconfigured TLS ignores.
- Gates: no client construction can panic for want of a provider (one
  shared builder or a provider installed before any client, covering
  tests); trust roots stay webpki-roots plus the custom PEM roots; aws-lc-rs
  stays out of `Cargo.lock`; the Dependabot ignore is removed.

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
  `SqliteServiceFactory` (`src/infrastructure/service_wiring.rs`) over
  `Arc<dyn …Store>` ports whose only production implementations are the
  SQLite adapters in `src/infrastructure/` (tests add fakes).
  Logs, outputs and health are already plain application functions.
- Keep the services that hold policy (stream-key lookup, auth and sessions,
  settings workflows, file ingest, media library, agent mutations); delete
  ports, adapters and factory methods that only forward to `db::*`.
- Gates: one vertical slice per change with its handler tests unchanged;
  API contract tests stay green; the noun checks in
  `tests/architecture/service_wiring.rs` go with the types they name; net
  production lines go down.

### Q-119 [architecture] [opus] Delete egress abstractions that only forward
- Goal: each candidate is kept with a stated reason or deleted:
  - `ResolvingSrtShardBackend` (`egress/backends/srt/resolve_runtime.rs`) and
    `ResolvingRtmpShardBackend` (`egress/backends/rtmp_shard_resolve_runtime.rs`):
    decorators whose methods mostly forward to the inner backend; each now
    only submits its `Add`/`Update` lookup to the process resolver. Move that
    into the backend if nothing else needs the wrapper.
  - `LeafLimits` (`egress/policy.rs`): a renamed subset of `LeafPolicy` built
    by `from_policy`.
  - `EgressFeed` (`egress/feed.rs`): keep unless one concrete feed can serve
    both rings. `RingFeed` follows the ring chain and `TsFeed` differs in
    its keyframe checks, so the two are not one implementation with a
    different `Unit`.
- Out of scope: `EngineProgress`, `CloseReason`, `WakeGate` coalescing,
  muxer port reuse, the connect-admission semaphore, and the model-based
  proptests (`scheduler.rs`, `journal/tests.rs`).
- Gates: per candidate, a layering-audit read of every call site; the egress
  unit tests, proptests and Kani proofs unchanged and passing; the
  `egress_visit` benchmark before and after for any change on the visit path.

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
