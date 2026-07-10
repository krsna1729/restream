# Architecture Gap Analysis: Current Code vs. Ideal State

> **Reference documents**: [arch.md](arch.md) · [impl.md](impl.md)
>
> Audit standard: this is a deep implementation audit, not a symbol-existence
> checklist. A phase is marked complete only when the current code satisfies
> the phase acceptance criteria and the stronger architectural intent in
> `arch.md`.

---

## Executive Summary

The codebase now satisfies the Phase 1-12 acceptance criteria at A-grade for
the audited scope. The important Phase 12 alert gaps have been closed: health
exposes stage snapshots, output status carries `blockedBy`, alerts derive from
causal fields, `/api/v1/pipelines/:id/graph` exists, and the diagnostics
context endpoint bundles graph, health, alerts, events, relevant logs, and
backend stderr tail.

The current A-grade evidence across Phases 1-12 is:

- typed contracts exist, and output desired state, job status, and runtime
  lifecycle state are typed through application/runtime/repository boundaries;
- configuration is centralized for production startup/runtime paths, with
  source-audit enforcement against raw runtime env reads outside config;
- API route modules exist, direct SQL calls have moved behind services, and
  runtime health/status/graph/telemetry read models go through
  `RuntimeViewService`;
- application services own the main pipeline/output/ingest/health/log/auth,
  settings, media-library, file-ingest, agent catalog, and runtime read-model
  use cases;
- the graph planner drives output preparation, graph rendering, HLS preview,
  persistent HLS output terminal-stage preparation and segmenter lifecycle,
  diagnostics, agent previews, recording terminal-stage/lifecycle registration
  plus writer start identity, and harness stage-count expectations;
- stage lifecycle is first-class for ring-backed FFmpeg stages and non-ring HLS
  / recording stage families;
- FFmpeg execution uses the shared narrow waist for stage planning, input pump,
  backend execution, and output normalization;
- recording metadata exists in the database, and the product/harness path now
  requires recording metadata identity instead of filename-token matching;
- diagnostics expose the Phase 12 causal context bundle, while the legacy SSE
  check endpoint remains as a separate active-ingest probe path.

Bottom line: **Phases 1-12 have completed the requested "Amit Singhal" pass for
their phase-scope criteria**. Phase 13 harness reporting, Phase 14 Agent/MCP
boundary cleanup, and Phase 15 large-file splitting now also meet their
acceptance criteria. Phase 16 rollout work now also meets its phase acceptance
criteria:
per-stage internal-backend policy is the active configuration model, the legacy
global transcoder switch no longer selects backends, runtime graph stage nodes
carry lifecycle/capacity details for the UI and harness, and CI now runs a
blocking internal-backend rollout smoke lane, with the full internal
video-preset SRT decode-scan/RSS promotion proof captured in
`scripts/check-internal-video-preset-rollout.sh`. The external-capacity guard
is captured in `scripts/check-external-capacity-rollout.sh`: the capacity-ok
leg passes, and the constrained default, recording-inclusive leg persists the
recording row before failing causally as `waitingForCapacity` with backend and
wait time attached. The internal video-preset H.264/SRT decode-scan matrix now
passes with RSS baselines and no external FFmpeg children. The internal
HEVC-to-H264 RTMP selected-audio lane also now passes with `decode-scan`, so
all internal-backend rollout smoke cases are blocking.

---

## Phase-by-Phase Status

### Phase 0 — Baseline & Guardrails

| Task | Status | Evidence |
|---|---|---|
| Source inventory doc / CI | ✅ Present | `scripts/source-audit.sh` checks forbidden imports, no-growth file-size baselines, and env reads; it emits `target/source-audit.json` and now runs in the CI architecture-guardrails job. |
| Smoke CI matrix | ✅ Present | `.github/workflows/ci.yml` runs fmt, strict lib clippy, workspace clippy, API contract, concurrency contract, test hygiene, coverage, integration harness modes, and Playwright. |
| Forbidden-import CI check | ✅ Present | `ARCHITECTURE_GUARDRAILS.md` documents the boundary rules, and CI runs `scripts/source-audit.sh` to reject `src/media` imports from API modules. |
| Regression fixture preservation | ✅ Present | `docs/regression-artifacts.md` links the specific historical failure classes from `impl.md` to checked-in fixtures, harness replay paths, generated-artifact locations, and proof gates; `docs/testing.md` and `ARCHITECTURE_GUARDRAILS.md` link the index. |

