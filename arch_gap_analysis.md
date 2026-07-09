# Architecture Gap Analysis: Current Code vs. Ideal State

> **Reference documents**: [arch.md](arch.md) · [impl.md](impl.md)
>
> Audit standard: this is a deep implementation audit, not a symbol-existence
> checklist. A phase is marked complete only when the current code satisfies
> the phase acceptance criteria and the stronger architectural intent in
> `arch.md`.

---

## Executive Summary

The codebase has made substantial progress through Phases 1-12, and the most
important Phase 12 alert gaps have now been closed: health exposes stage
snapshots, output status carries `blockedBy`, alerts derive from causal fields,
`/api/v1/pipelines/:id/graph` exists, and the diagnostics context endpoint now
bundles graph, health, alerts, events, relevant logs, and backend stderr tail.

However, **Phases 1-12 are not all truly complete in the ideal architecture
sense**. Several phases have strong scaffolding but incomplete adoption:

- typed contracts exist, and output desired state plus job status are now typed
  through the application/repository boundary;
- configuration is centralized for startup/runtime config, but env parsing still
  exists outside the central `AppConfig` path;
- API route modules exist, and direct SQL calls have mostly moved behind
  services, but some state/helper paths still construct persistence ports
  directly;
- application services exist, and the main pipeline/output/ingest/health/log/auth
  paths are port-backed, and some helper paths still construct repositories
  directly;
- a graph planner exists and now drives output preparation, graph rendering,
  HLS preview, HLS output terminal-stage preparation, diagnostics, agent
  previews, recording terminal-stage/lifecycle registration, and harness
  stage-count expectations, but the recording writer and HLS segmenter/uploader
  service boundaries are not yet fully graph-runtime driven;
- stage lifecycle and FFmpeg narrow-waist contracts exist, but some legacy
  compatibility paths and direct ring writes remain;
- recording metadata exists in the database, and the product/harness path now
  requires recording metadata identity instead of filename-token matching;
- diagnostics now expose the Phase 12 causal context bundle, while the legacy
  SSE check endpoint remains a separate active-ingest probe.

Bottom line: **the codebase has not completed a full "Amit Singhal" pass for
Phases 1-12**. The current honest state is: strong progress, several production
features complete, but broad architectural convergence is still partial.

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
| No env reads outside config/startup/test harness | ✅ Mostly | Direct `std::env::var` usage is mostly centralized or excluded to startup/test/process utilities. Remaining `AppConfig::from_env()` hits in media are `MediaEngine::new()` startup/default construction and one SRT bonding test helper. |
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
| Services exist | ✅ Present | `src/application/services/*` includes pipeline, output, ingest, file ingest, media library, settings, health, auth, logs, and agent context catalog assembly; `application::graph` now owns desired graph planning for pipeline graph/diagnostics read models; `SettingsService` now owns settings PATCH persistence, recording-enabled maps, and SRT ingest policy refresh; `AgentService` now owns context/catalog reads through repository ports; `FileIngestService` now owns file-ingest start/stop/delete orchestration, pipeline-file-ingest persistence/read models, and FFmpeg argument/process setup through ingest/pipeline ports; `MediaLibraryService` owns recording metadata lookup through `RecordingStore`, media-library list read models, recording companion artifact planning, media delete execution, media rename execution, and ingest retargeting after rename. |
| Handlers no longer call SQL directly | ✅ Complete | `rg` over `src/api` and `src/api_runtime_views` finds no direct `db::*` calls or SQLite repository construction; logs/auth/settings/output mutations, agent context/catalog/plan reads, media-library read models/deletes/renames, SRT policy refresh, and recording-enabled maps delegate through services. |
| Handlers do not call low-level media constructors | ⚠️ Mostly | `api/hls.rs` delegates to `application::hls_preview`; pipeline graph/diagnostics desired-plan selection moved into `application::graph`; file-ingest start/stop/delete plus pipeline-file-ingest persistence/read models moved into `FileIngestService`; media-library list read models, recording companion artifact planning, delete execution, rename execution, and ingest retargeting moved into `MediaLibraryService`. API/runtime read models still take `MediaEngine` for runtime snapshots and feature policy in other routes. |
| Services testable without Axum request types | ✅ Mostly | Service structs do not depend on Axum types. |

