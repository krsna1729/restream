# Backlog

The one list of open engineering work. Each item states what, why and the
gates that close it. Close an item by deleting it in the change that does the
work; the commit message keeps the evidence. Verification direction lives in
the [assurance roadmap](assurance-roadmap.md).

## Contents

- [Q-025 SRT shard law cross-host](#q-025-performance-opus-re-qualify-the-srt-shard-law-after-transport-convergence)
- [Q-074 bounded HLS ingest poll](#q-074-performance-opus-a-rare-multi-millisecond-hls-ingest-poll-is-bounded-whatever-its-cause)

- [Q-110 chrono to jiff](#q-110-platform-sonnet-move-from-chrono-to-jiff-after-jiff-10)
- [Q-115 reqwest 0.13](#q-115-platform-sonnet-move-to-reqwest-013-without-a-crypto-provider-panic-or-a-trust-root-change)
- [Q-116 MoQ ingest follows catalog updates](#q-116-correctness-opus-moq-ingest-takes-tracks-a-publisher-adds-after-its-first-catalog)
- [Q-117 drop vendored hang and moq-msf](#q-117-platform-sonnet-drop-the-vendored-hang-and-moq-msf)
- [Q-118 concrete control-plane composition](#q-118-architecture-opus-one-concrete-control-plane-composition-root)
- [Q-119 egress abstraction review](#q-119-architecture-opus-delete-egress-abstractions-that-only-forward)
- [Q-126 sibling fault gates](#q-126-correctness-opus-live-fault-gates-prove-siblings-are-unaffected)
- [Q-132 API SQLite deadlines and session survival](#q-132-correctness-opus-api-handlers-reach-sqlite-under-a-deadline-and-a-session-survives-a-pool-timeout)
- [Q-133 fabric construction off the registry lock](#q-133-correctness-opus-fabric-runtimes-are-built-outside-the-per-protocol-registry-lock)
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

### Q-116 [correctness] [opus] MoQ ingest takes tracks a publisher adds after its first catalog
- Goal: a publisher whose first catalog lists only some tracks still gets
  every track ingested. `read_catalog`
  (`crates/restream-engine/src/moq/ingest/session.rs`) reads one catalog
  version and cancels; tracks a later version adds are never subscribed.
- Why: the moq-gst publisher can publish a first catalog that lists only
  `0.aac`; the CI `moq-dev-interop --ietf 19` `ingestGstMoqSession` check
  then fails intermittently with "media probe was incomplete within 60s".
- Gates: a session test where the publisher's first catalog has audio only
  and a later version adds video puts both tracks in the ring; a catalog
  update that removes or retypes a live track keeps today's behavior.

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

### Q-126 [correctness] [opus] Live fault gates prove siblings are unaffected
- Goal: contract modes that inject one fault into one entity and assert on
  its siblings. The faults are a panic, a stall, malformed media, a stalled
  reader, a stalled database, a stdout collector that never reads, and an
  Owner fault. The siblings checked are a sibling output of the same
  pipeline, a sibling input of the same pipeline and another pipeline: each
  keeps media flowing without a gap, and its status stays healthy. With the
  stalled stdout, an SRT publisher also connects and disconnects on time
  while `logging.consoleDropped` rises.
- Gates: the modes are listed in `CONTRACT_MODES` and pass on master pushes.
  Disabling an existing boundary, for example the per-peer SRT containment,
  must fail the matching mode.

### Q-132 [correctness] [opus] API handlers reach SQLite under a deadline, and a session survives a pool timeout
- Goal: the shared loops (output pass, housekeeping, egress job rows, the
  scheduler's own rows, catalog reload) reach SQLite only through
  `db::DeadlineDb` on the lifecycle pool. What remains:
  - API handlers, and the services the scheduler's actions call (output
    start/stop requests, file-ingest start/stop), still wait on the main
    pool's 30 s acquire and 30 s busy timeout, which `with_busy_retry`
    multiplies up to 8×. Route their reads through a `DeadlineDb` with a long
    default; writes stay awaited to their outcome (the rule in
    `src/db/deadline.rs`).
  - A session check that hits a pool error evicts the cached session
    (`api/state.rs`), logging the operator out. Keep it and answer 503.
  - A `disallowed-methods` ban on sqlx execution outside `src/db/` is not
    planned: clippy's allow is per lint, so exempting `src/db/` would also
    drop its `Row::get` and `Mutex::lock` bans (`src/db/deadline.rs`).
- Gates: with every main-pool connection held, an API read answers at its
  deadline; a session survives a pool timeout.

### Q-133 [correctness] [opus] Fabric runtimes are built outside the per-protocol registry lock
- Goal: `retain_*_fabric_runtime` (`engine_*_egress_fabric.rs`) reads the
  RTMPS/HLS PUT trust roots from disk (`resolve_client_config`) and builds
  the shard group, waiting on each shard's backend construction
  (`EgressShardHandle::try_spawn_with` blocks in `ready_rx.recv()`), under
  the `fabric.<proto>` tokio mutex and on a Tokio worker; `rescale`'s
  `grow_with` does the same. A hung filesystem behind
  `rtmps_extra_trust_roots_pem_path` stalls every feed's dispatch for that
  protocol. Load the trust roots once off-lock, build the runtime on the
  blocking pool, then insert after a re-check (shutting down the loser off
  the worker).
- Gates: with the trust-roots read held, another feed's dispatch on the same
  protocol completes; no `try_spawn_with` wait runs on a Tokio worker.

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
