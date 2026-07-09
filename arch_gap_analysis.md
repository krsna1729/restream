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

- typed contracts exist, but runtime/application code still stores and compares
  raw string states in important paths;
- configuration is centralized for startup/runtime config, but env parsing still
  exists outside the central `AppConfig` path;
- API route modules exist, but several handlers still call `db::*` directly;
- application services exist, but most services still own `SqlitePool` and call
  repositories directly rather than depending on port traits;
- a graph planner exists and now drives output preparation, graph rendering,
  HLS preview, and diagnostics; harness expectations are pinned to it by tests,
  but recording and agent preview are not yet fully graph-plan driven;
- stage lifecycle and FFmpeg narrow-waist contracts exist, but some legacy
  compatibility paths and direct ring writes remain;
- recording metadata exists in the database, but the product/harness path still
  depends partly on filename matching;
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
| Source inventory doc / CI | ❌ Missing | No `scripts/source-audit.sh`; no generated architecture inventory found. |
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
| No new code writes raw string states except at DB/API boundary | ⚠️ Partial | Reconciliation now converts desired output state to `DesiredOutputState`; active egress status/phase and recent egress status/raw status/phase are typed. `types::Output.desired_state` still stores raw strings at the DB row boundary. |

**Verdict**: **Partial**. Contracts exist and are useful, but adoption is not
complete. The ideal contract boundary has not replaced string state in runtime
and application logic.

---

### Phase 2 — Centralized Config

| Artifact / criterion | Status | Evidence |
|---|---|---|
| `AppConfig::from_env()` | ✅ Present | `src/config.rs` centralizes many runtime settings. |
| Per-stage backend flags | ✅ Present | `BackendPolicy` has `internal_video_presets`, `internal_hevc_to_h264`, `internal_hls_preview`, `internal_complex_audio`. |
| Runtime receives typed config | ✅ Complete | `MediaEngine` carries config and graph planning uses `engine.config.backend_policy`. |
| No env reads outside config/startup/test harness | ✅ Mostly | `ServerPorts::from_env()`, `RuntimeTuning::from_env()`, `BackendPolicy::from_env()`, and `AppConfig::from_env()` are implemented in `src/config.rs`; remaining env reads are startup/thread setup, test harness, tests, or process utilities. |
| Startup logs show effective config | ✅ Present | Startup emits `restream.config.effective` with a redacted `AppConfig::effective_summary()` covering ports, tuning, paths, logging, backend policy, FFmpeg, buffers, SRT, and RTMP settings. |

**Verdict**: **Complete**. Production runtime env parsing is centralized in
`src/config.rs`, and startup now emits a comprehensive redacted effective config
summary.

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
| Handlers no longer call SQL directly | ⚠️ Partial | `api/logs.rs` delegates persisted list/backfill behavior to `LogService`, and auth initialization uses `AuthService`; `api/agent.rs` and some state/helper code still call `db::*` directly. |
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
| Application services depend on repository traits | ⚠️ Partial | `PipelineService` depends on `PipelineStore`, `OutputService` depends on `OutputStore`, and `IngestService` depends on `IngestLookup`/`IngestWriter`; `HealthService`, `SettingsService`, `LogService`, and `AuthService` still hold `SqlitePool` and call `db::*`. |
| String states converted at repository boundary | ⚠️ Partial | `recording_repo` maps `RecordingPhase`; `output_repo` still stores/returns `desired_state: String` in `types::Output`. |

**Verdict**: **Partial**. Repository files exist, but port isolation and typed
state conversion are incomplete.

---

### Phase 6 — Runtime Graph Plan as Single Planning Model

| Criterion | Status | Evidence |
|---|---|---|
| `StageGraphPlan`, `GraphRole`, `StagePlan` | ✅ Present | `src/runtime/graph.rs`. |
| Output graph planner | ✅ Present | `planner::graph_plan::plan_pipeline_graph()`. |
| HLS preview planner | ✅ Present | `planner::graph_plan::plan_hls_preview_graph()` and `planner/hls_preview.rs`. |
| HLS output and recording planned by same graph | ⚠️ Partial | `GraphRole::HlsOutput` and `Recording` exist, but output/HLS/recording execution is not all driven by one graph planner path. |
| Diagnostics/harness use same planner | ⚠️ Partial | Graph API and diagnostics expose `StageGraphPlan`, and graph rendering now consumes planner stage/terminal output; harness expectations still duplicate output-path logic. |
| Stage-sharing tests compare against graph planner | ✅ Present | Mixed harness expected stage counts are compared with `plan_pipeline_graph()` and duplicate-output sharing in `mixed_manifest` tests. |

