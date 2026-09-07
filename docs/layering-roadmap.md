# Layering Roadmap

This document turns the layering audit into an execution order that is safe for
an active repo: narrow seams first, broader packaging later.

## Contents

- [Current Shape](#current-shape)
- [Size Policy](#size-policy)
- [Ownership Matrix](#ownership-matrix)
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

The frontend now has a clear, modular shape:

- `web/ts/app` for dashboard composition, routing, and bootstrap (`app/modes/` sub-modules)
- `web/ts/core` for shared transport, state, and pure transforms
- `web/ts/features` for bounded UI modules with ownership subdirectories
  (`pipeline-view/`, `control-room/`, `editor/`, `pipeline-inspector/`, `settings/`, `status/`)
- `web/ts/history` for history-specific controller/rendering behavior

All authored frontend TypeScript files (`web/ts/*`) are strictly under 999 raw lines, enforced by `./scripts/check/source-audit.sh`.

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

- four oversized feature groups are now in ownership subdirectories, each with a
  barrel `index.ts`; `pipeline-inspector/index.ts` remains above the 1,000-line
  cap and needs a meaningful seam before the split pass is fully done
- some feature modules still import peer features because the composition owner is not yet narrow enough
- globals/window hooks remain as a compatibility surface that should stay edge-facing

## Size Policy

`scripts/check/source-audit.sh` measures raw physical lines for authored Rust
in the root `build.rs` and in `src/`, `test/`, `tests/`, and `benches/`.
Fixtures and generated artifacts remain outside this metric. Authored
TypeScript and JavaScript now share the same 1,000-line hard maximum as the
backend Rust policy.

Durable lessons from earlier size-limit and feature-topology mistakes:
a lexical file move and a passing line count are not ownership proof; compile a lower Cargo feature with the higher feature disabled; treat a feature dependency as an architectural edge; record the intended feature closure and the compile command that proves it (`mcp-core` no longer enables `agent-plane`; `mcp-embedded` is an intentional combo).

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

Typed stage/pipe metric snapshots; JSON assembly stays at the API edge.
See archived journal Q-008.

### 3. Continue frontend composition cleanup

Goal: keep cross-feature coordination in `web/ts/app`, not in oversized
feature modules.

Still-useful next candidates:

- move additional dashboard mode orchestration into focused app-owned helpers when that removes real coupling
- split oversized feature modules only when one concept clearly owns its state and render path
- keep hot refresh paths such as output cards and high-frequency dashboard rerenders optimized for DOM reuse

### 4. Keep protocol persistence behind owned capabilities — done (2026-07-19)

RTMP/SRT use media-owned auth/policy capabilities; adapters own persistence.

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

Run `scripts/check/source-audit.sh` after changing a candidate boundary. It
stays deliberately mechanical and bash/grep-only: forbidden-import greps, a
per-file raw-line-count check (`FAIL`/`WARN` on stdout for every file at or
over its size band, grouped by responsibility class), approved
`std::env::var` owners, and the API/harness guardrails it has always run.

Wrong-direction imports, upward-compatibility re-export facades, external
inherent `impl` placement, and Cargo feature-topology closures are reviewed
with the Graphify code graph instead of a maintained parser
(`docs/agent-guidance/graphify.md`): `graphify explain "<Type>"` and
`graphify path "<A>" "<B>"` answer "who depends on this" and "does an edge
exist between these two owners" directly against the real dependency graph,
without a second bespoke Rust-import parser to keep in sync with the
language. Use the Layering Ladder and Ownership Matrix in this document as
the ownership rules to check the graph against.

After MCP/agent feature-boundary changes, prove the topology by running these
commands directly rather than inspecting a generated claim:

```sh
# Lower-layer isolation: agent-plane and agent-execution must stay disabled.
scripts/build/resource-limit.sh cargo check --lib --no-default-features --features mcp-core
scripts/build/resource-limit.sh cargo check --lib --no-default-features --features mcp-server
scripts/build/resource-limit.sh cargo check --bin restream-mcp --no-default-features --features mcp-server,mcp-http-backend

# Compatibility combo: mcp-embedded intentionally enables agent-plane (with
# mcp-core). It must compile, must not enable agent-execution, and must not
# mount an in-process MCP backend (HTTP sidecar remains the only backend).
scripts/build/resource-limit.sh cargo check --lib --no-default-features --features mcp-embedded
```

The first three commands prove lower MCP surfaces compile without
`agent-plane`/`agent-execution`. The fourth proves the named combo compiles as
`mcp-core` + `agent-plane` without inventing a backend. A topology-only claim
about the feature graph is not a substitute for these compiler runs.

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
2. Use Graphify (`docs/agent-guidance/graphify.md`) when evaluating a proposed
   crate; query the real dependency graph for external inherent impls and
   move or replace only the ones that would actually cross that boundary.
3. Preserve the clean contracts/planner DAG and collect rebuild/reuse evidence
   before proposing packages.
4. Keep agent-core feature-independent and run the reported negative matrix
   whenever feature edges or adapter gates change.
5. Keep DB, application, media, and SRT as modules while their runtime-local
   composition and lifecycle remain valuable.
6. Keep moving dashboard composition concerns into `web/ts/app` only when that
   removes real cross-feature coupling.

That sequence keeps progress real without forcing a risky big-bang rewrite.