**Verdict**: Complete for the phase scope. CI enforces source inventory and
dependency-direction guardrails, the smoke matrix is broad, and the named
historical failure artifacts are linked into the regression-fixture
documentation without committing generated run directories.

---

### Phase 1 — Core Contracts

| Artifact / criterion | Status | Evidence |
|---|---|---|
| Typed IDs | ✅ Present | `src/domain/ids.rs` defines `PipelineId`, `OutputId`, `StageId`, `IngestId`, `RecordingId`, `JobId`. |
| Typed states | ✅ Present | `src/domain/state.rs` defines `DesiredOutputState`, `EgressPhase`, `StagePhase`, `IngestPhase`, `RecordingPhase`, `JobStatus`, `HealthState`. |
| Runtime errors | ✅ Present | `src/domain/errors.rs` defines `StageError` and `RuntimeError`. |
| `StageRuntimeSnapshot` | ✅ Present | `src/runtime/stage.rs`, including phase serialization and capacity fields. |
| `OutputRuntimeExplanation` | ✅ Present | `src/runtime/output.rs` and API status wiring. |
| No new code writes raw string states except at DB/API boundary | ✅ Complete | `types::Output.desired_state` is `DesiredOutputState`, `types::Job.status` is `JobStatus`, reconciliation and graph/runtime comparisons use enums directly, active/recent egress status/phase are typed, and runtime egress phase update APIs now accept `EgressPhase` instead of raw strings. API payload validation still accepts/serializes strings at the edge. |

**Verdict**: **Complete for the phase scope**. Contracts exist and the main
runtime/application state transitions now use typed state; string conversion is
kept at DB/API boundaries or diagnostic labels rather than lifecycle state.

---

### Phase 2 — Centralized Config

| Artifact / criterion | Status | Evidence |
|---|---|---|
| `AppConfig::from_env()` | ✅ Present | `src/config.rs` centralizes many runtime settings. |
| Per-stage backend flags | ✅ Present | `BackendPolicy` has `internal_video_presets`, `internal_hevc_to_h264`, `internal_hls_preview`, `internal_complex_audio`. |
| Runtime receives typed config | ✅ Complete for production runtime paths | `MediaEngine` carries config; graph planning uses `engine.config.backend_policy`; recording remux receives explicit `recording_threads`; HLS stores, file-ingest backend selection, AVIO queues, source/transcoder rings, SRT TS chunk rings, and external FFmpeg capacity snapshots use engine-owned typed config. |
| No env reads outside config/startup/test harness | ✅ Complete for phase scope | `scripts/source-audit.sh` reports no raw `std::env::var` usage outside configuration; the remaining production constructor call is `MediaEngine::new()` delegating to `AppConfig::from_env()`, and direct env reads outside config are limited to startup, MCP, tests, and harness/process utilities. |
| Startup logs show effective config | ✅ Present | Startup emits `restream.config.effective` with a redacted `AppConfig::effective_summary()` covering ports, tuning, paths, logging, backend policy, FFmpeg, buffers, SRT, and RTMP settings. |

**Verdict**: **Complete for the phase scope**. Production runtime env parsing is
centralized in `src/config.rs`, startup emits a comprehensive redacted effective
config summary, and runtime media capacities/configuration now flow from
`MediaEngine.config`. The remaining `AppConfig::from_env()` uses are the
startup/default constructor and a test-only SRT bonding helper, not production
runtime compatibility readers.

---

### Phase 3 — API Split Into Route Modules

| Artifact / criterion | Status | Evidence |
|---|---|---|
| Route modules exist | ✅ Complete | `src/api/{router,state,auth,pipelines,outputs,ingests,file_ingest,media_library,hls,health,logs,alerts,telemetry,settings,agent,static_assets}.rs`. |
| `api.rs` thin or gone | ✅ Complete | `src/api/mod.rs` is the module index. |
| Route behavior preserved | ✅ Tested | API tests cover health, graph, alerts, logs, pipelines, outputs, ingests, HLS, etc. |

**Verdict**: **Complete** for the phase scope.

---

### Phase 4 — Application Service Layer

