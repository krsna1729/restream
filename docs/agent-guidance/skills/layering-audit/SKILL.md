---
name: layering-audit
description: Use when refactoring Rust or frontend TypeScript architecture in this repository to improve layering, move orchestration into the right owner layer, evaluate whether a module boundary is justified, or decide when not to split further. Adds the repo-specific layering ladder, stop rules, and verification workflow learned from the restream layering passes.
---

# Layering Audit

Use this skill when the task is about layering, module ownership, or boundary
splits in this repository.

## Goals

- move orchestration out of edge/runtime-heavy code only when it removes real coupling
- keep hot-path or render-hot modules focused on runtime/UI concerns
- keep persistence policy in `application` or `db`, not in `media`
- keep frontend composition in `app` and feature-local behavior in bounded feature modules
- stop before layering turns into wrapper code and file churn

## Layering Ladder

Prefer the lightest boundary that fixes the coupling:

1. File split for readability or merge pressure.
2. Module for one concept that owns its types, validation, helpers, and local state.
3. Visibility tightening for an already-correct boundary.
4. Capability/port/interface when a layer should depend on a stable contract instead of a concrete implementation.
5. Crate or package boundary only after the module API is already stable and intentionally narrow.

Do not jump to a crate, package, or new top-level folder because a module feels busy.

## Size Is A Trigger, Not A Design

The source audit has a hard maximum of 999 raw lines for backend Rust. It warns
Rust files at 800:

- below 500 lines is a comfortable default, not a reason to merge unrelated owners
- 500-799 lines is a reviewable growth band for a cohesive module
- 800-999 lines is architectural pressure and requires an ownership review
- 1,000 or more Rust lines fails, including the root build script, dedicated
  tests, harnesses, integration tests, and benchmarks

Aim new or split files below 800. Do not target 990-999, and do not declare a
split successful merely because the receiving file passes the mechanical cap.
Line count starts the audit; responsibility, dependency direction, and a narrow
public surface decide the seam.

### Historical regression

The former audit introduced by `aa139026` used a 2,000-line limit and rejected
only files above it. It codified a threshold-seeking pattern already visible
in the immediately preceding split work: `9ad55101` created `srt_egress.rs` at
exactly 1,000 lines, and test-only moves in `6594db34`, `409da728`, and
`2ae8f6f8` improved production readability while transferring near-cap units
into dedicated files. The pattern then continued after the audit in
`63d44602`. The roomy global cap allowed production files to cluster between
roughly 1,900 and 2,000 lines; it did not cause the earlier splits.

Do not repeat that pattern:

- never use the cap as a target size
- split test files by behavior or fixture ownership when moving tests out
- distinguish a lexical split from an ownership split in the review summary
- treat several sibling files in the warning band as evidence that the parent
  module still lacks a clear owner boundary

Feature topology can hide the same regression. Commit `72f9441e` declared
`mcp-core = ["agent-plane"]`, so compiling the supposed lower feature also
compiled the higher layer and concealed upward agent-core dependencies. A
lower feature boundary is proven only when it compiles without the higher
feature. Keep HTTP/in-process adapter `cfg` gates beside the adapter modules,
not on a parent feature that silently pulls the higher layer in.

## Good Extractions In This Repo

Backend signals:

- repeated pipeline/ingest/runtime orchestration into `application::ingest`
- cross-source settings reads into `application::settings`
- meta-backed transcode profile persistence into `application::transcode_profiles`
- runtime-only profile cache/defaults staying in `media::profiles`

Frontend signals:

- dashboard feature wiring moving into `web/ts/app/`
- output-list rendering and delegated actions moving out of `pipeline-view.ts`
- shared fetch/state/URL helpers staying in `web/ts/core/`
- history-specific render/controller state staying inside `web/ts/history/`

Ownership pattern:

- backend `api` owns validation, auth checks, and response shaping
- backend `application` owns orchestration and persistence policy
- backend `media` owns runtime state, hot-path logic, and cache/defaults
- backend `db` owns raw SQL
- frontend `app` owns composition/bootstrap wiring
- frontend `core` owns shared transport, shared state, and pure transforms/helpers
- frontend `features` own bounded UI rendering and feature-local interaction logic
- frontend `history` owns history-specific state, polling, and rendering

## Stop Rules

Stop when the next move is more conceptual than operational.

A new boundary is justified when it:

- removes duplicated orchestration across handlers, runtime entry points, or frontend composition roots
- hides storage/runtime/UI coupling behind a stable capability or seam
- moves API-shaped or persistence-shaped logic out of runtime internals
- moves cross-feature composition out of feature modules and into a frontend app layer
- makes tests target behavior at the right layer

Do not extract when it mostly:

- renames code without changing dependency flow
- wraps one DB call or one DOM call in a paper-thin module
- adds ports that only one callsite uses and are unlikely to stabilize
- scatters endpoint-local CRUD or feature-local UI into many tiny files

