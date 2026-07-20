# Layering Roadmap

This document turns the layering audit into an execution order that is safe for
an active repo: narrow seams first, broader packaging later.

## Contents

- [Current Shape](#current-shape)
- [Historical Size-Limit Regression](#historical-size-limit-regression)
- [Size Policy](#size-policy)
- [Ownership Matrix](#ownership-matrix)
- [What We Already Moved](#what-we-already-moved)
- [Layering Ladder](#layering-ladder)
- [Crate Readiness](#crate-readiness)
- [Refactor Order](#refactor-order)
- [What Should Not Be Split Yet](#what-should-not-be-split-yet)
- [Working Rules](#working-rules)
- [Ratchet And Stop Rules](#ratchet-and-stop-rules)
- [Immediate Next Steps](#immediate-next-steps)

## Current Shape

The backend already has promising boundaries:

- `domain` for typed graph vocabulary
- `planner` for backend-selection policy
- `media` for packet/runtime/backend code
- `db` for persistence
- `api` for the HTTP/UI edge

The frontend now also has a clearer shape:

- `web/ts/app` for dashboard composition/bootstrap
- `web/ts/core` for shared transport, state, and pure transforms
- `web/ts/features` for bounded UI modules
- `web/ts/history` for history-specific controller/rendering behavior

The remaining issue is no longer a known lower-layer back-edge. Wave 2 removed
the encoded wrong-direction imports and made several candidate boundaries
mechanically clean. The remaining decision is whether another boundary creates
measurable isolation or only more packaging.

Current backend evidence:

- `domain` and `runtime` form a downward contract dependency: obsolete
  runtime snapshot/health back-edges are gone
- `planner` owns stage-plan construction and depends only on domain/runtime
  contracts
- DB repositories own persistence records; infrastructure adapters convert
  them into application models
- application errors are transport-neutral and the API owns Axum response
  conversion
- `agent_core` owns shared request/plan types without depending on agent-plane,
  MCP inputs, or Reqwest
- media packet, metadata, and ring-reader ownership is explicit inside `media`
- external inherent `impl` blocks remain review points because they cannot
  cross a future crate boundary unchanged

Frontend examples:

- large feature modules still mix rendering, async coordination, and cross-feature wiring
- some feature modules still import peer features because the composition owner is not yet narrow enough
- globals/window hooks remain as a compatibility surface that should stay edge-facing

## Historical Size-Limit Regression

The first global cap was useful but became a substitute for architectural
completion:

1. Before `aa139026`, split commits already improved navigation without always
   finishing ownership. `9ad55101` created `src/media/srt_egress.rs` at exactly
   1,000 lines, while `6594db34`, `409da728`, and `2ae8f6f8` transferred large
   test bodies into dedicated files that remained close to the emerging cap.
2. Commit `aa139026` then replaced two file-specific growth baselines with a
   global 2,000-line check. The check rejected only files greater than 2,000,
   codifying rather than originating the threshold-seeking pattern.
3. The same pattern continued after the audit in `63d44602`: a file move and a
   passing count could still stand in for a finished ownership seam.
4. A later audit found production files clustered from roughly 1,900 to 2,000
   lines. That clustering was pressure against the guardrail, not proof that
   the seams were optimal.

The lesson is not that extraction was wrong. Splitting API route families, DB
repositories, protocol helpers, engine snapshots, and test ownership was a
valuable first pass. The regression was treating a lexical file move and a
passing line count as sufficient evidence of an ownership boundary.

Feature topology produced a second historical lesson. Commit `72f9441e`
introduced `mcp-core = ["agent-plane"]`. That feature edge meant the lower MCP
surface was never compiled without the higher agent plane, so upward
agent-core dependencies could remain hidden. The durable rule is:

- compile a lower feature with the higher feature disabled
- keep HTTP and in-process adapter `cfg` gates on their own modules
- treat a feature dependency as an architectural edge, not merely build
  configuration
- record both the intended feature closure and the negative compile command

## Size Policy

`scripts/check/source-audit.sh` measures raw physical lines for authored Rust
in the root `build.rs` and in `src/`, `test/`, `tests/`, and `benches/`.
Fixtures and generated artifacts remain outside this metric. Authored
TypeScript and JavaScript keep their existing 2,000-line policy; reducing that
limit is a separate frontend task.

The backend Rust bands are:

| Raw lines | Meaning | Required response |
|---:|---|---|
| 0-499 | Comfortable default | Keep a cohesive owner; do not split for size alone |
| 500-799 | Reviewable growth | Watch responsibility count and dependency direction |
| 800-999 | Architectural pressure | Explain the owner and plan the next seam before adding scope |
| 1,000+ | Hard failure | Split by ownership; moving the same monolith into another file is not completion |

The audit reports Rust files separately as build script, production, dedicated
test, harness, benchmark, and integration test. Those classes share the same
hard maximum but must be interpreted separately: a large harness needs
scenario/runner/reporting seams, while a large production module needs runtime
or domain ownership seams.

## Ownership Matrix

Use this matrix before extracting a new module, trait, crate, or frontend app boundary.

### Backend `domain`

Owns:

- meaning
- validation
- parsing
- shared typed vocabulary

Does not own:

- SQL
- runtime caches
- HTTP response shape

### Backend `application`

Owns:

- orchestration
- persistence policy
- shared multi-step workflows
- ports/capabilities that isolate storage from orchestration

Does not own:

- raw SQL
- packet-level runtime behavior
- HTTP transport details

### Backend `db`

Owns:

- raw queries
- schema-aware CRUD

Does not own:

- workflow policy
- cross-layer orchestration

### Backend `media`

Owns:

- runtime state
- protocol loops
- hot-path transforms
- caches/defaults used directly by runtime consumers

Does not own:

- persistence serialization policy
- API-facing JSON contracts
- duplicated control-plane orchestration

### Backend `api`

Owns:

- request validation
- auth checks
- status codes
- edge/view shaping

Does not own:

- reusable orchestration
- runtime internals
- persistence policy

### Frontend `app`

Owns:

- bootstrap/composition wiring
- feature dependency assembly
- page-level mode orchestration

Does not own:

- low-level fetch helpers
- reusable render-hot widget logic
- feature-local DOM details

### Frontend `core`

Owns:

- shared transport helpers
- shared state
- URL/session helpers
- pure transforms and formatting shared across features

Does not own:

- cross-feature composition
- feature-local DOM ownership
- dashboard mode orchestration

### Frontend `features`

Owns:

- bounded UI rendering
- feature-local interaction logic
- feature-local transient state

Does not own:

- app-wide composition wiring
- shared transport primitives that multiple features depend on
- unrelated peer-feature orchestration

### Frontend `history`

Owns:

- history polling state
- history-specific render models
- history modal rendering and controls

Does not own:

- unrelated dashboard composition
- shared transport primitives beyond what it consumes from `core`

## What We Already Moved

Backend low-risk extractions already landed:

1. Audio-routing grammar now lives in `domain`.
2. Transcode-profile schema now lives in `domain`.
3. SRT ingest config and validation live in `domain`.
4. Ingest security policy config lives in `domain`.
5. Logging DTOs live in `logging::types`.

Frontend low-risk extractions already landed:

1. Dashboard feature wiring now has an `app` composition root.
2. Pipeline output-list rendering and delegated actions now live outside `pipeline-view.ts`.

These moves are useful because they move "how the app is composed" away from
"how one feature renders."

Backend file-level splits already landed and remain worth keeping:

1. The former API monolith is split by route family.
2. DB access is split into repository modules.
3. RTMP FLV, egress transport, timestamps, metadata, and enhanced-codec helpers
   have focused homes.
4. SRT policy, Stream ID, monitoring/quality, crypto, and egress concerns have
   focused homes.
5. Engine HLS, snapshot, lifecycle, test, and registry-access concerns are no
   longer all in one physical file.

These are not all finished ownership boundaries. Re-export facades and
extension `impl MediaEngine` blocks should be reassessed after their consumers
move, rather than preserved merely because they reduced one file's length.

Wave 2 also completed dependency-direction work that changes crate readiness:

1. `runtime::snapshots` and `runtime::health` compatibility owners were removed;
   the remaining runtime contracts depend on domain only.
2. The 973-line `domain::output_spec` was split into configuration, encoding,
   protocol, and video owners behind a curated facade.
3. `EncodingStagePlan` and its configuration-derived inherent implementation
   now live together in planner.
4. Agent request and proposed-change types moved into `agent_core`; Reqwest
   conversion moved to the HTTP adapter and MCP-only inputs moved to `agent_mcp`.
5. `mcp-core` no longer enables `agent-plane`, while HTTP and embedded adapters
   carry local feature gates.
6. DB repositories return DB-owned records; infrastructure owns conversion to
   application models.
7. `ServiceError` is application-owned and transport-neutral; `ApiError` owns
   Axum mapping.
8. `media::packet`, `media::metadata`, and the ring reader now own their
   respective vocabulary and behavior without engine-metadata back-dependencies.

## Layering Ladder

When deciding whether to use a file, module, trait/interface, crate, or frontend
app boundary, prefer the lightest boundary that prevents the wrong coupling.

### 1. File split

Use when the problem is readability or merge pressure, not ownership.

Good targets here:

- split an oversized edge module by one route or projection family
- split oversized frontend feature files by one real concept

### 2. Module

Use when one concept should own its types, parsing, validation, helpers, and
local state, but still live in the same crate/folder and dependency graph.

Good backend examples in this repo:

- `domain::audio_routing`
- `domain::transcode_profile`
- `domain::srt_ingest`
- `domain::ingest_security`

Good frontend examples in this repo:

- `web/ts/features/pipeline-output-list.ts`
- `web/ts/features/pipeline-dependencies.ts`

### 3. Visibility boundary

Use `pub`, `pub(crate)`, folder exports, and narrow import surfaces to turn
modules into real seams.

Rule of thumb:

- `domain` should expose stable typed meaning
- runtime helpers inside `media` should stay narrow
- frontend `core` should expose stable helpers, not feature internals
- frontend `features` should depend on `core` or `app`, not many peer features

### 4. Newtypes, contracts, ports, and interfaces

Use them when stringly-typed or concrete-implementation coupling is the problem.

Backend examples:

- stage vocabulary in `domain::stage`
- resolved ingest/security policy enums in `domain`
- lookup traits in `application::ports`

Frontend examples:

- explicit dependency bags for feature actions
- typed state envelopes and shared feature contracts

### 5. Crate or package boundary

Use a crate or package boundary only after the module boundary is already stable.

Signals that a split is justified:

- the API can be described in one sentence
- it should not depend on `axum`, `sqlx`, FFmpeg bindings, or unrelated feature DOM code
- compile-time, bundling, or dependency isolation is actually valuable

That makes crate/package splits the last step, not the first.

## Crate Readiness

No new backend crate is implied by this roadmap. The current package remains
the source of truth while module APIs are stabilized.

### Closest candidate: contracts

A future contracts crate could combine `domain` with the genuinely independent
parts of `runtime`. It is the strongest candidate because the intended surface
is typed meaning and runtime contracts with a small dependency set.

It is mechanically close now:

- runtime snapshot/health back-edges are gone
- `output_spec` is split behind a curated facade
- the candidate stays close to `std`, Serde, and dependency-light contract
  helpers rather than inheriting Axum, SQLx, FFmpeg, or libsrt

Do not create it solely because extraction is possible. First measure a
concrete benefit such as reduced rebuild scope, independent reuse, or enforced
dependency isolation that tests alone do not provide.

### Follow-on candidate: planner

`planner` is also mechanically close: it depends on domain/runtime contracts,
owns `EncodingStagePlan`, and has no application/media/DB/edge imports. It can
remain a module even if contracts later becomes a crate. Extract it only when
independent compilation or reuse of graph/backend selection policy is measured
to matter.

### Independently feature-compilable candidate: agent core

`agent_core` is now transport-neutral: shared plan types live there, Reqwest
belongs to the HTTP adapter, MCP-only inputs belong to `agent_mcp`, and the
`mcp-core` feature compiles without enabling `agent-plane`.

That proves a real module/feature boundary, not a need for another crate.
Extract it only if standalone sidecar packaging, independent versioning, or
dependency isolation is valuable enough to justify a separate package and
release surface.

### Keep as modules

- `db`: repository records are now DB-owned, but SQLx persistence and
  infrastructure conversion are runtime-local implementation details.
- `application`: errors are transport-neutral, but orchestration and its ports
  still compose the main process.
- `media`: packet/metadata/ring ownership is cleaner, while engine state,
  protocols, native bindings, and lifecycle remain intentionally coupled at
  runtime.
- RTMP/SRT/HLS implementations: keep their owned submodules in `media`. An
  `srt-sys` crate is only worth considering after the safe socket wrapper is
  stable and native-linkage isolation has measured value.
- ring buffer and packet primitives: their module boundary is now cleaner, but
  moving hot-path primitives to a crate needs compile-time or reuse evidence,
  not only a theoretically extractable API.

The crate gate is strict: the module boundary must already work, its public API
must fit in one sentence, dependency direction must be acyclic, and independent
compilation or dependency isolation must provide a concrete benefit.

## Refactor Order

### 1. Stabilize mechanically clean contract boundaries

Goal: preserve the new dependency direction without prematurely packaging it.

Current work:

- keep `domain`, `runtime`, and `planner` free of edge, persistence, media, and
  application imports
- keep the `output_spec` facade curated rather than exposing child layout
- measure rebuild/reuse/isolation value before proposing contracts or planner
  crates

### 2. Keep runtime views out of the engine core — done (2026-07-18)

Goal: `MediaEngine` should return typed state and snapshots, not primarily
`serde_json::Value`.

Success condition:

- engine code no longer needs to know UI/HTTP serialization details
- JSON assembly happens at the edge

`StageMetrics::snapshot()` and `PipeMetrics::snapshot()` (`src/media/`) now
return typed `StageMetricsSnapshot` / `PipeMetricsSnapshot` structs instead of
hand-built `serde_json::Value`. Callers in `src/api_runtime_views/` and
`src/api_view_models.rs` convert to JSON at the edge via
`serde_json::to_value(...)` (or implicitly through the `json!` macro). No
`serde_json::Value` or `json!` usage remains in `src/media/` outside tests.
See `docs/agent-guidance/quality/journal.md` Q-008.

### 3. Continue frontend composition cleanup

Goal: keep cross-feature coordination in `web/ts/app`, not in oversized
feature modules.

Still-useful next candidates:

- move additional dashboard mode orchestration into focused app-owned helpers when that removes real coupling
- split oversized feature modules only when one concept clearly owns its state and render path
- keep hot refresh paths such as output cards and high-frequency dashboard rerenders optimized for DOM reuse

### 4. Keep protocol persistence behind owned capabilities — done (2026-07-19)

Goal: RTMP and SRT should depend on lookup ports, not query text.

RTMP and SRT now consume media-owned authentication/policy capabilities.
Infrastructure/application adapters own persistence access; protocol modules no
longer contain raw SQL or import DB/application layers.

### 5. Keep API route families thin

The physical route-family split is complete. The remaining goal is to keep edge
ownership honest:

- keep agent operation orchestration in `application::services::agent_service`
- keep API validation, authorization, status codes, and response projection at
  the edge
- keep system metric collection in telemetry-owned submodules so handlers
  remain thin

## What Should Not Be Split Yet

### Backend `planner`

Keep it as a module for now.

Reason:

- its dependency direction and owner APIs are already mechanically clean
- no measured compile-time, reuse, or packaging benefit currently justifies a
  separate package

### Backend `db`

Keep it in the main crate.

Reason:

- DB-owned records fixed the wrong dependency direction
- SQLx repositories and their infrastructure adapters still ship and evolve
  with the main process
- extracting them would add package/API surface without measured isolation

### Backend protocol and engine internals

Keep RTMP, SRT, HLS, MPEG-TS, ring-buffer, and engine decompositions inside
`media` until safe wrappers and ownership APIs are proven.

Reason:

- packet and metadata vocabulary is cleaner, but native bindings,
  socket/thread lifecycle, engine registries, and protocol behavior are still
  coupled
- a crate boundary would force broad visibility before it provides useful
  isolation
- module-first extraction preserves hot-path optimization and makes the
  eventual public API evidence-based

SRT in particular should remain a module. The `sys`, socket, listener, ingest,
play, and egress seams are useful ownership boundaries, but native linkage,
safe-wrapper stability, and thread lifecycle still belong to the main media
runtime.

### Frontend features under active UI churn

Keep them whole until the next move removes real dependency flow.

Reason:

- splitting a large feature without changing ownership just creates wrapper files
- render-hot code needs proof that DOM churn or refresh cadence did not regress

## Working Rules

When making layering changes, prefer this order:

1. Move the type, helper, or owner concept.
2. Repoint callers.
3. Preserve compatibility with re-exports if helpful.
4. Only then move files, split crates, or add app-level composition seams.

When choosing the next refactor in an active worktree:

- avoid hot files already under parallel edit
- prefer pure-type or pure-helper extractions
- prefer compatibility-preserving moves over signature churn
- benchmark runtime hot paths and high-frequency frontend refresh paths when touched
- commit each seam independently

Do not leave a compatibility re-export indefinitely. Record why it exists, who
still consumes it, and the condition for deleting it.

The `media::stage_lifecycle::{StagePhase, StageBackendKind}` re-export is the
deliberate exception: it is the stable observer-facing API beside
`StageLifecycleSnapshot`, and integration consumers use that public path.
`domain::state` remains the defining owner. Remove the re-export only as a
versioned public-API change, not as an internal layering cleanup.

Run `scripts/check/source-audit.sh` after changing a candidate boundary. Its
additive report fields are:

- `boundaryHazards.wrongDirectionImports`: blocking encoded lower-to-higher
  imports
- `boundaryHazards.upwardCompatibilityReexports`: review-only wrong-direction,
  allowed cross-owner, and explicit/re-export-only same-owner facades that can
  conceal an unfinished migration; ordinary curated same-owner exports are
  omitted
- `boundaryHazards.parserChecks`: deterministic grouped/multiline Rust
  `crate::{...}` import coverage
- `boundaryHazards.externalInherentImpls`: review-only owner/implementation
  sites, including whether they cross layers
- `featureTopology.features` and `featureTopology.closures`: declared and
  transitive feature edges for `mcp-core`, `mcp-server`, `mcp-http-backend`,
  and `mcp-embedded`
- `featureTopology.staticChecks`: adapter-local gate and lower-feature topology
  checks
- `featureTopology.negativeMatrix`: requested features, computed closures,
  evaluated `mustEnable`/`mustNotEnable` claims, and unexecuted Cargo proof
  commands. Each row records `executed: false`; run those resource-limited
  commands separately before claiming compile proof.

Do not turn every external inherent implementation into a failure. Same-layer
engine/protocol extension impls and infrastructure constructors can be
intentional; they become blockers only when proposing a crate boundary they
cannot cross.

## Ratchet And Stop Rules

During a multi-commit reduction:

1. Never increase an already-failing file.
2. Never create a new file in the 800-999 warning band as the destination of a
   mechanical move.
3. Each checkpoint must reduce the number of failing files or their aggregate
   lines above 999.
4. Split production, dedicated tests, harnesses, benchmarks, and integration
   tests according to their own ownership patterns; do not hide one class
   inside another.
5. Do not raise the cap, change raw-line counting, or add a broad exception to
   make an intermediate checkpoint pass.

Stop the size-driven pass when:

- every audited file is below 1,000 raw lines
- warning-band files have a documented cohesive owner and no pending scope is
  being forced into them
- dependencies point toward the intended owner
- compatibility facades introduced by the pass have been removed or have a
  named migration condition
- another split would add wrappers/navigation without clarifying ownership

A file below 800 may still need a layering fix when dependency direction is
wrong. Conversely, a cohesive file should not be fragmented into tiny helpers
after the ownership and cap goals are met.

## Immediate Next Steps

Best next low-risk code steps:

1. Keep every Rust file below 1,000 and work down the 800-999 warning cluster
   only where another owner is clear.
2. Use `boundaryHazards.externalInherentImpls` when evaluating a proposed crate;
   move or replace only the impls that would actually cross that boundary.
3. Preserve the clean contracts/planner DAG and collect rebuild/reuse evidence
   before proposing packages.
4. Keep agent-core feature-independent and run the reported negative matrix
   whenever feature edges or adapter gates change.
5. Keep DB, application, media, and SRT as modules while their runtime-local
   composition and lifecycle remain valuable.
6. Keep moving dashboard composition concerns into `web/ts/app` only when that
   removes real cross-feature coupling.

That sequence keeps progress real without forcing a risky big-bang rewrite.