| Criterion | Status | Evidence |
|---|---|---|
| Services exist | ✅ Present | `src/application/services/*` includes pipeline, output, ingest, file ingest, media library, settings, health, auth, logs, runtime view, and agent context catalog assembly; `application::graph` owns desired graph planning for pipeline graph/diagnostics read models; `RuntimeViewService` owns live health/status/graph/telemetry runtime read-model access; `OutputService` owns pipeline-scoped output reads for graph/diagnostics/detail routes; `SettingsService` owns settings PATCH persistence, recording-enabled maps, and SRT ingest policy refresh; `AgentService` owns context/catalog reads through repository ports; `FileIngestService` owns file-ingest start/stop/delete orchestration, pipeline-file-ingest persistence/read models, and FFmpeg argument/process setup through ingest/pipeline ports; `MediaLibraryService` owns recording metadata lookup through `RecordingStore`, media-library list read models, recording companion artifact planning, media delete execution, media rename execution, and ingest retargeting after rename. |
| Handlers no longer call SQL directly | ✅ Complete | `rg` over `src/api` and `src/api_runtime_views` finds no direct `db::*` calls or SQLite repository construction; logs/auth/settings/output mutations, agent context/catalog/plan reads, media-library read models/deletes/renames, SRT policy refresh, and recording-enabled maps delegate through services. |
| Handlers do not call low-level media constructors | ✅ Complete for phase scope | `api/hls.rs` delegates to `application::hls_preview`; pipeline graph/diagnostics desired-plan selection moved into `application::graph`; pipeline-scoped output reads use `OutputService::list_for_pipeline`; file-ingest start/stop/delete plus pipeline-file-ingest persistence/read models live in `FileIngestService`; media-library list read models, recording companion artifact planning, delete execution, rename execution, and ingest retargeting live in `MediaLibraryService`; live health/status/graph/telemetry read-model access goes through `RuntimeViewService` instead of direct route calls into runtime adapters. |
| Services testable without Axum request types | ✅ Complete for phase scope | Service structs and use-case APIs do not depend on Axum extractors or request types; the only application-layer Axum dependency is the shared error response adapter at the HTTP boundary. |

**Verdict**: **Complete for the phase scope**. Route modules now validate/auth
and delegate persistence, runtime read models, graph desired-plan selection,
file ingest, media-library operations, settings, logs, health, auth, and agent
catalog/mutation work through application services.

---

### Phase 5 — Repository Modules and Persistence Cleanup

| Criterion | Status | Evidence |
|---|---|---|
| `db/` repository modules exist | ✅ Complete | `db/{pipeline_repo,output_repo,ingest_repo,job_repo,session_repo,meta_repo,log_repo,recording_repo,schema,migrations}.rs`. |
| `db.rs` is only module index / pool / schema helper | ✅ Complete | `src/db/mod.rs` is thin and re-exports repositories plus pool/schema helpers. |
| Application services depend on repository traits | ✅ Complete for phase scope | `PipelineService` and `HealthService` depend on `PipelineStore`, `OutputService` depends on `OutputStore`, `IngestService` depends on `IngestLookup`/`IngestWriter`, `LogService` depends on `LogStore`, `AuthService` depends on meta/session ports, `SettingsService` depends on meta/ingest-host/job ports, `AgentService` depends on pipeline/output/job/ingest/meta ports, `FileIngestService` depends on ingest/pipeline ports, and `MediaLibraryService` uses meta and recording ports for recording settings and recording metadata. SQL pool constructors remain convenience adapters around those ports, not route-owned DB calls. |
| String states converted at repository boundary | ✅ Complete for phase scope | `recording_repo` maps `RecordingPhase`, `output_repo` maps SQLite `desired_state` text into `DesiredOutputState`, and `job_repo` maps SQLite `status` text into `JobStatus`; API payloads still serialize strings at the edge. |

**Verdict**: **Complete for the phase scope**. Repository files exist, service
read/write dependencies are port-backed, and the main persisted state strings
are converted at repository/API boundaries.

---

### Phase 6 — Runtime Graph Plan as Single Planning Model

