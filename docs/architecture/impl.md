# Restream Implementation Plan: Whole-Codebase Path to the Ideal Architecture

> **Status: completed migration plan and design record.** Historical source
> paths describe the code that this plan set out to split. Use
> [the gap analysis](arch_gap_analysis.md) for completion evidence and
> [the maintained architecture overview](../architecture.md) for current code.

## Contents

- [Purpose](#purpose)
- [Guiding constraints](#guiding-constraints)
- [Evidence driving the plan](#evidence-driving-the-plan)
- [Phase 0 — Baseline and guardrails](#phase-0-baseline-and-guardrails)
- [Phase 1 — Define core contracts without moving code](#phase-1-define-core-contracts-without-moving-code)
- [Phase 2 — Make configuration typed and centralized](#phase-2-make-configuration-typed-and-centralized)
- [Phase 3 — Split API into route modules](#phase-3-split-api-into-route-modules)
- [Phase 4 — Move API logic into application services](#phase-4-move-api-logic-into-application-services)
- [Phase 5 — Repository modules and persistence cleanup](#phase-5-repository-modules-and-persistence-cleanup)
- [Phase 6 — Runtime graph plan as the single planning model](#phase-6-runtime-graph-plan-as-the-single-planning-model)
- [Phase 7 — First-class stage lifecycle](#phase-7-first-class-stage-lifecycle)
- [Phase 8 — Dependency-aware output status](#phase-8-dependency-aware-output-status)
- [Phase 9 — FFmpeg narrow waist](#phase-9-ffmpeg-narrow-waist)
- [Phase 10 — HLS preview joins the graph runtime](#phase-10-hls-preview-joins-the-graph-runtime)
- [Phase 11 — Recording lifecycle and media library metadata](#phase-11-recording-lifecycle-and-media-library-metadata)
- [Phase 12 — Health, alerts, and diagnostics v2](#phase-12-health-alerts-and-diagnostics-v2)
- [Phase 13 — Test harness v2 reporting](#phase-13-test-harness-v2-reporting)
- [Phase 14 — Agent/MCP cleanup](#phase-14-agentmcp-cleanup)
- [Phase 15 — Large-file split after contracts exist](#phase-15-large-file-split-after-contracts-exist)
- [Phase 16 — Default policy and rollout](#phase-16-default-policy-and-rollout)
- [Detailed task backlog](#detailed-task-backlog)
- [Concrete validation commands](#concrete-validation-commands)
- [New tests to add](#new-tests-to-add)
- [Risk register](#risk-register)
- [Definition of done for the ideal point](#definition-of-done-for-the-ideal-point)
- [Recommended first sprint](#recommended-first-sprint)
- [Addendum: Full-Codebase and Harness Implementation Pass](#addendum-full-codebase-and-harness-implementation-pass)
- [Phase A — Source-wide audit automation](#phase-a-source-wide-audit-automation)
- [Phase B — Harness v2 semantic model](#phase-b-harness-v2-semantic-model)
- [Phase C — Harness typed API client](#phase-c-harness-typed-api-client)
- [Phase D — Harness root-cause reporting](#phase-d-harness-root-cause-reporting)
- [Phase E — Harness artifact index](#phase-e-harness-artifact-index)
- [Phase F — Harness execution symmetry](#phase-f-harness-execution-symmetry)
- [Phase G — Harness/report module split](#phase-g-harnessreport-module-split)
- [Phase H — Whole-codebase service and adapter split](#phase-h-whole-codebase-service-and-adapter-split)
- [Phase I — Harness as architectural governor](#phase-i-harness-as-architectural-governor)
- [Updated first sprint](#updated-first-sprint)

## Purpose

This document turns `architecture.md` into an incremental implementation plan for the entire codebase. It is organized by phases that can be landed independently. The media graph work is important, but it is only one track. The plan also covers API boundaries, application services, persistence, observability, agent/MCP, and the test harness.

The plan assumes the current codebase must keep running throughout the migration.

## Guiding constraints

1. No big-bang rewrite.
2. Keep existing API behavior unless a change is explicitly versioned.
3. Keep the mixed matrix and fault harness usable during the migration.
4. Prefer adding typed contracts before moving code.
5. Every refactor must improve surfacing, tests, or local reasoning.
6. Do not duplicate internal/external backend behavior; build shared narrow-waist contracts.
7. Do not add new one-off media paths; make differences explicit policies.

## Evidence driving the plan

The current failure modes show that the codebase needs architectural fixes, not isolated patches:

- Default external-backend runs on a 20 CPU machine generally pass H.264 and fail in H.265/HLS/1080p/shared-batch paths, while a 6 CPU run can fail broadly because long-lived external stages become a hidden capacity boundary.
- With `RESTREAM_USE_INTERNAL_TRANSCODER=1`, external FFmpeg pressure is mostly removed, but the run regresses to 16/18 failures dominated by timestamp discontinuities and zero-byte startup stalls. This proves internal and external backends are not behaviorally symmetric and should not be selected by one global switch.
- Older runs exposed a recording harness race around `.tmp.mp4` / wrong-case media selection; the uploaded source has a fix, but the product architecture still needs recording metadata rather than filename inference.

## Phase 0 — Baseline and guardrails

### Goals

Create a safe baseline before refactoring.

### Tasks

1. Add a source-architecture inventory document generated by CI:
   - file line counts
   - module dependency summary
   - public route count
   - DB schema summary
   - feature-gated modules

2. Add a smoke CI matrix:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
./scripts/build/bench-harness.sh
./target/bench/test_harness preflight
```

3. Add targeted correctness smoke commands:

```bash
./target/bench/test_harness mixed.live.rtmp.h264.a1.bf0
./target/bench/test_harness mixed.live.srt.h264.a1.bf0
./target/bench/test_harness mixed.asset.file.h264.a1.bf0
```

4. Preserve known failure artifacts as regression fixtures:
   - external H.265 capacity/stall run
   - 6 CPU all-progress-fail run
   - internal-transcoder timestamp-discontinuity run
   - old recording `.tmp.mp4` run

5. Add an `ARCHITECTURE_GUARDRAILS.md` or CI check for forbidden imports:
   - media importing API view models
   - API spawning media backends directly
   - application depending on HTTP types

### Acceptance criteria

- CI can show when large files grow.
- CI can flag forbidden dependency direction.
- Known failure artifacts are linked from test docs.

## Phase 1 — Define core contracts without moving code

### Goals

Introduce typed contracts that existing code can gradually adopt.

### New modules

```text
src/domain/ids.rs
src/domain/state.rs
src/domain/errors.rs
src/runtime/mod.rs
src/runtime/stage.rs
src/runtime/output.rs
src/runtime/graph.rs
src/runtime/capacity.rs
src/runtime/health.rs
src/config/mod.rs
```

If adding `runtime/` is too disruptive, initially place these under `media/engine_runtime/` and re-export later.

### Tasks

1. Add typed IDs:

```rust
PipelineId
OutputId
StageId
IngestId
RecordingId
JobId
```

Implement `Display`, `From<String>`, `AsRef<str>`, and serde transparent conversion.

2. Add typed states:

```rust
DesiredOutputState
JobStatus
EgressPhase
StagePhase
IngestPhase
RecordingPhase
HealthState
```

Keep current string fields at DB/API boundaries, but convert internally.

3. Add `StageError` and `RuntimeError`:

```rust
StageError { code, message, retryable, stderr_tail, source }
RuntimeError { code, message, entity, retryable }
```

4. Add `StageRuntimeSnapshot`:

```rust
struct StageRuntimeSnapshot {
    key: StageKey,
    backend: StageBackend,
    phase: StagePhase,
    input: Option<StageKey>,
    bytes_in: u64,
    bytes_out: u64,
    packets_in: u64,
    packets_out: u64,
    first_input_at: Option<DateTime<Utc>>,
    first_output_at: Option<DateTime<Utc>>,
    last_error: Option<StageErrorSummary>,
}
```

5. Add `OutputRuntimeExplanation`:

```rust
struct OutputRuntimeExplanation {
    output_id: OutputId,
    output_name: String,
    encoding: String,
    url: String,
    phase: EgressPhase,
    terminal_stage: Option<StageKey>,
    blocked_by: Option<StageBlockedBy>,
}
```

6. Add conversion tests for all enums to/from existing strings.

### Acceptance criteria

- Existing code still compiles.
- No route behavior changes.
- New types have unit tests.
- No new code writes raw string states except at DB/API boundary.

## Phase 2 — Make configuration typed and centralized

### Goals

Stop scattering env parsing across runtime modules.

### New structure

```text
src/config/mod.rs
src/config/media.rs
src/config/server.rs
src/config/security.rs
src/config/logging.rs
src/config/agent.rs
```

### Tasks

1. Create `AppConfig::from_env()`.

2. Move these env reads into config:
   - server ports
   - runtime tuning
   - external FFmpeg capacity
   - internal FFmpeg feature switches
   - HLS settings
   - SRT settings
   - startup policy
   - logging retention
   - agent feature/authorization toggles

3. Add explicit media backend switches:

```text
RESTREAM_INTERNAL_VIDEO_PRESETS=0
RESTREAM_INTERNAL_HEVC_TO_H264=0
RESTREAM_INTERNAL_HLS_PREVIEW=0
RESTREAM_INTERNAL_AUDIO_COMPLEX=0
RESTREAM_EXTERNAL_FFMPEG_PERMITS=<optional explicit override>
```

4. Fix `RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN` semantics:
   - keep as hard cap if needed
   - add `RESTREAM_EXTERNAL_FFMPEG_PERMITS` as an actual explicit override
   - log derived values at startup

5. Pass `Arc<AppConfig>` or specific config structs into runtime services.

### Acceptance criteria

- Startup logs show effective config.
- No media module directly calls `std::env::var` except test-only helpers or `Config::from_env`.
- Existing env variables remain backward compatible.

## Phase 3 — Split API into route modules

### Goals

Shrink `src/api.rs` by moving handlers into coherent modules while preserving router behavior.

### Target files

```text
src/api/mod.rs
src/api/router.rs
src/api/state.rs
src/api/auth.rs
src/api/static_assets.rs
src/api/pipelines.rs
src/api/outputs.rs
src/api/ingests.rs
src/api/file_ingest.rs
src/api/media_library.rs
src/api/hls.rs
src/api/health.rs
src/api/logs.rs
src/api/alerts.rs
src/api/telemetry.rs
src/api/settings.rs
src/api/agent.rs
```

### Tasks

1. Move `AppState` to `api/state.rs`.
2. Move `create_router` to `api/router.rs`.
3. Move auth/session handlers to `api/auth.rs`.
4. Move pipeline/output handlers to route modules without changing logic.
5. Move HLS route handlers to `api/hls.rs`, but do not yet change HLS implementation.
6. Move media library endpoints to `api/media_library.rs`.
7. Move health/logs/alerts/telemetry endpoints to their modules.
8. Move static assets and SPA fallback to `api/static_assets.rs`.

### Acceptance criteria

- `src/api.rs` becomes a thin `mod`/re-export file or disappears.
- Route snapshot test confirms same route set.
- No media runtime behavior changes in this phase.

## Phase 4 — Move API logic into application services

### Goals

Handlers should validate/deserialize, call services, and serialize responses. They should not implement core business logic or spawn media work.

### New application services

```text
src/application/services/pipeline_service.rs
src/application/services/output_service.rs
src/application/services/ingest_service.rs
src/application/services/file_ingest_service.rs
src/application/services/media_library_service.rs
src/application/services/settings_service.rs
src/application/services/recording_service.rs
src/application/services/health_service.rs
```

### Tasks

1. Introduce service traits and structs:

```rust
PipelineService
OutputService
IngestService
FileIngestService
MediaLibraryService
RecordingService
HealthService
```

2. Move validation out of handlers where it is not HTTP-specific.

3. Move file-ingest create/update/start/stop orchestration from API into application service.

4. Move media rename/delete/list policy into media-library service.

5. Move pipeline file-ingest payload application into service.

6. Handlers return `ApiResult<T>` with a consistent error mapping.

### Acceptance criteria

- Handlers do not call SQLx directly.
- Handlers do not call low-level media stage constructors.
- Application services are testable without Axum request types.

## Phase 5 — Repository modules and persistence cleanup

### Goals

Split `db.rs` into repositories and isolate SQL.

### Target files

```text
db/mod.rs
db/schema.rs
db/migrations.rs
db/pipeline_repo.rs
db/output_repo.rs
db/ingest_repo.rs
db/job_repo.rs
db/meta_repo.rs
db/session_repo.rs
db/log_repo.rs
```

### Tasks

1. Move schema setup and migrations to `schema.rs` / `migrations.rs`.
2. Move pipeline functions into `pipeline_repo.rs`.
3. Move output functions into `output_repo.rs`.
4. Move ingest functions into `ingest_repo.rs`.
5. Move job functions into `job_repo.rs`.
6. Move sessions into `session_repo.rs`.
7. Move app logs into `log_repo.rs`.
8. Implement repository traits from `application::ports`.
9. Convert string states to typed enums at repository boundary.

### Acceptance criteria

- `db.rs` is a module index and pool/schema helper only.
- Application services depend on repository traits.
- SQL stays out of API and media modules.

## Phase 6 — Runtime graph plan as the single planning model

### Goals

Make output, preview, HLS output, recording, diagnostics, and test expectations use the same graph planner.

### New modules

```text
src/runtime/graph/plan.rs
src/runtime/graph/planner.rs
src/runtime/graph/policy.rs
src/runtime/graph/view.rs
```

### Core structs

```rust
struct StageGraphPlan {
    pipeline_id: PipelineId,
    role: GraphRole,
    terminal_stage: StageKey,
    stages: Vec<StagePlan>,
    edges: Vec<StageEdge>,
}

enum GraphRole {
    Output { output_id: OutputId },
    HlsPreview,
    HlsOutput { output_id: OutputId },
    Recording,
    Diagnostic,
}

struct StagePlan {
    key: StageKey,
    kind: StageKind,
    input: Option<StageKey>,
    backend_candidates: Vec<StageBackend>,
    readiness: ReadinessPolicy,
}
```

### Tasks

1. Keep `planner::output_path::OutputPath` inside the planner and have application code adapt persisted output models into planner inputs.
2. Add planner for:
   - output egress
   - HLS preview
   - HLS PUT/output
   - recording
3. Move test-harness expected stage generation to consume the same planner or a pure duplicate with equality tests.
4. Add graph plan snapshots to diagnostics.

### Acceptance criteria

- There is one planner function path for RTMP/SRT output stages.
- HLS preview no longer has a separate stage-key vocabulary.
- Stage-sharing tests compare against the graph planner.

## Phase 7 — First-class stage lifecycle

### Goals

Replace “stage exists as a ring buffer” with `StageRuntime`.

### New modules

```text
src/runtime/stage/runtime.rs
src/runtime/stage/registry.rs
src/runtime/stage/lifecycle.rs
src/runtime/stage/snapshot.rs
src/runtime/stage/error.rs
```

### Tasks

1. Wrap current `stages.buffers`, `input_queues`, `metrics`, `pipe_metrics`, and backend handles into a `StageRuntime` map.
2. Add `StagePhase` tracking.
3. Rename or supplement `StageStarted` event with:
   - `StageRegistered`
   - `StageWaitingForCapacity`
   - `StageBackendSpawned`
   - `StageFirstInput`
   - `StageFirstOutput`
   - `StageFailed`
   - `StageStopped`
4. Make external semaphore wait cancellation-aware:

```rust
tokio::select! {
    permit = semaphore.acquire() => ...,
    _ = cancel.cancelled() => return Ok(()),
}
```

5. Expose capacity state:
   - permits total
   - permits available
   - waiting stages
   - wait duration

6. Add `StageRuntimeSnapshot` to health and telemetry.

### Acceptance criteria

- Output status can identify terminal stage and blocked stage.
- Stage wait for capacity is visible.
- Existing `StageStarted` log no longer misrepresents backend readiness.
- Progress failures no longer show empty `lastError` when the upstream stage is blocked.

## Phase 8 — Dependency-aware output status

### Goals

Make output status answer “why is this output not sending?”

### Tasks

1. Add terminal stage key to egress registration.
2. Add `OutputRuntimeExplanation` to API view model.
3. Change RTMP/SRT warmup to common upstream-wait helper.
4. Add common phases:

```text
waitingUpstream
connecting
sending
retrying
failed
stopped
```

5. Fix harness `state=unknown` by reading `status` / `rawStatus`, not missing `state`.
6. Add output name, encoding, URL, and stage chain to progress-gate failure.

### Acceptance criteria

A failed progress gate prints something like:

```text
rtmp.1080p.a0-2 output_xxx encoding=1080p+atrack:0 url=...
phase=waitingUpstream
terminalStage=audio:atrack:0:from:hevc_to_h264:from:video:1080p
blockedBy=hevc_to_h264:from:video:1080p
blockedByPhase=waitingForCapacity
backend=externalFfmpeg waitMs=43122
```

## Phase 9 — FFmpeg narrow waist

### Goals

Make internal and external FFmpeg execution symmetric without duplicating behavior.

### New modules

```text
src/media/ffmpeg/mod.rs
src/media/ffmpeg/stage_plan.rs
src/media/ffmpeg/operation.rs
src/media/ffmpeg/input_pump.rs
src/media/ffmpeg/output_normalizer.rs
src/media/ffmpeg/timeline.rs
src/media/ffmpeg/backend.rs
src/media/ffmpeg/external.rs
src/media/ffmpeg/internal.rs
```

### Shared contracts

```rust
FfmpegStagePlan
FfmpegOperation
StageInputPump
StageOutputNormalizer
StageTimeline
FfmpegStageBackend
StageRunContext
```

### Tasks

1. Add `StageTimeline` and tests for:
   - file-loop backward timestamp reset
   - forward discontinuity
   - audio/video epoch alignment
   - negative DTS correction
   - monotonic DTS per stream

2. Add `StageOutputNormalizer`:
   - all paths write `MediaPacket` through it
   - normalizes timestamps
   - enforces DTS monotonicity
   - sets output parameter sets
   - records first output/keyframe

3. Add `StageInputPump`:
   - keyframe preroll
   - dynamic parameter-set refresh
   - metadata wait
   - cancellation-aware feed loop
   - common metrics

4. Convert external transcoder to use `StageInputPump` and `StageOutputNormalizer`.

5. Convert internal transcoder to use the same plan/input/output contracts.

6. Replace global `RESTREAM_USE_INTERNAL_TRANSCODER` with per-stage policy:

```text
RESTREAM_INTERNAL_VIDEO_PRESETS
RESTREAM_INTERNAL_HEVC_TO_H264
RESTREAM_INTERNAL_HLS_PREVIEW
```

7. Keep default production policy external for video presets until internal passes parity.

### Acceptance criteria

- Internal and external backends compile from the same `FfmpegOperation`.
- No backend writes directly to `RingBuffer`.
- Internal timestamp-discontinuity failures are covered by unit/integration tests.
- `RESTREAM_USE_INTERNAL_TRANSCODER=1` is deprecated or mapped to explicit per-stage switches with warnings.

## Phase 10 — HLS preview joins the graph runtime

### Goals

Remove API-created preview one-off.

### Tasks

1. Add `GraphRole::HlsPreview`.
2. Create preview graph plan:
   - H264 input: source -> fMP4 segmenter
   - HEVC input: browser-safe H264 preview stage -> fMP4 segmenter
3. Move preview stage creation out of `api.rs` into runtime/application service.
4. Make `active_hls_preview_stage_keys()` return the actual graph plan keys.
5. Share readiness, lifecycle, and metrics with output stages.

### Acceptance criteria

- No API handler directly creates a preview `RingBuffer` or calls external transcoder stage function.
- Preview stage keys in health match actual spawned keys.
- HLS `No segments yet` reports blocked video stage cause when applicable.

## Phase 11 — Recording lifecycle and media library metadata

### Goals

Make recordings identifiable by metadata, not filenames.

### Tasks

1. Add `recordings` metadata table or structured sidecar:

```text
recording_id
pipeline_id
started_at
ended_at
status
temp_name
final_name
codec_summary
error
```

2. Runtime recording writes lifecycle events:
   - started
   - finalizing
   - ready
   - failed

3. Media API returns recording metadata including `pipelineId` and status.
4. Harness recording check filters by pipeline ID / recording ID first, filename token only as fallback.

### Acceptance criteria

- Harness never probes `.tmp.mp4`.
- Harness never validates a paired case’s recording.
- Media list can explain in-progress/finalizing/failed recording state.

## Phase 12 — Health, alerts, and diagnostics v2

### Goals

Make health compact, causal, and alertable.

### Tasks

1. Add stage snapshots to health.
2. Add dependency chain to output status.
3. Add backend capacity metrics.
4. Add ring reader lag and keyframe wait information.
5. Update `alerts.rs` to derive alerts from new causal fields:
   - output blocked by stage
   - stage waiting for capacity too long
   - stage receiving input but no output
   - HLS preview waiting for keyframe
   - SRT receive drops
   - ring lag high
6. Update diagnostics endpoints to include:
   - graph plan
   - graph runtime state
   - backend stderr tail
   - recent events
   - relevant logs

### Acceptance criteria

- `/api/v1/engine/health` has dependency-aware status.
- `/api/v1/pipelines/:id/graph` can show desired graph and runtime graph.
- Alerts include recommended actions based on cause.

## Phase 13 — Test harness v2 reporting

### Goals

Make failures immediately actionable.

### Tasks

1. Build output-id to cell map at creation time:

```rust
struct OutputCellMap {
    output_id: String,
    scenario: String,
    cell_id: String,
    duplicate_index: usize,
    encoding: String,
    protocol: String,
    url: String,
}
```

2. Persist map in each scenario artifact.
3. Progress gate failure prints cell info and dependency chain.
4. Probe failures include API status snapshot.
5. Matrix summary groups failures by root cause:
   - waiting for capacity
   - no keyframe
   - timestamp discontinuity
   - no HLS segments
   - protocol connect failure
6. Keep JSONL assertions but add a stable schema version.

### Acceptance criteria

- Top-level `scenario.json` failure can be read without DB/log correlation.
- Every stalled output maps to semantic cell name.
- Failure grouping identifies repeated root causes across scenarios.

## Phase 14 — Agent/MCP cleanup

### Goals

Make the optional agent plane consume stable application/runtime read models.

### Tasks

1. Move shared agent request/response types out of HTTP-only modules.
2. Make MCP tools call application services or agent services, not duplicate HTTP DTOs.
3. Use graph planner for impact previews.
4. Use health/diagnostics v2 for investigation context.
5. Keep execution gated behind feature flags and idempotency/approval flow.

### Acceptance criteria

- Agent read/plan endpoints do not import media internals.
- Agent impact preview and runtime graph use the same planner.
- MCP and HTTP share command/query DTOs where feature boundaries permit.

## Phase 15 — Large-file split after contracts exist

### Goals

Reduce file size and improve local reasoning, after contracts are stable.

### Split plan

#### `src/media/engine.rs`

Move into:

```text
runtime/ingest_registry.rs
runtime/egress_registry.rs
runtime/stage_registry.rs
runtime/hls_registry.rs
runtime/recording_registry.rs
runtime/snapshots.rs
runtime/capacity.rs
```

#### `src/media/rtmp.rs`

Move into:

```text
media/protocols/rtmp/server.rs
media/protocols/rtmp/ingest.rs
media/protocols/rtmp/egress.rs
media/protocols/rtmp/flv.rs
media/protocols/rtmp/session.rs
```

#### `src/media/srt.rs`

Move into:

```text
media/protocols/srt/listener.rs
media/protocols/srt/ingest.rs
media/protocols/srt/egress.rs
media/protocols/srt/quality.rs
media/protocols/srt/socket.rs
```

#### `src/bin/test_harness.rs`

Move into:

```text
bin/test_harness/core.rs
bin/test_harness/api_client.rs
bin/test_harness/process.rs
bin/test_harness/artifacts.rs
bin/test_harness/sinks.rs
bin/test_harness/probes.rs
bin/test_harness/modes.rs
```

### Acceptance criteria

- No file exceeds 2,000 lines unless it is generated data or an intentionally grouped test manifest.
- Split files correspond to actual responsibilities, not arbitrary chunks.

## Phase 16 — Default policy and rollout

### Goals

Safely roll out the architecture changes.

### Backend policy rollout

1. Default remains external FFmpeg for video presets.
2. Add internal backend CI for selected single-case tests.
3. Enable internal HEVC→H264 only after:
   - live-before-EOF test passes
   - timestamp normalizer tests pass
   - mixed H.265 RTMP selected-audio smoke passes
4. Enable internal video presets only after:
   - file-loop timestamp tests pass
   - SRT decode-scan matrix passes
   - RSS does not regress unacceptably

### Runtime graph rollout

1. Add stage lifecycle fields to API as additive fields.
2. Update UI/harness to consume new fields.
3. Keep old fields until harness and UI are migrated.
4. Remove or deprecate misleading `StageStarted` semantics.

### Acceptance criteria

- A default 20 CPU matrix should pass or fail with clear causal messages.
- A constrained-capacity run should show `waitingForCapacity`, not zero-byte unknown stalls.
- Internal-transcoder runs should fail only on known/allowed tests until parity is reached.

## Detailed task backlog

### P0 tasks

- Add typed `StagePhase` and `StageRuntimeSnapshot`.
- Make external FFmpeg capacity waits visible and cancellation-aware.
- Fix harness `state=unknown` by using API `status`/`rawStatus`.
- Add output-cell map to harness failure reporting.
- Add `StageTimeline` and apply to internal video/audio emission.
- Add dynamic parameter-set refresh to internal stage input.
- Add keyframe preroll to internal stage readers.
- Stop using one global internal-transcoder switch for all stage families.

### P1 tasks

- Split API route modules.
- Create application service layer for pipelines/outputs/ingests/media library.
- Add graph planner for HLS preview and recording.
- Move HLS preview stage creation out of API.
- Add backend capacity metrics to health.
- Add stage stderr tail to diagnostics.
- Split `db.rs` into repositories.

### P2 tasks

- Split protocol modules.
- Split test harness core/probes/reports.
- Introduce typed IDs broadly.
- Agent/MCP DTO cleanup.
- Add recording metadata table.
- Add UI support for dependency-chain status.

## Concrete validation commands

### Default external path

```bash
./scripts/build/bench-harness.sh
./target/bench/test_harness mixed.matrix
```

Expected before all fixes: may still fail, but failures should become causal.

### Constrained capacity

```bash
RESTREAM_EXTERNAL_FFMPEG_PERMITS=2 \
./target/bench/test_harness mixed.live.srt.h264.a2.bf0
```

Expected after lifecycle work: outputs report `waitingForCapacity` or blocked upstream stage, not empty errors.

### Internal timestamp work

```bash
RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
ONLY_CHECKS=ffprobe,decode-scan \
./target/bench/test_harness mixed.asset.file.h264.a1.bf0
```

Expected after timeline work: no timestamp discontinuity on SRT scaled outputs.

### Internal live startup

```bash
RESTREAM_INTERNAL_VIDEO_PRESETS=1 \
ONLY_CHECKS=load,ffprobe \
./target/bench/test_harness mixed.live.srt.h264.a1.bf0
```

Expected after preroll/parameter-set work: outputs progress or explain blocked stage.

### Internal codec edge only

```bash
RESTREAM_INTERNAL_VIDEO_PRESETS=0 \
RESTREAM_INTERNAL_HEVC_TO_H264=1 \
ONLY_CHECKS=load,ffprobe,stage-sharing \
./target/bench/test_harness mixed.live.srt.h265.a2.bf2
```

Expected after internal codec-edge hardening: RTMP HEVC→H264 selected-audio cells progress.

## New tests to add

### Domain/planner

```text
output_config_roundtrip_preserves_audio_route
stage_graph_for_rtmp_hevc_contains_codec_edge
stage_graph_for_srt_hevc_does_not_require_codec_edge
hls_preview_hevc_plan_uses_actual_preview_stage_key
recording_plan_uses_source_stage
```

### Runtime lifecycle

```text
stage_waiting_for_capacity_is_visible
stage_first_output_transitions_to_producing
output_blocked_by_upstream_stage_reports_chain
cancelled_waiting_stage_does_not_spawn_later
```

### FFmpeg/timeline

```text
timeline_rebases_file_loop_backward_jump
timeline_rebases_large_forward_jump
timeline_aligns_audio_and_video_epoch
timeline_enforces_per_stream_dts_monotone
internal_video_preset_emits_monotone_srt_ts
internal_hevc_to_h264_emits_before_eof
```

### API/view models

```text
output_status_contains_status_raw_status_phase
output_status_contains_terminal_stage_and_blocked_by
health_snapshot_contains_capacity_metrics
hls_preview_status_reports_blocked_stage
```

### Harness

```text
progress_failure_includes_output_cell_map
recording_check_rejects_tmp_file
recording_check_filters_by_pipeline_or_token
matrix_summary_groups_by_root_cause
```

## Risk register

| Risk | Mitigation |
|---|---|
| Refactor breaks API compatibility | Route snapshot tests and additive fields first. |
| Stage lifecycle adds overhead | Use cheap atomics/locks; rich snapshots only on request. |
| Internal backend remains unstable | Keep per-stage flags off by default until parity tests pass. |
| External capacity changes alter production behavior | Add explicit config and visible metrics before changing defaults. |
| Large-file split causes merge pain | Split after contracts; use mechanical moves with minimal logic changes. |
| Agent feature gates complicate shared types | Put shared command/query DTOs in feature-neutral modules where possible. |

## Definition of done for the ideal point

The codebase reaches the ideal architecture when all of these are true:

1. API handlers are thin and grouped by route domain.
2. Application services own use cases and depend on repository/runtime ports.
3. DB access is isolated in repositories.
4. Graph planning is shared by outputs, HLS preview, HLS output, recording, diagnostics, agent impact previews, and harness expectations.
5. Stages are first-class runtime objects with lifecycle, backend, capacity, metrics, errors, and snapshots.
6. RTMP/SRT/HLS output status includes terminal stage and blocked dependency chain.
7. Internal and external FFmpeg paths share planning, input pumping, output normalization, lifecycle, and diagnostics.
8. HLS preview is no longer an API one-off.
9. Recording identity is metadata-driven.
10. Health and alerts are causal.
11. The harness maps every output ID to a semantic matrix cell.
12. No major production module is a god file.
13. Every runtime wait has a name, a metric, and a cancellation path.
14. Every important failure mode has a targeted regression test.

## Recommended first sprint

The highest-value first sprint is:

1. Add `StagePhase`, `StageRuntimeSnapshot`, and backend capacity metrics.
2. Make external FFmpeg capacity wait visible and cancellation-aware.
3. Fix harness `state=unknown` and output-cell failure mapping.
4. Add `StageTimeline` and apply it to internal emission.
5. Split backend policy into per-stage flags.

This sprint directly addresses the known operational pain while laying the foundation for cleaner architecture across the whole codebase.

---

## Addendum: Full-Codebase and Harness Implementation Pass

The previous plan covered the whole codebase at architectural level. This addendum makes the harness and source-wide cleanup concrete enough to execute. The bar is relevance: every new abstraction must remove duplicate work, make an ugly one-off harder, or make a failure explain itself.

## Phase A — Source-wide audit automation

### Tasks

1. Add `scripts/check/source-audit.sh` that emits:
   - line count by Rust file
   - public function count by module
   - route count by API module
   - harness mode/check inventory
   - forbidden import violations
   - env var usage inventory

2. Generate `target/source-audit.json` in CI.

3. Fail CI when:
   - `src/api.rs`, `src/media/engine.rs`, or `src/bin/test_harness.rs` grows after a replacement module exists
   - media imports API view models
   - API manually starts FFmpeg/transcoder stages
   - harness consumes a status field not present in API schema

### Acceptance criteria

- CI has a visible architecture drift report.
- New one-offs are caught before review depends on memory.

## Phase B — Harness v2 semantic model

### Tasks

1. Add `src/bin/test_harness/scenario_model.rs`:

```rust
struct ScenarioId(String);
struct CellId(String);
struct DuplicateIndex(usize);

struct HarnessOutputCell {
    scenario_id: ScenarioId,
    batch_group: String,
    wave: usize,
    pipeline_id: String,
    output_id: String,
    output_name: String,
    cell_id: CellId,
    duplicate_index: DuplicateIndex,
    protocol: String,
    encoding: String,
    selected_audio_track: Option<usize>,
    publish_url: String,
    read_url: Option<String>,
    expected_dimensions: Option<String>,
    expected_audio_tracks: Option<usize>,
    terminal_stage: Option<String>,
}
```

2. Add `HarnessOutputRegistry`:

```rust
struct HarnessOutputRegistry {
    by_output_id: HashMap<String, HarnessOutputCell>,
}

impl HarnessOutputRegistry {
    fn insert(&mut self, cell: HarnessOutputCell);
    fn get(&self, output_id: &str) -> Option<&HarnessOutputCell>;
    fn write_outputs_json(&self, path: &Path) -> Result<(), String>;
}
```

3. Change `add_mixed_group`, `add_mixed_output_cases`, `add_mixed_multi_output_cases`, and `add_mixed_srt_group` to return/register `HarnessOutputCell`, not just push output IDs.

4. Persist `outputs.json` per scenario and aggregate into matrix `scenario.json` for failed outputs.

### Acceptance criteria

- Every `output_id` in a progress failure maps to scenario/cell/duplicate/protocol/encoding/URL.
- No failure report requires SQLite or MediaMTX logs to know the failed cell.

## Phase C — Harness typed API client

### Tasks

1. Replace direct JSON indexing in high-value paths with typed DTOs:

```rust
struct ApiOutputStatus {
    output_id: String,
    status: String,
    raw_status: String,
    phase: String,
    bytes_out: u64,
    packets_out: u64,
    last_error: Option<String>,
    metrics: OutputMetrics,
    terminal_stage: Option<String>,
    blocked_by: Option<BlockedByStage>,
}
```

2. Fix the known `state=unknown` issue:

```rust
// before
let state = entry["state"].as_str().unwrap_or("unknown");

// after
let status = entry.status.as_str();
let raw_status = entry.raw_status.as_str();
let phase = entry.phase.as_str();
```

3. Add schema tests:

```text
api_output_status_has_status_raw_status_phase
harness_progress_status_consumes_existing_fields
harness_fails_if_status_schema_drops_required_fields
```

### Acceptance criteria

- `state=unknown` disappears from harness-generated progress failures unless the API truly reports unknown.
- API/harness schema drift fails unit tests.

## Phase D — Harness root-cause reporting

### Tasks

1. Add `FailureCause` enum:

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

2. Convert probe errors into structured causes:
   - decode-scan matching `timestamp discontinuity` -> `TimestampDiscontinuity`
   - HLS `404 No segments yet` -> `HlsNoSegments`
   - RTMP/SRT input open failure -> `ProbeProtocolConnectFailed`
   - zero-byte progress with blocked stage -> `OutputBlockedByStage`
   - zero-byte progress without dependency info -> `OutputNoProgress`

3. Write `root-cause-summary.json`:

```json
{
  "TimestampDiscontinuity": {
    "count": 18,
    "scenarios": [...],
    "cells": [...]
  }
}
```

4. Print a compact root-cause summary at the end of matrix runs.

### Acceptance criteria

- The internal-transcoder run classifies SRT decode-scan failures as `TimestampDiscontinuity` rather than long raw FFmpeg text.
- The default H.265 runs group no-progress failures by stage/capacity when API status provides dependency fields.

## Phase E — Harness artifact index

### Tasks

1. Add `ArtifactIndex`:

```rust
struct ArtifactIndex {
    run_id: String,
    command: Vec<String>,
    env: BTreeMap<String, String>,
    started_at: String,
    source_revision: Option<String>,
    scenario_json: PathBuf,
    assertions_jsonl: PathBuf,
    outputs_json: Vec<PathBuf>,
    stages_json: Vec<PathBuf>,
    logs: Vec<PathBuf>,
    media: Vec<PathBuf>,
    sqlite_db: Option<PathBuf>,
}
```

2. Write `artifact-index.json` atomically at root and scenario level.

3. Include checksums for large evidence files when practical.

4. Preserve DB snapshot or export relevant tables before teardown for failed matrix runs.

### Acceptance criteria

- One file can locate all logs/media/probe evidence for a failed run.
- Current and stale artifacts cannot be confused silently.

## Phase F — Harness execution symmetry

### Tasks

1. Define `ScenarioExecutor` trait:

```rust
trait ScenarioExecutor {
    async fn prepare(&mut self) -> Result<(), String>;
    async fn start_input(&mut self) -> Result<(), String>;
    async fn pre_fanout_checks(&mut self) -> Result<(), String>;
    async fn create_outputs(&mut self) -> Result<(), String>;
    async fn wait_for_progress(&mut self) -> Result<(), String>;
    async fn run_probes(&mut self) -> Result<(), String>;
    async fn cleanup(&mut self) -> Result<(), String>;
}
```

2. Implement executors for:
   - live RTMP
   - live SRT single-track
   - live SRT multi-track
   - file ingest single-track
   - file ingest multi-track

3. Make live/file HLS preview ordering a named policy:

```rust
enum HlsPreviewTiming {
    BeforeFanout,
    AfterProgress,
    Disabled,
}
```

Default should be `BeforeFanout` for both live and file-ingest unless a scenario explicitly tests late HLS attachment.

4. Make duplicate output probe strategy explicit:

```rust
enum ProbeSamplingPolicy {
    AllDuplicates,
    FirstDuplicate,
    LastDuplicate,
    Representative { index: usize },
}
```

### Acceptance criteria

- Live/file ordering differences are visible in manifest, not hidden in runner code.
- Probe reports say whether all duplicates or a sample was checked.

## Phase G — Harness/report module split

### Tasks

Move code mechanically, with minimal logic changes:

```text
src/bin/test_harness.rs
  remains command dispatch and shared imports only

src/bin/test_harness/core.rs
  env parsing, paths, atomic JSON, netns/cgroup, process cleanup

src/bin/test_harness/api_client.rs
  RampApi and typed DTOs

src/bin/test_harness/ports.rs
  TestPorts, HarnessPortDefaults

src/bin/test_harness/stacks.rs
  start/stop restream and MediaMTX stacks

src/bin/test_harness/probes/mod.rs
  ffprobe, decode-scan, signal, HLS, recording, sink, HLS PUT

src/bin/test_harness/reports.rs
  assertions, scenario progress, root cause, artifact index

src/bin/test_harness/modes/
  mixed, fault, resource, bitrate, branch, crypto, recovery
```

### Acceptance criteria

- `src/bin/test_harness.rs` drops below 2,000 lines.
- No behavior change except improved reports.
- Existing commands remain compatible.

## Phase H — Whole-codebase service and adapter split

### Tasks

1. API split:

```text
src/api/routes/pipelines.rs
src/api/routes/outputs.rs
src/api/routes/ingests.rs
src/api/routes/hls.rs
src/api/routes/media.rs
src/api/routes/settings.rs
src/api/routes/health.rs
src/api/routes/agent.rs
```

2. Application service split:

```text
PipelineService
OutputService
IngestService
RecordingService
SettingsService
GraphPlanningService
RuntimeStatusService
```

3. Runtime split:

```text
RuntimeGraph
StageRegistry
OutputRegistry
IngestRegistry
HlsRegistry
RecordingRegistry
CapacityRegistry
```

4. Media split:

```text
media/protocols/rtmp/{ingest,egress,flv,session}.rs
media/protocols/srt/{ingest,egress,quality,url}.rs
media/ffmpeg/{plan,input,output,timeline,external,internal}.rs
media/hls/{preview,upload,ts,fmp4}.rs
media/recording/{runtime,writer,catalog}.rs
```

### Acceptance criteria

- Route handlers no longer instantiate media stage internals.
- Media adapters no longer import API view models.
- Runtime graph is the only component that starts/stops stages.

## Phase I — Harness as architectural governor

### Tasks

1. Add harness tests that enforce runtime API quality:

```text
progress_failure_includes_cell_identity
progress_failure_includes_dependency_chain_when_available
timestamp_discontinuity_grouped_by_root_cause
recording_uses_metadata_identity_not_tmp_filename
hls_no_segments_reports_preview_stage_state
```

2. Add source tests that enforce no one-offs:

```text
hls_preview_plan_uses_graph_planner
external_and_internal_stage_plan_share_operation
backend_policy_does_not_use_global_internal_switch_for_all_stages
```

3. Add CI mode:

```bash
./target/bench/test_harness mixed.fast-breadth
```

with root-cause summary required on failure.

### Acceptance criteria

- The harness fails when observability regresses.
- The harness is not only a media verifier; it is a contract verifier for the whole product.

## Updated first sprint

The first sprint should now include harness work explicitly:

1. Add `HarnessOutputCell` and persist `outputs.json`.
2. Fix `state=unknown` by using typed API status DTOs.
3. Add root-cause enum and classify timestamp discontinuity / HLS no segments / no progress.
4. Add stage lifecycle snapshot fields to API as additive fields.
5. Add source-audit script and forbidden import guardrails.
6. Add `StageTimeline` and wire it into internal emission.

This sequence makes future failures more useful before attempting large media refactors.