**Verdict**: **Partial**. The service layer exists, but handlers are not yet
thin adapters everywhere.

---

### Phase 5 — Repository Modules and Persistence Cleanup

| Criterion | Status | Evidence |
|---|---|---|
| `db/` repository modules exist | ✅ Complete | `db/{pipeline_repo,output_repo,ingest_repo,job_repo,session_repo,meta_repo,log_repo,recording_repo,schema,migrations}.rs`. |
| `db.rs` is only module index / pool / schema helper | ✅ Complete | `src/db/mod.rs` is thin and re-exports repositories plus pool/schema helpers. |
| Application services depend on repository traits | ✅ Mostly | `PipelineService` and `HealthService` depend on `PipelineStore`, `OutputService` depends on `OutputStore`, `IngestService` depends on `IngestLookup`/`IngestWriter`, `LogService` depends on `LogStore`, `AuthService` depends on meta/session ports, `SettingsService` depends on meta/ingest-host/job ports, `AgentService` depends on pipeline/output/job/ingest/meta ports, `FileIngestService` depends on ingest/pipeline ports, and `MediaLibraryService` now uses meta and recording ports for recording settings and recording metadata. |
| String states converted at repository boundary | ✅ Mostly | `recording_repo` maps `RecordingPhase`, `output_repo` maps SQLite `desired_state` text into `DesiredOutputState`, and `job_repo` maps SQLite `status` text into `JobStatus`. |

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
| HLS output and recording planned by same graph | ⚠️ Partial | HLS output terminal-stage preparation uses `plan_hls_output_graph()` and `GraphRole::HlsOutput`, and HLS output desired graphs now terminate at a protocol segmenter node fed by the prepared media stage; recording lifecycle registration and graph rendering use `plan_recording_graph()` and `GraphRole::Recording`; the recording writer and HLS segmenter/uploader execution boundaries are not yet fully graph-runtime driven. |
| Diagnostics/harness/agent preview use same planner | ✅ Present | Graph API, diagnostics, agent graph/impact preview, and mixed harness stage-count expectations consume `StageGraphPlan`; diagnostics and `/graph` now expose per-output desired graphs that preserve HLS-output roles; no harness stage-count proof imports `OutputPath`. |
| Stage-sharing tests compare against graph planner | ✅ Present | Mixed harness expected stage counts are compared with `plan_pipeline_graph()` and duplicate-output sharing in `mixed_manifest` tests. |

**Verdict**: **Mostly complete for output execution, graph rendering,
diagnostics, HLS preview planning, HLS output terminal-stage/per-output
diagnostic planning with protocol segmenter nodes, recording
terminal-stage/lifecycle planning, agent preview, and harness stage-sharing
proof; still partial for the recording writer and HLS segmenter/uploader
execution boundaries**.

---

### Phase 7 — First-Class Stage Lifecycle

