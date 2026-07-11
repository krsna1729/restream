# Restream Architecture: Target State for the Whole Codebase

## Purpose

This document defines the target architecture for the entire `restream` codebase, not just the internal/external FFmpeg subsystem. The goal is to make the system easy to reason about, easy to operate under failure, and pleasant to evolve without adding more one-off paths.

The target architecture is built around one core idea:

> Restream is a desired-state media graph system. APIs and persistence describe desired state; the runtime plans and admits graph stages; protocol adapters attach to terminal stages; health and alerts explain the dependency chain when anything fails.

The current codebase already contains many of the right pieces: typed `StageKind`, `OutputConfig`, `OutputPath`, `BackendPolicy`, runtime view models, rich harnesses, ingest/output/reconcile services, app logs, alerts, and optional agent tooling. The issue is that these pieces are not consistently layered or joined by a single lifecycle contract. This document describes the shape to converge on.

## Non-goals

This is not a rewrite proposal. The design must be reachable through incremental refactors while preserving existing API behavior and the bench harness. The first milestone is improved correctness and surfacing; aesthetic cleanup follows the same boundaries.

This document does not choose internal FFmpeg over external FFmpeg. The target is backend symmetry: external and internal execution paths must share stage planning, input startup policy, output timestamp normalization, lifecycle, metrics, and status. Only the execution adapter differs.

## Current codebase map

The codebase is currently organized around these major areas:

| Area | Representative files | Current role |
|---|---|---|
| Entry/runtime wiring | `src/main.rs`, `src/lib.rs` | Runtime setup, DB setup, Axum, listeners, reconciler loop. |
| HTTP/API | `src/api.rs`, `src/api_view_models.rs`, `src/api_runtime_views/*` | Routes, handlers, DTO projection, health/graph/telemetry views. |
| Domain | `src/domain/*` | Output config, audio routing, stage vocabulary, ingest/security configs. |
| Application | `src/application/*` | Use cases: ingest, egress planning, reconciliation, recording settings, settings. |
| Planner | `src/planner/backend_policy.rs` | Backend choice between audio router, internal FFmpeg, external FFmpeg. |
| Media runtime | `src/media/*` | RTMP/SRT, ring buffers, stage registry, FFmpeg paths, HLS, recording, file ingest. |
| Persistence | `src/db.rs`, `src/application/ports.rs`, `src/types.rs` | SQLite schema and storage traits/row DTOs. |
| Observability | `src/logging.rs`, `src/logging/*`, `src/alerts.rs`, `src/diag.rs` | Logs, health snapshots, alerts, diagnostic context. |
| Agent/MCP | `src/agent_*`, `src/bin/restream-mcp.rs` | Optional read/planning/execution plane. |
| Test harness | `src/bin/test_harness.rs`, `src/bin/test_harness/*` | Bench/correctness/fault/matrix harnesses. |

Large files indicate where boundaries are currently too broad:

| File | Architectural concern |
|---|---|
| `src/bin/test_harness.rs` | Shared harness utilities, sinks, runtime helpers, scenario logic, and reporting are mixed. |
| `src/api.rs` | Routing, validation, application use cases, media/HLS internals, file serving, and agent endpoints are in one module. |
| `src/media/engine.rs` | Stage registry, ingest registry, egress registry, HLS lifecycle, recording lifecycle, status snapshots, and tests are mixed. |
| `src/media/rtmp.rs`, `src/media/srt.rs` | Protocol server, protocol egress, codec helpers, startup behavior, metrics, and tests are mixed. |
| `src/media/external_transcoder.rs`, `src/media/transcoder.rs` | Planning, input feeding, output demux/normalization, backend execution, lifecycle, and tests are coupled. |

## Architectural diagnosis

The system’s main weakness is not one bug. It is that important runtime concepts are implicit:

1. **Desired state and runtime state are not clearly separated.** DB rows say outputs should run; runtime maps say an egress exists; stage buffers say a stage exists; but there is no first-class explanation of whether the dependency graph is admitted, running, blocked, or failed.

2. **A stage is represented as a ring buffer plus a cancellation token, not as a lifecycle object.** `StageStarted` can mean “registered,” not “backend running,” “first input seen,” or “first output emitted.”

3. **Capacity is not a visible runtime state.** External FFmpeg stages can wait on a semaphore while downstream egresses see only zero-byte stalls.

4. **API and media internals cross layers.** The API creates some HLS preview media stages directly. The media layer sometimes imports application/API concerns. This makes one-off paths easy.