**Verdict**: **Mostly complete for output execution, graph rendering,
diagnostics, HLS preview planning, and harness stage-sharing proof; still
partial for recording and agent preview**.

---

### Phase 7 — First-Class Stage Lifecycle

| Criterion | Status | Evidence |
|---|---|---|
| Stage lifecycle tracking | ✅ Present | `src/media/stage_lifecycle.rs` and lifecycle snapshots. |
| Stage runtime manager | ✅ Present | `src/media/stage_runtime.rs` owns `ensure_stage()` / `spawn_stage()`. |
| Capacity wait visible and cancellation-aware | ✅ Present | `external_transcoder.rs` transitions to `WaitingForCapacity` and waits with `tokio::select!`. |
| Capacity metrics in snapshots | ✅ Present | `StageRuntimeSnapshot` includes total/available permits and wait duration. |
| Stage events beyond `StageStarted` | ✅ Present | `events.rs` has `StageRegistered`, `StageWaitingForCapacity`, `StageBackendSpawned`, `StageFirstInput`, `StageFirstOutput`, `StageFailed`, `StageStopped`. |
| Wrap current stage maps into a single `StageRuntime` map | ⚠️ Partial | Registries and `StageRuntimeManager` exist, but `MediaEngine` still owns separate buffers, metrics, queues, lifecycles, pipe metrics, and handles. |
| Existing `StageStarted` semantics removed | ✅ Mostly | New event names exist; no `StageStarted` variant found. |

**Verdict**: **Mostly complete, not ideal complete**. Lifecycle observability is
real, but the runtime object model is still split across registries and engine
state.

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
| HLS preview planning | ✅ Present | `planner::graph_plan::plan_hls_preview_graph()` and `planner::hls_preview::plan_hls_preview()`. |
| API no longer directly creates preview ring/backend | ✅ Mostly | `api/hls.rs` delegates to `application::hls_preview::ensure_hls_preview()`. |
| Runtime/application service owns preview orchestration | ✅ Present | `application/hls_preview.rs` plans preview and spawns fMP4 segmenter. |
| Actual keys in health match spawned keys | ✅ Tested | Engine tests cover `active_hls_preview_stage_keys_*`. |
| HLS blocked-stage cause surfaced | ✅ Tested | API test covers HLS playlist blocked-stage cause. |

**Verdict**: **Largely complete**. Remaining architectural cleanup is that the
application preview service still calls `MediaEngine::ensure_hls_preview_segmenter()`
and spawns the segmenter directly rather than going through a fully isolated
runtime graph service.

---

### Phase 11 — Recording Lifecycle and Metadata

| Criterion | Status | Evidence |
|---|---|---|
| Recording ID and phase types | ✅ Present | `RecordingId`, `RecordingPhase`. |
| Recording metadata table | ✅ Present | `db/schema.rs` creates `recordings`. |
| Recording repository | ✅ Present | `db/recording_repo.rs` with create/update/list/delete tests. |
| Runtime writes lifecycle metadata | ✅ Present | `media/recording.rs` builds service metadata and updates lifecycle state. |
| Media API returns metadata including pipeline/status | ✅ Present | `/api/v1/media` attaches persisted `recordingId`, `pipelineId`, status, timing, codec, and error fields via `MediaLibraryService::recording_metadata_by_filename()`. |
| Harness filters by pipeline/recording ID first | ⚠️ Partial | Harness rejects `.tmp.mp4`, but still has filename-token matching logic in `mixed_playback.rs`. |

**Verdict**: **Mostly complete at persistence/runtime/product API level,
partial at harness consumption level**.

---

### Phase 12 — Health, Alerts, and Diagnostics v2