| Criterion | Status | Evidence |
|---|---|---|
| `StageGraphPlan`, `GraphRole`, `StagePlan` | ✅ Present | `src/runtime/graph.rs`. |
| Output graph planner | ✅ Present | `planner::graph_plan::plan_pipeline_graph()`. |
| HLS preview planner | ✅ Present | `planner::graph_plan::plan_hls_preview_graph()` and `planner/hls_preview.rs`. |
| HLS output and recording planned by same graph | ✅ Complete for phase scope | HLS output terminal-stage preparation uses `plan_hls_output_graph()` and `GraphRole::HlsOutput`, persistent HLS output segmenters register lifecycle/metrics under the planned protocol segmenter key fed by the prepared media stage, and HLS upload start validates the same graph terminal key as the egress registration; recording lifecycle registration, writer start identity, and graph rendering use `plan_recording_graph()` and `GraphRole::Recording`. |
| Diagnostics/harness/agent preview use same planner | ✅ Present | Graph API, diagnostics, agent graph/impact preview, and mixed harness stage-count expectations consume `StageGraphPlan`; diagnostics and `/graph` now expose per-output desired graphs that preserve HLS-output roles; no harness stage-count proof imports `OutputPath`. |
| Stage-sharing tests compare against graph planner | ✅ Present | Mixed harness expected stage counts are compared with `plan_pipeline_graph()` and duplicate-output sharing in `mixed_manifest` tests. |

**Verdict**: **A-grade for the phase scope**. Output execution, graph rendering,
diagnostics, HLS preview planning, HLS output terminal-stage/per-output
diagnostic planning with protocol segmenter nodes and segmenter lifecycle
identity/uploader contracts, recording terminal-stage/lifecycle planning plus
writer start identity, agent preview, and harness stage-sharing proof all use
the graph planner as the authoritative planning model.

---

### Phase 7 — First-Class Stage Lifecycle

| Criterion | Status | Evidence |
|---|---|---|
| Stage lifecycle tracking | ✅ Present | `src/media/stage_lifecycle.rs` and lifecycle snapshots. |
| Stage runtime manager | ✅ Present | `src/media/stage_runtime.rs` owns `ensure_stage()` / `spawn_stage()`. |
| Capacity wait visible and cancellation-aware | ✅ Present | `external_transcoder.rs` transitions to `WaitingForCapacity` and waits with `tokio::select!`. |
| Capacity metrics in snapshots | ✅ Present | `StageRuntimeSnapshot` includes total/available permits and wait duration. |
| Stage events beyond `StageStarted` | ✅ Present | `events.rs` has `StageRegistered`, `StageWaitingForCapacity`, `StageBackendSpawned`, `StageFirstInput`, `StageFirstOutput`, `StageFailed`, `StageStopped`. |
| Wrap current stage maps into a single `StageRuntime` map | ✅ Complete for phases 1-12 | `StageRegistry.runtimes` now stores the authoritative runtime object with optional ring, cancel token, lifecycle, metrics, input queue, and pipe metrics. Shared FFmpeg stages are ring-backed runtimes; HLS segmenters and recording writers are non-ring runtimes. The old transcoder buffer map plus pipe-metrics/input-queue side maps are retired, and lifecycle/metrics side maps are compatibility fallback paths rather than production ownership for shared FFmpeg, HLS, or recording stage families. |
| Existing `StageStarted` semantics removed | ✅ Complete | New event names exist; no `StageStarted` variant found. |

**Verdict**: **A-grade for the phase scope**. Lifecycle observability is real,
and shared FFmpeg stages now use the first-class runtime object as the
ring/cancellation/lifecycle/metrics/input-queue/pipe-metrics authority through
the split `media::stage_registry_access` module for runtime-backed accessors,
snapshots, health/status, telemetry, and graph reads. HLS segmenters and
recording writers now use non-ring `StageRuntime` entries, so the runtime
registry owns their lifecycle and metrics without pretending they have output
rings.

---

### Phase 8 — Dependency-Aware Output Status

| Criterion | Status | Evidence |
|---|---|---|
| Terminal stage key on egress registration | ✅ Present | `ActiveEgress.terminal_stage_key`. |
| `OutputRuntimeExplanation` in API status | ✅ Present | `api_runtime_views/status.rs` fills `value["explanation"]`. |
| `blockedBy` stage snapshot | ✅ Present | `egress_runtime_json()` serializes `blockedBy` via `StageRuntimeSnapshot::to_json()`. |
| Common upstream-wait phase | ✅ Present | `waitingUpstream` is used when egress waits on upstream readiness. |
| Harness progress failures consume dependency status | ✅ Present | `src/bin/test_harness.rs` prints `terminalStage`, `blockedBy`, `blockedByPhase`, backend, waitMs, and lastError. |

**Verdict**: **Complete for the phase scope**. This phase meets its main
operator-facing goal, and runtime egress lifecycle state is now typed
internally.

---

### Phase 9 — FFmpeg Narrow Waist