5. **Protocol behavior is similar but not symmetrical.** RTMP and SRT wait for upstream readiness in different phases. HLS output and HLS preview use different engines and stage keys.

6. **Status does not answer the operator’s question.** A stalled output report should explain the blocked stage and reason; today it often reports an opaque output ID, `phase=starting/resolving`, zero bytes, and empty error.

7. **String encodings act as runtime behavior.** Values like `video:1080p`, `720p`, `h264`, `hevc_to_h264`, and `source+atrack:0` are parsed in multiple places for behavior. Stage identity, codec, profile, and operation should be typed separately.

8. **Tests are ambitious but not yet causality-rich.** The harness covers many cells, but failure messages do not always include output name, URL, stage chain, or blocked dependency.

## Target layering

The target architecture is a layered, port-and-adapter style system.

```text
┌──────────────────────────────────────────────────────────────────────┐
│ HTTP/UI/MCP/CLI adapters                                             │
│ - Axum routes, static UI, MCP tools, bench harness client             │
└─────────────────────────────┬────────────────────────────────────────┘
                              │ request/response DTOs
┌─────────────────────────────▼────────────────────────────────────────┐
│ Application services                                                  │
│ - pipeline service, output service, ingest service, recording service │
│ - reconciler, graph planner facade, settings/security service         │
└─────────────────────────────┬────────────────────────────────────────┘
                              │ commands, queries, ports
┌─────────────────────────────▼────────────────────────────────────────┐
│ Domain model                                                          │
│ - PipelineId, OutputId, OutputConfig, AudioRoute, StageKind, Codec    │
│ - DesiredState, RuntimeState, error codes, policy types               │
└─────────────────────────────┬────────────────────────────────────────┘
                              │ stage graph plans
┌─────────────────────────────▼────────────────────────────────────────┐
│ Runtime graph                                                         │
│ - StageGraphPlan, StageRuntime, OutputRuntime, IngestRuntime          │
│ - lifecycle, admission, capacity, dependency status, metrics          │
└─────────────────────────────┬────────────────────────────────────────┘
                              │ backend/protocol ports
┌─────────────────────────────▼────────────────────────────────────────┐
│ Media/protocol adapters                                               │
│ - RTMP, SRT, HLS, recording, file ingest, FFmpeg internal/external    │
│ - RingBuffer, MemoryQueue, TS mux/demux, codec helpers                │
└──────────────────────────────────────────────────────────────────────┘
```

Dependency rule:

```text
Adapters -> Application -> Domain
Runtime graph -> Domain
Media adapters -> Runtime graph contracts + media primitives
Domain -> nothing above it
```

Allowed imports:

| From | May import |
|---|---|
| `domain` | Rust std, serde for domain DTOs when needed. |
| `application` | `domain`, `application::ports`, selected runtime service traits. |
| `runtime` / `media::engine` | `domain`, `media` primitives, runtime registries; not HTTP view models. |
| `api` | application services and API view models; not low-level media stage constructors. |
| `media::{rtmp,srt,hls,...}` | media primitives, runtime backend ports, domain protocol types; not DB or API view models. |
| `db` | domain/row DTOs and SQLx; no runtime media types. |
| `test_harness` | public API client, process helpers, scenario definitions. It can use runtime internals only behind explicit test-only helpers. |

## Core bounded contexts

### 1. Pipeline catalog

Owns persisted pipelines, stream keys, input source metadata, and SRT ingest policy. It must not own active connections or media buffers.

Target service:

```rust
PipelineService {
    create_pipeline(...)
    update_pipeline(...)
    delete_pipeline(...)
    list_pipelines(...)
    set_input_source(...)
}
```

Persistence port:

```rust
trait PipelineRepository {
    get_by_id(...)
    get_by_stream_key(...)
    list(...)
    create(...)
    update(...)
    delete(...)
}
```

### 2. Output catalog and output lifecycle

Owns desired output specs and start/stop intent. It does not directly spawn protocol workers.

Target domain types:

```rust
enum DesiredOutputState { Running, Stopped, Failed }
enum OutputHealthState { Healthy, Starting, WaitingUpstream, Retrying, Failed, Stopped }
struct OutputSpec { id, pipeline_id, name, url, monitoring_url, config }
```

Application service:

```rust
OutputService {
    create_output(...)
    update_output(...)
    delete_output(...)
    request_start(...)
    request_stop(...)
    get_status(...)
}
```

### 3. Ingest runtime

