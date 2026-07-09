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
- API route modules exist, but several handlers still call `db::*` directly;
- application services exist, and the main pipeline/output/ingest/health/log/auth
  paths are port-backed, but settings still owns `SqlitePool` and some helper
  paths call repositories directly;
- a graph planner exists and now drives output preparation, graph rendering,
  HLS preview, HLS output terminal-stage preparation, diagnostics, agent
  previews, recording terminal-stage/lifecycle registration, and harness
  stage-count expectations, but the recording writer and HLS segmenter/uploader
  service boundaries are not yet fully graph-runtime driven;
- stage lifecycle and FFmpeg narrow-waist contracts exist, but some legacy
  compatibility paths and direct ring writes remain;
- recording metadata exists in the database, and the product/harness path now
  uses recording identity first with filename matching only as fallback;
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
| Source inventory doc / CI | ⚠️ Partial | `scripts/source-audit.sh` exists and checks forbidden imports, file size, and env reads, but it is not wired into CI and currently fails on large-file limits. |
| Smoke CI matrix | ❌ Missing | No repo evidence for the `impl.md` smoke CI matrix. |
| Forbidden-import CI check | ❌ Missing | No `ARCHITECTURE_GUARDRAILS.md`; no CI check for layer direction. |
| Regression fixture preservation | ⚠️ Unconfirmed | Some docs mention fixture discipline, but the listed known failure artifacts are not linked as a guardrail set. |

**Verdict**: Not started as a phase. Local checks are strong, but architectural
drift is not enforced by CI.

---

### Phase 1 — Core Contracts

| Artifact / criterion | Status | Evidence |
|---|---|---|
| Typed IDs | ✅ Present | `src/domain/ids.rs` defines `PipelineId`, `OutputId`, `StageId`, `IngestId`, `RecordingId`, `JobId`. |
| Typed states | ✅ Present | `src/domain/state.rs` defines `DesiredOutputState`, `EgressPhase`, `StagePhase`, `IngestPhase`, `RecordingPhase`, `JobStatus`, `HealthState`. |
| Runtime errors | ✅ Present | `src/domain/errors.rs` defines `StageError` and `RuntimeError`. |
| `StageRuntimeSnapshot` | ✅ Present | `src/runtime/stage.rs`, including phase serialization and capacity fields. |
| `OutputRuntimeExplanation` | ✅ Present | `src/runtime/output.rs` and API status wiring. |
| No new code writes raw string states except at DB/API boundary | ✅ Mostly | `types::Output.desired_state` is now `DesiredOutputState`, `types::Job.status` is now `JobStatus`, reconciliation and graph/runtime comparisons use enums directly, and active/recent egress status/phase are typed. API payload validation still accepts/serializes strings at the edge. |

**Verdict**: **Partial**. Contracts exist and are useful, but adoption is not
complete. The ideal contract boundary has not replaced string state in runtime
and application logic.

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
| Services exist | ✅ Present | `src/application/services/*` includes pipeline, output, ingest, file ingest, media library, settings, health, auth, logs. |
| Handlers no longer call SQL directly | ⚠️ Partial | Logs/auth/settings/output mutations delegate to services, including agent output add/update/remove/start/stop paths; `api/agent.rs` read/context helpers and some state/helper code still call `db::*` directly. |
| Handlers do not call low-level media constructors | ⚠️ Mostly | `api/hls.rs` delegates to `application::hls_preview`, but other API/runtime views still take `MediaEngine` directly for read models. |
| Services testable without Axum request types | ✅ Mostly | Service structs do not depend on Axum types. |

**Verdict**: **Partial**. The service layer exists, but handlers are not yet
thin adapters everywhere.

---

### Phase 5 — Repository Modules and Persistence Cleanup

| Criterion | Status | Evidence |
|---|---|---|
| `db/` repository modules exist | ✅ Complete | `db/{pipeline_repo,output_repo,ingest_repo,job_repo,session_repo,meta_repo,log_repo,recording_repo,schema,migrations}.rs`. |
| `db.rs` is only module index / pool / schema helper | ✅ Mostly | `src/db/mod.rs` is thin and re-exports repositories. |
| Application services depend on repository traits | ✅ Mostly | `PipelineService` and `HealthService` depend on `PipelineStore`, `OutputService` depends on `OutputStore`, `IngestService` depends on `IngestLookup`/`IngestWriter`, `LogService` depends on `LogStore`, `AuthService` depends on meta/session ports, and `SettingsService` depends on meta/ingest-host/job ports. |
| String states converted at repository boundary | ✅ Mostly | `recording_repo` maps `RecordingPhase`, `output_repo` maps SQLite `desired_state` text into `DesiredOutputState`, and `job_repo` maps SQLite `status` text into `JobStatus`. |