| Criterion | Status | Evidence |
|---|---|---|
| Stage lifecycle tracking | ✅ Present | `src/media/stage_lifecycle.rs` and lifecycle snapshots. |
| Stage runtime manager | ✅ Present | `src/media/stage_runtime.rs` owns `ensure_stage()` / `spawn_stage()`. |
| Capacity wait visible and cancellation-aware | ✅ Present | `external_transcoder.rs` transitions to `WaitingForCapacity` and waits with `tokio::select!`. |
| Capacity metrics in snapshots | ✅ Present | `StageRuntimeSnapshot` includes total/available permits and wait duration. |
| Stage events beyond `StageStarted` | ✅ Present | `events.rs` has `StageRegistered`, `StageWaitingForCapacity`, `StageBackendSpawned`, `StageFirstInput`, `StageFirstOutput`, `StageFailed`, `StageStopped`. |
| Wrap current stage maps into a single `StageRuntime` map | ⚠️ Mostly | `StageRegistry.runtimes` now stores the authoritative runtime object with ring, cancel token, lifecycle, metrics, input queue, and pipe metrics for shared FFmpeg stages. The old transcoder buffer map plus pipe-metrics and input-queue side maps are retired, and runtime-backed health/status, telemetry, and graph reads use lifecycle and metrics from `StageRuntime` first. Lifecycle and metrics side maps remain for map-only HLS/recording stage families while ownership is migrated. |
| Existing `StageStarted` semantics removed | ✅ Mostly | New event names exist; no `StageStarted` variant found. |

**Verdict**: **Near A-grade, not ideal complete**. Lifecycle observability is
real, and shared FFmpeg stages now use the first-class runtime object as the
ring/cancellation/lifecycle/metrics/input-queue/pipe-metrics authority for
runtime-backed health/status, telemetry, and graph reads. Remaining work is
retiring the lifecycle/metrics side maps for map-only stage families and
extending the same runtime-object ownership to every stage family.

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
| Runtime writes lifecycle metadata | ✅ Present | `media/recording.rs` builds service metadata and updates lifecycle state. |
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
| Phase 13 — Harness v2 reporting | ❌ Not complete | Harness prints some dependency-chain fields, but `HarnessOutputCell`, `HarnessOutputRegistry`, root-cause summary, schema-versioned artifact index, and `outputs.json` contract are not present. |
| Phase 14 — Agent/MCP cleanup | ❌ Not complete | Agent output mutations now use `OutputService`, agent context/catalog reads use port-backed `AgentService`, and agent graph/impact preview use `StageGraphPlan`; shared DTO and MCP/HTTP boundary cleanup remains outside the Phase 1-12 scope. |
| Phase 15 — Large-file split | ❌ Not complete | Major files remain large: `test_harness.rs` ~10k lines, `engine.rs` ~6.4k, `srt.rs` ~4.6k, `mpegts.rs` ~4.0k, `rtmp.rs` ~3.6k, `external_transcoder.rs` ~2.8k. |
| Phase 16 — Rollout policy | ❌ Not complete | Not audited as implemented; internal backend remains policy-gated and parity work is ongoing. |

---

## Critical Remaining Gaps

### P0 — Highest-Value Architectural Correctness

1. **Single graph planner is not yet the one source of truth**
   - Output preparation, graph rendering, diagnostics, HLS preview, agent
     graph/impact preview, HLS output terminal-stage preparation, recording
     lifecycle registration, and harness stage-count tests now use or prove
     `StageGraphPlan`, but the recording writer and HLS segmenter/uploader
     service boundaries are not all unified behind one graph-plan contract.

2. **Typed state adoption is now complete for the phase scope, but not a substitute for planner convergence**
   - Active egress status/phase, recent egress status/raw status/phase, output
     desired state, job status, and egress phase transitions are now typed. The
     remaining P0 blocker is unified graph planning/execution rather than
     row-state typing.

### P1 — Layering and Ownership

3. **API/service/repository boundary remains mixed**
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
     The remaining Phase 4 ownership debt is API/runtime read models and
     mutation helper paths that still orchestrate runtime work directly; pipeline
     graph/diagnostics desired-plan selection now lives in `application::graph`.
     Agent output mutations now use
     `OutputService`, and `PipelineService`, `OutputService`, `IngestService`,
     `HealthService`, `LogService`, `AuthService`, and `SettingsService` are now
     port-trait backed.

### P2 — Guardrails and Large-File Debt

6. **Large files still dominate reasoning cost**
   - The Phase 15 split remains important once contract convergence is stronger.