Owns live RTMP/SRT/file ingest registration, active ingest metadata, ring buffer source, recent disconnect state, and security policy enforcement.

Target split:

```text
application::ingest          persisted ingest commands and policy
media::ingest_runtime        active ingest registrations and source rings
media::rtmp_ingest           RTMP adapter
media::srt_ingest            SRT adapter
media::file_ingest_runtime   file source adapter
```

### 4. Stage graph runtime

This is the most important missing bounded context.

A stage is a first-class runtime object:

```rust
struct StageRuntime {
    key: StageKey,
    plan: StagePlan,
    backend: StageBackend,
    phase: StagePhase,
    input_stage: Option<StageKey>,
    output_ring: Arc<RingBuffer>,
    cancel: CancellationToken,
    metrics: StageMetrics,
    capacity: Option<CapacityClass>,
    backend_instance: Option<BackendInstanceInfo>,
    first_input_at: Option<Instant>,
    first_output_at: Option<Instant>,
    first_keyframe_at: Option<Instant>,
    last_error: Option<StageError>,
}
```

Canonical lifecycle:

```rust
enum StagePhase {
    Planned,
    Registered,
    WaitingForDependency,
    WaitingForMetadata,
    WaitingForParameterSets,
    WaitingForKeyframe,
    WaitingForCapacity { class: CapacityClass },
    StartingBackend,
    BackendSpawned,
    RunningNoOutputYet,
    Producing,
    Failed,
    Stopping,
    Stopped,
}
```

Every output status must include its terminal stage and blocked dependency chain.

### 5. Graph planner

The planner converts an output spec and input metadata into a stage graph.

Inputs:

```text
PipelineId
OutputConfig
OutputUrlScheme
Ingest codec/audio metadata
Preview/recording/output role
```

Output:

```rust
struct StageGraphPlan {
    pipeline_id: PipelineId,
    terminal_stage: StageKey,
    stages: Vec<StagePlan>,
    edges: Vec<StageEdge>,
}
```

A single graph planner should serve:

- RTMP output
- SRT output
- HLS output
- HLS preview
- recording
- diagnostics previews
- agent plan impact previews
- test-harness stage-sharing expectations

### 6. FFmpeg execution backend

Internal and external FFmpeg are execution adapters for the same `FfmpegStagePlan`.

Shared code:

- `FfmpegStagePlan`
- `StageInputPump`
- `StageOutputNormalizer`
- `StageTimeline`
- `StageLifecycle`
- `StageError`
- `StageMetrics`

Different code:

- `ExternalFfmpegBackend`: command args, child process, stdin/stdout, stderr tail.
- `InternalFfmpegBackend`: ffmpeg-next decoder/filter/encoder, in-process memory queue.

No backend should write directly to `RingBuffer`; all emitted packets go through the output normalizer.

### 7. Protocol egress adapters

RTMP/SRT/HLS egress workers attach to the terminal stage only through a common readiness API:

```rust
trait TerminalStageReader {
    async fn wait_ready(policy: ReadinessPolicy) -> Result<StageReady, StageBlocked>;
    fn reader(name: String) -> Reader;
}
```

RTMP and SRT should share the same upstream wait semantics:

```text
phase=waitingUpstream
blockedByStage=...
blockedReason=noPackets | noKeyframe | waitingForCapacity | waitingForMetadata | failed
```

Protocol-specific phases start after upstream readiness:

```text
RTMP: connecting -> sending
SRT: resolving -> connecting -> sending
HLS PUT: segmenting -> uploading
```

### 8. HLS preview and HLS output

Preview and output can use different packaging (fMP4 vs MPEG-TS), but they must share planning and stage lifecycle.

Target:

```text
HLS preview = graph consumer role
HLS output  = graph consumer role
```

No API route should construct `RingBuffer` or spawn an external transcoder for preview. API requests preview from an application/runtime service.

### 9. Recording and media library

Recording should be a consumer role in the same graph runtime. Media-library discovery and recording validation must use stable metadata, not filename heuristics.

Target persisted recording metadata:

```text
recording_id
pipeline_id
input_case/test_label optional
started_at
ended_at
final_path
temp_path
status: recording | finalizing | ready | failed
codec summary
```

The test-harness fix that rejects `.tmp.mp4` and matches scenario token is good, but the product architecture should not depend on filenames to identify recordings.

### 10. Observability and alerts

Observability should be causal, not just counters.

Health snapshot requirements:

- pipeline desired state
- active ingest state
- active output state
- terminal stage for each output
- blocked dependency chain
- stage lifecycle phase
- stage backend and capacity state
- bytes/packets in/out
- last error and stderr tail
- retry state
- ring lag and reader status
- relevant alerts and recommended action

Alert derivation should remain pure, but alert inputs should be richer.

### 11. Agent/MCP plane

The agent plane should sit above application services and runtime views. It should not call media internals. It can provide:

- read-only investigation
- plan generation
- impact preview
- validation
- optional controlled execution with approval/idempotency

Its contracts should reuse application DTOs and domain types, not mirror them ad hoc.

### 12. Test harness

The harness is a client and laboratory, not a runtime dependency. It should be organized as:

```text
test_harness/core        process, API client, artifact helpers
test_harness/scenarios   manifest and scenario DSL
test_harness/probes      ffprobe, signal, decode scan, HLS, recording
test_harness/sinks       RTMP/SRT/HTTP sinks
test_harness/reports     JSON, JSONL, summaries, failure mapping
test_harness/modes       mixed matrix, fault, resource, sweeps
```

Every failure should include:

```text
scenario
cell name
output id
output name
URL
encoding
protocol
terminal stage
blocked stage
stage phase
last stage error/stderr tail
```

## Domain model conventions

### IDs

Introduce typed IDs gradually:

```rust
struct PipelineId(String);
struct OutputId(String);
struct StageId(String);
struct IngestId(String);
struct JobId(String);
struct RecordingId(String);
```

Keep JSON strings at API boundary.

### State enums

Replace stringly internal states with enums:

```rust
enum DesiredState { Running, Stopped, Failed }
enum JobStatus { Running, Stopped, Failed }
enum EgressPhase { Starting, WaitingUpstream, Connecting, Sending, Segmenting, Uploading, Retrying, Failed, Stopped }
enum StagePhase { ... }
enum IngestPhase { Listening, Connected, Receiving, Disconnected, Failed }
```

Serialize with stable `camelCase` or `lowercase` names only in API view models.

### Error model

Use typed internal errors:

```rust
struct StageError {
    code: StageErrorCode,
    message: String,
    source: Option<anyhow::Error>,
    stderr_tail: Option<Vec<String>>,
    retryable: bool,
}
```

Public API errors should be problem-details-like:

```json
{
  "error": {
    "code": "stage.waitingForCapacity",
    "message": "Output is blocked by an upstream stage waiting for external FFmpeg capacity.",
    "pipelineId": "...",
    "outputId": "...",
    "stage": "...",
    "retryable": true
  }
}
```

Do not expose raw FFmpeg output as the top-level message; include it as evidence/tail.

## Runtime graph invariants

1. An output cannot be considered ready until its terminal stage is ready under the output’s protocol policy.
2. A stage cannot be called `started` unless its backend has actually started or the event name explicitly says `registered`.
3. Every stage wait has a named reason.
4. Every output can explain its dependency chain.
5. Every backend writes through the same output normalizer.
6. Every capacity limit is visible in health and telemetry.
7. Every long-lived worker has attempt identity and stale-worker protection.
8. Every cancellation wait is cancellation-aware.
9. No API route manually creates low-level media stages.
10. No status field used by the harness is absent from the API schema.

## API architecture

### Target route groups

Split `api.rs` into route modules:

```text
api/mod.rs
api/router.rs
api/auth.rs
api/pipelines.rs
api/outputs.rs
api/ingests.rs
api/file_ingest.rs
api/media_library.rs
api/hls.rs
api/health.rs
api/logs.rs
api/alerts.rs
api/telemetry.rs
api/settings.rs
api/agent.rs
api/static_assets.rs
```

Each module should contain handlers only. Business logic belongs in application services. Runtime graph manipulation belongs in runtime services.

### API response model

`api_view_models` should be the only module converting domain/runtime state to JSON. Media/runtime code must not import `api_view_models`.

Target split:

```text
api_view_models/status.rs
api_view_models/pipeline.rs
api_view_models/output.rs
api_view_models/stage.rs
api_view_models/ingest.rs
api_view_models/health.rs
api_view_models/media.rs
```

## Persistence architecture

`db.rs` should evolve from a large function collection to repository modules:

```text
db/mod.rs
db/schema.rs
db/pipeline_repo.rs
db/output_repo.rs
db/ingest_repo.rs
db/job_repo.rs
db/session_repo.rs
db/meta_repo.rs
db/log_repo.rs
db/migrations.rs
```

