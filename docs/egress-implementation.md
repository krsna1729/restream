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
- [Phase 4: SRT migration](#phase-4-srt-migration)
- [Phase 5: RTMP and RTMPS migration](#phase-5-rtmp-and-rtmps-migration)
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

### Native buffer accounting

Define and validate SRT native sender-buffer ceilings. A leaf is considered
backpressured or stalled based on both application pending state and native SRT
progress. An implementation that removes `MemoryQueue` but permits unlimited
libsrt buffering does not satisfy the architecture.

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

### RTMPS

Drive TLS incrementally:

- handshake, reads, and writes obey the same work budget;
- `wants_read` and `wants_write` map to common readiness interests;
- plaintext and encrypted pending bytes count toward leaf limits;
- TLS close and error paths map to common lifecycle reasons;
- no convenience API may spin internally without a bounded exit.

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

### Exit gate

Fabric RTMP and RTMPS become default only after media correctness, tail progress,
CPU, RSS, context switches, and allocator behavior match or improve on legacy.

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
8. **SRT non-blocking poller:** no production routing yet.
9. **SRT engine and shared-message handoff:** integration tests.
10. **SRT opt-in rollout:** live scale and bad-neighbor proof.
11. **SRT default and legacy removal:** remove sender queue and thread.
12. **TCP poller and RTMP engine extraction:** no default switch.
13. **RTMP partial-I/O and RTMPS:** protocol tests.
14. **RTMP opt-in rollout:** live scale and isolation proof.
15. **RTMP default and legacy removal:** one fabric path.
16. **Shard-count tuning and final documentation:** remove temporary flags.

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