**Verdict**: **Partial**. Repository files exist, but port isolation and typed
state conversion are incomplete.

---

### Phase 6 — Runtime Graph Plan as Single Planning Model

| Criterion | Status | Evidence |
|---|---|---|
| `StageGraphPlan`, `GraphRole`, `StagePlan` | ✅ Present | `src/runtime/graph.rs`. |
| Output graph planner | ✅ Present | `planner::graph_plan::plan_pipeline_graph()`. |
| HLS preview planner | ✅ Present | `planner::graph_plan::plan_hls_preview_graph()` and `planner/hls_preview.rs`. |
| HLS output and recording planned by same graph | ⚠️ Partial | HLS output terminal-stage preparation uses `plan_hls_output_graph()` and `GraphRole::HlsOutput`; recording lifecycle registration and graph rendering use `plan_recording_graph()` and `GraphRole::Recording`; the recording writer and HLS segmenter/uploader service boundaries are not yet fully graph-runtime driven. |
| Diagnostics/harness/agent preview use same planner | ✅ Present | Graph API, diagnostics, agent graph/impact preview, and mixed harness stage-count expectations consume `StageGraphPlan`; no harness stage-count proof imports `OutputPath`. |
| Stage-sharing tests compare against graph planner | ✅ Present | Mixed harness expected stage counts are compared with `plan_pipeline_graph()` and duplicate-output sharing in `mixed_manifest` tests. |

**Verdict**: **Mostly complete for output execution, graph rendering,
diagnostics, HLS preview planning, HLS output terminal-stage planning, recording
terminal-stage/lifecycle planning, agent preview, and harness stage-sharing
proof; still partial for the recording writer and HLS segmenter/uploader service
boundaries**.

---

### Phase 7 — First-Class Stage Lifecycle

| Criterion | Status | Evidence |
|---|---|---|
| Stage lifecycle tracking | ✅ Present | `src/media/stage_lifecycle.rs` and lifecycle snapshots. |
| Stage runtime manager | ✅ Present | `src/media/stage_runtime.rs` owns `ensure_stage()` / `spawn_stage()`. |
| Capacity wait visible and cancellation-aware | ✅ Present | `external_transcoder.rs` transitions to `WaitingForCapacity` and waits with `tokio::select!`. |
| Capacity metrics in snapshots | ✅ Present | `StageRuntimeSnapshot` includes total/available permits and wait duration. |
| Stage events beyond `StageStarted` | ✅ Present | `events.rs` has `StageRegistered`, `StageWaitingForCapacity`, `StageBackendSpawned`, `StageFirstInput`, `StageFirstOutput`, `StageFailed`, `StageStopped`. |
| Wrap current stage maps into a single `StageRuntime` map | ⚠️ Mostly | `StageRegistry.runtimes` now stores a first-class runtime object with ring, cancel token, lifecycle, metrics, input queue, and pipe metrics for shared FFmpeg stages. Compatibility maps remain for existing call sites while ownership is migrated. |
| Existing `StageStarted` semantics removed | ✅ Mostly | New event names exist; no `StageStarted` variant found. |

**Verdict**: **Near A-grade, not ideal complete**. Lifecycle observability is
real, and shared FFmpeg stages now have first-class runtime objects. Remaining
work is retiring the compatibility maps and extending the same runtime-object
ownership to every stage family.

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
| HLS preview planning | ✅ Present | `planner::graph_plan::plan_hls_preview_graph()` now drives both preview stage creation and active preview stage-key reporting. |
| API no longer directly creates preview ring/backend | ✅ Mostly | `api/hls.rs` delegates to `application::hls_preview::ensure_hls_preview()`. |
| Runtime/application service owns preview orchestration | ✅ Present | `application/hls_preview.rs` plans preview and spawns fMP4 segmenter. |
| Actual keys in health match spawned keys | ✅ Tested | Engine tests cover `active_hls_preview_stage_keys_*` through the same `plan_hls_preview_graph()` contract used by preview startup. |
| HLS blocked-stage cause surfaced | ✅ Tested | API test covers HLS playlist blocked-stage cause. |

**Verdict**: **Largely complete**. Preview stage creation and health key
reporting now share the dedicated graph planner; remaining architectural cleanup
is that the application preview service still calls
`MediaEngine::ensure_hls_preview_segmenter()` and spawns the segmenter directly
rather than going through a fully isolated runtime graph service.

---

### Phase 11 — Recording Lifecycle and Metadata