Application services depend on repository traits in `application::ports`, not on SQLx functions.

SQLite schema remains acceptable, but state stored as strings must be converted to typed enums at repository boundaries.

## Configuration architecture

Environment variables are currently read in many modules. Target:

```rust
struct AppConfig {
    ports: ServerPorts,
    runtime: RuntimeTuning,
    media: MediaConfig,
    security: SecurityConfig,
    logging: LoggingConfig,
    agent: AgentConfig,
    harness: Option<HarnessConfig>,
}
```

All env parsing should happen during startup or explicit test-harness setup. Runtime modules receive typed config.

Media config should include:

```rust
struct MediaConfig {
    external_ffmpeg: ExternalFfmpegConfig,
    internal_ffmpeg: InternalFfmpegConfig,
    ring: RingConfig,
    startup: StartupPolicyConfig,
    hls: HlsConfig,
    srt: SrtConfig,
}
```

## Security architecture

Keep authentication/session handling in API/application. Keep ingest security in domain/application/media boundary:

- Domain: policy representation and validation.
- Application: load/update policy.
- Media adapters: enforce policy.

Do not let media transports reach into session/auth concerns.

## Agent architecture

Agent modules are optional features. They should depend on stable application use cases and runtime read models:

```text
agent_core       pure request/response and validation types
agent_plane      read/plan/impact services using application APIs
agent_execution  mutation workflow with approval/idempotency
agent_mcp        transport/tool adapter
agent_backends   HTTP/in-process backend adapters
```

Avoid duplicating HTTP request structs in MCP. Prefer shared command/query DTOs in `application::commands` or `agent_core::types` when feature boundaries permit.

## Observability architecture

### Logs

Keep tracing as the source of app logs. Promote these fields consistently:

```text
correlation_id
pipeline_id
output_id
stage_key
ingest_id
job_id
event_class
event_type
phase
backend
error_code
```

### Metrics

Separate metrics types:

```text
PipelineMetrics
IngestMetrics
StageMetrics
OutputMetrics
BackendCapacityMetrics
RingMetrics
ProtocolQualityMetrics
```

### Events

Current `EventKind` is too small. Target event categories:

```rust
enum EventKind {
    IngestConnected,
    IngestDisconnected,
    StageRegistered,
    StageWaiting,
    StageBackendSpawned,
    StageFirstInput,
    StageFirstOutput,
    StageFailed,
    StageStopped,
    OutputStarted,
    OutputWaitingUpstream,
    OutputConnected,
    OutputFirstByte,
    OutputFailed,
    OutputStopped,
    RecordingStarted,
    RecordingFinalized,
}
```

### Health

Health snapshots should be compact but causal. Full diagnostics can be separate.

```json
{
  "outputs": {
    "output_...": {
      "status": "running",
      "phase": "waitingUpstream",
      "blockedBy": {
        "stage": "pipeline:video:1080p",
        "phase": "waitingForCapacity",
        "backend": "externalFfmpeg"
      }
    }
  }
}
```

## Test architecture

### Unit tests

Use pure tests for:

- output config parsing
- audio routing parsing
- stage key generation
- graph planning
- backend policy
- retry policy
- timeline normalization
- alert derivation

### Integration tests

Use small runtime tests for:

- stage lifecycle state transitions
- egress wait behavior
- HLS preview graph plan
- recording lifecycle
- file ingest loop timestamps

### Harness tests

Harness should remain the high-level confidence suite:

- mixed matrix
- fast breadth
- fault matrix
- resource sweep
- bitrate sweep
- SRT crypto matrix
- recovery and lifecycle scenarios

But the harness must consume dependency-aware status so failures are actionable.

## Module naming target

A future source tree can look like:

```text
src/
  main.rs
  lib.rs
  config/
  domain/
  application/
    commands/
    services/
    ports/
  db/
  api/
  runtime/
    graph/
    stage/
    output/
    ingest/
    recording/
    health/
  media/
    primitives/
    protocols/
      rtmp/
      srt/
      hls/
    ffmpeg/
      plan.rs
      input.rs
      output.rs
      timeline.rs
      external.rs
      internal.rs
    recording/
    file_ingest/
  observability/
    logging/
    events/
    metrics/
    alerts/
    diag/
  agent/
  bin/test_harness/
```

The existing tree can migrate toward this shape gradually. Do not block fixes on achieving this exact layout.

## Aesthetic standards