7. **Harness semantic model is still incomplete**
   - The harness now consumes dependency-aware status in at least one progress
     path, but it lacks persisted output-cell identity and structured root-cause
     reporting.

---

## Summary Scorecard

| Phase | Current Grade | Honest Status |
|---|---:|---|
| Ph 0 Guardrails | A | Source audit, forbidden-import guardrails, broad CI smoke gates, source-audit inventory, and historical failure artifact links are wired. |
| Ph 1 Core contracts | A | Types exist, output desired-state, job status, and active/recent egress lifecycle state are typed; string conversion is now kept at DB/API edges. |
| Ph 2 Config | A | Production env parsing is centralized in config, startup logs a comprehensive redacted effective-config summary, and runtime media paths receive typed config for recording remux, HLS stores, file-ingest backend selection, AVIO queues, rings, SRT TS chunk rings, and external FFmpeg capacity reporting. |
| Ph 3 API split | A | Route module split is complete. |
| Ph 4 App services | A- | Logs, auth initialization, settings reads/writes, pipeline, output, ingest, health checks, media-library operations, graph desired-plan selection, and agent catalog/plan reads/output mutations are service-backed; API/runtime read models still contain direct runtime snapshot work. |
| Ph 5 Repositories | A | Repo modules exist, pipeline/output/ingest/health/log/auth/settings/agent/file-ingest/media-library services are port-trait backed, and output/job/recording state maps at repository boundaries. |
| Ph 6 Graph planner | A- | Planner drives output preparation, HLS output terminal-stage prep, per-output HLS diagnostic graphs with protocol segmenter nodes, recording lifecycle registration, graph rendering, diagnostics, HLS preview planning, agent graph/impact preview, and harness stage-count expectations; recording writer and HLS segmenter/uploader execution boundaries remain. |
| Ph 7 Stage lifecycle | A- | Lifecycle/capacity visibility is strong and shared FFmpeg stages now use first-class `StageRuntime` objects as the ring/cancellation/lifecycle/metrics/input-queue/pipe-metrics authority for runtime-backed health/status, telemetry, and graph reads; lifecycle/metrics side maps remain for map-only stage families during migration. |
| Ph 8 Dependency-aware status | A | Operator-facing dependency status is complete for the phase scope, with typed internal egress lifecycle state. |
| Ph 9 FFmpeg waist | A | Shared FFmpeg plan/backend/input/output contracts are the backend entry path, and legacy input/output ring escape hatches are removed. |
| Ph 10 HLS preview | A | API one-off removed; preview startup/spawn, playlist/segment serving policy, blocked-cause selection, and health keys share the application/runtime graph path. |
| Ph 11 Recording metadata | A | Media API consumes persisted recording metadata and mixed harness now requires pipeline/recording identity; filename-token matching fallback has been removed. |
| Ph 12 Health/alerts/diagnostics | A | Health, alerts, graph, and the causal diagnostics context bundle meet the Phase 12 acceptance criteria; the legacy SSE diagnostics probe remains as an active probe path beside the read-only context endpoint. |
| Ph 13 Harness v2 | D | Some dependency fields printed; semantic model missing. |
| Ph 14 Agent/MCP cleanup | D | Agent still crosses DB/API/runtime boundaries. |
| Ph 15 Large-file split | F | Not done. |
| Ph 16 Rollout policy | F | Not done. |

---

## Answer to “Did We Truly Finish Phases 1-12?”

No. We finished several important implementation slices, and Phases 3, 8, 10,
and the health/alerts portion of Phase 12 are in good shape. But a full,
architecture-grade pass across Phases 1-12 is not complete.

The main reason is not missing Rust structs. It is incomplete convergence:
older string-state, direct DB, direct media-engine, duplicated planning, and
compatibility backend paths still coexist with the new contracts. The next
highest-value work is to seal those seams rather than add more surface area.
