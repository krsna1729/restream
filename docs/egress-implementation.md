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
- First live fabric proof at host scale (wsl-6cpu-12gb, N=100 healthy SRT
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
  **Re-recorded `w2-fabric-confirmed` capture (wsl-6cpu-12gb, N=100) shows
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
- Bad-neighbor evidence with the SRT rollout active (`w4-fabric` capture):
  fault.output-stall passed with a permanently stalled sink isolated beside
  32 healthy siblings while SRT outputs ran fabric-owned. This is
  mixed-ownership isolation; a pure-fabric stalled-SRT-destination live
  variant remains listed under live tests.

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
`test/harness/baselines/rtmp-fabric-matrix/wsl-6cpu-12gb/capture.json`
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
- comparison with the recorded 1,140 RTMP plus 60 SRT workload.

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

Integrate:

- desired output reconciliation;
- status and failure reasons;
- runtime resource map;
- alerts for stalled shards, repeated resync, command overload, and retry
  admission saturation;
- diagnostic snapshots;
- configuration validation;
- graceful shutdown and shard draining.

Add operator-visible attribution:

- output protocol and assigned shard;
- lifecycle and progress age;
- feed lag;
- backpressure reason;
- retry admission state;
- fabric versus legacy ownership during rollout.

### Rollout order

1. tests and local harness only;
2. SRT opt-in on a canary deployment;
3. SRT default with legacy rollback;
4. RTMP opt-in;
5. RTMP default;
6. all protocols on fabric;
7. remove rollback path after a defined observation window.

### Exit gate

At least one production-equivalent canary must complete normal operation,
configuration churn, destination failure, and graceful restart without legacy
fallback.

## Phase 7: Tuning and legacy removal

### Objective

Select the smallest efficient shard configuration, remove obsolete code, and
make the architecture the only egress path.

### Work

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