| Criterion | Status | Evidence |
|---|---|---|
| Stage snapshots in health | ✅ Complete | `api_runtime_views/status.rs` uses `StageRuntimeSnapshot::to_json()`. |
| Dependency chain in output status | ✅ Complete | `blockedBy`, `terminalStage`, and `explanation` are present. |
| Backend capacity metrics in health | ✅ Complete | `capacityPermitsTotal`, `capacityPermitsAvailable`, `capacityWaitMs`. |
| Ring reader lag | ✅ Complete | Health and graph expose reader `lagSlots`, overflow count, packet age. |
| Keyframe wait information | ⚠️ Partial | Stage phases include `waitingForKeyframe`; HLS/preview alerts now derive from it. Broader source GOP/keyframe diagnostic context lives separately. |
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
| Phase 14 — Agent/MCP cleanup | ❌ Not complete | Agent HTTP routes still call `db::*` directly and duplicate DTO boundaries; agent graph preview uses `OutputPath` and API runtime views, not one shared application read-model/planner layer. |
| Phase 15 — Large-file split | ❌ Not complete | Major files remain large: `test_harness.rs` ~10k lines, `engine.rs` ~6.4k, `srt.rs` ~4.6k, `mpegts.rs` ~4.0k, `rtmp.rs` ~3.6k, `external_transcoder.rs` ~2.8k. |
| Phase 16 — Rollout policy | ❌ Not complete | Not audited as implemented; internal backend remains policy-gated and parity work is ongoing. |

---

## Critical Remaining Gaps

### P0 — Highest-Value Architectural Correctness

1. **Single graph planner is not yet the one source of truth**
   - Output preparation, graph rendering, diagnostics, HLS preview, and harness
     stage-sharing tests now use or prove `StageGraphPlan`, but recording,
     agent impact previews, and HLS output are not all unified behind one
     graph-plan contract.

2. **String runtime state remains in core logic**
   - Active egress status/phase and recent egress status/raw status/phase are
     now typed, but `types::Output.desired_state` still uses raw strings at the
     DB row boundary. This is the remaining Phase 1 string-state adoption gap.

### P1 — Layering and Ownership

3. **API/service/repository boundary remains mixed**
   - Route modules exist, but `api/agent.rs` and several services still call
     `db::*` directly. `PipelineService`, `OutputService`, and `IngestService`
     are now port-trait backed.

4. **HLS preview is no longer an API one-off, but still not a pure graph service**
   - `application::hls_preview` owns orchestration, but it directly calls
     `MediaEngine` segmenter methods and spawns the segmenter task.

### P2 — Guardrails and Large-File Debt

6. **No architecture drift CI**
   - No source audit script, route inventory, forbidden import check, or file
     growth guard exists.

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
| Ph 1 Core contracts | A- | Types exist, reconciliation consumes typed desired-state logic, and active/recent egress lifecycle state is typed; DB output desired-state strings remain at the row boundary. |
| Ph 2 Config | A | Production env parsing is centralized in config, and startup logs a comprehensive redacted effective-config summary. |
| Ph 3 API split | A | Route module split is complete. |
| Ph 4 App services | B+ | Logs, auth initialization, pipeline, output, and ingest operations are service-backed; agent and some helper paths still contain direct DB/application work. |
| Ph 5 Repositories | B | Repo modules exist, and pipeline/output/ingest services are port-trait backed; several services still hold `SqlitePool` directly. |
| Ph 6 Graph planner | A- | Planner drives output preparation, graph rendering, diagnostics, and preview planning, with harness stage-sharing tests; recording/agent consumers remain. |
| Ph 7 Stage lifecycle | B+ | Lifecycle/capacity visibility strong; runtime object model still split. |
| Ph 8 Dependency-aware status | A | Operator-facing dependency status is complete for the phase scope, with typed internal egress lifecycle state. |
| Ph 9 FFmpeg waist | A | Shared FFmpeg plan/backend/input/output contracts are the backend entry path, and legacy input/output ring escape hatches are removed. |
| Ph 10 HLS preview | A- | API one-off removed; runtime service boundary still not ideal. |
| Ph 11 Recording metadata | B+ | Media API consumes persisted recording metadata; harness still partly filename-based. |
| Ph 12 Health/alerts/diagnostics | A- | Health, alerts, graph, and causal diagnostics bundle are complete; legacy SSE diagnostics remains a separate probe. |
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
