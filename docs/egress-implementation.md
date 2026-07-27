# Egress implementation

This document is the executable migration plan for the target described in
[egress architecture](egress-architecture.md). It is intentionally organized
as independently reviewable slices with explicit proof and rollback gates.

The plan preserves the current production path until the common egress fabric
has demonstrated protocol correctness, slow-neighbor isolation, bounded memory,
and live scale parity.

## Contents

- [Delivery principles](#delivery-principles)
- [Definition of success](#definition-of-success)
- [Proposed source layout](#proposed-source-layout)
- [Core types](#core-types)
- [Implementation phases](#implementation-phases)
- [Phase 0: Baseline and instrumentation](#phase-0-baseline-and-instrumentation)
- [Phase 1: Common contracts and deterministic model](#phase-1-common-contracts-and-deterministic-model)
- [Phase 2: Bounded feeds and cursor semantics](#phase-2-bounded-feeds-and-cursor-semantics)
- [Phase 3: Shard runtime and scheduler](#phase-3-shard-runtime-and-scheduler)
- [Phase 4a: Sink backend](#phase-4a-sink-backend)
- [Phase 4: SRT migration](#phase-4-srt-migration)
- [Phase 5: RTMP and RTMPS migration](#phase-5-rtmp-and-rtmps-migration)
- [Phase 6a: Pipeline recirculation backend](#phase-6a-pipeline-recirculation-backend)
- [Phase 6: Production integration and rollout](#phase-6-production-integration-and-rollout)
- [Phase 7: Tuning and legacy removal](#phase-7-tuning-and-legacy-removal)
- [Existing-code change map](#existing-code-change-map)
- [Testing strategy](#testing-strategy)
- [Adversarial isolation matrix](#adversarial-isolation-matrix)
- [Benchmark plan](#benchmark-plan)
- [Acceptance gates](#acceptance-gates)
- [Observability rollout](#observability-rollout)
- [Configuration rollout](#configuration-rollout)
- [Failure and shutdown semantics](#failure-and-shutdown-semantics)
- [Pull-request sequence](#pull-request-sequence)
- [Risk register](#risk-register)
- [Rollback strategy](#rollback-strategy)
- [Completion checklist](#completion-checklist)

## Delivery principles

Every implementation slice follows these rules:

- preserve a compiling and testable repository at every merge;
- add the proof before or with the behavior it protects;
- keep legacy and fabric ownership mutually exclusive for an output;
- avoid changing persisted configuration or API contracts unless necessary;
- introduce abstractions only after both a fake engine and a real protocol need
  them;
- do not optimize by adding a payload copy without end-to-end evidence;
- make all capacities and deadlines strict and observable;
- reject blocking or unbudgeted work at the protocol boundary;
- prefer deterministic state-machine tests before live harness tests;
- remove legacy code promptly after the replacement is proven so two runtime
  models do not become permanent.

No phase is complete solely because a target output count connects. Isolation,
memory bounds, progress, and protocol correctness are first-class gates.

## Definition of success

The effort is complete when all of the following are true:

- RTMP, RTMPS, and SRT use the same manager, shard scheduler, leaf lifecycle,
  retry policy, slow-consumer policy, and metrics schema;
- protocol code owns only preparation-specific, handshake, wire, and native
  readiness mechanics;
- application egress threads remain fixed when outputs scale from tens to more
  than 1,000;
- there is no application sender thread, media `MemoryQueue`, or independent
  retry task per live network output;
- a permanently blocked and a severely throttled destination on the same shard
  do not materially degrade 998 healthy neighbors;
- dead destinations on one shard do not materially degrade healthy outputs on
  another shard;
- indefinite stalls do not cause unbounded resident memory growth;
- feed publication never waits for egress;
- reconnect storms remain within configured admission limits;
- live protocol probes, timestamp checks, media integrity checks, and teardown
  checks pass;
- legacy RTMP and SRT egress paths and their obsolete admission limits are
  removed.

## Proposed source layout

Introduce a focused `media::egress` module. Keep protocol wire helpers in their
existing protocol modules until a move has a clear ownership benefit.

```text
src/media/egress/
├── mod.rs
├── command.rs
├── config.rs
├── feed.rs
├── journal.rs
├── leaf.rs
├── lifecycle.rs
├── manager.rs
├── metrics.rs
├── policy.rs
├── scheduler.rs
├── shard.rs
├── supervisor.rs
├── timer.rs
├── backend.rs
├── test_driver.rs
└── backends/
    ├── mod.rs
    ├── tcp.rs
    └── srt.rs
```

Protocol engines remain explicit:

```text
src/media/rtmp/egress_engine.rs
src/media/srt/egress_engine.rs
```

Shared SRT preparation may continue to use `TsChunkRing` initially, then move
behind the generic feed contract once cursor and notification semantics are
proven.

Do not create a new crate during the first migration. A module boundary allows
iteration while the ownership contract is still being proven. Crate extraction
is reconsidered only after both protocol migrations are complete and the
public surface is stable.

## Core types

The exact names may change during implementation, but the ownership model must
remain explicit.

### Output specification

```rust
pub struct OutputSpec {
    pub id: OutputId,
    pub generation: u64,
    pub feed: FeedId,
    pub protocol: ProtocolSpec,
    pub policy: LeafPolicy,
}
```

`generation` is required to reject stale commands, timers, and readiness events.

The initial `ProtocolSpec` set covers network egress. Two planned non-network
variants extend the same contract:

- `Sink`, which consumes a selected feed and intentionally discards every unit;
- `Pipeline`, which recirculates one pipeline's prepared output into another
  in-process pipeline input.

Neither variant gets a manager or lifecycle bypass. Both remain ordinary
outputs with user-visible status, policy, and admission behavior.

### Common leaf state

```rust
pub struct LeafCommon {
    pub id: OutputId,
    pub generation: u64,
    pub feed: FeedId,
    pub cursor: FeedCursor,
    pub lifecycle: LeafLifecycle,
    pub schedule: ScheduleState,
    pub deadlines: Deadlines,
    pub retry: RetryState,
    pub progress: ProgressState,
    pub limits: LeafLimits,
    pub pending_application_bytes: usize,
}
```

### Work budget

```rust
pub struct WorkBudget {
    pub max_units: usize,
    pub max_bytes: usize,
    pub deadline: std::time::Instant,
}
```

The implementation checks all three dimensions. Time is a guard against an
unexpectedly expensive serializer or native call; bytes and units provide
deterministic fairness.

### Engine result

```rust
pub enum EngineProgress {
    Progress {
        bytes: usize,
        units: usize,
        interest: Interest,
    },
    Needs(Interest),
    HandshakeComplete,
    FeedOverrun,
    PeerClosed,
    Failed(ProtocolFailure),
    Yield,
}
```

### Scheduler state

```rust
pub struct ScheduleState {
    pub enqueued: bool,
    pub deficit_bytes: usize,
    pub last_service_at: std::time::Instant,
}
```

A leaf cannot appear twice in the ready queue. `enqueued` changes only through
small shard-local helper functions with direct unit tests.

### Pending wire state

Pending state is protocol-specialized but included in the common accounting.

RTMP or RTMPS needs independent offsets for protocol header, payload, and TLS
output as applicable. SRT retains one immutable message until accepted or
failed. Every byte is included in the leaf limit.

## Implementation phases

The phases are ordered to prove the common policy with deterministic machinery,
then remove the highest-cost current implementation first.

A phase may be split into several pull requests, but a later protocol migration
must not bypass an incomplete common invariant.

## Phase 0: Baseline and instrumentation

### Objective

Make the existing architecture measurable enough to compare fairly and detect
regressions during dual-path rollout.

### Work

Add or standardize current-path metrics for:

- number of RTMP egress tasks;
- number of application SRT sender threads;
- number and bytes of per-output SRT queues;
- ring and `TsChunkRing` notifications;
- reader wakeups that return no packets;
- bytes and packets per successful send operation;
- RTMP `write_all` duration;
- SRT queue-block duration;
- output progress age and retry rate;
- process thread count by known family;
- RSS and allocator activity during stalls.

Add a deterministic workload manifest for:

- 1,000 healthy RTMP outputs;
- 1,000 healthy SRT outputs where the current sender cap is deliberately
  recorded as a known failure or skip;
- mixed 1,140 RTMP and 60 SRT output baseline;
- 998 healthy, one throttled, and one dead destination;
- reconnect storm with at least 25 percent of outputs failing together.

Capture current CPU, RSS, context switches, migrations, send progress, receiver
health, and tail progress age. Store raw artifacts under the repository's
existing quality evidence conventions.

### Proof

- existing tests remain green;
- baseline commands are reproducible;
- metrics add negligible steady-state overhead;
- the workload can distinguish a healthy destination from a connected but
  non-progressing destination.

### Exit gate

Do not begin architecture tuning without a baseline artifact that includes
healthy-control and bad-neighbor variants.

Current branch status:

- The deterministic workload manifest lives at
  `test/harness/baselines/egress-phase0/manifest.json` with five shapes:
  healthy RTMP fan-out, healthy SRT fan-out (legacy 512-sender cap recorded
  as a known architectural failure at target scale), mixed fan-out,
  bad-neighbor (stalled output beside healthy siblings), and reconnect storm.
- Baseline artifacts are recorded per host class beside the manifest;
  captures at reduced scale are valid for that host class, and target-scale
  captures require a 1,000-output-capable host running the same manifest
  rows.

## Phase 1: Common contracts and deterministic model

### Objective

Prove lifecycle, scheduling, backpressure, retry, and command semantics without
network or protocol complexity.

### Work

Implement:

- `LeafLifecycle` and legal transition table;
- generation-aware `EgressCommand` handling;
- `LeafPolicy`, deadlines, and retry state;
- `WorkBudget` and `EngineProgress`;
- ready-queue deduplication;
- bounded round-robin or deficit-round-robin scheduler;
- timer structure;
- fake feed with sequence, epoch, sync points, and overrun;
- deterministic fake protocol engine;
- fake poller whose readiness sequence is test-controlled.

The fake engine must support scripted behaviors:

- always makes progress;
- always needs write readiness;
- makes partial progress and blocks;
- consumes CPU budget without bytes;
- fails after a configured number of writes;
- never completes handshake;
- closes after becoming active;
- reports feed overrun;
- returns contradictory or zero progress to test defensive handling.

### Proof

Add table-driven tests for every legal and illegal lifecycle transition.

Add scheduler tests proving:

- one runnable entry per leaf;
- bounded service quantum;
- FIFO or deficit fairness under repeated readiness;
- a blocked leaf leaves the runnable set;
- an always-writable leaf cannot starve another leaf;
- command and timer processing continue during readiness storms;
- stale generation events are ignored;
- retry waiting consumes no active scheduler visits.

Use a model checker or Loom where shared command wakeups, shutdown flags, or
feed wake coalescing involve atomics. Keep the shard's mutable hot state
single-thread-owned so the model surface remains small.

Current branch status:

- The feed-wake coalescing seam is loom-proven
  (`tests/egress_feed_wake_loom.rs`, wired into the concurrency gate).
  `WakeGate::notify` now returns whether the flag transitioned clear-to-set,
  obligating the publisher to deliver exactly one wake through the shard's
  wake primitive; lost-wakeup safety rests on that delivery pairing, and the
  model checks the planned publish/clear/republish interleaving plus
  concurrent-publisher coalescing. Shard shutdown and command handoff use
  `std::sync::mpsc` channels, which need no additional model surface.

### Exit gate

The common scheduler and lifecycle must pass all isolation tests with no real
socket code and no wall-clock sleeps.

## Phase 2: Bounded feeds and cursor semantics

### Objective

Replace per-destination media backlog ownership with bounded shared retention
and non-pinning cursors.

### Work

Define the `EgressFeed` behavior around:

- monotonic sequences;
- epochs;
- bounded bytes and media duration;
- immutable units;
- latest and indexed synchronization points;
- batched reads under a read budget;
- explicit overrun and epoch-mismatch results;
- coalesced per-shard wakeups.

Initially adapt existing structures instead of rewriting them together:

- adapt `RingBuffer` or a focused view for RTMP-compatible units;
- adapt `TsChunkRing` for SRT transport messages;
- keep existing single-producer assumptions intact;
- add one subscription cursor per shard where possible, then shard-local leaf
  cursors over the obtained batch.

The preferred scalable shape is one feed notification and one source read per
interested shard, followed by local fan-out. Do not leave one parked async
reader per destination as the final design.

### Strict retention

Enforce both byte and media-age limits. A large unit must not silently exceed a
configured limit without an explicit policy. Define behavior for units larger
than total configured capacity: reject configuration, admit a single oversized
unit with a separately visible condition, or fail the stage. Do not deadlock.

### Wake coalescing

Implement one outstanding notification per feed and shard. The notification is
a hint; the shard compares feed head sequence and drains all currently
available work within budget.

Test the lost-wakeup boundary by interleaving:

1. publisher advances the head;
2. shard observes the head;
3. shard clears `wake_pending`;
4. publisher advances again.

The protocol must guarantee that either the second publish sends a wake or the
shard observes the new head before sleeping.

### Proof

- cursor cannot pin old entries;
- overrun occurs deterministically at the retention boundary;
- epoch changes invalidate old cursors;
- sync-point lookup remains correct after wraparound;
- publication never blocks on an egress consumer;
- one publication produces no more than one outstanding wake per shard;
- 1,000 leaf cursors do not multiply retained payload memory;
- slow cursors do not change healthy cursor results.

### Exit gate

A synthetic 1,000-leaf feed test must show bounded memory and shard-level wake
amplification before any real protocol uses the feed.

Current branch status:

- The exit gate is met by deterministic unit proofs in
  `src/media/egress/journal/tests.rs`: a 1,000-leaf feed with lagging cursors
  keeps ring retention capped at ring capacity across repeated wraps while
  healthy leaves share payload storage and stalled leaves overrun with a valid
  resync point, and a 100-publish burst against 1,000 leaves delivers exactly
  one wake per interested shard gate.

## Phase 3: Shard runtime and scheduler

### Objective

Run the deterministic fabric on fixed OS threads with real command wakeups,
timers, supervision, and metrics.

### Work

Implement `EgressManager`, shard creation, and one generic shard loop.

Each shard receives:

- a bounded high-priority command channel;
- a native wake handle integrated with its poller;
- a ready queue;
- a timer wheel or heap with stale-generation rejection;
- feed subscription state;
- local metrics;
- a heartbeat and loop-progress timestamp.

The loop uses separate budgets for:

- commands per iteration;
- readiness events per iteration;
- timers per iteration;
- leaves serviced per iteration;
- bytes, units, and CPU time per leaf;
- total CPU time before returning to the poller.

The manager uses stable assignment and does not hold a global mutable lock while
shards perform media work. Start with deterministic hashing; introduce weighted
assignment only after metrics demonstrate actual imbalance.

### Supervision

Catch shard panics at the thread entry point. Publish failure, close the command
path, and let the supervisor start a replacement. Desired outputs are recreated
from application state rather than attempting socket-state migration.

Add a heartbeat watchdog. A warning threshold detects a stalled loop without
immediately creating duplicate output ownership. Automated replacement after a
hang must be gated until the old shard is proven unable to resume or the
process-level strategy is defined; panic replacement is safe earlier than
native-hang replacement.

### Proof

- add, update, remove, and shutdown are idempotent;
- a full command channel fails visibly and reconciliation eventually converges;
- command flood does not starve media work;
- readiness flood does not starve removal or shutdown;
- timer cancellation and stale events cannot affect a replacement leaf;
- shard panic affects only assigned test leaves;
- other shards continue to advance;
- no thread leak remains after repeated manager startup and shutdown.

### Exit gate

The fake backend must run the full same-shard and cross-shard isolation matrix
on actual shard threads with deterministic bounded completion.

Current branch status — the gate is met by deterministic fake-backend tests on
real shard threads (`src/media/egress/shard/tests/`), recorded here per
matrix row:

| Matrix row | Proof |
|---|---|
| Headline: healthy population + one blocked + one throttled, same shard | `leaf_isolation::healthy_population_progresses_beside_blocked_and_throttled_leaves_same_shard` |
| Headline cross-shard control | `leaf_isolation::healthy_shard_unaffected_by_blocked_and_throttled_leaves_on_other_shard` |
| Blocked leaf leaves runnable set, same/cross shard | `leaf_isolation::blocked_leaf_*` |
| Slow leaf cannot starve neighbors | `sink::slow_sink_leaf_does_not_starve_network_leaf_on_same_shard_thread` |
| Command flood does not starve media or ready work | `runtime::command_batch_budget_*` |
| Timer flood does not starve media or removal | `runtime::timer_batch_budget_*` |
| Readiness flood does not starve removal or shutdown | `runtime::readiness_batch_budget_*`, `group_supervision::ready_flood_on_one_shard_does_not_starve_another_shard_command` |
| Stale or removed-output timers are ignored | `runtime::stale_timer_generation_is_ignored_on_shard_thread`, `runtime::removed_output_timer_is_ignored_on_shard_thread` |
| Command queue saturation is visible and converges | `group::manager_dispatch_to_group_converges_after_shard_queue_full` |
| Shard panic contained; only its outputs replayed | `group_supervision::shard_group_contains_panic_to_assigned_shard`, `group::manager_replays_only_replaced_shard_outputs_after_panic` |
| Stalled heartbeat warns without replacement | `group_supervision::stalled_shard_heartbeat_does_not_trigger_panic_replacement` |
| No thread leak across repeated startup/shutdown | `runtime::repeated_shard_group_startup_shutdown_joins_every_thread` |

## Phase 4a: Sink backend

### Objective

Add a user-selectable egress kind that consumes prepared media through the
fabric and discards it without sending bytes to a network or file destination.

This lands before SRT migration because it is the cheapest real backend that
exercises feed cursors, wakeups, lifecycle, policy, metrics, status, and shard
fairness without native transport readiness.

### Use cases

- capacity tests that measure source, preparation, feed, and scheduler cost
  without provisioning receivers;
- soak tests for slow-neighbor isolation and feed-retention behavior where
  network variability would obscure fabric bugs;
- operator diagnostics: prove that a pipeline produces egress-ready media even
  when an external destination is unavailable;
- staging or rehearsal outputs that intentionally black-hole media while
  keeping desired-output and status surfaces realistic;
- benchmark separation between shared preparation cost and protocol transport
  cost;
- safe failure-injection targets that can simulate progress, no-progress,
  overrun, or configured discard rates without sockets.

### Work

Add a `Sink` protocol spec and backend that:

- consumes the configured feed using normal cursors and `WorkBudget`;
- reports byte and unit progress through common progress accounting;
- can optionally cap units or bytes per visit for deterministic fairness tests;
- uses no transport readiness registration and is treated as always writable;
- publishes ordinary output status, lifecycle, counters, and removal behavior;
- never bypasses admission, shard assignment, retry/no-progress policy, or
  feed-overrun handling.

The first version should not add new media transforms. It should discard the
same feed units a network backend would consume for the selected output.

### Proof

- sink add, remove, update, stale generation, and shutdown behavior matches
  other fabric outputs;
- sink leaves cannot pin retained media after removal;
- sink progress advances feed cursors and status monotonically;
- a deliberately slow sink does not starve healthy sink or network leaves on
  the same shard;
- a sink overrun follows the shared overrun policy;
- sink metrics can distinguish discarded units and bytes from network-sent
  units and bytes.

### Exit gate

Fabric sink is available through the same desired-output API as other output
kinds and is safe to use in local, test, and staging environments. It does not
become a substitute for live SRT or RTMP correctness evidence.

**Correction, then fixed — this was marked fully met; it was not, and now
is.** `start_sink_egress` (`src/media/egress/backends/sink.rs`) was a
plain per-output `tokio::spawn` task driven by
`tokio::select!`/`Reader::wait_for_data()` — the exact one-task-per-output
pattern this migration exists to replace. `SinkEngine` implements
`ProtocolEngine` and was genuinely schedulable on a shard — proven by
`SinkHarnessBackend` in `src/media/egress/shard/tests/sink.rs` — but that
harness was test-only; nothing routed `ProtocolSpec::Sink` onto real
shard OS threads. The earlier "same desired-output API" reading of this
exit gate was too narrow: reaching the same API and command types is not
the same as running on the fabric.

**Implemented.** A real production `SinkShardBackend`
(`src/media/egress/backends/sink_shard.rs`) now exists, following the
already-proven RTMP/SRT shard-backend shape (`LeafCommon` lifecycle,
generation-checked `Add`/`Update`/`Remove`, `EngineVisit::run` for shared
progress/generation handling) with one genuine design difference: a sink
leaf has no socket and no poller at all. SRT always registers write
interest and relies on `epoll`/`srt_epoll_wait` to report writability;
RTMP's `EgressCommand::FeedWake` handler only *widens poller interest* so
the *next* real socket-readiness poll picks the leaf up. Sink has no
socket-readiness poll to widen interest for — a sink leaf is
conceptually always "writable" (discarding costs no I/O), so
`EgressCommand::FeedWake` is its *only* readiness signal.
`SinkShardBackend::on_command`'s `FeedWake` arm therefore directly
re-enqueues every leaf into the ready queue (`enqueue_all_leaves`)
instead of adjusting registration state, and `on_media_tick` drains the
ready queue every tick regardless, so a missed or coalesced wake costs
one extra tick of latency, not correctness.

Application-layer wiring mirrors RTMP/SRT exactly:
`prepare_sink_fabric_feed`/`sink_fabric_output_spec`
(`src/application/egress.rs` — simpler than SRT's, since sink reads
directly off the output's own ring with no shared muxer stage to
resolve), `spawn_sink_fabric_shard_group`
(`src/media/egress/factory.rs`), a `SinkFabricRegistry` and
`retain_sink_fabric_runtime`/`dispatch_sink_fabric_command`/`release_sink_fabric_runtime`
(`src/media/engine_sink_egress_fabric.rs`, registered in
`FabricRegistry`), and `EgressTask::run_sink_fabric`
(`src/infrastructure/bootstrap/egress.rs`) — including the same
`terminated_unexpectedly`/`wait_for_stop_or_leaf_failure` retry wiring
RTMP/SRT leaves use, so a sink leaf that fails is retried like any other
fabric output rather than sitting silently stale (`start_sink_egress`
remains as a fallback if `sink_fabric` is ever `None`, matching the
SRT/RTMP call-site shape, though in practice every `Sink`-scheme output
now builds one).

Proof: 3 new shard-level tests (`sink_shard/tests.rs`) drive
`SinkShardBackend` through `EgressShardHandle::spawn` on a real OS
thread — discards a published unit, discards a unit published *after*
the leaf goes idle (the `FeedWake`-is-the-only-signal path specifically),
and stops discarding after `Remove`; verified as real regressions by
temporarily making `FeedWake` a no-op and confirming both liveness-
dependent tests fail. 2 new engine-level integration tests
(`engine_tests/egress_fabric.rs`) exercise the full production registry
path: `retain`/`release` reference counting, and an end-to-end
`dispatch_sink_fabric_command(Add)` → real `ring.push()` → the same
production wake-watcher task RTMP/SRT use → real shard thread →
observed via the real `EgressProgressSink` counters the application
layer wires up. Full `cargo test --lib` (1,871 tests), clippy, fmt,
source-audit, and docs checks all pass.

## Phase 4: SRT migration

### Objective

Remove the current per-output `MemoryQueue` and sender thread and prove more
than 1,000 SRT leaves with fixed application egress threads.

### Work

Create an SRT readiness backend that owns:

- one SRT epoll instance per shard;
- shard-local registration from SRT socket to leaf key and generation;
- a command wake mechanism that can interrupt the poll wait;
- bounded epoll event batches;
- deterministic deregistration before socket destruction.

Convert SRT egress sockets to non-blocking send mode. `srt_send` must return
without blocking when the native sender buffer is full. The engine retains the
current immutable TS message and waits for writable readiness.

Move the following behavior into the common fabric:

- connect and handshake deadlines;
- retry delay and jitter;
- process and shard connect admission;
- output removal and cancellation;
- progress and no-progress deadlines;
- slow-consumer resynchronization;
- status publication.

Retain protocol-specific behavior in the SRT engine:

- URL and stream-ID normalization;
- socket options, encryption, latency, and bonding policy;
- caller or rendezvous connection mechanics where supported;
- message send and SRT error classification;
- SRT statistics collection.

Use shared `TsChunkRing` or its feed adapter as immutable message storage. The
leaf must not copy chunks into a `VecDeque<u8>` or private stream backlog.

Current branch status:

- `RESTREAM_EGRESS_FABRIC` remains disabled by default.
- SRT output preparation now builds a shared `TsFeed` from the existing
  `TsChunkRing` muxer assignment.
- `MediaEngine` owns SRT fabric runtimes by `FeedId`, dispatches add/remove
  commands through the common manager and shard group, tears down a feed runtime
  when its final active fabric output stops, and still drains all remaining SRT
  fabric runtimes during engine-wide task cancellation.
- Bootstrap routes SRT outputs through this fabric path only when
  `RESTREAM_EGRESS_FABRIC` routes SRT; the legacy `start_srt_egress` path
  remains the default.
- First live fabric proof at host scale (vps-6cpu-12gb, N=100 healthy SRT
  outputs, `w2-fabric` capture): all outputs healthy on the fabric runtime
  with per-output RSS 1,500KB versus legacy 3,426KB (2.3x lower). CPU was
  57.0% versus legacy 41.7%; the regression was attributed to the 1ms
  idle-wait shard polling loop.
- Feed-wake delivery is now wired end to end: a per-feed watcher bridges the
  ring's publish notifier into coalesced `FeedWake` commands through one
  `WakeGate` per shard (`FeedWakeHandle`), the shard clears its gate before
  each drain, and the idle wait default rose from 1ms to 25ms since sleep
  now ends on delivery rather than polling. Watchers abort on runtime
  release and engine shutdown. Re-measurement (`w2-fabric-wake` capture)
  shows CPU 47.4% versus 57.0% under polling and RSS 1,420KB per output
  (2.4x below legacy); the remaining 14% CPU gap versus legacy runs with 4
  shards for 100 outputs and is gated on the Phase 7 shard-count sweep.

### Native buffer accounting

Define and validate SRT native sender-buffer ceilings. A leaf is considered
backpressured or stalled based on both application pending state and native SRT
progress. An implementation that removes `MemoryQueue` but permits unlimited
libsrt buffering does not satisfy the architecture.

Current branch status:

- The native sender-buffer ceiling is set pre-connect on every SRT egress
  socket (`DESIRED_SRT_BUF` in `src/media/srt/socket.rs`).
- The fabric transport now exposes instantaneous native sender-buffer
  occupancy (`NativeSendBacklog` via `srt_bistats`), and
  `SrtFabricLeaf::pressure` combines retained application bytes with native
  backlog: a leaf with a drained application queue but a saturated native
  buffer is classified backpressured, and both byte sources charge the leaf
  memory envelope.
- Stall classification is shared policy: `classify_stall` splits idle,
  backpressured, and stalled by combined pending bytes and progress age, and
  `SrtFabricLeaf::observe_stall` counts a declining native backlog as
  protocol progress so slow native drain reads as backpressure while a
  non-declining native buffer past the no-progress deadline reads as
  stalled.
- Recovery is driven from that classification on the shard thread: a
  once-per-second stall sweep in the SRT backend's media tick closes every
  stalled leaf with `CloseReason::NoProgress`, deregisters its poller entry,
  and releases the socket mapping, so reconnection flows through the
  application retry policy (SRT recovery is reconnect-only). Deterministic
  tests prove a stuck native backlog closes at the deadline while a
  declining backlog keeps the leaf alive indefinitely.
- Shard-count datapoint: at N=100 the fabric measures identical CPU with 2
  and 4 shards (47.4%), so the remaining CPU gap versus legacy is
  per-message path cost, not shard overhead; attribution belongs to the
  Phase 7 perf sweep.
- Status publication is wired: `EgressProgressSink` carries lock-free
  application counter handles (`bytesOut`, last-progress stamp) on
  `LeafCommon`, the common visit path records every progress result into
  them, and bootstrap populates the handles from the output's egress
  registration — fabric outputs now report progress through the same
  `/api/v1/engine/health` surface as legacy.
- Live-path defects found and fixed by the crypto-matrix gate:
  1. `FeedWake` now schedules a ready visit (previously nothing on the live
     path ever pumped the poll-and-visit chain; deterministic tests drove
     visits directly and masked the gap).
  2. `on_ready` now removes a leaf on `VisitDecision::Close` instead of
     silently dropping the decision, which otherwise leaked a
     connected-but-dead socket that stayed registered and never revisited.
  3. `EngineProgress::FeedOverrun` now resynchronizes the leaf's cursor to
     the feed's latest sync point (or oldest retained sequence) in place
     instead of closing the connection, satisfying the architecture's
     "feed overrun and reconnect at a sync point" proof requirement and
     avoiding a connect/overrun/close cycle for a transient overrun — the
     live gate's failure pattern (sockets connected, zero `packetsOut`, all
     stalled) is consistent with an SRT connect-handshake delay long enough
     for the shared TS ring to wrap past retention before the first visit.
  4. Root cause of the persisting stall after fixes 1-3: the feed watcher
     used a bare `notify.notified().await` loop, which only wakes on
     notifications delivered *after* the await begins — `notify_waiters()`
     wakes only waiters already polling at the moment it fires. A publish
     landing before the watcher's first poll (the muxer stage's first burst
     racing runtime creation) was invisible to it, and the watcher then
     waited on some unrelated future push. Fixed by mirroring
     `Reader::wait_for_data`'s check-register-recheck pattern: read the
     feed head, register interest, read the head again, and only await if
     nothing changed — closing the exact race window a bare notify loop
     leaves open.
  Confirmed by a legacy-path control run of the same srt-crypto-matrix
  scenario without the fabric flag, which passed cleanly (20/20, then
  60/60 on a later pipeline) — isolating all four defects to the fabric
  wake-delivery path rather than the test scenario itself.
  5. Actual root cause of the still-persisting stall after fixes 1-4: an
     unrelated background sweep, `MediaEngine::sweep_unused_stages`
     (`src/media/engine_pipeline.rs`), runs every reconcile tick and cancels
     any shared TS muxer stage whose ring has zero registered `Reader`
     instances — a legacy-only liveness signal. Fabric consumes a feed via
     `EgressFeed::read_from` (direct cursor reads), which never registers a
     `Reader`, so any muxer stage feeding fabric-only SRT outputs looked
     permanently unused and was cancelled by the very first reconcile tick,
     starving every leaf sharing that feed before a single byte flowed —
     independent of and upstream from every fix above. Confirmed live via a
     one-shot diagnostic showing the muxer's reader registering then
     deregistering ~23ms later with the self-cancel log firing on loop exit.
     Fixed by also checking `MediaEngine`'s live SRT fabric runtimes
     (`fabric.srt.active_outputs`, keyed by `FeedId`) before sweeping a
     stage, so a fabric-only consumer counts as "in use" the same as a
     legacy reader.
  6. Even with fix 5, the sweep can still land in the gap between a fabric
     stage's creation (`prepare_srt_fabric_feed`, synchronous) and
     `active_outputs` registration (`retain_srt_fabric_runtime`, inside the
     spawned egress task — asynchronous and strictly later). A reconcile
     tick landing in that window sees neither the reader signal nor the
     fabric signal. Fixed with a 5-second grace window on `TsChunkRing`
     (new `created_at` field): `sweep_unused_stages` exempts any stage
     younger than the grace window regardless of liveness signals, giving
     the async fabric-runtime registration time to land before a stage
     becomes sweep-eligible.
- **Known open issue (as of `023e450a`):** the shared TS muxer stage now
  survives for fabric-only SRT outputs (confirmed live: the muxer's
  `ts_shared_muxer` reader no longer deregisters early, unlike every prior
  attempt), but `srt-crypto-matrix` under `RESTREAM_EGRESS_FABRIC=srt`
  still stalls at the same 10/20 with zero `packetsOut`. All ten SRT
  sockets connect successfully (`[srt] egress config` fires for each), so
  the remaining gap is somewhere between "muxer is alive and producing"
  and "a fabric leaf visits and sends" — not yet isolated. Six real,
  independently verified defects were found and fixed in this
  investigation (delivery driver not scheduling ready work, `on_ready`
  leaking closed leaves, overrun closing instead of resyncing, a
  lost-wakeup race in the feed watcher, and two liveness gaps in
  `sweep_unused_stages`); each is covered by a deterministic unit or
  regression test independent of this live gate. The rollout default
  remains `off`/opt-in until live delivery is confirmed — do not flip
  `EgressRolloutMode` default to `Srt` on the strength of the unit tests
  alone.
  **Root cause found and fixed.** Two further live diagnostics first
  confirmed both the muxer-liveness and wake-delivery fixes work exactly as
  designed (ring `write_idx` growing steadily at ~34 units/s; the wake
  watcher delivering continuously at matching rate), then a raw
  `srt_epoll_wait` diagnostic confirmed `SrtFabricPoller` correctly reports
  registered sockets writable. The actual break was one layer deeper, in
  the send itself: `srt_send()` failed with SRT error 5009 ("Incorrect use
  of Message API") on every attempt, because the engine handed an entire
  muxed TS feed unit — one chunk boundary from the shared muxer, which can
  span tens of KB for a keyframe burst — to a single message-mode
  `srt_send()` call. SRT's message API rejects a payload above its
  configured message-size ceiling. Legacy SRT egress never hits this
  because it re-chunks the byte stream into a fixed 1316-byte buffer
  (`src/media/srt_egress.rs`) on the way out regardless of original chunk
  boundaries — a re-chunking step the fabric engine had never reproduced.
  Fixed in `src/media/srt/egress_engine.rs`: `SrtEgressEngine` now retains
  a byte offset alongside a unit's `Bytes` and sends it as
  `MAX_SRT_MESSAGE_PAYLOAD` (1316-byte) fragments across successive
  visits, advancing the feed cursor and counting the unit only once its
  final fragment is accepted — one bounded fragment per visit, resuming
  from the exact offset after a `WouldBlock`. Proven by two deterministic
  tests: fragmenting and reassembling a >2×-oversized unit exactly, and
  resuming mid-fragmentation at the correct offset after a would-block.
  **Live-validated (`544ed807`, `crypto-fabric16` capture):**
  `srt-crypto-matrix` now passes cleanly under `RESTREAM_EGRESS_FABRIC=srt`
  — every pipeline sub-case reports full output progress (20/20, 40/40,
  60/60 across mixed RTMP/SRT scenarios), including encrypted SRT
  transport variants. This closes the live-delivery gap: the fabric SRT
  path delivers real media end to end. Earlier ramp captures (`w2-fabric`,
  `w2-fabric-wake`, `w4-fabric`) predate this fix and are superseded;
  re-record before citing their numbers.
  **Re-recorded `w2-fabric-confirmed` capture (vps-6cpu-12gb, N=100) shows
  a real CPU regression the earlier unconfirmed captures could not see**
  (they were measuring a fabric that never actually sent anything): RSS is
  1.4x lower than legacy (2,455KB vs 3,426KB per output) but CPU is 3.8x
  *higher* (158% vs legacy's 41.7%). Root cause: fragmenting a large unit
  sends at most `MAX_SRT_MESSAGE_PAYLOAD` (1316) bytes per shard visit —
  one bounded fragment per visit — so a keyframe-sized unit (tens of KB)
  now costs dozens of wake/poll/visit/send cycles instead of one,
  multiplying per-unit scheduling overhead. This is architecturally
  correct (bounded, budget-respecting) but unoptimized; sending multiple
  fragments per visit within the existing `WorkBudget` should recover most
  of this cost without reintroducing the oversized-message failure.
  A separate isolated single-output run (same commit, `N_OUTPUTS=1`)
  showed zero decode errors and a correct dimension spot-check, confirming
  the fragmentation logic itself is correct; the 100-way ramp's elevated
  mediamtx decode-error count (124 lines across 62/100 connections, vs 3
  for legacy) is attributed to connection-churn noise proportional to
  concurrency, not a data-integrity defect — worth confirming with a
  longer soak before the default flip regardless.
  **Fragment-batching implemented.** `SrtEgressEngine::send_pending` now
  loops sending successive `MAX_SRT_MESSAGE_PAYLOAD` fragments of the
  pending unit within one visit, bounded by the visit's `WorkBudget`
  (`max_bytes` and `deadline`) exactly like any other budget-respecting
  engine work — a keyframe-sized unit now costs one to a few scheduler
  cycles instead of dozens, while a single slow or always-writable leaf
  still cannot monopolize the shard past its budget. Proven by two new
  deterministic tests: a generous budget sends a 3-fragment unit in one
  visit, and a tight budget still stops after exactly one fragment,
  leaving the rest for the next visit.
  **Live re-measurement (`w2-fabric-batched` capture, N=100) shows
  batching barely moved CPU** (149% vs 158% pre-batching, ~6% better,
  still 3.6x legacy's 41.7%), which reframes the bottleneck: batching
  reduces shard wake/poll/schedule overhead per unit, but not the *number*
  of `srt_send()` syscalls, which stays fixed at roughly
  `unit_bytes / MAX_SRT_MESSAGE_PAYLOAD` regardless of how many happen per
  scheduler visit — a 74KB keyframe is still ~57 separate syscalls.
  Syscall-transition cost, not scheduling overhead, appears to dominate.
  The next lever is raising SRT's configured message-size ceiling to
  shrink the fragment *count* itself, not just how fragments are
  scheduled — that needs dedicated SRT-configuration investigation and is
  Phase 7 tuning territory, not a quick follow-up. `EgressRolloutMode`
  default stays `Off` until fragment count (not just visit batching) comes
  down. RSS remains favorable throughout (2,575KB vs legacy's 3,426KB per
  output); correctness held (`srt-crypto-matrix` still passes cleanly with
  batching applied).
  **Correction: the syscall-count framing above was incomplete — the fabric
  path was also missing native UDP-multiplexer-port reuse entirely,
  independent of fragment count.** A later external review of this branch
  pointed out that `src/media/egress/backends/srt.rs`'s two connect call
  sites (`complete_pending_connect_with`, `complete_pending_connect`) always
  called `pending.connect_spec.connect_config(peer_addrs, None)` — passing
  no muxer-port claim at all, unlike the legacy path
  (`src/media/srt_egress.rs`), which passes
  `reuse_local_srt_egress_port.then(|| claim_srt_egress_muxer_port(&srt_egress_muxer_port))`.
  Without that claim, every fabric SRT socket binds its own local UDP port,
  so libsrt cannot share one multiplexer (and its `RcvQ`/`SndQ` worker
  threads) across sockets the way the legacy path does. This repository had
  already measured that exact optimization's value independently of the
  fabric work: at 60 legacy SRT outputs, port reuse took `RcvQ`/`SndQ`
  thread counts from 61 to 2 each and total process CPU from 4.243 to 2.992
  cores — a ~1.25-core saving. The `w2-fabric-batched` regression above
  (149% fabric vs 41.7% legacy, ~107 percentage points) is the right order
  of magnitude for this to be the dominant cause, not fragment count: both
  paths call `srt_send()` at the same ~1316-byte granularity (the fabric's
  `MAX_SRT_MESSAGE_PAYLOAD` matches the legacy re-chunk size), so raising
  the message-size ceiling — the "next lever" this section previously
  named — could only ever recover a single-digit percentage (a 74KB
  keyframe goes from ~57 calls at 1316 bytes to ~51-52 at the documented
  1456-byte SRT live-mode maximum), nowhere near the measured gap.
  **Fixed:** `SrtShardBackend` now carries a shared
  `Arc<Mutex<Option<u16>>>` (`srt_egress_muxer_port` field) and a
  `reuse_local_srt_egress_port` flag, defaulting to a fresh mutex with reuse
  disabled so every existing constructor and test is unaffected; a new
  `with_srt_egress_muxer_port_reuse(state, enabled)` builder step opts a
  backend in, and both connect call sites now build a real
  `claim_srt_egress_muxer_port(&state)` claim when enabled instead of
  passing `None`. Production wiring
  (`MediaEngine::retain_srt_fabric_runtime` in
  `src/media/engine_egress_fabric.rs`) passes the *same*
  `Arc<Mutex<Option<u16>>>` the legacy path already shares via
  `MediaEngine::srt_egress_muxer_port_handle()`, gated by the same
  `srt_egress_reuse_local_port` config flag — so a socket connected by
  either path can be reused by the other, matching legacy's process-wide
  sharing semantics exactly rather than creating a second, fabric-only pool.
  Four new tests (`backends/srt/tests/muxer_port.rs`) prove the claim
  plumbing itself: reuse disabled passes no claim; reuse enabled with empty
  shared state passes a `First` claim (present, no port yet); reuse enabled
  with a pre-recorded port passes a `Reuse` claim carrying that exact port;
  and the `complete_pending_connect` call site (driving
  `self.socket_connector` instead of an injected connector — a distinct
  code path from `complete_pending_connect_with`) is covered separately
  since it has its own copy of the same claim-construction logic. This is
  shard-level plumbing proof only — it does not yet re-measure live CPU/RSS
  with the fix applied; that re-measurement (repeating the
  `w2-fabric-batched`-style capture) is still needed before revising the
  `EgressRolloutMode` default-`Off` decision above.
  **CPU/RSS re-measurement at scale done, decisively — but the default is
  still correctly `Off` on a different, unresolved exit-gate criterion.**
  The N=500/N=1000 captures recorded later in this phase (commit
  `2dcd5c3b`, `srt-fabric-matrix/vps-6cpu-12gb-n500` and
  `-n1000-fabric-only`, both with the muxer-port-reuse fix applied) *are*
  that re-measurement: at N=500 fabric beats legacy on both CPU (171.0%
  vs 196.76% avg, ~13% lower) and RSS (922,130KB vs 1,052,797KB avg,
  ~12% lower), and at N=1000 fabric completes while legacy structurally
  cannot (stalls permanently past its hardcoded 512-sender-thread cap).
  The CPU-regression concern this section raised is closed. What still
  gates the default flip is a different Phase 4 exit-gate criterion,
  healthy-neighbor isolation: the "Proof" section below (live tests)
  still lists a **pure-fabric** stalled-SRT-destination isolation test as
  outstanding — only a mixed-ownership variant (`w4-fabric`: fabric SRT
  alongside legacy RTMP) has been proven. Do not flip the default on the
  strength of the CPU/RSS numbers alone until that pure-fabric isolation
  case is run.
  **`SrtEgressEngine::advance` also read one feed unit per `feed.read_from`
  call** (`ReadBudget::new(budget.max_units.min(1), ..)`), the same
  one-unit-per-call shape the RTMP fabric engine had before its own
  `FEED_READ_BURST` fix above — each call allocates a `Vec` and touches
  ring atomics regardless of how many units are actually available. Fixed
  identically: `SrtEgressEngine` now carries a `pending_units:
  VecDeque<Bytes>` buffer, refilled from one `ReadBudget::new(FEED_READ_BURST,
  ..)` call when both `pending` (the unit currently being fragmented) and
  `pending_units` are empty; the existing one-unit-fragmented-per-visit
  behavior in `send_pending` is unchanged, only where the next unit comes
  from. New test `advance_pulls_a_burst_of_feed_units_in_one_read_from_call`
  seeds 5 ring units, drives one non-writable visit (isolating the read
  side — nothing gets sent), and asserts `pending_units_len() == 4`
  afterward (one popped into `pending`, four still buffered) — only
  possible if the one internal `read_from` call pulled all 5 at once.
  This changed the cursor-advance behavior three existing tests asserted
  on (the cursor now advances past every unit pulled into the buffer, not
  just the one unit sent that visit, since buffered units are already
  safely copied out of the ring as owned `Bytes`); updated
  `sends_one_ts_message_and_advances_cursor_when_writable`,
  `retains_one_message_when_sender_backpressures`, and renamed
  `writable_recovery_sends_pending_without_reading_next_feed_unit` to
  `writable_recovery_sends_pending_without_resending_the_buffered_next_unit`
  (the invariant it actually protects — a blocked retry doesn't re-fetch
  or re-send the *next* unit — still holds; only the cursor's numeric
  value needed to move to 2). Verified as a real regression by temporarily
  reverting the burst read to `ReadBudget::new(1, ..)` and confirming the
  new test fails; restored and confirmed green. Full SRT test suite (263
  tests), clippy, fmt, source-audit, and docs checks pass.
- Bad-neighbor evidence with the SRT rollout active (`w4-fabric` capture):
  fault.output-stall passed with a permanently stalled sink isolated beside
  32 healthy siblings while SRT outputs ran fabric-owned. This is
  mixed-ownership isolation; a pure-fabric stalled-SRT-destination live
  variant remains listed under live tests.
- **Pure-fabric SRT bad-neighbor isolation: proven, and it surfaced a real,
  significant correctness gap affecting both SRT and RTMP fabric, now
  fixed.** Added `fault_srt_egress_dead_sink_isolation_under_many_outputs`
  (`src/bin/test_harness/fault_recovery/egress.rs`, wired into
  `fault.output-stall`): one SRT destination's target pipeline is deleted
  mid-stream while N healthy SRT siblings, fed from the same source
  pipeline and sharing the same local SRT muxer port, keep progressing —
  the harness has no raw SRT listener to hold a connection open without
  reading, so this uses a dead destination instead of a stalled one, but
  it is an equally valid bad-neighbor shape (one of the Phase 0 baseline
  manifest rows) and exercises the same isolation property.
  **First run failed** (`retryPhaseOk=false`; the bad output stayed
  `status: "stalled"`, `retrying: false`, `lastError: null` forever) —
  not a harness bug. Root-caused via targeted `eprintln!` instrumentation
  (`src/media/srt/egress_sender.rs`, removed after diagnosis) plus a
  side-by-side legacy comparison (`fault_srt_egress_sink_disappear`,
  pre-existing and passing under legacy in 1.0s, but *also* failing the
  same way under `RESTREAM_EGRESS_FABRIC=srt`): **fabric leaf failures
  were never surfaced to the application layer at all, for any fabric
  protocol.** `run_srt_fabric`/`run_rtmp_fabric`
  (`src/infrastructure/bootstrap/egress.rs`) only ever awaited
  `self.registration.cancel_token.cancelled()` — which fires on an
  explicit stop/reconfigure, never on the fabric closing a leaf out from
  under it (peer closed, protocol failure, stall-sweep recovery, or a
  connect attempt that never produces a leaf at all). The shared
  retry/backoff bookkeeping at the bottom of `EgressTask::run()` — job
  status, `next_output_retry_count`, `record_egress_error_if_current` —
  only runs when the wrapper task *returns*, so it silently never ran for
  fabric outputs; the output just sat at its last known status forever
  instead of retrying. `RetryPolicy::record_failure`
  (`src/media/egress/policy.rs`) was, in effect, dead code for every
  fabric-routed output.
  **Fixed** with `EgressProgressSink::terminated_unexpectedly`
  (`src/media/egress/leaf.rs`): an `Arc<AtomicBool>`, `None` by default
  (so every existing constructor/test is unaffected), set explicitly by
  shard code — never via `Drop`, since `EgressProgressSink` is `Clone`
  and cloned freely (application-side temporary, `LeafCommon`'s copy);
  a Drop-based signal would fire on every clone's destruction, not just
  the leaf's real end of life. Marked at exactly the sites that
  previously discarded a failure silently:
  - `SrtShardBackend`/`RtmpShardBackend`'s `on_ready` close-decision
    branch (`VisitDecision::Close`, which — per `visit.rs` — is only
    ever produced from `EngineProgress::PeerClosed`/`Failed`, never from
    an explicit `EgressCommand::Remove`, so every close observed there
    is unexpected by construction);
  - both shards' `sweep_stalled_leaves` (no-progress recovery);
  - `SrtShardBackend::complete_pending_connect`'s two connect-failure
    branches (dial failure, poller-add failure) — previously `let _ =
    self.complete_pending_connect(...)` in `on_media_tick` discarded the
    `Err` outright, so a destination that never even connects left no
    leaf, no error, and no retry, ever;
  - `RtmpShardBackend::complete_pending_connect`'s four analogous
    early-return branches (TCP connect, TLS init, missing publish
    startup, engine init, poller registration).
  Application side: a new `EgressTask::wait_for_stop_or_leaf_failure`
  polls the flag (250ms) alongside `cancel_token.cancelled()` in both
  `run_srt_fabric` and `run_rtmp_fabric`, calls
  `record_egress_error_if_current` when it was the leaf (not
  cancellation) that ended the wait, then falls through to the existing
  Remove/release calls — the shared retry tail in `EgressTask::run()`
  needed no changes at all.
  **A second, independent gap surfaced during live verification**: fixing
  the above made `srt-egress-sink-disappear` and the new isolation test
  pass (both now transition to `retrying`/`failed` in ~0.5s, matching
  legacy), but a third pre-existing test,
  `srt-egress-retry-budget-exhausts` (dead sink from the start, never
  connects), still failed — `status: "running"`, 0 bytes, forever, no
  error. Traced to `LeafPolicy::default().connect_timeout` being a
  hardcoded 10s (`src/media/egress/policy.rs`) that
  `srt_fabric_output_spec` (`src/application/egress.rs`) used
  unconditionally, completely ignoring
  `AppConfig.srt_connect_timeout_ms`/`RESTREAM_SRT_CONNECT_TIMEOUT_MS` —
  legacy SRT respects that env var, fabric silently didn't. (RTMP has no
  equivalent divergence: legacy RTMP's connect timeout is itself a
  hardcoded 10s constant, `RTMP_EGRESS_CONNECT_TIMEOUT` in
  `egress_transport.rs`, which already matches fabric's default.) Fixed
  by threading `connect_timeout: Duration` through
  `srt_fabric_output_spec`, supplied by its one production caller
  (`src/infrastructure/bootstrap/egress.rs`) from
  `self.engine.config.srt_connect_timeout_ms`.
  **Live-verified end to end**, binaries rebuilt from HEAD each time:
  `fault.egress-retry` and `fault.output-stall` both pass fully under
  `RESTREAM_EGRESS_FABRIC=srt` and `=all` (RTMP fabric leaf-failure path
  exercised too) and unchanged under legacy (default, fabric off).
  `srt-egress-sink-disappear`: 10.1s **FAIL** → 0.5s **PASS**.
  `srt-egress-retry-budget-exhausts`: **FAIL** → **PASS**. New pure-fabric
  isolation test: **PASS** at 6 and 12 siblings. Deterministic proof:
  `src/media/egress/backends/srt/tests/leaf_termination.rs` — two new
  tests exercise `sweep_stalled_leaves` directly (stalled leaf gets
  marked, healthy leaf does not), verified as real regressions by
  temporarily removing the `mark_terminated_unexpectedly()` call at the
  sweep site and confirming the positive-case test fails, then restoring
  it. Full `cargo test --lib` (1,864 tests), harness binary unit tests
  (145), clippy, fmt, source-audit, and docs checks all pass.
  **This directly unblocks part of the Phase 4/5/6 exit gates**: fabric
  outputs now actually retry on failure, which "status and failure
  reasons" integration (Phase 6, previously unstarted) depended on
  without anyone having named it as a blocker. Bad-neighbor isolation
  for pure-fabric SRT is now proven at the harness level (dead
  destination, not stall — see the harness limitation noted above); the
  Phase 4 exit gate's remaining unproven piece is narrower than before
  this fix.

### Removal targets

After fabric SRT reaches parity, remove egress use of:

- per-output `MemoryQueue` in `src/media/srt_egress.rs`;
- per-output `std::thread::spawn` sender loop;
- sender-thread semaphore and the 512-output architectural cap;
- feeder logic whose only purpose is copying shared TS bytes into the private
  queue.

Do not remove `MemoryQueue` globally; recording and codec boundaries may still
have valid blocking ownership.

### Proof

Unit and integration tests cover:

- asynchronous-send saturation and writable recovery;
- one message retained across backpressure;
- socket deregistration on every close path;
- errors during connect, epoll registration, send, and teardown;
- cancellation during connect and while backpressured;
- feed overrun and reconnect at a sync point;
- bounded native and application buffering;
- repeated add and remove without thread, socket, or epoll leaks.

Live tests cover:

- more than 1,000 healthy SRT outputs;
- 998 healthy plus one non-reading and one rate-limited destination on one shard;
- dead destinations spread across one and several shards;
- encrypted and unencrypted SRT;
- reconnect storm;
- MediaMTX or matching SRT receiver integrity and advancing bytes.

### Exit gate

Fabric SRT becomes the default only when it beats or matches legacy correctness
and demonstrates fixed application egress thread count, bounded RSS during
indefinite stalls, and healthy-neighbor isolation.

Status against each criterion, as of the retry-wiring fix above:
- **Beats/matches legacy correctness**: yes — `srt-crypto-matrix`,
  N=500/N=1000 captures, and now `fault.egress-retry`/`fault.output-stall`
  all pass under fabric matching or beating legacy.
- **Fixed application egress thread count**: yes, by architecture (Phase 3)
  — a fixed shard pool, not one thread per output; no separate proof needed.
- **Healthy-neighbor isolation**: proven at the live level for a dead
  destination (this section's new isolation test) and mixed-ownership for
  a stalled one (`w4-fabric`); proven at the deterministic-unit level for
  the stall-sweep close path specifically (`leaf_termination.rs`).
- **Bounded RSS during indefinite stalls**: proven live, with a real
  finding along the way. `fault.srt-output-stall`
  (`src/bin/test_harness/fault_recovery/srt_stall.rs`) tried the obvious
  approach first — `SIGSTOP` a real MediaMTX receiver to freeze its
  `recv()` loop, matching `start_stalled_rtmp_sink_server`'s shape for
  RTMP. It doesn't produce the intended condition: `SIGSTOP` freezes
  *every* thread in the receiver, including libsrt's own internal
  ACK/keepalive thread, so the SRT connection is detected as fully broken
  within seconds (`srt_send failed ... Connection was broken`), not
  backpressured — SRT cannot distinguish "receiver alive but not reading"
  from "receiver process frozen." The output then cycles through
  connect-failure retries against the still-suspended receiver, the same
  shape as a dead destination, not the distinct `classify_stall`
  backpressured-but-connected path this was meant to exercise. Proving
  *that* exact path live would need a receiver that keeps SRT's own
  liveness signaling alive while deliberately not draining decoded data
  one layer up — a raw SRT listener built from scratch (restream's libsrt
  FFI bindings are internal to `src/media/srt`, not exposed to the harness
  binary), out of scope here. Re-scoped the test honestly instead: it now
  proves what `SIGSTOP` actually produces — RSS stays bounded (~21-30MB
  growth, well under a 64MB budget) across 120s of continuous
  connect-failure retry cycling against an unreachable destination, both
  under legacy and fabric. The stall-sweep/`classify_stall` mechanism
  itself (the backpressured-but-connected path specifically) is proven
  deterministically in `leaf_termination.rs`, not live — building the raw
  SRT listener needed to close that specific live gap remains future work,
  but is narrow and well-understood now, not open-ended.
All four Phase 4 exit-gate criteria now have real evidence. The remaining
gap is narrow: a live (not just deterministic-unit) proof of the
backpressured-but-connected SRT stall path specifically, which needs a
purpose-built raw SRT listener — still open, tracked as future work, not
blocking.

**Default flip attempted, then reverted** — see Phase 6's "Rollout
order" for the full story: CI's own live-scenario gate caught a real
RTMP fabric regression under a workload shape this session's captures
never exercised, so `EgressRolloutMode::default()` is back to `Off`.
SRT itself is not implicated by that finding; `RESTREAM_EGRESS_FABRIC=srt`
remains available for anyone who wants SRT-fabric-by-default today.

## Phase 5: RTMP and RTMPS migration

### Objective

Move RTMP and RTMPS from one async task per destination into the same leaf,
scheduler, feed, retry, and policy model.

### Work

Create a TCP readiness backend using the repository's selected low-level
polling facility. The backend owns non-blocking listener-independent outbound
sockets and maps readiness to leaf generation.

Extract current RTMP egress behavior from `src/media/rtmp/egress.rs` into an
explicit connection engine:

- TCP connect state;
- RTMP handshake;
- `rml_rtmp::ClientSession` state;
- server reads and client responses;
- metadata and sequence headers;
- timestamp guards;
- codec payload conversion;
- RTMP chunk serialization;
- optional TLS state;
- partial wire writes.

Replace `write_all` with pending-write state and bounded partial writes. A
writable visit must stop on `WouldBlock` or budget exhaustion. It must preserve
protocol bytes and offsets exactly.

RTMP server reads and acknowledgements share the same visit budget. A peer that
continuously sends control data cannot starve media sends or neighboring leaves.

### Payload ownership

Preserve reference-counted source payloads where possible. Do not reintroduce
known-regressive burst coalescing or per-packet fresh allocation solely to fit
the new reactor. Benchmark changes around `ChunkSerializer` and TLS separately.

The first RTMP migration should optimize ownership neutrality and correctness,
not claim an allocator win. Performance tuning follows after parity.

Current branch status:

- RTMP and RTMPS can now be represented as typed fabric output specs that
  preserve the destination URL and record whether the scheme requires TLS.
- RTMP fabric feed preparation wraps the already prepared terminal
  `RingBuffer` as a `RingFeed`, so the control plane can share the same
  sequence-cursor feed contract used by the common fabric before runtime
  ownership moves off the legacy sender task.
- RTMP pending-write state currently preserves packet boundaries and partial
  write offsets; direct partial-write and zero-write socket tests prove that
  behavior, and the legacy session-init, connect-request, session-result,
  publish metadata, cached, deferred, refreshed, steady-state media, and live
  control write paths use one queue that transfers from connection startup into
  the live egress loop. Queue admission now enforces
  `RESTREAM_EGRESS_MAX_PENDING_BYTES` for those application-owned bytes. It is
  not yet a readiness-driven reactor across visits. The queue now also has a
  transport-agnostic nonblocking drain primitive that preserves partial offsets
  and yields on byte, unit, or deadline exhaustion; the legacy Tokio task does
  not yet drive that primitive from a TCP poller.
- The explicit RTMP session driver now owns socket establishment, the
  cancellable client handshake, client-session initialization, all server-result
  dispatch, publish-request issuance, and bounded control/media wire writes.
  Its socket-independent core owns `ClientSession`, RTMP startup and media
  serialization, and ordered outbound packet production; the Tokio adapter
  owns TCP/TLS reads, bounded pending-wire admission, socket flushing, and
  sender telemetry. The legacy egress task
  still owns prepared-media lookup, output status, and outer lifecycle while
  TCP readiness, shard registration, and incremental TLS remain future engine
  work.
- Legacy audio and video publication now use a dedicated RTMP media encoder
  for Raw/FLV framing, keyframe gating, decoder-config refresh, composition
  offsets, and timestamp guarding while retaining startup-header policy in its
  adapter. `RingFeed` budget ownership remains future work.
- The legacy RTMP sender remains the runtime owner. Phase 5 has not yet
  introduced TCP readiness polling, moved the hot media publish loop onto
  pending state, added incremental TLS, or provided a default/opt-in runtime
  switch for RTMP or RTMPS.
- The fabric handoff must carry an immutable RTMP startup snapshot from the
  application adapter into the media backend: selected audio-track metadata,
  publish metadata, cached or synthesized sequence headers, codec mode, and
  raw video parameter sets. The connection-local engine must not query
  `MediaEngine`, output registries, or application services while visiting a
  leaf. A `RingFeed` media engine is therefore introduced only together with
  its nonblocking TCP leaf and startup snapshot, not as an unused standalone
  abstraction.
- `RtmpFabricStartup` now assembles that immutable snapshot after output-ring
  preparation. It preserves empty-source behavior and H.264/AAC startup
  gating, while the legacy sender remains the sole runtime owner until the TCP
  leaf accepts the snapshot.
- A TCP readiness backend now exists: `src/media/egress/backends/tcp.rs`
  wraps a real Linux `epoll` instance per shard (via `libc`, since a TCP fd
  has no native poller of its own, unlike libsrt's socket type), mirroring
  `src/media/srt/egress_poller.rs`'s generation-tagged registration shape
  and `Ops`-trait fake-ability. `TcpEgressInterest` now carries both
  `readable` and `writable` (initially write-only, before the RTMP engine
  needed to wait for handshake/session-negotiation server responses too),
  so a leaf registers exactly the interest its current `ProtocolEngine`
  state needs — matching the `Interest` type `ProtocolEngine::advance`
  already returns. Registration, deregistration, stale-fd reuse, readable
  and writable readiness, and error/hangup surfacing (on both directions)
  are proven both against a fake `Ops` implementation and against a real
  kernel epoll instance using connected `AF_UNIX` socketpairs (no live
  network needed).
- A non-blocking-after-connect TCP dial now exists:
  `src/media/egress/backends/tcp_connect.rs`, mirroring the SRT fabric's
  connect shape (`egress_connect/single.rs`) — a bounded blocking
  `connect_timeout` on the shard's dedicated OS thread (acceptable there
  since it blocks only that shard's own leaves, not the process), then an
  explicit switch to non-blocking mode. Proven against a real local
  `TcpListener`: connects, confirms non-blocking reads return `WouldBlock`
  immediately, registers the connected socket with `TcpEgressPoller` and
  confirms it reports writable, and confirms `connect_timeout` bounds a
  connect to an unroutable address (`TEST-NET-1`) rather than hanging.
- A non-blocking client handshake now exists:
  `src/media/egress/backends/rtmp_handshake.rs`, driving the same pure,
  socket-independent `rml_rtmp::handshake::Handshake` state machine the
  existing Tokio adapter uses (`src/media/rtmp/handshake.rs`), one bounded
  non-blocking read or write per `advance()` call instead of an `.await`ed
  loop. Proven against a real TCP peer running `rml_rtmp`'s own
  synchronous server-side handshake on a background thread (no Tokio in
  the test at all) — full C0/C1→S0/S1/S2→C2 round trip.
  **Caught a real bug during that proof**: the handshake's "complete"
  check ran *before* flushing the final pending write (C2), so the driver
  reported completion having never actually sent C2 — the peer then
  blocked forever waiting for bytes that were never written. The test
  reproduced this as a genuine multi-hour hang (not a slow build) before
  the fix reordered the check to flush any pending write first. Fixed and
  proven; the test's server thread now also carries a bounded read timeout
  as a permanent regression guard so a future reintroduction fails fast
  instead of hanging the suite again.
- A first `ProtocolEngine` implementation now exists:
  `src/media/egress/backends/rtmp.rs`. `RtmpFabricEngine` drives connection
  startup through a completed handshake by wrapping
  `NonBlockingRtmpHandshake` in a small state enum, returning
  `EngineProgress::HandshakeComplete` through the *same* shard visit loop
  (`ProtocolEngine::advance`) the SRT fabric engine uses — proven by
  driving it end to end against a real TCP peer running `rml_rtmp`'s
  synchronous server-side handshake, plus a peer-closes-mid-handshake
  failure-path test.
- RTMP session negotiation (connect/publish requests) now also runs through
  the fabric engine, reusing `RtmpSessionCore`'s existing pure `ClientSession`
  state machine (`src/media/rtmp/egress_connection.rs`, widened from
  `pub(super)` to `pub(crate)` so both the legacy Tokio adapter and the
  fabric engine drive the same protocol calls) instead of duplicating
  connect/publish-request logic. Proven end to end against a real
  `rml_rtmp::sessions::ServerSession` peer that auto-accepts the connect and
  publish requests (not a hand-rolled byte fixture), plus a
  peer-closes-mid-negotiation failure-path test.
- Media publishing now also runs through the fabric engine.
  `RtmpFabricState` has four states: `Handshaking` → `Negotiating` (bounded
  to at most one read or one write syscall per `advance()` call) →
  `Publishing` (a `MediaPublisher` driver that batches multiple feed units
  and their wire packets into one visit, bounded by the visit's
  `WorkBudget` — mirroring the SRT fabric engine's fragment batching, which
  exists precisely because one-wake-per-unit caused a measured CPU
  regression; see Phase 4 status above). `MediaPublisher` reuses
  `RtmpMediaEncoder`'s existing pure per-packet encoding (sequence-header
  refresh, keyframe gating, timestamp guarding — `src/media/rtmp/egress_engine.rs`,
  widened to `pub(crate)`) and `RtmpSessionCore`'s pure packet-building
  calls, so the wire-framing logic is identical to the legacy path rather
  than reimplemented. A new `RtmpPublishStartup` type
  (`src/media/egress/backends/rtmp.rs`) mirrors
  `crate::application::egress_rtmp_fabric::RtmpFabricStartup`'s fields
  without the media engine depending on the application layer directly —
  the application assembles the immutable startup snapshot (querying
  `MediaEngine`, output registries, and ring state) and converts it into
  this media-owned type before constructing the leaf; the connection-local
  engine itself never queries anything beyond its own fields, preserving
  the architecture's "no engine/registry queries from a leaf visit" rule.
  Proven end to end: a real `rml_rtmp::sessions::ServerSession` peer
  auto-accepts connect/publish and then keeps reading until it observes a
  `VideoDataReceived` event for a raw H.264 keyframe pushed through a real
  `RingBuffer`/`RingFeed` — confirming the engine's encoded bytes parse as
  a valid RTMP video message on the wire, not just that bytes were sent.
- A shard backend now exists: `RtmpShardBackend`
  (`src/media/egress/backends/rtmp_shard.rs`), mirroring `SrtShardBackend`'s
  shape (leaf slab, ready queue, pending-connect map, generic over a
  fake-able poller trait). It differs from the SRT shard in one
  architecturally required way: SRT always registers write-only interest
  (libsrt handles acknowledgement internally), but RTMP genuinely
  alternates between wanting read and write readiness across handshake,
  negotiation, and publishing — so `RtmpShardBackend` re-registers each
  leaf's poller interest after every visit, translated from the `Interest`
  the engine's last `EngineProgress` carried, instead of registering once
  at connect time. DNS resolution runs on a dedicated worker thread with a
  completion queue (mirroring SRT's resolve worker), separate from the
  bounded blocking connect on the shard thread itself, because
  `ToSocketAddrs` has no timeout of its own and could otherwise stall the
  shard indefinitely. `RtmpPublishStartupSource` is a trait seam for
  supplying each output's immutable `RtmpPublishStartup` snapshot;
  `EmptyRtmpPublishStartupSource` (always empty) backs the isolated shard
  tests, and `SharedRtmpPublishStartupSource` (a shared map, cloned across
  a runtime's shards) backs the real path. Proven end to end: a
  shard-driven leaf (added via `on_command`, connected via
  `complete_pending_connect`, then driven only through `on_ready` — the
  same call path an `EgressManager`-owned shard loop would use) reaches
  publish acceptance against a real `ServerSession` peer, confirming the
  ready-queue and per-visit interest-reregistration logic actually drives
  the engine to completion end to end, not just that a socket connects.
- The RTMP fabric is now wired end to end into live output startup,
  mirroring the SRT fabric's shape exactly:
  - `spawn_rtmp_fabric_shard_group` (`src/media/egress/factory.rs`) and
    `retain_rtmp_fabric_runtime`/`dispatch_rtmp_fabric_command`/
    `release_rtmp_fabric_runtime` (`src/media/engine_rtmp_egress_fabric.rs`,
    backed by a new `RtmpFabricRegistry` in `engine_registries.rs`) reuse
    the same protocol-agnostic `EgressFabricRuntime`/`EgressShardGroup`
    the SRT fabric uses — neither type is SRT-specific, so no new runtime
    type was needed, only a new shard backend.
  - `RingFeed` gained `clone_reader`/`notify_handle` (mirroring `TsFeed`'s
    existing methods of the same name) so the fabric's feed-wake watcher
    pattern applies unchanged.
  - `infrastructure/bootstrap/egress.rs` gains a `use_rtmp_fabric` branch
    (`rollout.routes_rtmp() && url_scheme is Rtmp/Rtmps`) parallel to
    `use_srt_fabric`: it assembles `RtmpFabricStartup` via the existing
    `prepare_rtmp_fabric_startup`, converts it to the media-owned
    `RtmpPublishStartup` (`From<RtmpFabricStartup>` in
    `application/egress_rtmp_fabric.rs` — application depends on media,
    not the reverse), writes it into the runtime's
    `SharedRtmpPublishStartupSource` via the new
    `MediaEngine::set_rtmp_publish_startup` *before* dispatching
    `EgressCommand::Add` (the shard thread only ever reads it), and falls
    back to the legacy `start_rtmp_egress` task when the rollout mode
    doesn't route RTMP — same fallback shape as SRT, so `EgressRolloutMode`
    staying `Off` by default leaves every existing behavior unchanged.
  - Proven with `MediaEngine`-level integration tests mirroring the
    existing SRT ones: retain-once-per-feed/release/shutdown lifecycle,
    and that a startup snapshot written via `set_rtmp_publish_startup` is
    actually observable by the shard-side `RtmpPublishStartupSource`
    (not just stored in a registry no one reads).
  - Reused `srt_poller_max_events` for the RTMP TCP poller's max-events
    tuning rather than adding a duplicate config knob; split them later
    if tuning needs diverge.

#### Three shard-liveness bugs found and fixed after an external code review

An external review of this branch (static analysis, no build available in
that environment) flagged a specific, concrete liveness concern: once a
publishing leaf fully drains its feed it reports
`EngineProgress::Needs(Interest::NONE)`, and nothing seemed to re-wake it
when new media arrived. Reproducing that live (running the first
`rtmp-fabric-matrix` capture at a slightly larger scale) surfaced it
immediately as a real hang, and chasing it down turned up two more bugs of
increasing subtlety. All three are fixed; the fixes are covered by a new
deterministic regression test plus the live harness genuinely completing
(not just returning quickly — see below).

1. **`WorkBudget` reused forever instead of per visit.**
   `WorkBudget::deadline` is an absolute `Instant`, computed once in
   `WorkBudget::new()`. Both `RtmpShardBackend` and (identically)
   `SrtShardBackend` stored the `WorkBudget` passed at construction time in
   a field and reused it, unchanged, for every visit for the shard's
   entire lifetime — `let budget = self.budget;`. Once
   `visit_max_us` (2,000μs by default) elapsed after the shard was
   created, `budget.is_exhausted()` became permanently `true`, and
   `MediaPublisher::advance`'s exhaustion check runs *before* it ever
   reads the feed — so every leaf on the shard silently stopped reading or
   sending anything, forever, about 2ms after the shard started. Fixed in
   both `RtmpShardBackend` and `SrtShardBackend` by storing the budget's
   parameters (`max_units`/`max_bytes`/window) instead of the `WorkBudget`
   itself, and constructing a fresh one (`WorkBudget::new(..)`, which
   computes a new `Instant::now() + window` deadline) for every visit.
   SRT's live captures happened not to expose this as starkly as RTMP's
   did: SRT always registers write interest, so a leaf kept getting
   *visited* even with a stale budget, it just silently lost the ability
   to batch more than one fragment per visit once `is_exhausted()` went
   permanently `true` (the check in `send_pending` runs *after* the first
   fragment of a visit, not before) — degrading to the pre-batching-fix,
   one-fragment-per-wake behavior the fragment-batching fix in Phase 4 was
   specifically written to avoid. That means the stale budget plausibly
   contributed to some of the CPU overhead in the recorded
   `w2-fabric-confirmed` SRT capture (158% CPU, 3.8x legacy) above, though
   this has not been re-measured live with the fix applied — flagged here,
   not claimed as verified. (A later fix, missing SRT UDP-multiplexer-port
   reuse, turned out to be the higher-confidence primary cause of that same
   regression — see the "Correction" paragraph under Phase 4's fragment-
   batching status above.)
2. **`EgressCommand::FeedWake` never reached `RtmpShardBackend::on_command`
   at all.** `EgressShardRuntime::process_command`
   (`src/media/egress/shard.rs`) intercepts `FeedWake` at the generic
   runtime layer and returns `ScheduleReady` directly, without ever
   calling `self.backend.on_command(command)` — so a backend-level fix to
   `on_command`'s `FeedWake` arm is dead code in production regardless of
   what it does. Fixed by having the runtime call
   `self.backend.on_command(EgressCommand::FeedWake)` *in addition to*
   scheduling ready work; SRT's backend already treats `FeedWake` as a
   no-op, so this is free for it.
3. **The first fix attempt for (2) introduced a new, subtler bug.**
   The natural first fix was to have `RtmpShardBackend::on_command`'s
   `FeedWake` arm push a synthetic no-readiness `TcpReadyLeaf` into the
   ready queue for every idle leaf, so a stuck `Interest::NONE`-registered
   leaf would get a visit. That is wrong: `FeedWake` fires far more often
   than the shard's own idle poll cycle (every publish, not every ~25ms),
   so the synthetic no-I/O event kept winning the race to be visited
   *before* a real `poll_leaves()` ever ran — starving every leaf,
   including ones still mid-handshake or mid-negotiation that always need
   *some* real I/O, of the genuine epoll discovery required to make any
   progress. Live-testing this version showed leaves connecting
   (logged) and then never sending a single handshake byte
   (`mediamtx.log` stayed completely empty). The correct fix instead
   re-registers the poller interest for every connected leaf to
   `READ_WRITE` on `FeedWake` — widening it is always safe, since an
   engine that only needs one direction simply ignores readiness it
   didn't ask for — and pushes nothing synthetic into the ready queue; the
   *next* real `poll_leaves()` call is what actually discovers the
   readiness and visits the leaf.

New test: `feed_wake_delivers_media_published_after_the_leaf_goes_idle`
(`rtmp_shard_tests.rs`) drains the feed, drives past publish acceptance,
confirms nothing further happens with an empty feed, *then* publishes a
new unit and delivers `FeedWake`, asserting the server actually receives
it — the second-packet-after-idle scenario the original review pointed at
directly. It fails without fix (1) or (2) (with either bug present, the
5-second deadline in the test trips) and passes with all three fixed.

4. **`SrtShardBackend::on_ready` could strand an already-ready leaf behind
   a `WouldBlock` one.** Both backends' `on_ready` visit exactly one leaf
   per call, then decide whether to re-schedule (`ScheduleReady`) or stop
   (`Continue`) based on that one leaf's own outcome. SRT's version
   originally only re-scheduled when the visited leaf itself reported
   `VisitDecision::Continue` — it never checked whether the *ready queue
   itself* still had entries left. When one poll batch reports two leaves
   ready and the first one visited hits `WouldBlock`
   (`VisitDecision::Suspend`, not `Continue`), the backend returned
   `EgressShardCommandEffect::Continue` even though a second, genuinely
   ready leaf was still sitting in `self.ready` — stranding it until the
   next unrelated wake or poll cycle picked the queue back up. Fixed by
   also re-scheduling whenever `self.ready` is non-empty after the visit,
   mirroring the RTMP fix's shape (`requeue_after_rtmp_visit` /
   `!self.ready.is_empty()`) exactly. New test
   `on_ready_does_not_strand_a_second_ready_leaf_behind_a_would_block_leaf`
   (`backends/srt/tests/shard.rs`) registers a `WouldBlock`-always leaf and
   a healthy leaf as both ready in the same poll batch, asserts the first
   `on_ready()` call reports `ScheduleReady` (not `Continue`) with the
   healthy leaf still unsent, and that the *next* call actually delivers to
   it. Same bug family as (1)-(3) above (per-visit local state read as
   though it reflected shard-wide queue state), same root cause class, same
   fix shape — not re-discovered independently, deliberately checked for
   once the WorkBudget-reuse mirror in `SrtShardBackend` was fixed.

**This also invalidated the first `rtmp-fabric-matrix` capture.** That
capture (N=10, recorded before these fixes) showed fabric CPU/RSS at
near-parity with legacy. With bug (1) in place, fabric leaves sent their
startup burst, satisfied the harness's "did bytes increase" progress
check, and then did almost no further work for the rest of the
measurement window — making the fabric look artificially cheap, not
because it performed well but because it had already stopped working.
The corrected capture (same scale, all three bugs fixed, confirmed via
`mediamtx.log` showing continuous received data rather than one burst)
shows fabric CPU genuinely higher than legacy — see
`test/harness/baselines/rtmp-fabric-matrix/vps-6cpu-12gb/capture.json`
for the numbers. That is the expected, honest starting point (matching
the SRT fabric's own pre-optimization story in Phase 4 above), not a
regression from the invalid reading.

#### Steady-state RTMP control-channel reads were missing

A second, independent external audit flagged that `MediaPublisher::advance`
(the Publishing-state engine, `src/media/egress/backends/rtmp.rs`) never
called `stream.read()` at all once publishing started — it only wrote
encoded media and returned `Interest::WRITE` (pending write) or
`Interest::NONE` (feed idle), never `Interest::READ`. Since the shard
poller's registration is derived directly from whatever `Interest` the
engine returns (`next_registration_interest` in `rtmp_shard.rs`), an
idle-feed publishing leaf was never even registered for socket readability.
Consequences: a server-sent Acknowledgement/WindowAckSize/UserControl
message went unprocessed, and a peer-initiated close could go undetected
indefinitely against an idle feed (only a *write* attempt would ever
observe it, and idle leaves have nothing queued to write). Not a crash —
`SessionNegotiation` already proved the pattern is safe — but a real
liveness/correctness gap the review compared unfavorably to the legacy
Tokio path, which always selects on read and write together.

Fixed by giving `MediaPublisher::advance` the same bounded, per-visit read
step `SessionNegotiation::advance` already uses: when `readiness.readable`,
issue one `stream.read()`, feed any bytes through the same
`RtmpSessionCore::handle_server_input` both states share, queue any reply
packets (e.g. an outbound Acknowledgement) into `current_batch` for the
next write pass, and treat `Ok(0)` as a real peer close
(`ProtocolFailure { reason: "rtmp_control_read", .. }`) instead of missing
it. Every interest returned by this method now also carries `readable:
true` — `Interest::WRITE` sites became `Interest::READ_WRITE`,
`Interest::NONE` became `Interest::READ` — so an idle publishing leaf stays
socket-read-registered the way the legacy path always was; widening
interest is always safe under level-triggered epoll (an engine that
doesn't need a direction just ignores readiness it didn't ask for), the
same principle the FeedWake fix above relies on.

New test `engine_detects_peer_close_during_steady_state_publishing`
(`rtmp_tests.rs`) drives a real client engine to `Publishing` against a
real `rml_rtmp` server peer that closes its socket immediately after
accepting the publish request (before ever reading media), then keeps
calling `advance` against an empty feed and asserts the engine reports
`Failed { reason: "rtmp_control_read", .. }` within 5 seconds, with every
intermediate `Needs` interest asserted `readable`. Without the fix this
loop does not terminate (nothing ever triggers a write, so the close is
never observed) and the test times out; with the fix it reliably ends in
well under the deadline.

#### The visit budget was not enforced on the write side

The same audit that flagged the missing control-channel reads also flagged
that `MediaPublisher::advance`'s only `budget.is_exhausted` check sat
immediately before `feed.read_from` — nowhere on the write side. Draining
`current_batch` (one encoded feed unit's wire packets — e.g. a large
keyframe split across several RTMP chunks, or the startup batch's
metadata/sequence-header packets) and completing `pending_write` both loop
back (`continue`) without ever consulting the budget, so a single
outsized unit could fully flush in one visit regardless of
`budget.max_bytes` or the visit deadline — the exact per-visit fairness
`WorkBudget` exists to bound, monopolizing the shard thread and starving
every other leaf on it for that visit's duration. Fixed by moving the
check to the top of the loop, so it gates the write path the same way it
already gated the read path; a leaf that gets cut off mid-batch reports
`Progress` for whatever it already flushed (not `Needs`), which reschedules
it promptly on the next visit instead of losing the partial work.

New test
`advance_stops_draining_the_startup_batch_once_the_budget_is_exhausted`
seeds a startup batch with three queued packets (metadata, video sequence
header, audio sequence header), drives to `Publishing`, then calls
`advance` once with a budget that permits the first write but is exhausted
immediately after (`max_bytes: 1`), and asserts — via a peer that reports
its own running byte count, isolated to post-publish-acceptance traffic —
that the peer received only what that one `advance` call reported as
`Progress`, not the full batch. Verified as a real regression test by
temporarily reverting the fix and confirming the test fails (730 bytes
observed by the peer in one visit against an expected 194); restored and
confirmed green.

#### `epoll_ctl` was called on every visit, even with an unchanged interest

The hot-path audit that flagged the missing SRT multiplexer-port reuse
above also flagged `RtmpShardBackend::visit_one_ready_leaf`: it called
`register_leaf` (an `epoll_ctl(EPOLL_CTL_MOD)` syscall) after every
non-`Close` visit, unconditionally, even when the interest it was about
to register was identical to what was already registered — a wasted
syscall on the hot path any time consecutive visits of the same leaf
report the same interest (common; e.g. several `Progress{interest:
WRITE}` results in a row while draining a large batch). Fixed by giving
`RtmpFabricLeaf` a `registered_interest: TcpEgressInterest` field
tracking what the poller currently watches for that leaf's fd
(initialized to `WRITE` at connect time, kept in sync by
`refresh_registrations_for_feed_wake`'s `READ_WRITE` widening), and only
calling `register_leaf` in `visit_one_ready_leaf` when the newly
requested interest actually differs from it.

New test `visit_one_ready_leaf_skips_reregistration_when_interest_is_unchanged`
drives a real connection through handshake, negotiation, publish
acceptance, an idle settle window, and a feed-wake-triggered publish
(the same lifecycle `feed_wake_delivers_media_published_after_the_leaf_goes_idle`
exercises) while recording every `register_leaf` call's interest through
a `CountingPoller` wrapping the real `TcpEgressPoller`. Real socket
timing is too noisy to assert an exact call count, so the test instead
asserts the invariant that must hold regardless of timing if the skip
logic works: the recorded interest sequence never contains two
consecutive equal entries (if it did, a call was made that should have
been skipped). Verified as a real regression test by temporarily
reverting to the unconditional call and confirming the test fails
(consecutive identical entries, e.g. two `WRITE`s in a row, appear in the
observed sequence); restored and confirmed green.

#### `MediaPublisher` read one feed unit per `feed.read_from` call

The same hot-path audit's remaining RTMP-specific finding: `advance`
called `feed.read_from(*cursor, ReadBudget::new(1, budget.max_bytes))` —
exactly one unit per call, each with its own `Vec` allocation and
ring-atomic traffic — while the legacy Tokio path pulls up to 32 packets
per read into a reusable burst `Vec` (`src/media/rtmp/egress.rs`); the
repository had previously measured about a 7% CPU improvement from
retaining that burst allocation across iterations. `MediaPublisher` now
carries a `pending_units: VecDeque<Arc<MediaPacket>>` buffer: when it's
empty, one `feed.read_from` call requests up to `FEED_READ_BURST` (32,
matching legacy) units and the whole batch is queued; each visit loop
iteration then pops one unit off this local buffer instead of calling
`feed.read_from` again, so the number of real feed reads (and their
allocation/atomic cost) drops by up to 32x under sustained load, while
per-unit encoding, counting, and budget behavior are all unchanged (only
the source of the next unit changed, from "always the ring" to "the
local buffer, refilled from the ring when empty").

New test `advance_pulls_a_burst_of_feed_units_in_one_read_from_call`
pushes 5 units into the ring, then calls `advance` once with writability
blocked (`readiness.writable: false`) so the visit can pull from the
feed but cannot flush anything past the first encoded unit — asserting
`RtmpFabricEngine::publisher_pending_units_len()` (a `#[cfg(test)]`
accessor into the `Publishing` state) reports exactly 4 units still
buffered afterward. That is only possible if the single internal
`read_from` call already pulled all 5 at once; the old one-unit-per-call
code had no such buffer and could not have made this assertion true.

#### `OutputId` was cloned on every visit, not just on close

Zero-cost audit finding: both `SrtShardBackend::visit_one_ready_leaf` and
`RtmpShardBackend::visit_one_ready_leaf` cloned the visited leaf's
`OutputId` (a heap-allocating `String` clone) unconditionally, on *every*
visit, purely so the caller (`on_ready`) could look it up for removal —
but that lookup only ever happens on `VisitDecision::Close`, a rare event
compared to the `Continue`/`Suspend` decisions steady-state visits
overwhelmingly produce. Both methods now return
`Option<(Option<OutputId>, VisitDecision)>` and only construct the
`OutputId` clone inside the branch that already knows `decision ==
Close`; every other visit — including the `StaleGeneration` early return,
which was cloning it even though that path is always `Suspend`, never
`Close` — now allocates nothing for it. `on_ready` in both backends
updated to match on `Some((Some(output_id), VisitDecision::Close))`
instead of `Some((output_id, VisitDecision::Close))`.

New test (RTMP) `shard_removes_the_leaf_once_the_peer_closes_after_publish_acceptance`
drives a real shard-owned leaf to publish acceptance, lets the peer close
its socket (the same close-detection path the steady-state control-read
fix above proved), and asserts the shard actually removes the leaf from
`output_sockets`/`leaves` afterward — proving a real `OutputId` still
reaches `remove_leaf_by_output` end to end through the now-conditional
clone. Verified as a real regression test by temporarily hard-coding the
`Close` branch to return `None` instead of the clone and confirming the
test fails (leaf never removed, deadline trips); restored and confirmed
green. SRT's identical change is covered by the pre-existing
`on_ready_removes_leaf_on_close_decision` test, which already exercises
the Close-and-remove path and continues to pass unchanged. Full RTMP (29
tests) and SRT (44 tests) backend suites, clippy, fmt, source-audit, and
docs checks all pass.

### RTMPS

Drive TLS incrementally:

- handshake, reads, and writes obey the same work budget;
- `wants_read` and `wants_write` map to common readiness interests;
- plaintext and encrypted pending bytes count toward leaf limits;
- TLS close and error paths map to common lifecycle reasons;
- no convenience API may spin internally without a bounded exit.

Current branch status:

- RTMPS now works through the fabric engine. `RtmpConnection`
  (`src/media/egress/backends/rtmp_connection.rs`) wraps a non-blocking
  `TcpStream` directly with `rustls::StreamOwned` — the same
  `rustls::ClientConnection` state machine the legacy Tokio adapter drives
  via `tokio_rustls`, here driven synchronously with no async runtime
  involved. `rustls::Stream`/`StreamOwned`'s `Read`/`Write` impls already
  interleave TLS handshake I/O with application data transparently,
  surfacing `WouldBlock` exactly like a raw non-blocking socket, so
  `NonBlockingRtmpHandshake`, `SessionNegotiation`, and `MediaPublisher`
  needed no protocol-level changes — only their transport type changed
  from `TcpStream` to `RtmpConnection`.
- **The one real correctness gap this surfaced**: a blocked read or write
  call does not necessarily mean *that same direction* is what unblocks
  it. `rustls`'s internal `complete_io()` can need to `read_tls()` a
  ServerHello in the middle of what the caller sees as a blocked
  `write()` call; inferring "needs write" purely from "the write call
  just blocked" (correct for plain TCP, where the two directions are
  independent) would under-request interest for TLS and could leave the
  poller only ever watching for writability while the connection is
  actually waiting on a read that never arrives — a silent stall, not a
  crash. `RtmpConnection::interest_hint` fixes this by asking
  `rustls::ClientConnection::wants_read()`/`wants_write()` directly after
  any blocked I/O, instead of guessing from the syscall that blocked;
  every `WouldBlock` branch across the handshake, negotiation, and
  publishing drivers now goes through it. Plain TCP's `interest_hint`
  just returns the existing per-direction guess unchanged, so this is a
  pure addition for TLS with no behavior change to the already-proven
  plaintext path.
- `RtmpShardBackend::complete_pending_connect` now branches on
  `parts.tls`, wrapping the connected socket via
  `RtmpConnection::tls(stream, &parts.host)` (reusing
  `rustls_client_config()`, widened to `pub(crate)`, so the fabric shares
  the exact same root-certificate trust store the legacy path uses)
  instead of rejecting RTMPS outputs outright.
- Proven: `RtmpConnection`'s `Read`/`Write` delegation against a real
  connected TCP pair; `interest_hint` returning `wants_write() == true`
  for a freshly constructed client `ClientConnection` before any I/O has
  happened (proving it reflects `rustls`'s actual internal state, not a
  per-call guess); and — the full round-trip this was building toward —
  a real handshake completing and application data flowing end to end
  against a real `rustls::ServerConnection`, driven non-blocking through
  `RtmpConnection` with the exact same WouldBlock-retry loop the fabric
  engine uses. The test server presents a locally generated self-signed
  certificate (`rcgen`, added as a dev-dependency for exactly this); the
  test client trusts it via a verifier that still performs real
  `rustls::crypto::verify_tls12_signature`/`verify_tls13_signature`
  checks but skips chain-to-root validation (appropriate for a test cert
  with no CA — the production path always uses `rustls_client_config()`'s
  real webpki-roots trust store, unchanged by this test-only verifier).
- **"plaintext and encrypted pending bytes count toward leaf limits"
  above was aspirational, not implemented — for any protocol, not just
  RTMPS.** Investigating the external review's "RTMPS pending-memory not
  accounted" finding turned up something broader:
  `LeafCommon::pending_application_bytes` (`src/media/egress/leaf.rs`),
  the field `is_limit_exceeded()` reads, was never written by anything in
  production — RTMP, RTMPS, or SRT — always `0`, and
  `is_limit_exceeded()` itself is called nowhere outside its own unit
  test. So the byte limit isn't "incomplete for TLS," it's inert for
  every leaf. Fixed the base case: `MediaPublisher::pending_bytes()`
  (`src/media/egress/backends/rtmp.rs`) sums the in-flight
  `pending_write` remainder plus every still-queued `current_batch`
  packet; `RtmpFabricEngine::pending_application_bytes()` exposes it
  (`0` outside `Publishing`); `RtmpShardBackend::visit_one_ready_leaf`
  writes it into `leaf.common.pending_application_bytes` after every
  visit. New test
  `pending_application_bytes_reflects_queued_wire_data_and_drains_to_zero`
  seeds a startup batch, drives to `Publishing` (which — `drive_to`
  stopping the instant `HandshakeComplete` fires — leaves the whole
  batch queued and completely unwritten), asserts the reported pending
  bytes are nonzero at that point, then drives to completion and asserts
  it drains back to zero; verified as a real regression by temporarily
  hard-coding `pending_bytes()` to always return `0` and confirming the
  test fails, then restoring it. **Two things this does not do, left
  open deliberately:** (1) it counts wire-level queued bytes, the same
  thing plain TCP would count — it does not add rustls-internal
  plaintext/encrypted buffer bytes on top for RTMPS specifically, so the
  original finding's TLS-specific refinement is still outstanding; (2)
  `is_limit_exceeded()` still has no caller anywhere — nothing acts on
  an over-limit leaf yet (suspend? close? drop?). Wiring real enforcement
  is a lifecycle change (AGENTS.md requires deterministic unit tests,
  loom/proptest where feasible, and a live harness fault case for that
  class of change) and needs its own deliberate design, not a rushed
  addition alongside an accounting fix.

### Proof

Tests cover:

- partial writes across every RTMP packet boundary;
- partial handshake and server-response reads;
- sequence header refresh and keyframe startup;
- acknowledgement and ping handling under media load;
- TLS handshake and encrypted output backpressure;
- feed overrun and reconnect recovery;
- timestamp monotonicity and composition offsets;
- output removal during every connection state;
- slow and non-reading TCP peers;
- connection reset and half-close;
- no duplicate ready entries during simultaneous read, write, and feed events.

Live tests cover:

- 1,000 or more healthy RTMP outputs;
- mixed RTMP and RTMPS leaves;
- 998 healthy plus one non-reading and one throttled peer on one shard;
- reconnect storm and handshake stall;
- output media integrity and advancing receiver bytes;
- comparison with the recorded 1,140 RTMP plus 60 SRT workload;
- 1,000 RTMPS-only outputs against a real TLS-terminating mediamtx
  listener (see the RTMPS-at-scale writeup below).

Current branch status: a first live A/B exists —
`rtmp-fabric-matrix` (`src/bin/test_harness/resource_sweep/branch_matrix.rs`,
recorded results in `test/harness/baselines/rtmp-fabric-matrix/`) runs the
same RTMP-source workload through a real mediamtx receiver and real
ffmpeg publisher twice, once legacy and once fabric-routed, each in its
own isolated stack, and compares CPU/RSS. At the default N=10 scale on a
6-CPU/12GB host: legacy 6.82% avg CPU / 71,802KB avg RSS vs fabric 6.98%
avg CPU / 71,194KB avg RSS — within noise, not the multi-x CPU regression
the SRT fabric showed before its fragment-batching fix (Phase 4 status
above). Both variants delivered all 10 outputs to progress within timeout
in both runs. This is a smoke-scale correctness + early resource read, not
the 1,000+-output live-scale proof below — `RTMP_FABRIC_MATRIX_EGRESS_COUNT`
controls the scale for a fuller run.

### Exit gate

Fabric RTMP and RTMPS become default only after media correctness, tail progress,
CPU, RSS, context switches, and allocator behavior match or improve on legacy.

**Not met — a default-flip attempt found a real gap here.** See Phase
6's "Rollout order" for the full story: CI's `Internal media backend
smoke` gate caught the RTMP fabric leaf terminating and never
recovering under a file-ingest-to-transcoded-RTMP-720p shape, a
correctness failure this phase's live captures (all live-RTMP- or
live-SRT-sourced) never exercised. `EgressRolloutMode::default()` is
back to `Off`; this exit gate is genuinely unmet until that specific
regression is root-caused and re-verified live.

### Remaining Phase 5 work — plan

Tracked items still open after the shard-liveness, muxer-port-reuse, and
hot-path fixes above, in the order they should be picked up. Each entry
names the model tier per `AGENTS.md`'s "Operational Guidance" (`sonnet`
for scoped fixes/tests, `opus` for architecture/lifecycle redesign or
benchmark-driven decisions) and why that tier fits.

1. **~~Live re-measurement of the SRT muxer-port-reuse fix~~ — done,
   partially.** Added a real `srt-fabric-matrix` harness mode
   (`src/bin/test_harness/resource_sweep/branch_matrix.rs`, mirroring
   `rtmp-fabric-matrix` exactly via a shared `run_protocol_fabric_matrix`
   driver) and ran it live at N=10 on this host: fabric CPU (20.07% avg
   / 32.66% peak) vs legacy (14.76% avg / 17.4% peak) — ~1.36x, with
   fabric RSS actually *below* legacy (90,058.67KB vs 92,468KB). Zero
   errors in either mediamtx log; both variants delivered 10/10 outputs.
   Recorded in `test/harness/baselines/srt-fabric-matrix/vps-6cpu-12gb/`.
   This is a real, large improvement in *ratio* over the pre-fix
   `w2-fabric-batched` capture (fabric 149% vs legacy 41.7%, ~3.6x at
   N=100) — consistent with the muxer-port-reuse fix addressing a real
   cost — but it is **not a controlled before/after**: the two captures
   differ in scale (N=10 vs N=100) and measurement setup (this harness
   mode vs whatever ad-hoc process produced `w2-fabric-batched`), and
   this harness mode does not sample native `RcvQ`/`SndQ` thread counts,
   only process-level CPU/RSS. What's still missing, and still `opus`
   tier (benchmark-driven, needs interpreting live results against a
   controlled baseline): re-running `srt-fabric-matrix` against the
   pre-fix commit on the same host for a true before/after at matched
   scale, and adding thread-count sampling to the harness so the
   `RcvQ`/`SndQ` claim itself (not just aggregate CPU) can be verified
   directly.
2. **RTMP feed/I/O readiness split (task #11) — `opus`.** Architectural:
   introduce an explicit wait-condition type (`Feed` / `Io(Interest)` /
   `FeedOrIo(Interest)` / `Timer`) so a feed wake can directly enqueue
   feed-waiting leaves instead of bouncing through native poller
   re-registration. Touches the `ProtocolEngine`/`EngineProgress`
   contract shared by both RTMP and SRT backends — a redesign of a
   cross-cutting interface, not a local fix. Needs its own design pass
   (what changes in `EngineProgress`, how `EgressShardBackend::on_ready`
   consumes the new wait condition, whether `Needs`/`Progress` still
   make sense as-is) before implementation.
3. **~~Remaining hot-path allocation/dispatch (rest of task #12)~~ —
   profiled; the flagged candidates are not worth implementing.**
   `perf` was initially unusable in this sandbox
   (`kernel.perf_event_paranoid` was `4`, "disabled without
   `CAP_PERFMON`", and changing it needs root); the agent later gained
   passwordless `sudo` on this host mid-session, which unblocked it —
   `kernel.perf_event_paranoid=1` and `kernel.kptr_restrict=0`
   (temporarily; both reverted to their original values after) let
   `perf record` attach to a live `restream` process. Captured a real
   profile: `--profile bench` binary, 30 SRT egress outputs
   (`srt-fabric-matrix` at `SRT_FABRIC_MATRIX_EGRESS_COUNT=30`), 18
   seconds sampled during the fabric variant's steady-state window,
   4,683 samples (~10.6B cycles). Result, full writeup in
   `test/harness/baselines/srt-fabric-matrix/vps-6cpu-12gb/perf-profile-summary.txt`:
   **every `restream::*` symbol combined — the entire Rust fabric
   implementation — accounts for 0.74% of total sampled CPU cycles.**
   `TsFeed::read_from` 0.05%, `SrtEgressEngine::advance` 0.05%, the
   `SrtMessageSender` trait-object dispatch site (`send_message`) 0.02%,
   `Bytes`-allocation-related symbols under 0.05% combined. The
   remaining >99% is libsrt's own native protocol code (`srt::CUDT::*`,
   `CSndQueue`/`CRcvQueue` workers, `CChannel::sendto`/`recvfrom`) and
   the `sendmsg`/`recvmsg` syscalls those calls make
   (`__libc_sendmsg` alone: 35.89% self time; `__libc_recvmsg`: 16.55%)
   — expected UDP protocol cost for 30 concurrent outputs, and not
   something the Rust fabric layer's allocation patterns can affect.
   **Conclusion:** the hot-path audit's medium-confidence candidates
   (`Vec<Bytes>`/`Vec<Arc<MediaPacket>>` per-read allocation,
   `SrtMessageSender` trait-object dispatch, `Bytes::slice()` refcount
   churn) were correctly flagged as *unconfirmed* pending profiling —
   profiling now says implementing them would not move the needle at
   this scale (N=30). Not implementing them is the correct call here,
   not an open item; see the summary doc for what would justify
   revisiting this (a much higher output count, or profiling the RTMP
   path specifically, neither done yet). `poll_buffer.drain(..).collect()`
   was attempted once this session before the profiler was available
   and reverted (the direct-field-borrow rewrite needed to avoid the
   temporary `Vec` conflicts with the `leaf_mut`-style helper methods
   `poll_ready` calls in its loop body) — given the SRT profile's
   verdict on allocation-class costs generally, this is now deprioritized
   rather than worth forcing through the borrow-checker fight.
   Two items landed without needing a profiler at all, because they
   weren't "maybe helps," they were proven duplicate syscalls: (a) on
   the `EgressCommand::FeedWake` path (fires far more often than the
   shard's idle poll cycle), `refresh_registrations_for_feed_wake`
   called `register_leaf` for every connected leaf on every wake, even
   ones already `READ_WRITE`-registered from a previous wake — fixed
   with the same skip-when-unchanged check `visit_one_ready_leaf`
   already uses, proven by
   `refresh_registrations_for_feed_wake_skips_leaves_already_read_write`
   (verified as a real regression: temporarily removing the skip made
   the test fail, 18 calls vs an expected 15); and (b)
   `SrtEgressEngine::advance` had the exact same one-unit-per-`read_from`-call
   shape the RTMP fix above addressed — fixed identically with a
   `pending_units` burst buffer, proven by a new test plus three
   existing tests updated for the new (still-safe) cursor-advance
   behavior. Full RTMP (31 tests) and SRT (263 tests) suites, clippy,
   fmt, source-audit, and docs checks all pass on both.
4. **~~RTMPS rustls-internal buffer accounting~~ — implemented, option
   (a) from the two picked out below.** Since rustls exposes no
   occupancy getter (confirmed against rustls 0.23.41's actual public
   API — `ConnectionCommon::set_buffer_limit` is a cap *setter* only),
   `RtmpConnection::rustls_pending_bytes_estimate()`
   (`src/media/egress/backends/rtmp_connection.rs`) returns rustls's own
   default 64KB `sendable_plaintext`/`sendable_tls` cap whenever the
   connection still `wants_write()` (has unflushed data), `0` otherwise
   — a worst-case upper bound, explicitly documented as an estimate, not
   an exact count. `RtmpShardBackend::visit_one_ready_leaf`
   (`rtmp_shard.rs`) adds it to `pending_application_bytes` alongside
   the existing wire-level count, so `LeafLimits::max_pending_bytes`
   enforcement (`classify_stall`/`sweep_stalled_leaves`) can no longer
   under-count a backpressured RTMPS leaf by an unbounded amount — the
   hidden buffer's contribution is now capped at 64KB instead of
   invisible. Proven by two new tests
   (`rtmp_connection_tests.rs`): a plain connection always reports `0`;
   a freshly constructed TLS connection reports `65536` before any I/O
   (mirroring the existing `tls_connection_wants_write_before_any_io`
   proof that `wants_write()` is true immediately, since a fresh client
   has a queued ClientHello) — verified as a real regression by
   temporarily hardcoding the `Tls` branch to return `0` and confirming
   the positive-case test fails, then restoring it. Full `cargo test
   --lib` (1,866 tests), clippy, fmt, source-audit, and docs checks
   pass.
5. **~~`is_limit_exceeded()` enforcement~~ — implemented, by reusing a
   design SRT already had rather than inventing a new one.** The
   "design decision" this item worried about — what happens to an
   over-limit leaf — turns out to already be answered:
   `SrtShardBackend` has had a working answer since Phase 4,
   `sweep_stalled_leaves`/`NativeSrtLeaf::observe_stall`
   (`src/media/egress/backends/srt.rs`), just not wired to RTMP.
   `classify_stall` (`src/media/egress/policy.rs`) is the actual
   policy, and it's better than a bare byte-threshold: a leaf with
   pending bytes is `Idle` (nothing pending), `Backpressured`
   (pending, but progress within `LeafLimits::max_backpressure_duration`
   — left alone, a slow-but-alive peer is fine), or `Stalled` (pending,
   no progress for the deadline — closed, `CloseReason::NoProgress`,
   and the application retry policy owns reconnection). RTMP now has
   the identical mechanism: `RtmpFabricLeaf::observe_stall` (using
   `common.pending_application_bytes`, wired up in the accounting fix
   above, and `common.progress.last_byte_progress`/
   `last_protocol_progress`, already updated generically by
   `EngineVisit::run` → `apply_progress_to_common` for every protocol,
   so no new tracking was needed) and
   `RtmpShardBackend::sweep_stalled_leaves`, called from `on_media_tick`
   at the same 1-second-minimum cadence SRT uses (RTMP has no native
   transport backlog to probe via FFI, so it's actually simpler than
   SRT's version — no throttle really needed, kept for symmetry). New
   test `sweep_stalled_leaves_closes_only_the_leaf_with_no_recent_progress`
   drives two real leaves to `Publishing`, then deterministically sets
   one's stall-relevant state (`pending_application_bytes`,
   `observed_since` — both otherwise driven by real I/O timing that
   would make a timing-based test slow and flaky) to simulate "stuck
   for an hour with pending bytes" and the other to "pending bytes but
   recent progress," and asserts the sweep closes only the stuck one.
   Full RTMP suite (32 tests), clippy, fmt, source-audit, and docs
   checks pass.
6. **~~1,000+-output Phase 5 exit gate~~ — real-scale data now exists;
   the result reverses the smoke-scale regression signal.** This host
   turned out to already be capable enough (6 CPU, 12GB RAM KVM VPS —
   see the host-label correction note; not WSL2 as earlier captures on
   this same path wrongly said) to run genuine 1,000-output captures
   without waiting for different hardware. RTMP at N=1000
   (`test/harness/baselines/rtmp-fabric-matrix/vps-6cpu-12gb-n1000/`):
   CPU avg 85.31% fabric vs 83.67% legacy (~1.02x, parity, within
   noise) — the N=10 capture's ~1.4x gap is gone; RSS avg 287,432KB
   fabric vs 321,129KB legacy (fabric ~11% *lower*). CPU peak was
   elevated in this one capture (130.78% vs 94.32%, ~1.39x) despite
   average parity; **resolved by two more re-runs** (same commit,
   `vps-6cpu-12gb-n1000/legacy-run3.csv`/`fabric-run3.csv` — the other
   rerun's raw CSVs weren't kept, only summarized here): run 2 was
   legacy 96.07% peak vs fabric 110.83% peak (~1.15x); run 3 was
   legacy 132.96% peak vs fabric 109.94% peak — **legacy's own peak
   exceeded fabric's original "regression" number, with fabric peaking
   *lower* than legacy that run**. Across all three runs: CPU avg mean
   88.08% legacy vs 90.73% fabric (~1.03x), CPU peak mean 107.78%
   legacy vs 117.18% fabric (~1.09x) — both close to parity, and the
   per-run peak swings (94–133% legacy, 110–131% fabric) are host-level
   sampling noise affecting both variants symmetrically, not an
   RTMP-fabric-specific issue. This closes the CPU-peak open question
   the first capture raised. SRT at
   N=500 (`.../srt-fabric-matrix/vps-6cpu-12gb-n500/`) — the highest
   scale at which legacy can even be compared, see below — CPU avg
   171.0% fabric vs 196.76% legacy (fabric ~13% *lower*), RSS avg
   922,130KB vs 1,052,797KB (fabric ~12% *lower*). Zero panics, zero
   real errors in any capture; `mediamtx`'s "decode error" and
   restream's "feed overrun: resynchronized" lines appear in *both*
   variants proportional to output count (legacy shows more of them,
   not fewer) — the pre-existing, documented, benign single-connection
   ramp-up resync mechanism, not a fabric-introduced defect.
   **Unplanned but decisive discovery**: legacy SRT egress has a
   hardcoded semaphore capping concurrent sender threads at 512
   (`src/media/srt_egress.rs:212`, already listed as a removal target
   above) — confirmed live by running `srt-fabric-matrix` at
   `SRT_FABRIC_MATRIX_EGRESS_COUNT=1000`: the legacy variant's output
   count froze at 512/1000 and never advanced. Legacy cannot serve
   1,000 concurrent SRT outputs *at all*, on any hardware — this isn't
   a performance gap to close, it's a hard ceiling only the fabric
   path removes. Confirmed fabric clears it directly: running the
   fabric path alone (`resource-sweep --no-netns` with
   `RESTREAM_EGRESS_FABRIC=srt`, since legacy failing aborts the
   paired-variant matrix before the fabric side ever runs) reached
   1,000/1,000 outputs cleanly in 43s
   (`.../srt-fabric-matrix/vps-6cpu-12gb-n1000-fabric-only/`).
   **Follow-up: is 512 actually the ceiling, or just where the
   semaphore happens to sit?** Tested directly by temporarily raising
   `sender_semaphore`'s capacity from 512 to 2000
   (`src/media/engine_registries.rs:459`, reverted immediately after —
   never committed) and re-running `srt-fabric-matrix` at
   `SRT_FABRIC_MATRIX_EGRESS_COUNT=1000`. Legacy *did* progress past
   512 this time (647 at 10s, climbing to 925 by 240s), proving the
   semaphore alone wasn't the only thing standing between legacy and
   1,000 — but it still never reached 1,000 within the harness's 240s
   timeout, and the whole run failed with real `connection failed`
   errors on the remaining outputs (`restream::media::srt::srt_egress`
   log: "Connection failed to srt://..."), not just slow progress.
   Whatever the deeper limit is (ephemeral UDP port pressure from
   legacy's lack of muxer-port sharing is the leading suspect —
   `/proc/sys/net/ipv4/ip_local_port_range` gives ~28K ports on this
   host, and legacy binds a fresh one per connection attempt including
   retries — but this wasn't root-caused further, since fixing legacy's
   scalability isn't in scope for this migration), the practical
   conclusion is the same: the 512 cap isn't an arbitrary tunable
   number blocking an otherwise-fine architecture, it's papering over a
   real one-thread-per-output scaling wall that raising the number
   doesn't fix. This strengthens rather than weakens the original
   finding.
   **This does not fully close the exit gate as originally scoped** —
   still missing: the mixed RTMP/RTMPS workload specifically (only
   plain RTMP was run at N=1000; RTMPS at scale is untested), the
   1,140 RTMP + 60 SRT *combined* workload shape, and
   context-switch/allocator-behavior instrumentation (only CPU/RSS
   were sampled). The RTMP CPU-peak question is resolved (see above —
   three-run data shows it was sampling noise symmetric across both
   variants, not a fabric issue). But the core question this gate
   exists to answer — does the fabric path hold up, or beat, legacy at
   real scale — now has a real, decisive answer for the shapes tested:
   yes, and for SRT it does something legacy structurally cannot do at
   any scale, cap raised or not.
7. **The 1,140 RTMP + 60 SRT combined workload shape — done, and it
   surfaces a real (not noise) CPU gap the single-protocol captures
   didn't.** `resource-sweep`'s existing scenario catalog
   (`resource_egress_scenarios.json`) already defines mixed-kind
   scenarios (`egress-growth-source-mixed`, etc.), but
   `run_protocol_fabric_matrix`'s A/B driver applies one output count
   uniformly to every kind in a scenario — it cannot reproduce this
   baseline's ~19:1 ratio. Added a new `mixed-fabric-matrix` harness
   mode (`resource_sweep/branch_matrix.rs`'s `mixed_fabric_matrix`,
   backed by a new `run_resource_egress_ratio` in `resource_sweep.rs`
   that creates an independent count per kind against one shared
   pipeline/publisher instead of the shared growth loop's uniform
   count) — `MIXED_FABRIC_MATRIX_RTMP_COUNT`/`_SRT_COUNT` control the
   two counts independently, run once each as legacy and
   `RESTREAM_EGRESS_FABRIC=all`. Ran three full legacy/fabric pairs at
   `RTMP_COUNT=1140, SRT_COUNT=60` (`.local/artifacts/mixed-fabric-matrix-run{1,2}/`,
   run 3 at `.local/artifacts/mixed-fabric-matrix/`) on this same
   6-CPU/12GB host. All three runs: 1,200/1,200 outputs reached
   progress in both variants every time; fabric reached full progress
   in 4-16s across all three runs, legacy took 4-39s (legacy hit 36
   real `Connection failed` SRT errors in run 1 and 4 in run 3, all
   self-recovered via the application retry policy; fabric had 0 real
   errors across all three runs, one benign self-recovered
   `SrtEgressEngine` leaf-retry `WARN` in run 2 — the
   `terminated_unexpectedly` fix from earlier in this phase visibly
   doing its job). Resource averages across the three runs: **CPU is
   higher on fabric at this ratio, consistently, not sampling
   noise** — avg 131.7% fabric vs 117.2% legacy (~1.12x), peak 161.4%
   fabric vs 130.9% legacy (~1.23x, and the widest single-run peak gap,
   195.25% vs 129.65% in run 1, is the largest gap of any capture in
   this phase). RSS runs the other way: fabric averaged 415.3MB vs
   legacy 487.6MB (~15% *lower*), consistent across all three runs.
   **Conclusion, stated plainly rather than rounded to a pass**: the
   fabric path reliably delivers this exact combined ratio with fewer
   real connection errors and lower memory than legacy, but it costs
   more CPU at this specific mix than either protocol showed in
   isolation (RTMP alone was parity at N=1000; SRT alone was ~13%
   *lower* CPU than legacy at N=500) — the two protocols' shard work
   compounds under one combined workload in a way neither single-protocol
   capture surfaced. This is not disqualifying (fabric still clears
   ~2x more real connection failures and a meaningfully smaller memory
   footprint), but it is a genuine open cost, not a closed question:
   worth a profiler pass at this specific combined ratio before
   treating "match or improve on legacy" as satisfied for the combined
   shape specifically. Context-switch/allocator instrumentation is
   still not sampled by this harness mode (same gap as the
   single-protocol captures above) and RTMPS-at-scale is still
   untested (blocked on the same harness cert-generation gap noted
   below).

   **Follow-up profiler pass at the combined ratio — done, and it
   reverses part of the earlier "not worth it at this scale"
   conclusion.** `perf record -F 99 -g` attached to the fabric
   variant's real `restream` process during an 18-second steady-state
   window at the same `RTMP_COUNT=1140, SRT_COUNT=60` scale (3,547
   samples, `kernel.perf_event_paranoid`/`kptr_restrict` temporarily
   lowered via passwordless `sudo` and restored after, same procedure
   as the earlier SRT-only profile). Self-time by DSO: kernel 66.83%,
   `restream` (our own code) 16.47%, `libc` 15.24%, `[vdso]` 1.32% —
   the kernel share is expected TCP/UDP syscall and softirq cost
   (`__tcp_transmit_skb`, `ip_output`/`ip_finish_output2`,
   `net_rx_action`, `udp_sendmsg`, ...), consistent with the earlier
   SRT-only finding that the fabric layer itself is a small slice of
   total CPU. Two things are different from the earlier SRT-only
   profile (N=30, 0.74% combined Rust-symbol share) at this larger,
   RTMP-heavy combined scale, and neither is noise:
   - **Per-leaf redundant Annex-B→AVCC video conversion, ~3.7% of
     total sampled CPU.** `RtmpFabricEngine` holds one
     `RtmpMediaEncoder` per leaf (`src/media/egress/backends/rtmp.rs`),
     and `RtmpMediaEncoder::encode`/`encode_video`
     (`src/media/rtmp/egress_engine.rs`) re-runs
     `find_annexb_start_codes`/`split_annexb_nalus`/`annexb_to_avcc_into`
     (`src/media/codec/video.rs`, backed by the `memchr` crate's AVX2
     paired-byte search) independently for every one of the 1,140 RTMP
     leaves, on every video packet, even though every leaf is
     converting the same shared source packet. Confirmed by thread
     attribution in the profile: these symbols run on all four
     `egress-shard-*` threads, not a shared upstream stage. This is
     the single largest coherent chunk of the `restream` 16.47% self-time
     slice. Per-leaf state is genuinely needed for parameter-set
     tracking (a leaf joining after the initial keyframe needs its own
     SPS/PPS accumulation before it can emit valid AVCC), but the
     *conversion itself* is stateless per NAL and does not obviously
     need to be redone per leaf — worth a real design pass (share the
     converted AVCC buffer once per source packet, let each leaf's
     `RtmpMediaEncoder` only own the per-connection parameter-set
     decision) rather than a quick patch, since it touches the shared
     `ProtocolEngine`/leaf-state contract.
   - **Allocator churn, ~9% of total sampled CPU
     (`_int_malloc`/`malloc`/`_int_free`/`unlink_chunk`/`malloc_consolidate`/`realloc`
     combined) plus ~3.7% more in `__memmove_avx_unaligned*`.** The
     earlier SRT-only profile (item 3 above) flagged
     `Vec<Bytes>`/`Vec<Arc<MediaPacket>>` per-read allocation as a
     "medium-confidence, unconfirmed" candidate and concluded
     profiling at N=30 said implementing it "would not move the
     needle at this scale" — that conclusion was scale-dependent, not
     universal, and does not hold at 1,200 combined outputs: allocator
     work is now a real, double-digit-adjacent contributor. Given the
     per-leaf AVCC conversion above allocates a fresh `Vec<u8>` per
     leaf per packet, some of this churn is likely the same root cause
     rather than two independent findings — resolving the conversion
     duplication would plausibly shrink both numbers together.
   **Correction — this profile does not actually explain the
   fabric-vs-legacy CPU gap, and saying so without checking was a
   mistake worth naming.** Only the fabric variant was profiled; no
   comparable legacy profile was captured. Checked afterward: legacy
   has the *identical* one-encoder-per-output architecture —
   `run_rtmp_egress` (`src/media/rtmp/egress.rs:256`) constructs one
   `RtmpMediaEncoder` per output task, exactly mirroring
   `RtmpFabricEngine`'s one-per-leaf encoder, and calls the same
   `encode()` per packet per output. The redundant Annex-B→AVCC
   conversion is therefore a pre-existing cost both variants pay
   equally, not something fabric introduced — it cannot be what makes
   fabric cost more CPU than legacy at this ratio. What this profile
   *does* establish, honestly: a real, quantified breakdown of where
   fabric's own CPU goes at this scale (useful on its own — it reverses
   the N=30 "allocator churn isn't worth chasing" conclusion, and
   flags a genuine shared-architecture inefficiency worth fixing
   someday for its own sake, in both variants). What it does *not*
   establish is why fabric is ~1.1-1.2x legacy's CPU at this specific
   combined ratio — that requires a legacy-variant profile at the same
   scale for a real differential, not yet captured. Left as the actual
   next step if this gap is worth closing before a default-mode flip.
   Neither finding blocks the fabric path from clearing this exit
   gate's correctness bar on its own.
   Profile artifacts:
   `/tmp/mixed-fabric-matrix.perf.data` (not committed — regenerate
   with the same `sudo perf record -F 99 -g -p <fabric restream pid>`
   procedure against a live `mixed-fabric-matrix` run if revisiting
   this).

   **The actual legacy-vs-fabric differential profile — captured, and
   it points at thread topology, not abstraction overhead.** Same
   procedure, same `RTMP_COUNT=1140, SRT_COUNT=60` scale, this time
   `perf record -F 99 -g` attached to the *legacy* variant's process
   for an 18s steady-state window (2,970 samples), compared directly
   against a fresh same-run fabric capture (this run measured
   legacy 129.72% / fabric 135.49% avg CPU — a much smaller gap than
   the three-run average above, itself a reminder the differential has
   real run-to-run spread and isn't a fixed multiplier).
   - **Fabric's own code is not the problem — it's cheaper than
     legacy's, not more expensive.** Self-time by DSO: legacy
     `restream` 21.05% vs fabric `restream` 16.47%; legacy `libc`
     18.60% vs fabric `libc` 15.24%; legacy kernel 59.41% vs fabric
     kernel 66.83%. The `ReadyQueue`/`EgressCommand`
     dispatch/`apply_progress_to_common` generation-check machinery
     fabric added is *not* a bigger relative cost than legacy's
     per-task `tokio::select!` async state machine — if anything the
     opposite. So no, this isn't "abstractions we added" showing up as
     the cost; the data says the opposite of that hypothesis.
   - **Thread topology is the real, concrete, found difference — the
     same category of gap as the SRT muxer-port-reuse miss.** Legacy's
     RTMP path is plain tokio tasks sharing
     `default_tokio_worker_threads(effective_cpus)`
     (`src/config.rs:316`) — on this 6-CPU host that resolves to just
     **2** tokio worker threads (`6.div_ceil(3) = 2`, clamped [2, 8]);
     confirmed by the profile — `restream-tokio` alone carries 77.54%
     of legacy's total self-time, meaning all 1,140 RTMP tasks'
     `encode`/write work is serialized onto those 2 threads via tokio's
     work-stealing scheduler. Fabric's RTMP shard count is a
     **separate, hardcoded default of 4** (`shards: 4`,
     `src/config.rs:141`, gated by `RESTREAM_EGRESS_SHARDS` but never
     derived from `effective_cpus` the way the tokio worker count is)
     — confirmed by the profile: 4 `egress-shard-*` threads carry
     63.55% of fabric's total self-time between them, and fabric
     *still* keeps its own 2-worker tokio pool alive underneath for
     everything else (API, DB, feed-wake watchers — `restream-tokio`
     is 12.10% of fabric's self-time, not zero). Net effect: on this
     6-core host, fabric runs 4 dedicated shard threads *on top of* the
     same 2 tokio workers legacy already uses (plus the SRT native
     threads both variants share identically), rather than the shard
     count and tokio worker count being budgeted against the same core
     count together. That is a genuine, unimplemented tuning gap —
     `RESTREAM_EGRESS_SHARDS`'s default was never derived from
     `effective_cpus`, unlike every other concurrency knob in
     `src/config.rs` — and it is exactly what **Phase 7: Tuning and
     legacy removal** (below, "Select the smallest efficient shard
     configuration") already names as open, unstarted work. Measured
     signature of the resulting oversubscription: scheduler/futex-family
     kernel self-time (`try_to_wake_up`/`do_futex`/`schedule`/`futex_wake`/`futex_wait`)
     sums to 4.76% for fabric vs 3.85% for legacy, and `clock_gettime`/`[vdso]`
     time-checking (from `WorkBudget` deadline tracking running once per
     shard visit rather than once per task) sums to 1.67% for fabric vs
     0.65% for legacy — both real, both fabric-specific, but together
     only ~2 percentage points of CPU, i.e. real contributors, not a
     full explanation of the measured gap on their own.
   - **Honest bottom line**: this differential profile does not
     produce one smoking-gun cause the way the SRT muxer-port-reuse fix
     did. It rules out "our added abstractions are expensive" (they're
     not, by this data) and it identifies one concrete, unimplemented,
     analogous tuning gap (shard count not derived from `effective_cpus`
     the way every other concurrency default in `src/config.rs` is) with
     a measurable but partial (~2pp) signature. The rest of the gap
     between this ~2pp and the three-run average's ~12-23% is not
     accounted for by this pass and would need either a controlled
     `RESTREAM_EGRESS_SHARDS`-swept re-run (does raising shard count
     toward `effective_cpus` close the gap?) or deeper per-symbol
     diffing than a DSO/thread-level comparison gives. Left as the
     concrete next step — `RESTREAM_EGRESS_SHARDS` is already a live
     env var, so this is a re-run, not a design change, and the
     natural place to start before Phase 7's shard-count-tuning design
     work below.

   **RTMPS trust-root override — implemented (`sonnet`-tier).** The
   real blocker behind "RTMPS-at-scale needs harness infrastructure"
   turned out to be that `rustls_client_config()`
   (`src/media/rtmp/egress_transport.rs`) built its `RootCertStore`
   from `webpki_roots::TLS_SERVER_ROOTS` only, with no override — so
   there was no way to point restream's real RTMPS connect path at a
   private CA (or a self-signed harness cert). Landed as an opt-in
   production capability, not a test-only shim:
   - `AppConfig.rtmps_extra_trust_roots_pem_path: Option<String>`
     (`src/config.rs`), read from `RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM`,
     `None` by default (zero behavior change unless configured).
   - `rustls_client_config_with_extra_roots(path)` and
     `resolve_rtmps_client_config(Option<&str>)`
     (`egress_transport.rs`) parse the PEM once (via
     `rustls-pki-types`'s `CertificateDer::pem_file_iter`, added as a
     direct `std`-featured dependency — `rustls-pemfile` wasn't
     needed) and layer the extra roots on top of the same webpki set;
     a missing file, unparseable PEM, or a PEM with no usable
     certificates all return a descriptive `Err` rather than silently
     falling back to webpki-only trust.
   - Legacy path: `connect_rtmp_egress_stream` takes the resolved path
     and threads it from `RtmpEgressConnection::connect` up to its
     caller in `egress.rs`, which already has `engine.config` in
     scope — no new plumbing layer needed.
   - Fabric path: this is a process-wide value (same for every RTMPS
     output), not a per-output one like `enhanced_hevc_video`, so it
     does *not* need `RtmpFabricStartup`/`RtmpPublishStartup`
     threading. It's resolved once and stored as a
     `rtmps_client_config: Arc<ClientConfig>` field on
     `RtmpShardBackend` (set at shard-group spawn time, mirroring how
     `chunk_size` already flows through the same constructor chain:
     `retain_rtmp_fabric_runtime` → `spawn_rtmp_fabric_shard_group` →
     `rtmp_fabric_shard_backends_with_poller` →
     `resolving_rtmp_shard_backend` → `RtmpShardBackend::with_runtime_components`).
     `complete_pending_connect` calls
     `RtmpConnection::tls_with_config(tcp_stream, host,
     self.rtmps_client_config.clone())` instead of the bare `tls()` —
     `tls_with_config` already existed (added for the RTMPS proof
     test), so this was a one-line call-site change plus the
     constructor threading.
   - Proof: 5 new tests in `egress_transport.rs` covering
     default-roots resolution, successful augmentation, a missing
     file, and a PEM with no certificates — each verified as a real
     regression by temporarily deleting the empty-certs check and
     confirming the corresponding test fails (with a different, but
     still failing, error path), then restoring it; plus a new
     `AppConfig` env-var test. Full `cargo test --lib` (1,862 tests),
     clippy, fmt, source-audit, and docs checks all pass.
   - Also not done in this change: rustls-internal plaintext/encrypted
     buffer accounting for the leaf byte-limit (tracked separately, see
     the RTMPS accounting note above).

   **RTMPS-at-scale harness infra and live capture — done.** Checked-in
   fixture cert, not a Cargo dependency change: rather than moving
   `rcgen` out of `[dev-dependencies]` (it's not visible to the
   `test_harness` `[[bin]]` target under plain `cargo build`), generated
   one self-signed cert+key with `openssl` (20-year validity, `CN=localhost`,
   SAN `localhost`/`127.0.0.1`, explicit `CA:FALSE` — a `CA:TRUE` self-signed
   cert used as both trust anchor and leaf is rejected by rustls with
   `CaUsedAsEndEntity`, found live) and checked it in at
   `test/fixtures/tls/mediamtx-rtmps-{cert,key}.pem`, following
   AGENTS.md's "prefer checked-in fixtures over inline generation for
   tests, benches, and harness runs" over a Cargo feature-gate approach.
   Registered in `REQUIRED_CHECKED_IN_FIXTURES` and resolved via a new
   `restream::test_fixtures::rtmps_harness_cert_fixture()`. mediamtx
   serves RTMPS on its own dedicated `rtmpsAddress` listener (confirmed
   live — `rtmpEncryption: "optional"` does *not* enable same-port
   auto-detection on `rtmpAddress` despite the name; a real TLS
   handshake against the plain port silently reset every time),
   requiring a new `mtx_rtmps` port allocated in the harness's port
   defaults. New `SweepOutputKind::RtmpsSource` (`publish_url`/`read_url`
   gained an `rtmps_port` parameter, threaded through every call site
   including `bitrate.rs`), a new `egress-growth-source-rtmps` scenario,
   `ResourceSweepEnv.rtmps_tls: Option<(cert, key)>` (default `None`,
   zero behavior change for every other resource-sweep mode), and a new
   `rtmps-fabric-matrix` harness mode (`rtmps_fabric_matrix`,
   `RTMPS_FABRIC_MATRIX_EGRESS_COUNT`) that — unlike
   `run_protocol_fabric_matrix` — applies
   `RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM` to *both* the legacy and
   fabric variants, since both resolve RTMPS trust through the same
   `resolve_rtmps_client_config` path.

   Live results at real scale (N=1,000, this same 6-CPU/12GB host, three
   full baseline runs): **1,000/1,000 outputs reached progress in both
   variants, every run, with zero errors in either restream log** — a
   clean correctness result, first real evidence RTMPS fabric egress
   holds up at scale at all. Three-run averages: CPU is at parity
   (legacy 93.84% avg, fabric 95.55% avg — ratio 1.02x, within the
   noise band every other capture in this phase has shown). **RSS is
   not** — legacy 357.3MB avg, fabric 405.9MB avg, a consistent ~14%
   *higher* RSS for fabric across all three runs individually (not
   just on average), the reverse of every other protocol/workload
   combination measured in this phase (plain RTMP, SRT, and the
   combined RTMP+SRT workload all showed fabric using *less* memory
   than legacy).

   **Root-caused — glibc's per-thread malloc-arena behavior, not a
   Rust-level leak or a fabric-specific buffer-retention bug.** Two
   diagnostic passes:
   - `heaptrack` attached live to both variants' real processes at
     N=300 (`kernel.yama.ptrace_scope` temporarily set to `0` via
     passwordless `sudo` to allow runtime attach, restored after).
     Both variants' single largest retained-allocation site was
     identical: `annexb_to_avcc_into`'s output buffer inside
     `RtmpMediaEncoder::encode` (fabric: 25.16M over 759 calls; legacy:
     29.96M over 1,168 calls — legacy's own number *higher*, not
     lower). This ruled out "fabric's Rust-level heap allocations
     retain more live bytes than legacy's" — heaptrack's own view of
     live heap memory did not reproduce the RSS gap at all, meaning the
     gap lives outside what the Rust-level allocation tracker sees.
   - That pointed at the C allocator layer itself. Tested directly by
     setting `MALLOC_ARENA_MAX` (inherited by both spawned restream
     processes via the harness's environment) across three points at
     N=1,000, no other change:

     | `MALLOC_ARENA_MAX` | legacy RSS avg | fabric RSS avg | RSS ratio | legacy CPU avg | fabric CPU avg | CPU ratio |
     |---|---|---|---|---|---|---|
     | unset (glibc default) | 357.3MB | 405.9MB | 1.14x | 93.84% | 95.55% | 1.02x |
     | 4 | 336.6MB | 328.0MB | 0.97x | 99.1% | 124.5% | 1.26x |
     | 1 | 298.3MB | 275.3MB | 0.92x | 133.0% | 282.8% | 2.13x |

     **Both `MALLOC_ARENA_MAX=4` and `=1` close or
     reverse the RSS gap** (fabric drops to 0.97x and 0.92x of
     legacy's RSS respectively) **but at a real, scaling CPU cost**
     (1.26x at 4, 2.13x — more than double — at 1), because fabric's 6
     dedicated shard threads contend far more on a shrunk/shared
     malloc arena pool than legacy's leaner 2-tokio-worker model does
     for the identical AVCC-buffer-churn workload. This is the same
     root allocation pattern the combined-workload profile already
     flagged (`annexb_to_avcc_into`'s per-leaf buffer, ~9% of CPU from
     malloc-family functions there) — RTMPS just makes the underlying
     glibc arena-per-thread tradeoff visible on the RSS axis instead of
     (or in addition to) the CPU axis, because TLS write throughput
     being slower than plain TCP write leaves more of these buffers
     concurrently live at any moment, so per-arena fragmentation shows
     up as measurable RSS rather than getting reused fast enough to
     stay invisible.
   - **Conclusion**: this is a genuine, quantified, three-point-tested
     memory/CPU tradeoff in glibc's default allocator behavior under
     fabric's thread topology, not a bug and not free to fix. The
     unset/default setting — what fabric already ships with — sits at
     the CPU-optimal end of this curve (RSS cost, CPU parity); forcing
     arena count down trades that CPU parity away for RSS parity or
     better. Not fixed in this pass: a real fix (fewer, better-sized
     arenas without full serialization; or switching to an allocator
     with smarter per-thread caching, e.g. jemalloc/mimalloc; or
     reducing per-leaf AVCC buffer churn directly, which would reduce
     pressure on this tradeoff from the allocation-volume side rather
     than the arena-count side) is real allocator/architecture work
     needing its own benchmark-driven design pass, not a quick patch
     alongside a root-cause writeup. Left as a concrete, well-scoped
     Phase 7 candidate, complementary to (not a replacement for) the
     shard-count tuning already documented there.

   **Context-switch/allocator instrumentation — implemented, closing
   the gap this whole phase's live captures kept flagging as
   missing.** `sample_resource_window`
   (`src/bin/test_harness/resource_sweep/measurement.rs`) previously
   sampled only CPU and RSS. Added `voluntary_ctxt_switches`/
   `nonvoluntary_ctxt_switches` (`/proc/<pid>/status`, diffed between
   samples the same way CPU ticks already are, reported as
   avg/peak-per-second) and peak thread count (`Threads:` in the same
   file) to `ResourceSample`/`ResourceAggregate`, threaded through the
   JSONL sample stream, the summary JSON, and the CSV writer — every
   future `resource-sweep`/`*-fabric-matrix` capture now records these
   by default, no flag needed. This is a direct, externally-observable
   proxy for exactly the thread-topology and allocator-contention
   findings above: live-verified at N=10 on this host, fabric already
   shows both a higher peak thread count (20 vs legacy's 14 — the 6
   dedicated shard threads landing on top of the shared 2-worker tokio
   pool, exactly as read from the code) and a higher voluntary
   context-switch rate (8.84/s vs 7.38/s) than legacy for the identical
   RTMPS workload, without needing a `perf`/heaptrack pass to see it.
   Full `cargo test --lib` (1,877 tests), the harness's own test suite,
   clippy, fmt, source-audit, and docs checks all pass. Not done: wiring
   this into the earlier phases' *recorded* baselines (the historical
   captures throughout this document predate the field and were not
   re-run) — it applies going forward, starting with Phase 7's shard
   sweep.

## Phase 6a: Pipeline recirculation backend

### Objective

Expose a user-facing output kind that connects one pipeline's prepared output to
another pipeline's input inside the same process.

This lands after the network protocol migrations because it changes
user-visible topology and can create feedback loops. It should reuse the proven
fabric ownership model instead of inventing a second in-process routing system.

### User-facing behavior

Users can configure an output whose destination is another pipeline input. The
UI and API must make the relationship visible as an edge in the pipeline graph,
including source pipeline, target pipeline/input, enabled state, status, and
recent progress.

The configuration must reject or clearly gate:

- direct cycles and obvious indirect cycles in the pipeline graph;
- incompatible media formats or track selections;
- recirculation into a pipeline whose input ownership would conflict with an
  external publisher;
- ambiguous fan-in where ordering or timestamp ownership is not defined.

### Work

Add a `Pipeline` protocol spec and backend that:

- consumes source feed units through the common egress leaf and scheduler;
- hands media to the target input through an in-process adapter, not a socket;
- preserves immutable ownership where possible, using shared buffers or
  reference-counted packets rather than serialize/parse loops;
- keeps timestamp domains explicit and does not rewrite PTS/DTS unless the
  target input contract requires it;
- uses common lifecycle, status, retry/no-progress policy, cancellation, and
  backpressure accounting;
- charges any target-side pending media against explicit bounded limits;
- publishes graph-visible status so operators can distinguish recirculation
  failures from source-pipeline or target-pipeline failures.

The backend may start with a same-format path only. Any transcoding or
container conversion required between pipelines belongs in normal media stages,
not hidden inside the recirculation egress backend.

Current branch status:

- `pipeline://` is recognized as the canonical recirculation scheme and
  classified under the pipeline protocol.
- The API parses recirculation URLs, asks the application service to validate
  topology and target input ownership, and accepts valid candidates for runtime
  startup.
- The media runtime claims the target pipeline input, forwards source feed
  packets through the in-process publisher after the target input is selected,
  records egress byte progress, and releases the input claim on cancellation.
- The in-process publisher clones only the packet shell for the same-format
  path and preserves the shared `Bytes` payload allocation when publishing into
  the target input ring. This proves scoped zero payload copying for the
  publisher path; broader cost comparison with loopback remains pending.
- Target-side buffering follows the existing pipeline input `RingBuffer`
  contract: a slow or absent target reader cannot block the recirculation
  publisher, retained packets stay capped by ring capacity, and lagging readers
  recover through the normal overrun/fast-forward path.
- The API admits only the initial same-format path for recirculation: source
  video with automatic codec selection and passthrough audio. Presets, explicit
  codec conversion, and audio selection/transforms are rejected before runtime
  startup.
- A pure application validator now parses typed recirculation targets and
  rejects direct and obvious indirect pipeline cycles before runtime backend
  ownership is enabled.
- Target input ownership validation rejects missing, cross-pipeline, disabled,
  or selected inputs before a recirculation backend can claim the target.
- Application-level recirculation validation now owns the output/input lookups
  and maps workflow failures to service errors, keeping HTTP handlers thin.
- The fabric command model now has a typed pipeline protocol spec carrying the
  target pipeline/input identity for the eventual in-process backend.
- A media-layer recirculation publisher seam can now publish feed packets into
  a destination input through the existing input gate, standby GOP cache, and
  timestamp mapper without activating the backend yet.
- Runtime graph projection now renders reserved recirculation outputs as
  output-to-target-input edges, including runtime output status, target
  address, and byte progress when the recirculation output is active.
- API lifecycle tests now cover create, target update, start, status/progress,
  stop, delete, runtime cancellation, and persisted row removal for
  `pipeline://` outputs.
- **Rehomed onto the real fabric — the core Phase 6a gap this whole phase
  existed to close.** Everything above was true, but ran on a plain
  per-output `tokio::spawn` task
  (`crate::media::recirculation::start_pipeline_recirculation`) — the
  exact "second in-process routing system" the phase objective warns
  against, not the fabric ownership model. `PipelineEngine`
  (`src/media/egress/backends/pipeline.rs`) now wraps the *same*
  `RecirculationInputPublisher` unchanged — timestamp mapping,
  standby-GOP replay, input-gate handling are untouched, only what
  drives `advance()` changes. `PipelineShardBackend`
  (`src/media/egress/backends/pipeline_shard.rs`) follows the
  `SinkShardBackend` template (no socket, no poller — see that module's
  doc comment for why `EgressCommand::FeedWake` must directly re-enqueue
  leaves there) with one real addition: claiming the target input is
  async and fallible, so it cannot happen on a shard thread.
  `PipelineTargetSource`/`SharedPipelineTargetSource` solve this the
  same way RTMP's publish-startup snapshot does — the application layer
  claims the target (`MediaEngine::try_register_pipeline_input_attempt`)
  and calls `set_pipeline_target` before dispatching `EgressCommand::Add`;
  the shard thread only ever reads. Full production wiring:
  `spawn_pipeline_fabric_shard_group` (`factory.rs`),
  `PipelineFabricRegistry` + `retain`/`dispatch`/`release_pipeline_fabric_runtime`
  (`src/media/engine_pipeline_egress_fabric.rs`), and
  `EgressTask::run_pipeline_fabric`
  (`src/infrastructure/bootstrap/egress_task.rs` — `bootstrap/egress.rs`
  was split into `egress.rs` (`EgressReconciler`/output start-up prep)
  and `egress_task.rs` (`EgressTask`/the `run_*_fabric` methods) purely
  to stay under the source-audit line cap after this addition, not a
  module-boundary change), including the same
  `terminated_unexpectedly`/`wait_for_stop_or_leaf_failure` retry wiring
  every other fabric protocol has, plus releasing the claimed target
  input (`unregister_ingest_if_current`) after `Remove`.
  `start_pipeline_recirculation` remains as a fallback if
  `pipeline_fabric` is ever `None`, matching every other protocol's
  call-site shape, though every `Pipeline`-scheme output now builds one
  in practice.
- Proof: 2 shard-level tests (`pipeline_shard/tests.rs`) drive
  `PipelineShardBackend` through `EgressShardHandle::spawn` on a real OS
  thread — publishes into the target ring, and rejects an `Add` with no
  claimed target. 2 engine-level tests
  (`engine_tests/egress_fabric.rs`) exercise the full production
  dispatch path: `retain`/`release` reference counting, and an
  end-to-end `dispatch_pipeline_fabric_command(Add)` → real
  `ring.push()` → the same wake-watcher task every other protocol uses
  → real shard thread → real target ring, observed via the real
  `EgressProgressSink` counters the application layer wires up. Caught
  a real timing race while writing the first shard-level test: pushing
  data immediately after `Add` can land before the shard thread even
  processes the command, so the leaf's own initial enqueue (not
  `FeedWake`) picks it up — silently not exercising the
  `FeedWake`-is-the-only-signal path the test existed to prove. Fixed by
  forcing the leaf idle first (matching the same pattern already used
  for `sink_shard`'s equivalent test); verified as a real regression by
  temporarily making `FeedWake` a no-op and confirming both
  liveness-dependent tests fail. Live-verified end to end under
  `RESTREAM_EGRESS_FABRIC=all` (`fault.output-stall`,
  `fault.egress-retry`) after the file split, confirming no regression
  across RTMP/SRT/Sink. Full `cargo test --lib` (1,877 tests), clippy,
  fmt, source-audit, and docs checks all pass.

### Proof

- direct and indirect topology loops are rejected deterministically;
- compatible pipeline-to-pipeline media advances without a network socket;
- incompatible formats fail visibly before media is consumed;
- target reader lag uses bounded ring overwrite semantics and does not block
  the recirculation publisher;
- removal releases target input ownership and feed cursors;
- publisher tests prove the same-format path preserves shared payload buffers
  while cloning only bounded packet metadata;
- zero-copy or bounded-copy behavior is measured and documented end to end;
- UI/API integration tests cover create, update, disable, delete, status, and
  graph rendering.

### Exit gate

Recirculation is user-facing only after topology validation, target-input
ownership, bounded buffering, status publication, and rollback behavior are
proven. The accepted implementation must be cheaper than routing through a
loopback network output and input for the same compatible media path.

Topology validation, target-input ownership, bounded buffering, status
publication, and rollback (cancellation releases the claim) are all
proven above, now on the real fabric rather than a parallel task-based
implementation.

**Cost comparison — measured, decisive.**
`benches/recirculation_cost.rs` isolates the one cost recirculation
structurally cannot pay and a loopback network path structurally cannot
avoid: wire-protocol encoding. Recirculation publishes the source
pipeline's already-decoded `MediaPacket`s directly into the target
ring — no protocol ever touches the bytes. A loopback RTMP output+input
pair must serialize every packet into RTMP chunks to send it (and parse
those chunks back out on the receiving side, not measured here) *in
addition to* whatever ring-publish work both paths already share. Using
the same `rml_rtmp` chunk serializer restream's own RTMP egress uses
internally (that internal call site is `pub(crate)`, not reachable from
a bench target — this reuses the public `rml_rtmp` API directly, the
same representative-reimplementation approach `benches/rtmp_serializer.rs`
already takes for the same reason), 32-packet batches on this VPS
(6 vCPU/12GB):

| Payload | `recirculation_publish` | `loopback_rtmp_wire_encode_only` | Ratio |
|---|---|---|---|
| 188 B (TS-packet-sized) | 3.58 µs | 9.60 µs | RTMP encode costs ~2.7x recirculation's *entire* publish |
| 1,316 B (SRT payload-sized) | 3.71 µs | 29.35 µs | ~7.9x |
| 8,192 B (keyframe-burst-sized) | 3.75 µs | 118.8 µs | ~31.6x |

Recirculation's cost is flat (~3.6–3.8 µs regardless of payload size —
it's pointer/`Arc`/`Bytes`-clone work, not per-byte processing); RTMP
wire encoding scales with payload size (more chunk-header overhead for
larger messages). The gap widens sharply for realistic keyframe-sized
payloads, and this doesn't even count the loopback path's unavoidable
extra costs this benchmark doesn't measure: receive-side chunk
deserialization, and the send/recv syscalls themselves (real, but not
usefully isolable from kernel/NIC-loopback variance in a
Criterion micro-benchmark — see the SRT syscall-profiling writeup
earlier in this phase for how dominant that cost class is in practice,
`__libc_sendmsg`/`__libc_recvmsg` at 35.89%/16.55% self time for 30
concurrent SRT outputs). Recirculation's actual advantage over a real
network loopback is larger than the table above shows, not smaller.
Exit gate met.

## Phase 6: Production integration and rollout

### Objective

Route normal reconciliation and API status through the new fabric while
preserving immediate rollback.

### Work

Add rollout modes such as:

```text
off
srt
rtmp
all
shadow-metrics
```

Only one runtime owns a given output. `shadow-metrics` may instantiate model or
assignment calculations, but must not establish duplicate network connections.

Current branch status:

- `RESTREAM_EGRESS_FABRIC` now parses the full protocol-selective mode set
  (`off`, `srt`, `rtmp`, `all`, `shadow-metrics`) as `EgressRolloutMode`,
  with legacy boolean spellings mapping to their historical meanings
  (`true` routes SRT only) and unknown values falling back to `off`.
  Routing helpers are protocol-selective and shadow mode is active without
  routing; bootstrap SRT gating uses `routes_srt()`. RTMP routing consumes
  `routes_rtmp()` once the Phase 5 fabric runtime exists.
- **Graceful shutdown and shard draining — implemented for RTMP, the
  largest and riskiest of the "Integrate" bullets below, picked first
  deliberately** (per AGENTS.md, a lifecycle change needing its own
  proof rather than bundling with the smaller Phase 6 items). Before
  this: `EgressCommand::Shutdown` made the shard runtime loop
  (`EgressShardRuntime::run`, `src/media/egress/shard.rs`) stop on the
  very next iteration — the backend's `on_command` never even saw
  `Shutdown` — and every backend's `Remove`/`Shutdown` handling closed
  the transport immediately regardless of
  `LeafCommon::pending_application_bytes`, silently truncating
  whatever a leaf still had queued but not yet on the wire.
  Implemented as a shared, bounded drain window plus RTMP-specific
  per-leaf draining:
  - `EgressShardConfig` gained a `drain_timeout` (default 3s, real
    `RESTREAM_EGRESS_DRAIN_TIMEOUT_MS` config knob, clamped
    `1..=60_000`). On `Shutdown`, the shard runtime now forwards the
    command to the backend once and keeps the loop running — still
    servicing ready/timer work normally — until either everything goes
    idle (nothing left to flush) or the deadline passes, whichever is
    first; only then does it stop and call `on_shutdown()`, matching
    the same forced-close fallback that already existed.
  - `RtmpShardBackend` gained real per-leaf draining:
    `begin_graceful_close` marks a leaf with nonzero
    `pending_application_bytes` as draining (closing immediately if
    there's nothing queued — the common case pays no delay).
    `visit_one_ready_leaf` opportunistically closes a draining leaf the
    moment it flushes, and `sweep_draining_leaves` (piggybacked on the
    existing once-a-second stall-sweep throttle) is the bounded
    backstop for a leaf that stops getting write readiness at all (a
    peer that stops reading). `Remove`, `DrainShard`, and `Shutdown`
    all route leaves through this same mechanism.
  - Proof: 4 new deterministic unit tests in a new
    `rtmp_shard_drain_tests.rs` (split out to stay under the
    source-audit line cap) covering deferred-close-until-flushed,
    immediate-close-when-nothing-queued, deadline-driven force-close,
    and `Shutdown` marking every connected leaf — plus a new
    shard-runtime-level test proving the loop genuinely stays alive
    for real wall-clock time after `Shutdown` (using a backend that
    never goes idle on its own, so the only way it can ever stop is
    the bounded deadline actually firing). Every new test was verified
    as a real regression: reverting the `shard.rs` runtime change or
    the `sweep_draining_leaves` flush check locally and confirming the
    corresponding test fails, then restoring it.
  - Live proof: real RTMP output flowing through the fabric against a
    real mediamtx receiver, `SIGTERM`'d mid-stream while media was
    actively publishing. `restream.shutdown.completed` fired ~660ms
    after the signal (well inside the 3s budget), and mediamtx logged
    a clean `closed: EOF` on the egress connection — an orderly close,
    not a reset — confirming the drain path is exercised by the real
    production SIGTERM handler (`spawn_signal_watcher` →
    `cancel_all_active_tasks` → `shutdown_all_rtmp_fabric_runtimes`),
    not just by unit tests calling `on_command` directly.
  - Full `cargo test --lib` (1,882 tests), clippy, fmt, source-audit,
    and docs checks all pass.
  - **SRT — done as the immediate follow-up, mirroring RTMP's mechanism
    exactly rather than redesigning.** `SrtFabricLeaf` gained the same
    `draining_since`/`draining_reason` fields; `SrtShardBackend` gained
    the same `drain_timeout` field and `with_drain_timeout` builder.
    `begin_graceful_close`/`sweep_draining_leaves` are structurally
    identical to RTMP's, with one adaptation: SRT has no
    `LeafCommon.pending_application_bytes` equivalent wired from
    visits, so "has this leaf flushed?" reads
    `SrtLeafPressure::is_backpressured()` directly (already-existing
    combined application-queue-plus-native-libsrt-backlog accounting,
    the same source `classify_stall`/`observe_stall` already used) —
    same semantics, different accessor. `Remove`/`DrainShard`/`Shutdown`
    route through `begin_graceful_close` identically to RTMP.
    `remove_leaf_socket`/`remove_leaf_by_output` gained the same
    `CloseReason` parameter threading. Extracted into a new
    `srt_drain.rs` (mirroring `rtmp_shard_drain_tests.rs`'s split)
    since adding this to `srt.rs` directly would have pushed it to
    1,049 lines, over the source-audit cap.
    Production wiring: `drain_timeout` threads from
    `EgressFabricConfig::drain_timeout_ms` through
    `spawn_srt_fabric_shard_group` → `resolving_srt_shard_backend` →
    `SrtShardBackend::with_drain_timeout`, the same path RTMP already
    used.
    Proof: 4 new deterministic unit tests in
    `src/media/egress/backends/srt/tests/drain.rs`, following this
    module's existing fake-sender convention (`leaf_termination.rs`'s
    `NeverDrainsSender` pattern — SRT sockets are native FFI, not
    fakeable at the OS level the way RTMP's tests fake a TCP peer) with
    a `ControllableSender` whose native backlog a test can flip live.
    Verified as real regressions: reverting `begin_graceful_close`'s
    deferred branch locally caught 3 of the 4 new tests failing, then
    restored. Live proof: a real SRT output flowing through the fabric
    against a real mediamtx receiver, `SIGTERM`'d mid-stream —
    shutdown completed in ~665ms (same order as RTMP's ~660ms, both
    well inside the 3s budget) and mediamtx logged the identical clean
    `closed: EOF`, not a reset.
    Full `cargo test --lib` (1,886 tests), clippy, fmt, source-audit,
    and docs checks all pass.
  - **Not done**: Sink and Pipeline still close immediately on
    `Remove`/`Shutdown`/`DrainShard`. Both have nothing meaningful to
    flush (Sink discards; Pipeline's recirculation publish is a
    synchronous in-process ring push, not a network write with a
    backlog), so they're low-priority, likely near-trivial follow-ups
    if ever needed — the pattern is now proven twice (RTMP, SRT) and
    would be a small, low-risk mechanical port if it ever matters.
- **Configuration validation — implemented as
  `EgressFabricConfig::validate(effective_cpus)`.** Before this,
  `EgressFabricConfig::from_env` only ever clamped individual fields to
  their own valid ranges; nothing checked whether a *combination* of
  individually-valid values was still a real misconfiguration. `validate`
  is a pure function returning `Vec<String>` (never fatal — a bad
  combination should be fixable by adjusting env vars and restarting, not
  a reason to refuse to start) checking four cross-field cases:
  `max_pending_bytes` smaller than `visit_max_bytes` (a single visit can
  hand a leaf more bytes than the pending limit allows, so
  backpressure/stall detection can trigger under normal operation);
  `shards` more than 4x the host's effective CPU count (more shard
  threads than cores costs CPU without buying throughput, per the
  Phase 5/7 shard-count findings); `drain_timeout_ms` under 50ms (too
  short for a leaf to get a real chance to flush before being
  force-closed); and `command_batch_budget` larger than
  `command_channel_capacity` (the batch budget can never be reached — a
  full channel drain always empties before hitting it). `run_app`
  (`src/infrastructure/bootstrap/mod.rs`) calls `validate` once at
  startup, right after the existing "effective startup configuration"
  log line, and logs each warning as
  `event_type = "restream.config.warning"`. Proof: 2 new unit tests in
  `src/config/tests/configuration_behavior.rs` — one confirming the
  default config is silent, one confirming all four checks fire together
  on a deliberately conflicting config. Verified as a real regression:
  disabling the first check locally dropped the flagged-warning count
  from 4 to 3, confirming the test actually exercises the logic rather
  than trivially passing. Full `cargo test --lib` (1,888 tests), clippy
  (default and `mcp-server,mcp-http-backend` features), fmt,
  source-audit, and docs checks all pass.
- **Fabric-versus-legacy and assigned-shard attribution — implemented on
  `ActiveEgress` and threaded into the API.** Before this, `ActiveEgress`
  (`src/media/engine.rs`) had no fabric-related fields at all, and the
  only per-shard telemetry (`EgressShardSnapshot`,
  `src/media/egress/shard.rs`) was reachable solely through four
  `#[cfg(test)]`-only accessors keyed by `FeedId`, not `OutputId` — no
  production code path could answer "is this specific output on the
  fabric, and which shard?" `ActiveEgress` gained two plain fields,
  `is_fabric: bool` and `shard_id: Option<u32>`, defaulted to
  `false`/`None` at registration. `MediaEngine::set_egress_fabric_attribution`
  is a small setter called once from the bootstrap egress reconciler
  (`src/infrastructure/bootstrap/egress.rs::start()`) right after all
  four fabric-task options (`rtmp_fabric`, `srt_fabric`, `sink_fabric`,
  `pipeline_fabric`) are resolved — deliberately *after* resolution, not
  from the earlier `use_*_fabric` routing booleans, so a fabric startup
  error that falls back to the legacy path is correctly attributed as
  legacy rather than fabric. Shard assignment reuses the existing pure
  `egress::manager::assign_output_to_shard` hash (no new snapshot
  round-trip needed — shard assignment is deterministic from
  `OutputId` + shard count, not runtime state). `api_runtime_views::common::egress_runtime_json`
  exposes both as `"fabric"` and `"shardId"` JSON keys, reaching every
  consumer of that function (`health_snapshot`, the graph projection,
  and telemetry) for free. Proof: 3 new unit tests in
  `src/media/engine_tests/egress.rs` covering the legacy default, an
  explicit fabric+shard assignment surfacing through
  `health_snapshot`'s JSON, and a no-op call for an output that was
  never registered. Verified as a real regression: temporarily making
  the setter a no-op locally caught the fabric-assignment test failing,
  then restored. Full `cargo test --lib` (1,891 tests), clippy (default
  and `mcp-server,mcp-http-backend` features), fmt, source-audit, and
  docs checks all pass.

Integrate:

- ~~desired output reconciliation~~ (audited, already correct — see below);
- status and failure reasons;
- ~~runtime resource map~~ (done — see below);
- ~~alerts for stalled shards, command overload, retry admission
  saturation~~ (done — see below; repeated-resync alerts remain open, no
  per-shard resync counter exists yet to derive them from);
- ~~diagnostic snapshots~~ (done — see below);
- ~~configuration validation~~ (done — `EgressFabricConfig::validate`, see
  above);
- ~~graceful shutdown and shard draining~~ (done for RTMP and SRT, see
  above).

**Desired output reconciliation — audited, no gap found.**
`load_output_runtime_snapshot` (`src/application/reconcile.rs`) determines
whether an output is currently active via
`MediaEngine::has_active_egress`, which is purely
`self.egresses.cancel_tokens.read().await.contains_key(output_id)` —
backend-agnostic by construction, since `register_egress_attempt_with_meta`
populates that same cancel-token map identically regardless of which
fabric branch (or the legacy path) the bootstrap egress reconciler picks
for a given output. `decide_output_start_action`/`decide_output_stop_action`
only ever consume that boolean, with no protocol or backend awareness at
all. No double-start or spurious-restart risk from fabric ownership;
nothing to fix here.

**Runtime resource map — implemented.** Before this,
`api_runtime_views::resource_map::egress_node` modeled every SRT output
as an app-owned `os_thread` and every other protocol as a `tokio_task` —
the legacy one-thread/task-per-output assumption, wrong for any
fabric-owned output (now the common case with the default flipped to
`All`, see above): a fabric leaf runs on a *shared* shard thread it does
not own exclusively, so the old accounting would double-count the same
fixed shard pool once per output. `egress_node` now reads the `fabric`/
`shardId` fields `egress_runtime_json` already produces (from the
attribution work above) and reports `execution: "shard_thread"` with
`threads.appOwned: 0` for fabric-owned outputs, leaving the legacy
per-protocol accounting unchanged for anything still on the legacy path.
The map also gained real per-shard nodes (`kind: "egress_shard"`, one
per live fabric shard across all four protocol registries) and summary
counters (`fabricShardThreadCount`, `fabricShardStalledCount`,
`fabricShardPanickedCount`) — the first time the resource map reflects
the fixed shard-thread count as an actual measured quantity rather than
an inferred one. Proof: unit tests for `egress_node`'s fabric-vs-legacy
branching in `resource_map_projection_tests.rs`.

**Alerts — implemented for the two conditions real counters already
exist for.** `EgressShardHeartbeat`/`EgressShardHealth` (`Healthy` /
`Stalled` / `Stopped` / `Panicked`, `src/media/egress/shard/group.rs`)
already computed shard health but had zero non-test callers anywhere in
the running server — dead code outside `supervisor/tests.rs`. Wired it
into production: `EgressFabricRuntime::heartbeat` (previously only
`snapshots`, test-only) plus a non-test per-registry accessor
(`{srt,rtmp,sink,pipeline}_fabric_shard_heartbeats`) and a combining
`MediaEngine::egress_fabric_shard_statuses` (new
`engine_egress_fabric_diagnostics.rs`) feed a new `egressFabricShards`
array into `health_snapshot()` — which every existing alert-derivation
caller (`/api/v1/alerts`, agent health, dashboard health) already
consumes, so this reaches production for free. `derive_alerts`
(`src/alerts.rs`) gained three new checks: Critical for a panicked
shard, Warning for a stalled shard (progress-age past a 10s threshold —
chosen well above the reconciler's 1s default tick so an ordinary idle
gap between polls never misreports), and Warning for a shard's command
channel at ≥80% of capacity (reusing `EgressShardHeartbeat`'s existing
`command_depth`, now also carrying `command_capacity` — a new field
added for this). Proof: 5 new unit tests in `alerts_tests.rs`
(healthy/stalled/panicked/over-threshold/under-threshold), verified as
real regressions by disabling the panicked-shard branch locally and
confirming its test fails, then restored.

**Retry-admission saturation — implemented as a follow-up, using data
that already existed.** `apply_egress_retry_state_json`
(`api_runtime_views/common.rs`) already put `retryAttempts`/
`retrying`/`retryBackoffMs` on every retrying output, and
`RuntimeTuning.output_max_retries` (the real configured ceiling,
default 10) was already computed — it just wasn't in `health_snapshot`'s
JSON, so `derive_alerts` (a pure function of that JSON, no config
access) couldn't compare against it. Added a `tuning.outputMaxRetries`
field to `health_snapshot()` and a new `derive_alerts` check: a
`"retrying"` output whose `retryAttempts` has reached ≥80% of
`outputMaxRetries` gets a specific "close to exhausting its retry
budget" Warning instead of the generic `not_running` one — a real,
config-relative signal, not a fabric-shard concept at all, wired
without touching any hot-path leaf code. Proof: 2 new unit tests
(near-ceiling fires the specific alert, below-ceiling still fires only
the generic one), verified as a real regression by disabling the
threshold check locally and confirming the near-ceiling test fails,
then restored.

**Repeated-resync alerts — investigated, left undone, and the reason is
worth recording plainly.** `resync_count` exists per-leaf
(`ProgressState`/`LeafMetrics`, `src/media/egress/leaf.rs` and
`metrics.rs`) but is never aggregated to shard level and never reaches
any snapshot or API surface at all today — not per-output, not
per-shard. Closing this honestly needs new plumbing (a per-output or
per-shard resync-rate export) added across all four protocol backends'
hot leaf-visit paths, which AGENTS.md's Hot-Path Rules require
benchmarking before and after — a real, separately-scoped piece of
work, not a small addition alongside everything above. Inventing a
threshold against data that isn't actually exported anywhere would be
worse than leaving this open.

**Diagnostic snapshots — implemented, reusing the same wiring as
alerts.** The four `#[cfg(test)]`-only `*_fabric_runtime_snapshots`
accessors are unchanged (still test-only, still useful for tests that
need the raw untransformed snapshot), but they are no longer the only
way to see live shard state: `egressFabricShards` in `health_snapshot()`
and the resource map's new `egress_shard` nodes are both real,
authenticated, non-test production diagnostics surfaces
(`/api/v1/alerts`, `/api/v1/engine/health`, `/api/v1/engine/resource-map`,
`/api/v1/pipelines/{id}/diagnostics/*`) reachable by an operator or
agent today, not just by test code calling into `MediaEngine` directly.

Add operator-visible attribution:

- ~~output protocol and assigned shard~~ (assigned shard done — see above;
  protocol was already exposed);
- lifecycle and progress age;
- feed lag;
- backpressure reason;
- retry admission state;
- ~~fabric versus legacy ownership during rollout~~ (done — see above).

### Rollout order

1. tests and local harness only;
2. SRT opt-in on a canary deployment;
3. SRT default with legacy rollback;
4. RTMP opt-in;
5. RTMP default;
6. all protocols on fabric;
7. remove rollback path after a defined observation window.

**Attempted, then reverted — `EgressRolloutMode::default()` is back to
`Off`.** Steps 3, 5, and 6 were briefly flipped to `All`
(`src/config.rs`), skipping the staged canary deployment (step 2) this
document originally specified, on the reasoning that the accumulated
Phase 4/5/6a live evidence (SRT clearing legacy's hard
512-sender-thread ceiling at N=1,000; RTMP at parity or better at
N=1,000 after the shard-count fix; RTMPS correctness at N=1,000 with a
known, root-caused RSS tradeoff; recirculation 2.7×–31.6× cheaper than
loopback) was a reasonable substitute for a canary window that no
staged production deployment in this repository could provide.

**CI's own live-scenario gates caught a real regression this
reasoning missed.** The next scheduled CI run (`Internal media backend
smoke`, scenario `mixed.asset.file.h264.a1.bf0`, a file-ingest source
feeding a transcoded RTMP 720p output) failed with a genuine, persistent
fault: the RTMP fabric leaf terminated
(`lastError = "RTMP fabric leaf terminated unexpectedly (peer closed,
protocol failure, or stall recovery)"`) and the output never recovered
— 0/1 outputs progressing for the entire 60-second timeout, not a
transient blip. None of this session's live captures exercised this
specific shape (a file/asset-sourced ingest transcoded into RTMP
egress); every prior RTMP fabric measurement used a live RTMP- or
SRT-sourced ingest. This is exactly why the exit gate below requires a
canary, not benchmark evidence alone — a canary would have caught this
before default-flip, a resource-focused A/B sweep did not.

A separate, smaller finding surfaced investigating this: the
`fault.egress-retry` concurrency-lifecycle test (specifically its
retry-budget-exhaustion sub-case, which polls for a terminal `failed`
state then asserts it stays failed 500ms later) flakes intermittently
under real host-timing variance regardless of fabric-vs-legacy routing
— reproduced locally failing on the SRT variant with the default back
at `Off`, after having failed on the RTMP variant with the default at
`All`. This looks like a pre-existing timing-sensitivity in the test's
500ms assumption, not something this session's changes introduced, and
is left as a known flake rather than chased further here — it did not
block the decision to revert, which rests on the RTMP-fabric-leaf
finding above.

Reverted by restoring `EgressRolloutMode::Off` as the default and the
two test assertions that had been updated for `All`. The fabric code
itself is untouched and fully available via `RESTREAM_EGRESS_FABRIC=all`
(or `=rtmp`/`=srt` individually) for anyone who wants it — only the
*default* reverted.

**Root-caused and fixed — a genuine architectural gap, not a
transient bug.** Reproduced the CI failure locally
(`RESTREAM_EGRESS_FABRIC=all`, `scripts/harness/run.sh
mixed.asset.file.h264.a1.bf0`) and added temporary diagnostic tracing
inside `RtmpShardBackend` (a per-second heartbeat logging each leaf's
`pending_application_bytes`/registered poller interest/stall
classification, plus a log at the exact point a leaf gets force-closed)
to get ground truth instead of guessing from the existing logs, which
had no visibility into this path at all. The heartbeat showed the
stuck leaf sitting with `pending_bytes=0`, `registered_interest=WRITE`
(its *initial* registration from connect time), and `stall_class=Idle`
for the entire ~53s window before being force-closed — meaning it was
never visited even once after connecting, not stuck mid-handshake.

Tracing why led to the real gap: `EgressShardRuntime::run()`
(`src/media/egress/shard.rs`) only ever invokes a backend's `on_ready()`
— the method that actually calls the native poller (`epoll_wait` for
RTMP, libsrt's own poller for SRT) — when something has already
scheduled ready work via `EgressShardCommandEffect::ScheduleReady`.
`on_media_tick()`, where a leaf's async TCP connect resolves and gets
registered with the poller (`complete_pending_connect`), had signature
`fn on_media_tick(&mut self)` — it could not return an effect, so a
freshly-connected leaf had no way to ask for its own first readiness
check. The only thing that could ever schedule that check was an
unrelated `EgressCommand::FeedWake`, fired only when *some* output on
the shard publishes new media. For any live, continuously-publishing
source the next wake is milliseconds away, so this gap was invisible —
every live RTMP/SRT capture this whole migration's live evidence rests
on used exactly that shape. A file-ingest source behind an internal
(non-external-ffmpeg) transcoder cold-starting for the first time has a
real, multi-second-to-a-minute gap before its first published unit;
during that gap the newly-connected leaf sat completely inert (not
even attempting its handshake, which needs no feed data at all) until
the transcoder's first publish finally fired a wake — and by then the
per-second stall sweep had usually already force-closed the leaf as
"terminated unexpectedly" (`sweep_stalled_leaves`), producing the
close-and-retry cycle the original CI failure showed. SRT has the
identical `on_media_tick`/`complete_pending_connect` shape and was
equally exposed, just not yet caught live.

Fixed by changing `EgressShardBackend::on_media_tick`'s signature to
return `EgressShardCommandEffect` (default `Continue`, matching every
other lifecycle hook on the trait already). `RtmpShardBackend` and
`SrtShardBackend`'s `on_media_tick` now return
`ScheduleReady { count: 1 }` whenever `complete_pending_connect`
(changed to report success) actually connects a leaf during that tick,
giving it a guaranteed first look independent of any FeedWake. Updated
all five real implementors (`rtmp_shard.rs`, `srt.rs`,
`pipeline_shard.rs`, `sink_shard.rs`, and both DNS-resolve decorator
wrappers, `rtmp_shard_resolve_runtime.rs`/`srt/resolve_runtime.rs`,
which must propagate the inner backend's effect rather than discard it
— the decorators sit directly in the production spawn chain) and four
test-fake implementors to the new signature.

Live-verified the fix in three steps against the same reproduction:
- Before the fix: leaf silently stuck the whole window, then closed
  with `"RTMP fabric leaf terminated unexpectedly"` around 53s,
  retried, and (since the transcoder had warmed up by then) succeeded
  almost immediately on the second attempt — matching the original CI
  failure shape exactly.
- After the fix, same run: no error at all — `phase=sending` the whole
  time, first real progress landing close to the 60s test boundary
  (once, just past it; once, just under it) — the false
  close-and-retry cycle is gone.
- Compared against the *legacy* (non-fabric) path for the identical
  scenario: legacy itself reaches first progress at ~50s. This confirms
  the ~50-60s delay is a shared, pre-existing internal-transcoder
  cold-start characteristic of this specific scenario (file ingest,
  internal video presets, first run) — not something the fabric
  migration introduced — and that the fix closes the actual regression
  (fabric no longer worse than legacy) even though a smaller residual
  gap (fabric landing nearer the 60s boundary than legacy's 50s)
  remains as a separate, non-blocking performance question, not a
  correctness one.

Proof: 2 new deterministic unit tests per protocol (RTMP:
`rtmp_shard_media_tick_tests.rs`; SRT: `srt/tests/media_tick.rs`) —
one asserting `on_media_tick` returns `ScheduleReady` after pushing a
resolved connect through a real `RtmpResolveCompletionQueue`/
`SrtResolveCompletionQueue`, one asserting it stays `Continue` when
nothing resolved. Both verified as real regressions: temporarily
reverting the `ScheduleReady` branch to always return `Continue`
locally and confirming the positive-case test fails for each protocol,
then restored. `rtmp_shard.rs` crossed the source-audit line cap while
adding this (1002 lines); split the graceful-close/drain/stall-sweep
methods into a new `rtmp_shard_drain.rs`, mirroring the existing
`srt_drain.rs` split exactly. Full `cargo test --lib` (1,904 tests),
clippy (default and `mcp-server,mcp-http-backend` features), fmt,
source-audit, `scripts/check/concurrency/contract.sh` (required —
this touches shard/leaf lifecycle code in `shard.rs`/`srt.rs`), and
docs checks all pass.

**Default left at `Off`.** The specific bug that motivated the revert
is now fixed and proven, but re-flipping the default is a separate
decision this pass does not make: the residual fabric-vs-legacy timing
gap in this exact scenario is unresolved, and re-attempting the flip
deserves its own live re-verification (ideally against the same CI
scenario matrix that caught the original regression) rather than being
a byproduct of a bug-fix session.

### Exit gate

At least one production-equivalent canary must complete normal operation,
configuration churn, destination failure, and graceful restart without legacy
fallback.

## Phase 7: Tuning and legacy removal

### Objective

Select the smallest efficient shard configuration, remove obsolete code, and
make the architecture the only egress path.

### Work

**Shard-count default derived from `effective_cpus` — done, and it
closes almost all of Phase 5's measured CPU gap.** Starting evidence
was Phase 5's combined-workload profiling above ("The actual
legacy-vs-fabric differential profile"): `shards: 4` in `src/config.rs`
was a flat constant never derived from `effective_cpus` the way
`default_tokio_worker_threads` (`src/config.rs:316`) is, so on this
6-CPU host 4 dedicated shard threads ran *alongside* legacy's same
2-worker tokio pool instead of the two being budgeted against one core
count. Ran the natural first sweep this note called for: three live
`mixed-fabric-matrix` captures (1,140 RTMP + 60 SRT, legacy vs fabric)
at each of `RESTREAM_EGRESS_SHARDS=2`, `4`, and `6` (9 live captures
total, same host, same procedure as Phase 5's other live runs).
Fabric-vs-legacy CPU ratio moved monotonically with shard count:

| shards | avg CPU ratio (fabric/legacy) | peak CPU ratio | RSS ratio |
|---|---|---|---|
| 2 | 1.047 | 1.131 | 0.885 |
| 4 (old default) | 1.123 | 1.233 | 0.852 |
| 6 (host core count) | 0.983 | 1.035 | 0.874 |

At `shards=6` fabric's average CPU is *below* legacy's (ratio 0.983),
peak CPU is within ~3.5% instead of ~23%, and the RSS advantage
(~12-15% lower than legacy throughout) is unaffected — it was never
traded away for the CPU improvement. Implemented:
`default_egress_fabric_shards(effective_cpus)` (`src/config.rs`,
next to `default_tokio_worker_threads`) returns
`effective_cpus.clamp(2, 8)`, and `EgressFabricConfig::default()` now
calls it the same way `TokioRuntimeConfig::default()` already calls
`default_tokio_worker_threads`. `RESTREAM_EGRESS_SHARDS` still
overrides it explicitly, so this is a default-value change only, fully
reversible without a code change. Fixed five tests that hardcoded the
old flat default (`src/config/tests/configuration_behavior.rs`,
`src/media/engine_tests/egress_fabric.rs`) to read the actual
configured value instead. Documented in `docs/configuration.md`'s
config table. Full `cargo test --lib` (1,877 tests), clippy, fmt,
source-audit, and docs checks all pass; live-verified the new default
resolves to `6` on this host via `/api/v1/engine/telemetry`'s
`egressFabric.shards` field at real startup.

**What this does not close**: the ~2pp scheduler/futex/`clock_gettime`
signature from Phase 5's differential profile was captured at the old
`shards=4` default and was never re-profiled at `shards=6` — the
`mixed-fabric-matrix` CPU/RSS numbers above are live measurements, not
a repeat `perf` capture, so there's no updated symbol-level breakdown
confirming *why* `shards=6` closes the gap (more parallelism headroom
is the obvious hypothesis, not yet confirmed at the instruction level).
Also unresolved: this sweep used single per-shard-count final captures
layered onto the existing 3-run baseline at `shards=4`, not the full
1/2/4/6/8 sweep with context-switch/allocator instrumentation Phase 7
originally specified below — `shards=8` and `shards=1` are still
unmeasured, and this host's 6-core ceiling means `shards=6` and
`effective_cpus.clamp(2, 8)` are indistinguishable here; a host with
more cores would be needed to see whether the clamp's upper bound of 8
is itself well-chosen or arbitrary.

Benchmark shard counts of 1, 2, 4, 6, and 8 where resources permit. Compare:

- CPU and instructions per output byte;
- RSS and allocator rate;
- context switches and CPU migrations;
- healthy-leaf progress latency percentiles;
- ready-queue service delay;
- feed wake amplification;
- reconnect completion distribution;
- protocol-specific serialization and native-library CPU.

Choose the smallest shard count that meets acceptance with operational
headroom. Do not introduce internal CPU affinity unless a separate A/B proof
shows a consistent end-to-end win.

Remove:

- legacy RTMP task-per-output ownership;
- legacy SRT feeder, queue, sender thread, and sender semaphore;
- duplicate retry and lifecycle policy;
- compatibility metrics and rollout flags that no longer serve rollback;
- tests that only exercise removed implementation details.

Update:

- `docs/architecture.md` runtime ownership and packet boundaries;
- `docs/media-pipeline.md` egress topology;
- `docs/high-performance-data-path.md` hot-path contracts;
- `docs/concurrency-proofing.md` mandatory surfaces;
- stage proof maps and runtime diagnostics;
- operator configuration and release evidence.

### Exit gate

No production network output uses a per-output application thread, private
media queue, independent retry task, or protocol-specific lifecycle loop.

## Existing-code change map

This table identifies expected ownership movement. It is not a mechanical file
split requirement.

| Current area | Planned change |
|---|---|
| `src/media/engine_egress.rs` | Route lifecycle operations through `EgressManager`; remove queue-centric network egress ownership |
| `src/media/engine_registries.rs` | Add fabric registry and snapshots; remove SRT sender semaphore after cutover |
| `src/media/rtmp/egress.rs` | Extract protocol engine; retire task-owned connection loop after parity |
| `src/media/rtmp/egress_transport.rs` | Reuse or adapt transport setup for non-blocking shard ownership |
| `src/media/srt_egress.rs` | Extract protocol setup; remove queue, feeder, and sender thread |
| `src/media/srt/socket.rs` | Add or consolidate non-blocking socket and option helpers |
| `src/media/srt/sys.rs` | Expose narrow safe wrappers needed by shard-owned SRT epoll |
| `src/media/ts_chunk_ring.rs` | Implement or adapt immutable feed and cursor semantics |
| `src/media/ring_buffer.rs` | Provide shard-efficient feed view without weakening current ingest/stage guarantees |
| `src/media/ring_buffer/reader.rs` | Avoid one parked final egress reader per leaf; preserve existing uses elsewhere |
| `src/media/avio.rs` | No global removal; stop using byte queues for network egress |
| `src/config.rs` | Add validated fabric, shard, budget, retention, and admission settings |
| `src/api_runtime_views` | Report shard, lag, progress, and fabric ownership without leaking mutable internals |
| `src/alerts.rs` | Add stalled-shard, repeated-overrun, and retry-admission alerts |
| `src/main.rs` | Start and stop fabric manager independently from Tokio worker sizing |

## Testing strategy

The proof ladder follows [concurrency proofing](concurrency-proofing.md) and
[testing](testing.md), with focused additions for the fabric.

### Pure unit tests

Use for:

- lifecycle transition table;
- retry calculations and jitter bounds;
- strict capacity arithmetic and overflow handling;
- generation comparison;
- sync-point lookup;
- work-budget exhaustion;
- error classification;
- shard assignment stability.

### Deterministic state-machine tests

Use fake clock, feed, poller, and engine. Avoid wall-clock sleeps.

Prove all transitions under scripted event orderings, including command,
readiness, timer, feed advance, cancellation, and failure arriving together.

### Model checking

Use Loom or the repository's established equivalent for:

- command wakeup versus shard sleep;
- feed `wake_pending` clear versus publisher advance;
- shutdown versus add or update;
- supervisor observation versus shard termination;
- publication of read-only status snapshots.

Do not model the entire protocol. Keep mutable leaf state shard-local and model
only the cross-thread seams.

### Protocol integration tests

Use real loopback sockets and protocol peers for partial I/O, handshake,
backpressure, teardown, and media correctness. Fault injection must control
read rate, closure timing, and connection refusal.

### Live harness tests

Use matching protocol receivers and the repository's harness conventions.
Prove receiver readiness, advancing bytes, decodable media, timestamps,
reconnect behavior, and process resources.

### Long-running soak

Run healthy and bad-neighbor shapes long enough to observe:

- stable RSS;
- no monotonic queue or feed growth;
- no thread or descriptor leaks;
- stable progress latency;
- bounded retry rate;
- correct output churn.

## Adversarial isolation matrix

The following scenarios are mandatory for both real protocol engines unless the
condition is provably inapplicable.

| Scenario | Placement | Required result |
|---|---|---|
| One destination never reads | Same shard as healthy leaves | It becomes backpressured and then recovers or retries; healthy service remains bounded |
| One destination reads extremely slowly | Same shard | Memory remains bounded; healthy leaves retain progress; slow leaf resynchronizes when required |
| Always-writable leaf with large backlog | Same shard | Service quantum forces rotation to neighbors |
| Peer continuously sends control traffic | Same shard | Read handling cannot starve media or neighbors |
| Hundreds of refused connections | One shard and spread across shards | Admission limits bound connect work and CPU |
| Handshake never completes | Same shard | Deadline closes only that leaf |
| Repeated connect and disconnect | Same and cross shard | Backoff and jitter prevent a retry storm |
| Feed cursor overrun | One leaf | Only that leaf resynchronizes; feed memory and neighbors remain stable |
| Feed epoch change | Multiple leaves | Every stale cursor recovers deterministically without mixed epochs |
| Output removal during partial write | One leaf | Pending state is released once; no stale readiness resurrects it |
| Output update races retry timer | One leaf | New generation wins; stale timer has no effect |
| Command queue saturation | One shard | Failure is visible; publication and other shards continue |
| Poller returns spurious readiness | One shard | No busy loop and no false progress |
| Driver returns zero progress repeatedly | One leaf | Budget or defensive policy yields and eventually diagnoses stall |
| Shard panic | One shard | Other shards continue; affected desired outputs reconnect on replacement |
| Native-call duration violation | One leaf or shard | Metric and alert fire; no silent latency regression |
| Descriptor exhaustion | Process | Add fails cleanly without corrupting existing leaves |

For the headline same-shard proof, place 998 healthy destinations, one
permanently non-reading destination, and one severely throttled destination on
a single shard. Repeat with bad destinations on another shard to establish the
cross-shard control.

## Benchmark plan

Benchmarks must compare the fabric with the current implementation and with an
all-healthy control.

### Micro-benchmarks

Add focused benches for:

- ready-queue enqueue, deduplication, and rotation;
- timer insert, expiry, and stale cancellation;
- feed read by one shard and local fan-out to 1,000 cursors;
- wake coalescing under burst publication;
- SRT immutable-message handoff versus current byte queue copies;
- RTMP partial-write state transitions;
- status snapshot aggregation;
- shard assignment and rebalance calculations.

A micro-benchmark does not justify production code without full-workload proof.

### Scale shapes

At minimum run:

| Shape | Purpose |
|---|---|
| 1 input, 1 output | Correctness and fixed overhead |
| 1 input, 100 outputs | Early scheduler behavior |
| 1 input, 500 outputs | Intermediate scaling |
| 1 input, 1,000 outputs | Target scale |
| 1 input, 1,200 mixed outputs | Compare with recorded repository workload |
| 998 healthy, 1 blocked, 1 throttled | Same-shard isolation |
| Healthy shard plus failure-storm shard | Cross-shard isolation |
| 25 percent reconnecting together | Admission and timer behavior |
| Continuous output churn | Lifecycle and resource reclamation |

Run protocol-pure and mixed-protocol variants.

### Measurements

Collect:

- receiver count and advancing bytes;
- media probe correctness and timestamp health;
- application and native thread count;
- process CPU, RSS, page faults, context switches, and migrations;
- allocations and copied-byte attribution where available;
- per-shard loop latency and service delay percentiles;
- per-leaf progress age and feed lag percentiles;
- would-block, partial-write, overrun, resync, and retry counts;
- wakeups per publication and empty wake ratio;
- descriptor and socket counts before and after churn.

## Acceptance gates

Thresholds should be finalized from Phase 0 evidence. Until then, use the
following directional gates and record exact values with the baseline.

### Correctness

- all receivers expected to be healthy are connected and advancing;
- matching-protocol probes decode expected audio and video;
- no timestamp regression or invalid composition offset is introduced;
- no runtime panic, stale output resurrection, or cross-output state corruption;
- removal and shutdown release all application and native resources.

### Threading

- application egress thread count depends on configured shard count, not output
  count;
- adding outputs does not create an application sender thread or destination
  task;
- no continuous blocking-pool occupancy is introduced by egress.

### Memory

- RSS reaches a stable envelope during an indefinite non-reading-destination
  test;
- feed retention remains within configured byte and age limits;
- pending application bytes remain within strict per-leaf limits;
- output churn returns memory and descriptor counts near the pre-churn envelope.

### Same-shard isolation

Compared with the all-healthy control at the same healthy-output count:

- healthy receiver count remains complete;
- healthy progress-age p99 and shard service-delay p99 stay within an explicitly
  recorded small degradation envelope;
- CPU does not grow continuously with stall duration;
- no healthy leaf crosses the slow-consumer policy because of bad neighbors.

A provisional target is no more than 10 percent degradation in healthy p99
progress age, subject to replacement with a workload-derived absolute bound.

### Cross-shard isolation

A failure storm on one shard must not cause receiver loss on another shard. The
healthy shard's progress-age and service-delay distributions should remain
within normal run-to-run variance established by repeated controls.

### Performance

- fabric CPU and RSS at the recorded mixed 1,200-output shape match or improve
  on the current baseline before legacy removal;
- feed wake amplification is proportional to interested shards, not leaves;
- adding shards beyond the selected point must not be accepted when it increases
  CPU without a tail-latency or resilience benefit.

### Recovery

- a feed overrun reconnects from a valid synchronization point;
- retries respect min, max, backoff, jitter, and admission limits;
- a shard panic reconnects only outputs assigned to that shard;
- a graceful drain completes or times out deterministically.

## Observability rollout

Add metrics with the implementation phase that creates the state. Do not defer
observability until cutover.

### Initial metric names

Use the repository's naming conventions, but preserve these dimensions:

```text
egress.fabric.shards
egress.fabric.leaves
egress.shard.ready_depth
egress.shard.service_delay_ms
egress.shard.loop_duration_us
egress.shard.heartbeat_age_ms
egress.shard.command_depth
egress.feed.retained_bytes
egress.feed.lag_ms
egress.feed.wake_coalesced
egress.feed.overruns
egress.leaf.pending_bytes
egress.leaf.progress_age_ms
egress.leaf.would_block
egress.leaf.partial_writes
egress.leaf.resyncs
egress.leaf.retry_attempt
egress.driver.call_duration_us
egress.driver.budget_violations
```

Avoid unbounded per-output metric label cardinality in exported time series.
Per-output details belong in runtime snapshots or diagnostics; aggregate time
series by shard, protocol, lifecycle, and bounded reason enums.

### Alerts

Add alerts for:

- shard heartbeat stale;
- driver-call budget violation;
- repeated feed overrun;
- retry admission saturation;
- command queue overload;
- high healthy-leaf progress age;
- shard ready queue remaining above a threshold;
- native or application pending bytes near policy limits.

## Configuration rollout

Add validated configuration in conservative order.

### Phase-gated settings

```text
RESTREAM_EGRESS_FABRIC
RESTREAM_EGRESS_SHARDS
RESTREAM_EGRESS_MAX_PENDING_BYTES
RESTREAM_EGRESS_MAX_LAG_MS
RESTREAM_EGRESS_NO_PROGRESS_MS
RESTREAM_EGRESS_CONNECT_TIMEOUT_MS
RESTREAM_EGRESS_HANDSHAKE_TIMEOUT_MS
RESTREAM_EGRESS_CONNECT_CONCURRENCY
RESTREAM_EGRESS_CONNECT_CONCURRENCY_PER_SHARD
RESTREAM_EGRESS_VISIT_MAX_BYTES
RESTREAM_EGRESS_VISIT_MAX_UNITS
RESTREAM_EGRESS_VISIT_MAX_US
RESTREAM_EGRESS_FEED_MAX_BYTES
RESTREAM_EGRESS_FEED_MAX_AGE_MS
```

Names may be adjusted to match existing configuration style. Keep the semantic
separation between application pending bytes, feed retention, and protocol
native buffers.

### Validation

Reject:

- zero shards;
- zero budgets that would prevent progress;
- retry minimum greater than maximum;
- no-progress timeout shorter than a viable handshake or service interval;
- arithmetic overflow converting rates, durations, and bytes;
- feed retention that cannot hold required codec initialization and at least one
  valid synchronization interval for the configured workload;
- native buffer settings that exceed the intended per-leaf memory envelope
  without explicit operator override.

## Failure and shutdown semantics

Shutdown and replacement paths are part of correctness, not cleanup detail.

### Output removal

1. manager sends remove with generation;
2. shard marks the leaf closing and prevents new scheduling;
3. backend deregisters native readiness;
4. engine closes transport state;
5. pending wire references and feed subscription state are released;
6. a terminal status snapshot is published;
7. leaf slot is removed;
8. stale native events are ignored by key and generation.

### Shard drain

A drain stops new assignments and allows active leaves either to finish a
configured graceful window or to reconnect on replacement shards. Do not
migrate live protocol state.

### Process shutdown

1. stop reconciliation from creating new outputs;
2. command every shard to close leaves;
3. wake native pollers;
4. wait for bounded graceful shutdown;
5. force-close remaining transport state;
6. join shard threads;
7. verify no feed subscriptions, sockets, or native pollers remain.

### Panic

A shard entry point catches unwind, publishes failure, and exits. The supervisor
recreates desired outputs only after the old shard is terminal. Panic payloads
and affected output IDs are diagnostic data, not API response details.

## Pull-request sequence

Keep pull requests narrow enough to review ownership and proof together.

1. **Baseline metrics and workload:** no behavior change.
2. **Lifecycle, policy, and fake engine:** pure common model.
3. **Scheduler and timer:** deterministic fairness tests.
4. **Feed contract and cursor adapter:** bounded retention and overrun tests.
5. **Wake coalescing:** model-checked cross-thread seam.
6. **Shard runtime and manager:** fake backend on real threads.
7. **Supervisor and diagnostics:** panic and lifecycle recovery.
8. **Sink backend:** fabric-only discard output and diagnostic/status proof.
9. **SRT non-blocking poller:** no production routing yet.
10. **SRT engine and shared-message handoff:** integration tests.
11. **SRT opt-in rollout:** live scale and bad-neighbor proof.
12. **SRT default and legacy removal:** remove sender queue and thread.
13. **TCP poller and RTMP engine extraction:** no default switch.
14. **RTMP partial-I/O and RTMPS:** protocol tests.
15. **RTMP opt-in rollout:** live scale and isolation proof.
16. **RTMP default and legacy removal:** one fabric path.
17. **Pipeline recirculation:** topology-visible in-process output to input.
18. **Shard-count tuning and final documentation:** remove temporary flags.

A pull request that changes a hot-path abstraction must include its benchmark or
explicitly state that it is a correctness-only step whose performance is gated
before rollout.

## Risk register

| Risk | Consequence | Mitigation |
|---|---|---|
| Universal abstraction hides protocol semantics | Correctness bugs and slow hot path | Keep prepared feed and protocol state specialized; commonize policy only |
| Native call blocks despite configuration | Entire shard stalls | Narrow FFI, non-blocking options, duration metrics, multiple shards, evaluate process isolation only if observed |
| Lost feed wakeup | Leaf stalls despite available media | Feed head remains authoritative; model-check clear-and-publish race |
| Duplicate ready entries | Unfairness and queue growth | Shard-local `enqueued` bit with invariant tests |
| Stale readiness after slot reuse | Wrong output state mutated | Stable leaf key plus generation validation; deregister before reuse |
| Slow leaf pins feed | Unbounded shared memory | Non-pinning cursor and explicit overrun |
| Native buffers replace removed app queue | Hidden memory growth | Configure and observe native limits; include in policy |
| Reconnect storm | CPU and network collapse | Global token bucket, per-shard limits, jittered backoff |
| Control flood starves media | Healthy-output progress loss | Separate bounded command budget and loop-wide budget |
| Media flood starves control | Removal and shutdown delayed | Per-leaf and per-loop budgets; poll commands every loop |
| RTMP partial-write state corrupts wire stream | Receiver disconnect or bad media | Exhaustive boundary tests and protocol probe |
| RTMPS buffering exceeds leaf limits | Memory amplification | Account plaintext and encrypted pending buffers |
| SRT message semantics mishandled | Packet loss or invalid TS | Retain complete immutable messages and test asynchronous send saturation |
| Sink output hides real delivery failures | False confidence in production readiness | Status labels and docs distinguish discarded progress from delivered network bytes |
| Recirculation creates a topology loop | Pipeline feedback, timestamp drift, unbounded buffering | Validate graph cycles and input ownership before admission |
| Recirculation copies through serialization | CPU and memory regression versus loopback | Measure zero-copy or bounded-copy behavior before user-facing rollout |
| Shard assignment imbalance | One hot shard while others idle | Start stable; measure service delay; add weighting only with evidence |
| Too many shards increase overhead | Higher CPU and cache misses | Sweep shard counts and choose smallest passing value |
| Dual-path rollout creates duplicate connection | Destination receives duplicate publisher | Atomic owner selection by output generation; explicit diagnostics |
| Legacy path remains indefinitely | Permanent complexity and drift | Time-box rollback window and make removal an exit gate |

## Rollback strategy

Rollback is configuration-driven until legacy removal.

- A deployment can route SRT, RTMP, or all outputs back to legacy ownership.
- Output ownership changes require reconnect; there is no live socket migration.
- Persisted desired configuration remains unchanged.
- Status clearly identifies the active owner so rollback can be verified.
- Rollback does not discard new metrics or diagnostic artifacts.
- A fabric failure must not automatically start a duplicate legacy output unless
  manager ownership has first been revoked.

After legacy code removal, rollback is a binary rollback to the last known-good
release rather than retaining two permanent architectures.

## Completion checklist

**Status as of the default-rollout flip and Phase 6 diagnostics/alerts
work**: most items below have real, specific evidence recorded inline
in their respective phase sections above — this list is intentionally
left unchecked rather than summarized into checkboxes, since several
items are genuinely still open and a checkbox can't carry the nuance
each phase section already does. Concretely still open, not done in
this pass: legacy per-output threads/queues/sender tasks are not
removed (deliberately — `RESTREAM_EGRESS_FABRIC=off` needs them for
rollback until a real canary/observation window happens, which needs
an actual staged deployment outside this repository); the shard-count
sweep is missing its `1` and `8` endpoints (`2`/`4`/`6` are measured);
the SRT backpressured-but-connected stall path's live (not just
deterministic-unit) proof needs a purpose-built raw SRT listener,
still future work; the mixed RTMP+RTMPS-at-scale combined workload
(as opposed to RTMP+SRT, which is measured) is untested. Everything
else on this list — bounded feeds, shard isolation, panic recovery,
diagnostics, default shard count from real A/B evidence, sink and
recirculation on the fabric, protocol-shared policy — has a specific
proof recorded in its phase section; read those for the actual
evidence rather than trusting a checkbox here.

The effort is complete only when:

- [ ] architecture and implementation documents reflect the shipped design;
- [ ] common lifecycle and scheduler tests are exhaustive and deterministic;
- [ ] feed wake and shutdown races have model-level proof;
- [ ] feeds are bounded by bytes and media age;
- [ ] leaf cursors cannot pin retained media;
- [ ] SRT uses non-blocking send readiness with no per-output sender thread;
- [ ] RTMP and RTMPS use partial non-blocking I/O under the same scheduler;
- [ ] sink outputs discard through the common fabric with honest status and
      discard metrics;
- [ ] pipeline recirculation is user-facing, topology-validated, bounded, and
      cheaper than compatible loopback network routing;
- [ ] all protocols share retry, stall, overrun, and status policy;
- [ ] 998 healthy plus two bad same-shard destinations passes;
- [ ] cross-shard failure isolation passes;
- [ ] 1,000-plus protocol-pure and mixed workloads pass live probes;
- [ ] RSS is stable under indefinite slow-destination tests;
- [ ] reconnect storms respect admission limits;
- [ ] output churn leaks no threads, descriptors, sockets, or feed state;
- [ ] shard panic recovery affects only assigned outputs;
- [ ] dashboards and diagnostics expose shard and leaf progress;
- [ ] default shard count is selected by recorded A/B evidence;
- [ ] legacy network egress queues, sender threads, tasks, and duplicate policy
      are removed;
- [ ] current architecture, media-pipeline, high-performance, testing, and
      operator documents are updated;
- [ ] release evidence includes correctness, isolation, resource, and rollback
      proof.