| Criterion | Status | Evidence |
|---|---|---|
| Shared FFmpeg plan/backend/input/output/timeline modules | ✅ Present | `src/media/ffmpeg/{backend,stage_plan,stage_input,stage_output,timeline,operation,operation_compiler}.rs`. |
| External backend uses shared contracts | ✅ Present | `run_external_ffmpeg_backend()` takes `FfmpegStagePlan`, `StageInputPump`, `StageOutputNormalizer`, `StageRunContext`. |
| Internal backend uses shared trait | ✅ Present | `InternalFfmpegBackend` implements `FfmpegStageBackend`. |
| Per-stage internal/backend policy | ✅ Present | `BackendPolicy` per stage family. |
| No backend writes directly to `RingBuffer` | ✅ Complete | Backends receive `StageInputPump` plus `StageOutputNormalizer`; `StageOutputNormalizer::output_ring()` and `StageInputPump::source_ring()` are gone, and internal dispatch passes an existing normalizer through `StageOutputSink`. |
| Legacy compatibility paths gone | ✅ Complete | External wrapper functions are gone, input/output ring escape hatches are gone, and internal backend bodies are named as implementation functions (`run_internal_video_stage`, `run_h264_codec_edge_stage`) rather than legacy `start_*_inner` bridge entry points. |

**Verdict**: **Complete for the phase scope**. Internal and external FFmpeg
paths now enter through the shared plan/backend/input/output contracts, and the
legacy ring escape hatches have been removed.

---

### Phase 10 — HLS Preview Joins Graph Runtime

| Criterion | Status | Evidence |
|---|---|---|
| `GraphRole::HlsPreview` | ✅ Present | `runtime/graph.rs`. |
| HLS preview planning | ✅ Present | `planner::graph_plan::plan_hls_preview_graph()` now models H264 as `source -> fMP4 segmenter` and HEVC as `source -> preview -> fMP4 segmenter`, and `media/hls_preview_runtime.rs::MediaEngine::ensure_hls_preview_runtime()` owns preview graph planning, store/cancel setup, segmenter task spawning, and active preview stage-key reporting. |
| API no longer directly creates preview ring/backend | ✅ Complete | `api/hls.rs` delegates preview startup, playlist/segment reads, and blocked-cause selection to `application::hls_preview`; it only handles auth, path extraction, and HTTP response mapping. |
| Runtime/application service owns preview orchestration | ✅ Present | `application/hls_preview.rs` owns request/serving policy, while `media/hls_preview_runtime.rs` owns preview graph planning, store/cancel setup, and fMP4 segmenter spawning. |
| Actual keys in health match spawned keys | ✅ Tested | Engine tests cover `active_hls_preview_stage_keys_*` through the same `plan_hls_preview_graph()` contract used by preview startup. |
| HLS blocked-stage cause surfaced | ✅ Tested | Application and API tests cover HLS playlist blocked-stage cause, and engine tests prove blocked preview causes come from graph-planned stage keys rather than preview-name heuristics. |

**Verdict**: **A-grade for the phase scope**. API handlers no longer construct
preview rings/backends or read preview stores directly; preview startup,
segmenter spawning, playlist/segment serving policy, blocked-cause selection,
and health key reporting now flow through the application/runtime graph path.

---

### Phase 11 — Recording Lifecycle and Metadata

| Criterion | Status | Evidence |
|---|---|---|
| Recording ID and phase types | ✅ Present | `RecordingId`, `RecordingPhase`. |
| Recording metadata table | ✅ Present | `db/schema.rs` creates `recordings`. |
| Recording repository | ✅ Present | `db/recording_repo.rs` with create/update/list/delete tests. |
| Runtime writes lifecycle metadata | ✅ Present | `media/recording.rs` emits recording metadata lifecycle events, and `application::recording` persists start/finalize/failure rows through `recording_repo`. |
| Media API returns metadata including pipeline/status | ✅ Present | `/api/v1/media` attaches persisted `recordingId`, `pipelineId`, status, timing, codec, and error fields via `MediaLibraryService::recording_metadata_by_filename()`. |
| Harness filters by pipeline/recording ID first | ✅ Complete | Mixed harness recording checks snapshot API media recording identities, selects new entries by `pipelineId`/`recordingId`, rejects `.tmp.mp4`, and no longer falls back to filename-token matching for metadata-less entries. |

