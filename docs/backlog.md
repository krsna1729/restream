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
- [Q-120 supervise the MoQ ingest listener](#q-120-correctness-opus-a-faulted-moq-ingest-listener-is-restarted)
- [Q-125 HLS push and MoQ isolation rows](#q-125-correctness-opus-the-isolation-audit-covers-hls-push-and-moq)
- [Q-126 sibling fault gates](#q-126-correctness-opus-live-fault-gates-prove-siblings-are-unaffected)
- [Q-127 reconciler deadlines and listener supervision](#q-127-correctness-opus-the-reconciler-never-stalls-on-one-dependency-and-listeners-restart-without-it)
- [Q-128 deadline-bounded DNS resolver](#q-128-correctness-opus-one-deadline-bounded-resolver-serves-every-lookup)
- [Q-129 non-blocking stdout/stderr logging](#q-129-correctness-opus-logging-never-blocks-the-thread-that-logs)
- [Q-130 SRT loop without engine-lock awaits](#q-130-correctness-opus-the-srt-lifecycle-loop-awaits-no-engine-lock-and-selection-is-per-pipeline)
- [Q-131 scheduler, recording and planning faults](#q-131-correctness-opus-scheduler-recording-and-planning-faults-stay-with-their-own-entity)
- [Q-132 SQLite deadline executor](#q-132-correctness-opus-shared-loops-reach-sqlite-only-through-a-deadline-executor)
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
    decorators whose methods mostly forward to the inner backend. Move
    resolution batching into the backend if nothing else needs the wrapper.
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

### Q-120 [correctness] [opus] A faulted MoQ ingest listener is restarted
- Goal: a MoQ ingest listener that faults (a thread-entry panic, reported as
  `restream.moq.ingest.faulted`, or an accept loop that ends without a
  shutdown) is restarted with backoff, as RTMP and SRT are by
  `SupervisedListener` (`src/infrastructure/bootstrap/listener_supervisor.rs`).
  Today `moq::ingest::listener::launch` starts one thread and nothing
  watches it, so MoQ ingest stays down until the process restarts.
- Gates: a test that injects the accept-loop panic
  (`INJECTED_PANIC_MAX_CONNECTIONS`) then publishes successfully to the same
  port after the restart; the listener thread still joins at shutdown.

### Q-125 [correctness] [opus] The isolation audit covers HLS push and MoQ
- Goal: close the audit's open scope. HLS push ingest and MoQ ingest/origin
  get shared-resource rows with per-entity bounds, total bounds and proofs,
  as RTMP, SRT and egress already have. Q-120 (MoQ listener supervision)
  is part of this.
- Gates: the rows exist and each one names a test that passes.

### Q-126 [correctness] [opus] Live fault gates prove siblings are unaffected
- Goal: contract modes that inject one fault into one entity and assert on
  its siblings. The faults are a panic, a stall, malformed media, a stalled
  reader, a stalled database and an Owner fault. The siblings checked are a
  sibling output of the same pipeline, a sibling input of the same pipeline
  and another pipeline: each keeps media flowing without a gap, and its
  status stays healthy.
- Gates: the modes are listed in `CONTRACT_MODES` and pass on master pushes.
  Disabling an existing boundary, for example the per-peer SRT containment,
  must fail the matching mode.

### Q-127 [correctness] [opus] The reconciler never stalls on one dependency, and listeners restart without it
- Goal: one output's slow SQLite call or file read cannot hold the
  reconciler pass for every pipeline, and a listener fault is handled while
  a pass runs.
  - RTMP/SRT `restart_if_due` and the HTTP-exit arm
    (`src/infrastructure/bootstrap/runtime.rs`) are polled only between
    passes (`bootstrap/reconcile.rs`), so a protocol whose listener faults
    during a stalled pass stays down for the whole stall. Move them into
    their own `spawn_supervised` task.
  - The pass and housekeeping run back to back with no per-step deadline,
    on the pool the API shares (10 connections, 30 s acquire, 30 s busy,
    `with_busy_retry` up to 8×). Move `create_job` into `EgressTask::run`.
    Put a deadline and a `wake_at` retry on `mark_output_retries_exhausted`
    and each pass step. Replace housekeeping's 2N `get_meta` with one bulk
    read, and run housekeeping (prune, recording/QA plans) in its own
    supervised task, pruning `app_logs` in `LIMIT` batches. Pull, SRT-call
    and key-state convergence also run when `list_outputs` fails.
  - The MoQ CA file read (`egress_moq_fabric.rs`) is awaited in the pass;
    move it to the per-output task or put a deadline on it.
- Gates: with every pool connection held by API reads, another pipeline's
  output starts within its deadline; a listener faulted while a pass is
  stalled restarts within its 1–30 s backoff; an isolation-audit row for the
  reconciler names both tests.

### Q-128 [correctness] [opus] One deadline-bounded resolver serves every lookup
- Goal: one `resolve(host, port, deadline)` wrapper, a bounded pool of
  threads that are never joined with in-flight dedupe by key (generalize the
  HLS PUT resolver in `hls_put_shard.rs`). `disallowed-methods` bans
  `std::net::ToSocketAddrs::to_socket_addrs` and `tokio::net::lookup_host`
  outside it.
  - HLS pull's private-address check (`hls_pull.rs`, `hls_pull/fetch.rs`)
    runs before the cancel `select!` with no deadline, and the reconciler
    joins the pull task (`hls_ingest/registry.rs` `ensure_pull`,
    `cancel_pulls`): a black-holed origin nameserver stalls the pass.
  - RTMP egress (`rtmp_shard_resolve_runtime.rs`, `rtmp_shard.rs`): one
    FIFO resolver thread per shard, no deadline, no dedupe; a pending connect
    takes its deadline from `LeafPolicy.resolve_timeout`.
  - SRT egress (`egress/backends/srt/resolve_runtime.rs`, `srt.rs`): the
    same, plus bonded hosts resolved one after another, and `spawn_batch`
    silently drops the rest of a batch after the first full `try_send`. Those
    outputs stay in `pending_connects` holding leaf slots and never retry; a
    failed submit must terminate and remove the output.
  - `srt/calls.rs` and `moq/egress/leaf.rs` migrate to the wrapper.
  - Teardown never joins a resolver: `shutdown()` leaves the Tokio worker
    (`spawn_blocking`), and a panicked shard's `replace_with` under the
    `fabric.<proto>` lock detaches as `shrink` does. Fabric runtimes and
    trust roots are built outside that lock.
- Gates: clippy `-D warnings` with the ban; one black-holed destination on a
  shard leaves a sibling output on that shard connected within its deadline
  (RTMP and SRT); a pull edited while its DNS is black-holed leaves a sibling
  pipeline converging within one pass; an SRT batch past the queue capacity
  leaves every output with a terminal state and no leaked leaf slot.

### Q-129 [correctness] [opus] Logging never blocks the thread that logs
- Goal: the stdout/stderr tracing writer (`SplitWriter` in
  `src/logging/mod.rs`) is synchronous; only the file sink is
  `non_blocking`. A stalled log collector fills the pipe and `write(2)`
  blocks whichever thread logs next: the SRT Tokio loop (every disconnect
  logs), the reconciler, the runtime-event consumer, Owner and shard
  threads. Use `tracing_appender::non_blocking` in lossy mode with a drop
  counter. Count the app-log `try_send` drops too, supervise that drain and
  the recording-metadata writer, and bound the latter's queue.
- Gates: with a stdout writer that never returns, an SRT publisher connects
  and disconnects on time while the drop counter rises; a panicked drain
  restarts; the isolation-audit catalog row stops relying on stdout.

### Q-130 [correctness] [opus] The SRT lifecycle loop awaits no engine lock, and selection is per pipeline
- Goal: the loop shared by every SRT peer (`srt/tokio_ingress.rs`) still
  awaits engine locks inline: `start_caller` registers on the loop,
  `apply_probe` and `finish_publisher` take `selection_lock` and the ingest
  maps, `fold_telemetry` reads `sessions`, `close_deleted_sessions` reads
  `pipelines`. Route `start_caller` through `Admissions`; run `apply_probe`
  and `finish_publisher` in per-peer tasks that answer through a join set;
  keep the `Arc<ActiveIngest>` in `PublisherSession` so telemetry writes
  `metadata.quality` with no lock; `close_deleted_sessions` uses `try_read`
  and skips a tick. `selection_lock` (`engine_registries.rs`) serializes
  every pipeline's ingest lifecycle; key it by `pipeline_id`, keeping the
  map order documented at `IngestRegistry`. Amortize the tracked-IP eviction
  in `security.rs` that runs on the loop.
- Gates: with one pipeline's selection held by a stuck registration, an SRT
  publisher on another pipeline connects, gets its probe answer and
  disconnects on time; with `ingests.sessions` held for writing, the loop
  answers another peer's probe inside the Owner's 2 s probe hold.

### Q-131 [correctness] [opus] Scheduler, recording and planning faults stay with their own entity
- Goal:
  - The scheduler awaits `run_occurrence` serially (`src/api/schedules.rs`):
    a slow statement or a file-ingest stop's `kill` + `wait` pushes other
    pipelines' occurrences past `RUN_GRACE_MS`, recorded as Skip. Spawn each
    `Due::Run` with a deadline around `execute`.
  - A failed meta lookup reads as "stop" (`application/recording.rs`,
    `stream_qa.rs`), so one pool timeout stops every recording and QA
    sampler it hits. Make the lookup tri-state and keep on error.
  - The per-output egress planning loop (`application/egress.rs`) has no
    panic boundary; one panicking output aborts the pass on every retry.
    Wrap each iteration in `contained`.
  - Child processes awaited from async code go through one
    `reap(child, grace)` helper (from `reap_external_child`) and `run_tool`;
    `disallowed-methods` bans `tokio::process::Child::{wait,
    wait_with_output}` and `tokio::process::Command::output` elsewhere.
- Gates: a stalled occurrence leaves another pipeline's due occurrence run,
  not skipped; a pool timeout during housekeeping stops no recording or QA
  sampler; an output whose planning panics leaves the later outputs started
  and stopped as wanted in the same pass; clippy `-D warnings` with the ban.

### Q-132 [correctness] [opus] Shared loops reach SQLite only through a deadline executor
- Goal: a `Db::deadline(d)`-style executor in `src/db/`, applied outside
  `with_busy_retry` (which multiplies the 30 s busy wait up to 8×).
  `disallowed-methods` bans sqlx query execution (`execute`, `fetch*`) and
  `Pool::{acquire, begin, connect_with}` outside `src/db/`. Repository
  functions take a deadline: the shared loops (reconciler, topology, egress
  start, housekeeping, recording/QA, scheduler, catalog reload, runtime-event
  persistence) pass seconds with a `wake_at` retry, API handlers a long
  default. Lifecycle work gets capacity API load cannot take (a small second
  pool or a short acquire deadline). A session check that hits a pool error
  keeps the cached session and answers 503 instead of logging the operator
  out (`api/state.rs`).
- Gates: clippy `-D warnings` with the ban; with every pool connection held
  by API reads, each shared-loop call returns at its deadline and the next
  pass retries, while another pipeline's connect, disconnect and probe answer
  stay on time; a session survives a pool timeout.

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