1. A module should have one reason to change.
2. String encodings are allowed at API/config boundaries only; internals use typed models.
3. Every special case should be named as a policy, not hidden in an `if` branch.
4. Every runtime wait must be cancellation-aware and observable.
5. Every test failure must identify the failed semantic cell, not only generated IDs.
6. Every duplicated live/file/protocol path should be eliminated unless the difference is a named policy.
7. Every backend must share the same contracts, not merely output the same bytes in happy paths.
8. Every public status field must be consumed by the harness at least once.

## Migration principle

Refactor around contracts first, files second.

Do not start by splitting large files randomly. First introduce small, typed contracts around:

- graph plan
- stage lifecycle
- stage backend
- output status
- config
- error model

Then move code behind those contracts into smaller files.

## Ideal end state

A user or operator should be able to ask:

> Why is this output not sending?

And the system should answer:

```text
Output rtmp.1080p.a0-2 is waiting on terminal stage
pipeline_x/hevc_to_h264:from:video:1080p.
That stage is waiting for external FFmpeg capacity for 42.1s.
There are 9/9 external FFmpeg permits in use.
Upstream video:1080p is producing 1920x1080 HEVC at 31 packets/s.
Recommended action: increase external FFmpeg capacity or enable internal HEVC→H264 codec edge.
```

That is the architecture standard.

---

# Addendum: Relevance-First Whole-Codebase Audit

This addendum is a stricter pass over the entire source tree, including the bench/test harness. The standard is simple: every layer must answer the next human question. A runtime status should explain causality. A test failure should point to the failed semantic cell and blocked dependency. A module boundary should make the right thing the easy thing and the ugly one-off hard to introduce.

## Source-wide heat map

The source tree contains several healthy domain concepts, but the largest files show where the architecture still relies on accumulation rather than crisp boundaries.

| File or area | Approx. size in uploaded source | Current smell | Target shape |
|---|---:|---|---|
| `src/bin/test_harness.rs` | 10,244 lines | Dispatch, process control, sinks, probes, sweeps, resource accounting, API helpers, and reporting share one global namespace. | Thin binary shell plus `test_harness/core`, `scenario`, `stack`, `probes`, `sinks`, `reports`, `modes`. |
| `src/api.rs` | 7,795 lines | Route handlers, validation, application logic, media/HLS internals, and file serving are coupled. | Route modules call application services and read models only. |
| `src/media/engine.rs` | 6,236 lines | Ingest, egress, stages, HLS, recording, diagnostics, and lifecycle maps are one engine object. | Runtime graph orchestrator plus separate registries/services for ingest, output, stage, HLS, recording. |
| `src/media/srt.rs` | 4,627 lines | Protocol server, egress, socket quality, startup policy, bonding, and tests live together. | SRT transport adapter split by ingest, egress, quality, URL/config, tests. |
| `src/media/rtmp.rs` | 3,600 lines | RTMP server, egress, FLV helpers, startup wait, and codec behavior mix. | RTMP transport adapter split by protocol session, ingest, egress, FLV codec helpers. |
| `src/media/external_transcoder.rs` | 2,984 lines | FFmpeg command construction, stage input feeding, child lifecycle, stdout demux, stderr, metrics, and backend policy details mix. | External backend adapter behind shared FFmpeg stage plan/input/output/lifecycle contracts. |
| `src/media/transcoder.rs` | 1,417 lines | Internal backend, audio routing, scaling, timestamp policy, and env rereads mix. | Internal backend adapter; shared planner/input pump/output normalizer used by both internal and external paths. |
| `src/bin/test_harness/mixed_runner.rs` | 2,389 lines | Scenario lifecycle, matrix scheduling, stack binding, output fanout, checks, and result assembly mix. | Matrix scheduler plus per-source scenario runners plus report aggregator. |

This does not mean every large file must be split first. It means every change should move behavior behind one of the target contracts instead of adding another branch inside these files.

## Codebase-level relevance standards

The codebase should obey these standards everywhere:

1. **Every runtime wait has a name.** `waitingForCapacity`, `waitingForKeyframe`, `waitingForMetadata`, `waitingForFirstOutput`, `waitingForProtocolConnect` are different states and must not collapse into `starting`.
2. **Every status has a dependency chain.** An output status is incomplete without terminal stage, blocked stage, stage phase, and last causal error.
3. **Every special case is a policy.** HLS preview, HEVC→H.264, file-loop timestamp handling, SRT listener crypto, and direct signal sinks should be named policies, not loose `if` branches.
4. **Every harness failure must be readable from `scenario.json`.** No DB spelunking or log correlation should be required for first triage.
5. **Every layer owns one kind of truth.** DB owns desired/persisted state, runtime owns live state, media adapters own protocol I/O, harness owns experiments and reports.
6. **Every generated identifier must map back to semantic identity.** `output_...` is never sufficient; the report needs `mixed.asset.file.h265.a2.bf0 / rtmp.1080p.a1 / duplicate=2`.
7. **Every internal/external backend difference must be behind the backend adapter.** Planning, input startup, timestamp normalization, lifecycle, metrics, and diagnostics are shared.
8. **Every public JSON schema consumed by the harness is versioned or schema-tested.** The `state=unknown` issue existed because harness and API status shape drifted.

## Whole-codebase target contracts

The ideal point is not only a media graph refactor. The whole codebase should converge on these contracts:

```text
ApplicationCommand       persisted intent, validation, authorization
ApplicationReadModel     stable HTTP/MCP/UI/harness read model
StageGraphPlan           desired runtime graph for outputs, HLS, recording, preview
StageRuntimeSnapshot     live stage state, capacity, metrics, causal errors
OutputRuntimeExplanation live output state plus blocked dependency chain
HarnessScenarioManifest  scenario/cell/check contract independent of execution
HarnessRunArtifact       complete, schema-versioned result/report bundle
```

The API, UI, MCP plane, diagnostics, and harness should all read the same `ApplicationReadModel` and `OutputRuntimeExplanation`. The harness should not need private runtime guesses to understand failures.

## Test harness: full audit

The harness is not incidental. It is a product-quality verification system and should have the same architectural standards as runtime code. It currently does unusually valuable work: matrix scenarios, shared-batch concurrency, process isolation, MediaMTX orchestration, fault scenarios, resource/bitrate sweeps, SRT crypto, signal capture, decode scans, HLS probes, recording probes, and runtime log hygiene. That breadth is good. The issue is that the harness does not yet have a crisp semantic model of its own.

### Harness bounded contexts

The harness should be layered as:

```text
test_harness/bin
  command parsing, profile checks, top-level dispatch only

test_harness/core
  environment/config parsing, netns/cgroup/process helpers, ports, paths, atomic JSON writes

test_harness/api
  typed RampApi client, status snapshots, output/pipeline/ingest helpers

test_harness/stack
  RestreamStack, MediaMtxStack, shared-batch stack lifecycle, log/artifact ownership

test_harness/scenario
  scenario DSL, manifest loading, case IDs, cells, checks, expected graph, skip/resume semantics

test_harness/probes
  ffprobe, decode-scan, signal, HLS playlist, recording, sink, HLS PUT

test_harness/sinks
  direct RTMP/SRT/HTTP sinks and their metrics

test_harness/reports
  assertion JSONL, scenario JSON, progress snapshots, root-cause grouping, artifact index

test_harness/modes
  mixed matrix, fast breadth, fault, resource sweep, bitrate sweep, SRT crypto, recovery
```

`src/bin/test_harness.rs` should eventually become a dispatcher and shared prelude, not the main home for unrelated harness subsystems.

### Harness anti-patterns to remove

| Current pattern | Why it is bad | Replacement |
|---|---|---|
| Opaque output IDs in failure text | Requires DB/log correlation to know the failed cell. | Persist `OutputCellMap` at creation and print it in every failure. |
| Harness reads `entry["state"]` when API exposes `status/rawStatus/phase` | Produces `state=unknown` and hides actual status. | Typed API status client and schema test. |
| Matrix progress records only scenario-level error strings | Loses structured root cause and output dependency state. | `FailureCause` enum plus embedded output/stage snapshots. |
| Some checks sample only one duplicate output without making sampling explicit | Passing duplicate can hide failing duplicate, or vice versa. | Declare `ProbeSamplingPolicy`: all, first, last, representative; report it. |
| Scenario runners own both orchestration and assertions | Makes ordering differences easy, such as live/file HLS preview asymmetry. | Separate `ScenarioExecutor` from `CheckRunner`. |
| Shared-batch media/log directories require filename inference | Causes race risk and stale artifact confusion. | Record/pipeline/output IDs as primary artifact identity; filenames are display only. |
| Timeouts are numerical constants spread by scenario shape | Hard to know whether timeout is startup, progress, probe, or soak. | Typed timeout policy with `reason`, `base`, `per_output`, and emitted effective values. |
| JSONL assertions are append-only events without a complete run index | Hard to query run after failure. | `artifact-index.json` linking scenario, assertion log, output map, logs, DB snapshot, probes. |
| Harness status assertions duplicate planner expectations manually | Test model can drift from runtime planner. | Harness imports a pure planner contract or compares against planner-exported graph. |