**Verdict**: **Complete for the phase scope**. Recording metadata is persisted,
surfaced in the product API, and consumed as the mixed harness recording
identity. Filename-token matching is no longer used as a compatibility fallback
for metadata-less entries.

---

### Phase 12 — Health, Alerts, and Diagnostics v2

| Criterion | Status | Evidence |
|---|---|---|
| Stage snapshots in health | ✅ Complete | `api_runtime_views/status.rs` uses `StageRuntimeSnapshot::to_json()`. |
| Dependency chain in output status | ✅ Complete | `blockedBy`, `terminalStage`, and `explanation` are present. |
| Backend capacity metrics in health | ✅ Complete | `capacityPermitsTotal`, `capacityPermitsAvailable`, `capacityWaitMs`. |
| Ring reader lag | ✅ Complete | Health and graph expose reader `lagSlots`, overflow count, packet age. |
| Keyframe wait information | ✅ Complete for Phase 12 | Stage phases include `waitingForKeyframe`; health serializes the phase, and HLS/preview alerts derive recommended actions from it. Broader source GOP analysis remains adjacent diagnostics depth, not a Phase 12 blocker. |
| Alerts derive from causal fields | ✅ Complete for listed tasks | `alerts.rs` covers output blocked by stage, capacity wait, input/no-output, preview keyframe wait, SRT drops, and ring lag, with recommended actions. |
| `/api/v1/pipelines/:id/graph` endpoint | ✅ Present | `api/pipelines.rs::pipeline_graph_handler` and `api_runtime_views::processing_graph()`. |
| Graph endpoint shows desired and runtime graph | ✅ Complete | `/graph` preserves legacy `nodes`/`edges` and adds `desiredGraph` plus `runtimeGraph`. |
| Diagnostics endpoint includes graph plan/runtime/stderr/events/logs | ✅ Complete | `/diagnostics/context` returns health, desired/runtime graph, alerts, recent events, relevant logs, and backend stderr tail. The SSE diagnostics probe remains separate. |

**Verdict**: **Phase 12 is now A-grade for health, alerts, and the causal
diagnostics bundle**. Remaining adjacent work belongs mostly to Phase 6 planner
convergence and later harness/reporting phases.

---

## Phases 13-16 Snapshot

| Phase | Status | Notes |
|---|---|---|
| Phase 13 — Harness v2 reporting | ✅ Complete | Harness now has `HarnessOutputCell`, `HarnessOutputRegistry`, per-scenario `outputs.json`, scenario result embedding, semantic cell labels in progress stalls, matrix `root-cause-summary.json` grouping, schema-versioned assertion rows, per-scenario `artifact-index.json` with file metadata/checksums, and probe failure API snapshots carrying output status plus engine health. |
| Phase 14 — Agent/MCP cleanup | ✅ Complete | Shared agent command/query DTOs live in `agent_core::types`, HTTP/execution modules re-export those DTOs instead of duplicating structs, MCP backends consume the same shared types, agent context/catalog reads use port-backed `AgentService`, agent output mutations use `OutputService`, agent graph/impact preview use `StageGraphPlan`, and agent API read/plan paths no longer import media internals. |
| Phase 15 — Large-file split | ✅ Complete | No Rust or TypeScript source file now exceeds the 2,000-line ideal, and `scripts/source-audit.sh` enforces that cap while reporting the largest current files in `target/source-audit.json`. The final split set covers runtime snapshots, HLS lifecycle/consumer ownership, engine test modules, MPEG-TS codec probing/tests, external FFmpeg process/argument helpers, SRT egress/policy/stream-id/monitor/tests, RTMP FLV/tests/egress transport, mixed matrix orchestration, harness core/sinks/HLS PUT/media probes/fault recovery/live modes/resource sweep/suite helpers, and the test-harness root. |
| Phase 16 — Rollout policy | ✅ Complete | Per-stage backend policy is implemented and tested, the legacy global internal-transcoder switch is ignored, runtime graph stage nodes expose lifecycle/capacity details, the UI renders those details, and CI has a blocking internal-backend rollout smoke lane. Live external-capacity evidence now includes `scripts/check-external-capacity-rollout.sh`, which proves a capacity-ok external run passes and a constrained default, recording-inclusive run for `mixed.live.srt.h264.a2.bf0` persists a `ready` recording metadata row before failing causally with `waitingForCapacity`, `backend=externalFfmpeg`, and nonzero `waitMs` instead of an unknown stall. Current internal smoke evidence includes the file-loop/timestamp case passing, `RESTREAM_INTERNAL_VIDEO_PRESETS=1 ONLY_CHECKS=load,ffprobe,decode-scan scripts/run-bench-harness.sh mixed.live.srt.h264.a1.bf0` passing with `passed: true` on the current tree, `RESTREAM_INTERNAL_VIDEO_PRESETS=0 RESTREAM_INTERNAL_HEVC_TO_H264=1 ONLY_CHECKS=load,ffprobe,decode-scan,stage-sharing scripts/run-bench-harness.sh mixed.live.srt.h265.a2.bf2` passing with all 48 assertions green, and `scripts/check-internal-video-preset-rollout.sh` passing the four-case H.264 SRT decode-scan matrix against `test/harness/baselines/internal-video-presets-rss.csv` with zero external FFmpeg children. |

