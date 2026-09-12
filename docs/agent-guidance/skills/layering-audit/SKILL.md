---
name: layering-audit
description: Audit or improve Rust/TypeScript ownership and dependency boundaries in restream, including quality-backlog modularity items.
---

# Layering Audit

Find the concrete coupling, name its owner, and use the smallest boundary that
removes it. Check an existing owner before creating a module. For an audit-only
request, report findings without moving code.

## Ownership

- Backend: `api` owns auth/validation/response shaping; `application` owns
  orchestration and persistence policy; `media` owns runtime and hot paths;
  `db` owns SQL; `domain` owns graph vocabulary; `planner` owns selection policy.
- Frontend: `app` composes features; `core` owns shared transport/state/pure
  helpers; `features` own bounded UI; `history` owns its state and rendering.

Read the relevant part of [the layering roadmap](../../../layering-roadmap.md)
for the current boundary. Use an existing code index when available, and verify
suspected upward dependencies against current source.

## Choosing a boundary

A file split can improve readability; a module should own a coherent concept.
Tighten visibility where the owner already exists. Introduce a capability or
interface only to remove concrete coupling. A crate needs a stable narrow API,
acyclic dependencies, and a demonstrated compilation, reuse, or isolation benefit.

Stop when another split adds wrappers or navigation without clarifying ownership.
Avoid endpoint-local CRUD wrappers, compatibility facades that recreate coupling,
and hot-path dispatch/channel hops introduced solely for layering.

For size-driven work, `scripts/check/source-audit.sh` is authoritative: Rust
warns at 800 lines and fails above 999; frontend TypeScript also caps at 999.
Do not aim for the cap. Split by behavior/ownership, including tests, and keep
new extractions below the warning band. Reduce existing oversize pressure
without growing other oversized files or weakening the gate.

## Verification

Keep a boundary move behavior-preserving and run focused tests for every moved
responsibility, then `scripts/check/source-audit.sh`. Apply AGENTS.md contract,
frontend, concurrency, and benchmark gates to the actual changes.

For MCP/agent feature boundaries, run the roadmap's negative feature-matrix
compile commands. Lower features must compile with `agent-plane` and
`agent-execution` disabled; a feature graph that implicitly enables the upper
layer does not prove isolation. Keep the `mcp-http-backend` gate beside its
adapter module. Treat `mcp-embedded` as the intentional `mcp-core` plus
`agent-plane` combination; it must not enable `agent-execution`.

Report the dependency change and whether the result is a lexical split,
ownership change, or independently justified crate boundary.