| Criterion | Status | Evidence |
|---|---|---|
| Recording ID and phase types | ✅ Present | `RecordingId`, `RecordingPhase`. |
| Recording metadata table | ✅ Present | `db/schema.rs` creates `recordings`. |
| Recording repository | ✅ Present | `db/recording_repo.rs` with create/update/list/delete tests. |
| Runtime writes lifecycle metadata | ✅ Present | `media/recording.rs` builds service metadata and updates lifecycle state. |
| Media API returns metadata including pipeline/status | ✅ Present | `/api/v1/media` attaches persisted `recordingId`, `pipelineId`, status, timing, codec, and error fields via `MediaLibraryService::recording_metadata_by_filename()`. |
| Harness filters by pipeline/recording ID first | ✅ Present | Mixed harness recording checks snapshot API media recording identities, select new entries by `pipelineId`/`recordingId`, reject `.tmp.mp4`, and keep filename-token matching only as metadata-less fallback. |

**Verdict**: **Largely complete**. Recording metadata is persisted, surfaced in
the product API, and now consumed identity-first by mixed harness recording
checks. Filename-token matching remains only as a compatibility fallback for
metadata-less entries.

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
| Phase 14 — Agent/MCP cleanup | ❌ Not complete | Agent output mutations now use `OutputService`, and agent graph/impact preview use `StageGraphPlan`; agent context/read routes still call `db::*` directly and duplicate DTO boundaries. |
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

2. **Typed state adoption is now strong, but not a substitute for planner convergence**
   - Active egress status/phase, recent egress status/raw status/phase, output
     desired state, and job status are now typed. The remaining P0 blocker is
     unified graph planning/execution rather than row-state typing.

### P1 — Layering and Ownership

3. **API/service/repository boundary remains mixed**
   - Route modules exist, but `api/agent.rs` read/context helpers and some
     helper paths still call `db::*` directly. Agent output mutations now use
     `OutputService`, and `PipelineService`, `OutputService`, `IngestService`,
     `HealthService`, `LogService`, `AuthService`, and `SettingsService` are now
     port-trait backed.

4. **HLS preview is no longer an API one-off, but still not a pure graph service**
   - `application::hls_preview` owns orchestration, but it directly calls
     `MediaEngine` segmenter methods and spawns the segmenter task.

### P2 — Guardrails and Large-File Debt

6. **Architecture drift checks exist locally but are not CI-grade**
   - `scripts/source-audit.sh` exists, but it is not wired into CI and currently
     fails on large-file limits for `engine.rs` and `test_harness.rs`.

7. **Large files still dominate reasoning cost**
   - The Phase 15 split remains important once contract convergence is stronger.

8. **Harness semantic model is still incomplete**
   - The harness now consumes dependency-aware status in at least one progress
     path, but it lacks persisted output-cell identity and structured root-cause
     reporting.

---

## Summary Scorecard

| Phase | Current Grade | Honest Status |
|---|---:|---|
| Ph 0 Guardrails | F | Not started. |
| Ph 1 Core contracts | A | Types exist, output desired-state, job status, and active/recent egress lifecycle state are typed; string conversion is now kept at DB/API edges. |
| Ph 2 Config | A | Production env parsing is centralized in config, startup logs a comprehensive redacted effective-config summary, and runtime media paths receive typed config for recording remux, HLS stores, file-ingest backend selection, AVIO queues, rings, SRT TS chunk rings, and external FFmpeg capacity reporting. |
| Ph 3 API split | A | Route module split is complete. |
| Ph 4 App services | A- | Logs, auth initialization, pipeline, output, ingest, health checks, and agent output mutations are service-backed; agent read/context helpers still contain direct DB/application work. |
| Ph 5 Repositories | A | Repo modules exist, pipeline/output/ingest/health/log/auth/settings services are port-trait backed, and output/job/recording state maps at repository boundaries. |
| Ph 6 Graph planner | A- | Planner drives output preparation, HLS output terminal-stage prep, recording lifecycle registration, graph rendering, diagnostics, HLS preview planning, agent graph/impact preview, and harness stage-count expectations; recording writer and HLS segmenter/uploader boundaries remain. |
| Ph 7 Stage lifecycle | A- | Lifecycle/capacity visibility is strong and shared FFmpeg stages now have first-class `StageRuntime` objects; compatibility maps remain during migration. |
| Ph 8 Dependency-aware status | A | Operator-facing dependency status is complete for the phase scope, with typed internal egress lifecycle state. |
| Ph 9 FFmpeg waist | A | Shared FFmpeg plan/backend/input/output contracts are the backend entry path, and legacy input/output ring escape hatches are removed. |
| Ph 10 HLS preview | A- | API one-off removed and preview startup/health keys share the dedicated graph planner; runtime service boundary still not ideal. |
| Ph 11 Recording metadata | A- | Media API consumes persisted recording metadata and mixed harness now uses pipeline/recording identity first; filename-token matching remains only as fallback compatibility. |
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