For a size-driven pass, stop only when all of these are true:

- every audited file is below 1,000 raw lines
- no new or extracted file was deliberately parked in the 800-999 warning band
- each moved concept has one stated owner and dependencies still point inward
- compatibility re-exports are removed or have a named, time-bounded migration
  purpose
- the next proposed split would add more navigation or wrappers than ownership
  clarity

If the pass spans multiple commits, ratchet monotonically: do not increase any
already-oversized file, do not add a new warning-band file, and reduce either
the count of failing files or their aggregate excess in every checkpoint. Do
not weaken the global limit or add broad permanent exceptions to make an
intermediate checkpoint green.

## Module Versus Crate Gate

Choose a private helper or lower-level type when the code has no independent
lifecycle and is used by one owner. Choose a module when the concept owns
related state/validation/helpers but still shares crate internals. Consider a
crate only after the same boundary has already worked as a module and all of
these are true:

1. Its purpose and public API fit in one sentence.
2. Its dependency direction is acyclic and enforceable.
3. Its public surface is intentionally narrow; it does not export a broad set
   of internals merely to make the move compile.
4. It can avoid edge/runtime-heavy dependencies such as `axum`, `sqlx`,
   FFmpeg/libsrt bindings, and unrelated application state unless those are the
   crate's explicit purpose.
5. Independent compilation, reuse, feature isolation, or test isolation has a
   measured benefit.
6. Moving it does not require compatibility facades that recreate the old
   coupling.

Current backend readiness:

- `domain` and `runtime` contracts are mechanically close to a contracts
  crate: runtime now depends downward on domain, snapshot/health back-edges are
  gone, and `output_spec` has a curated facade over focused children.
- `planner` is also mechanically close: it depends on domain/runtime contracts
  and owns `EncodingStagePlan` plus its configuration-derived behavior.
- Do not create either crate without a measured compile-time, reuse, release,
  or dependency-isolation benefit. A clean module DAG is already valuable.
- `agent_core` is transport-neutral and independently feature-compilable;
  extract it only if standalone sidecar/package isolation is valuable enough
  to justify another package and release surface.
- `db`, `application`, `media`, SRT/protocol implementations, and bootstrap
  remain modules. Their persistence adapters, orchestration, native bindings,
  or lifecycle ownership still belong to the main runtime.

These are readiness assessments, not claims that any additional crate exists
or instructions to create one during a layering pass.

## Review Checklist

When auditing a candidate seam:

1. Find the repeated behavior or wrong dependency direction.
2. State the owner layer in one sentence.
3. Check whether an existing module can own it before creating a new one.
4. Keep the edge layer responsible for transport or composition concerns.
5. Keep the runtime or render-hot layer responsible for hot-path/runtime/UI concerns.
6. Add or update focused tests that prove the moved behavior still works.
7. Reassess after the change whether another extraction is still justified.
8. Record whether the result is a lexical split, an ownership split, or a
   crate-ready module boundary.
9. Run `scripts/check/source-audit.sh` and read its stdout `FAIL`/`WARN` lines
   by Rust responsibility class so the root build script, production code,
   dedicated tests, harnesses, benchmarks, and integration tests cannot hide
   one another's pressure.
10. For wrong-direction imports, upward-compatibility re-export facades, and
    types with inherent `impl` blocks outside their owner file, query the
    Graphify code graph (`docs/agent-guidance/graphify.md`) rather than
    grepping by hand: `graphify explain "<Type>"` and
    `graphify path "<A>" "<B>"` show the real dependency edges. A
    lower-to-higher edge blocks the audit; a facade re-export or an external
    inherent impl is review evidence, not an automatic failure.
11. After MCP/agent feature-boundary changes, run the negative feature-matrix
    compile commands from `docs/layering-roadmap.md` directly (`cargo check
    --lib --no-default-features --features mcp-core`, `mcp-server`,
    `mcp-embedded`, and the `restream-mcp` binary with
    `mcp-server,mcp-http-backend`). The proof is that lower features compile
    while `agent-plane`/`agent-execution` stay disabled — run the compiler,
    do not infer it from the feature graph.

## Verification

- run focused tests first for the touched seam
- prefer application-level tests for backend orchestration extractions
- keep API contract tests when edge behavior still depends on the seam
- keep frontend DOM/render tests around refactored UI seams
- if the change touches hot runtime code or high-frequency frontend refresh paths, follow the benchmark/proof rules in `AGENTS.md`
- after MCP/agent feature-boundary changes, run at least the `mcp-core`
  and standalone sidecar (`mcp-server,mcp-http-backend`) negative feature-matrix
  compile commands from `docs/layering-roadmap.md`

## Read This Reference

- [../../../layering-roadmap.md](../../../layering-roadmap.md)