### Harness output identity model

Every output created by the harness should create and persist this record:

```rust
struct HarnessOutputCell {
    scenario_id: String,
    batch_group: String,
    wave: usize,
    pipeline_id: String,
    output_id: String,
    output_name: String,
    cell_id: String,
    duplicate_index: usize,
    protocol: String,
    encoding: String,
    selected_audio_track: Option<usize>,
    publish_url: String,
    read_url: Option<String>,
    expected_dimensions: Option<String>,
    expected_audio_tracks: Option<usize>,
    terminal_stage: Option<StageKey>,
}
```

This should be written to:

```text
<scenario workdir>/outputs.json
<scenario workdir>/artifact-index.json
root matrix scenario summary under failed outputs
```

A progress failure should read like:

```text
mixed.live.srt.h265.a2.bf2 / rtmp.1080p.a1 / out2
output_id=output_...
phase=starting
terminal_stage=audio:atrack:1:from:hevc_to_h264:from:video:1080p
blocked_by=hevc_to_h264:from:video:1080p
blocked_phase=waitingForCapacity
backend=externalFfmpeg
bytes_in=0 bytes_out=0
last_stage_error=null
```

### Harness check model

Checks should be typed, not stringly coordinated across many functions:

```rust
enum HarnessCheck {
    Ffprobe,
    DecodeScan,
    Signal,
    AudioRoute,
    RuntimeLog,
    StageSharing,
    HlsPreview,
    Recording,
    Load,
    Smoke,
    Lifecycle,
    SinkProbe,
    HlsPutProbe,
    BurstGraph,
    SoakDrift,
}

struct CheckRequest {
    scenario: ScenarioId,
    cell: Option<HarnessOutputCell>,
    check: HarnessCheck,
    sampling: ProbeSamplingPolicy,
    timeout: TimeoutPolicy,
    expected: ExpectedMediaShape,
}

struct CheckResult {
    id: String,
    status: PassFail,
    duration_ms: u64,
    cause: Option<FailureCause>,
    evidence: serde_json::Value,
}
```

### Harness failure taxonomy

Harness failures should be grouped by structured cause:

```rust
enum FailureCause {
    OutputNoProgress,
    OutputBlockedByStage,
    StageWaitingForCapacity,
    StageNoFirstOutput,
    StageNoKeyframe,
    StageNoParameterSets,
    TimestampDiscontinuity,
    ProbeProtocolConnectFailed,
    HlsNoSegments,
    RecordingNotFound,
    RecordingWrongScenario,
    RecordingTmpFileExposed,
    RuntimeLogError,
    LifecycleDidNotStop,
    HarnessInfrastructure,
}
```

The current internal-transcoder run would then summarize as `TimestampDiscontinuity` for many SRT decode scans and `OutputNoProgress/StageNoFirstOutput` for live SRT cells, instead of a long undifferentiated string.

### Harness artifact contract

A complete run directory should have:

```text
scenario.json                 human-readable scenario/matrix result
assertions.jsonl              all check events, schema-versioned
outputs.json                  output ID -> semantic cell map
stages.json                   expected and observed stage graph snapshots
artifact-index.json           paths and checksums for logs/probes/media/db
root-cause-summary.json       grouped failures across scenarios
_shared/<group>/*.log         stack logs
<scenario>/probe/*            ffprobe/decode/signal/HLS raw evidence
```

The harness should be able to answer these questions without code changes:

- Which semantic cells failed?
- Which root cause repeated the most?
- Did failures correlate with backend, codec, protocol, audio layout, B-frames, source adapter, or wave?
- Were outputs blocked before connection or after first bytes?
- Was a failed cell probed or sampled?
- Which artifact is authoritative for the failure?

### Harness role in architecture

The harness is both a client and an architectural governor. It should enforce:

1. Public API status is sufficient for failure diagnosis.
2. The runtime graph explains blocked outputs.
3. Recording identity is metadata-driven.
4. Internal and external FFmpeg paths satisfy the same packet contract.
5. HLS preview, HLS output, SRT, RTMP, recording, and file ingest all use the same stage graph concepts.
6. Every change in runtime status schema has a harness/client compatibility test.

If the harness needs to read private logs or SQLite to understand a failure, that is a product observability bug, not just a harness limitation.