---

## Critical Remaining Gaps

### P0 — Phase 1-12 Correctness Blockers Cleared

1. **Single graph planner convergence is now complete for Phase 6 scope**
   - Output preparation, graph rendering, diagnostics, HLS preview, agent
     graph/impact preview, HLS output terminal-stage preparation, recording
     lifecycle registration, and harness stage-count tests now use or prove
     `StageGraphPlan`, and persistent HLS segmenter lifecycle now uses the
     planned protocol stage key, and recording writer start now receives the
     planned recording stage key, and HLS upload start validates the planned
     HLS terminal stage key.

2. **Typed state adoption is complete for the phase scope**
   - Active egress status/phase, recent egress status/raw status/phase, output
     desired state, job status, and egress phase transitions are now typed.

### P1 — Layering and Ownership

3. **API/service/repository boundary is complete for Phase 4 scope**
   - Route modules exist, agent context catalog reads now use port-backed `AgentService`,
     and file-ingest start/stop/delete plus pipeline-file-ingest persistence/read
     models now live in `FileIngestService`, and media-library recording
     list read models, companion artifact planning, and delete/rename execution
     now live in
     `MediaLibraryService`, and agent catalog/plan reads now go through
     port-backed `AgentService`; file-ingest orchestration now reuses injected
     ingest/pipeline ports instead of constructing repositories inside methods;
     media-library recording metadata and recording settings now flow through
     recording/meta ports; settings-backed helper paths for SRT policy refresh
     and recording-enabled maps now go through `SettingsService`.
     Runtime health/status/graph/telemetry read models now go through
     `RuntimeViewService`; pipeline graph/diagnostics desired-plan selection
     lives in `application::graph`. Agent output mutations use
     `OutputService`, and `PipelineService`, `OutputService`, `IngestService`,
     `HealthService`, `LogService`, `AuthService`, `SettingsService`, and the
     runtime read-model facade are now service-backed.

### P2 — Guardrails and Large-File Debt

6. **Large-file debt cleared for Phase 15**
   - Rust and TypeScript source files are all under the 2,000-line ideal, and
     `scripts/source-audit.sh` now fails if any checked `src` or `public/ts`
     source file exceeds that cap. The split files now map to concrete
     responsibilities instead of arbitrary chunks: runtime/HLS/test ownership,
     protocol helpers, RTMP egress transport, external FFmpeg process helpers,
     mixed matrix orchestration, harness core/sinks/probes/modes/suite, and
     engine lifecycle versus stage tests.

7. **Harness reporting is now complete for Phase 13 scope**
   - The harness now persists output-cell identity through `outputs.json` and
     uses semantic cell labels in progress stalls; matrix progress and final
     summaries now include `root-cause-summary.json` grouped by repeated
     failure class, assertion rows now carry a stable schema version, and
     scenario artifacts now include `artifact-index.json` with path, existence,
     size, and SHA-256 metadata. Probe failures now enrich assertion rows with
     the semantic cell, output status snapshot, and engine health snapshot.

---

## Summary Scorecard

| Phase | Current Grade | Honest Status |
|---|---:|---|
| Ph 0 Guardrails | A | Source audit, forbidden-import guardrails, broad CI smoke gates, source-audit inventory with a 2,000-line source cap, and historical failure artifact links are wired. |
| Ph 1 Core contracts | A | Types exist, output desired-state, job status, and active/recent egress lifecycle state are typed; string conversion is now kept at DB/API edges. |
| Ph 2 Config | A | Production env parsing is centralized in config, startup logs a comprehensive redacted effective-config summary, and runtime media paths receive typed config for recording remux, HLS stores, file-ingest backend selection, AVIO queues, rings, SRT TS chunk rings, and external FFmpeg capacity reporting. |
| Ph 3 API split | A | Route module split is complete. |
| Ph 4 App services | A | Logs, auth initialization, settings reads/writes, pipeline, output, ingest, health checks, media-library operations, pipeline-scoped output reads, graph desired-plan selection, runtime health/status/graph/telemetry read models, and agent catalog/plan reads/output mutations are service-backed. |
| Ph 5 Repositories | A | Repo modules exist, pipeline/output/ingest/health/log/auth/settings/agent/file-ingest/media-library services are port-trait backed, and output/job/recording state maps at repository boundaries. |
| Ph 6 Graph planner | A | Planner drives output preparation, HLS output terminal-stage prep, persistent HLS segmenter lifecycle/uploader identity, per-output HLS diagnostic graphs with protocol segmenter nodes, recording lifecycle/writer start identity, graph rendering, diagnostics, HLS preview planning, agent graph/impact preview, and harness stage-count expectations. |
| Ph 7 Stage lifecycle | A | Lifecycle/capacity visibility is strong and shared FFmpeg stages now use first-class ring-backed `StageRuntime` objects as the ring/cancellation/lifecycle/metrics/input-queue/pipe-metrics authority through `media::stage_registry_access`; HLS and recording use non-ring `StageRuntime` entries, so lifecycle/metrics side maps are compatibility fallback paths rather than production ownership for phase-scope stage families. |
| Ph 8 Dependency-aware status | A | Operator-facing dependency status is complete for the phase scope, with typed internal egress lifecycle state. |
| Ph 9 FFmpeg waist | A | Shared FFmpeg plan/backend/input/output contracts are the backend entry path, and legacy input/output ring escape hatches are removed. |
| Ph 10 HLS preview | A | API one-off removed; preview startup/spawn, playlist/segment serving policy, blocked-cause selection, and health keys share the application/runtime graph path. |
| Ph 11 Recording metadata | A | Media API consumes persisted recording metadata and mixed harness now requires pipeline/recording identity; filename-token matching fallback has been removed. |
| Ph 12 Health/alerts/diagnostics | A | Health, alerts, graph, and the causal diagnostics context bundle meet the Phase 12 acceptance criteria; the legacy SSE diagnostics probe remains as an active probe path beside the read-only context endpoint. |
| Ph 13 Harness v2 | A | Output-cell registry, `outputs.json`, semantic progress-stall labels, matrix root-cause grouping, assertion schema versioning, artifact indexing, and probe failure API snapshots are implemented. |
| Ph 14 Agent/MCP cleanup | A | Shared DTOs live in `agent_core::types`; MCP and HTTP/execution share command/query payloads where feature boundaries permit; agent graph/impact preview uses the shared planner; agent reads use service/runtime read models; agent API read/plan paths have no direct media-internal imports. |
| Ph 15 Large-file split | A | No Rust or TypeScript source file exceeds 2,000 lines; `scripts/source-audit.sh` now enforces that cap, and the extracted modules are below the ideal cap through responsibility-based splits. |
| Ph 16 Rollout policy | A | Per-stage policy, runtime graph lifecycle rollout, blocking internal backend smoke CI, HEVC-to-H264 RTMP selected-audio decode-scan, internal video-preset SRT decode-scan/RSS promotion proof, and the external constrained-capacity rollout proof are implemented and tested. Live evidence proves the external constrained-capacity path surfaces causal `waitingForCapacity` with backend/wait details after recording metadata is persisted. |

---

## Answer to “Did We Truly Finish Phases 1-12?”

Yes for the phase-scope acceptance criteria, with the usual caveat that this is
not the whole roadmap. Phases 1-12 now have A-grade implementation evidence:
typed contracts and state boundaries are in place, config and repository
ownership are centralized, routes delegate through services, graph planning is
the active planning model for outputs/HLS/recording/diagnostics/harness/agent
preview, stage lifecycle is first-class for both ring-backed and non-ring stage
families, output status is dependency-aware, FFmpeg execution goes through the
shared waist, HLS preview and recording identity are graph/metadata-driven, and
health/alerts/diagnostics expose causal runtime state.

The phase-scope work in this document is now A-grade through Phase 16. Larger
roadmap work can continue, but it is no longer a blocker for the Phases 1-16
acceptance criteria audited here.
