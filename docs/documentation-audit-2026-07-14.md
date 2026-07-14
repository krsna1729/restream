# Documentation audit: 2026-07-14

This record captures the consolidation decisions from the repository-wide
documentation audit. It is evidence of scope and maintenance policy, not a
second source of product behavior.

## Contents

- [Scope](#scope)
- [Information architecture](#information-architecture)
- [Executable owners](#executable-owners)
- [Content retired](#content-retired)
- [Terminology decisions](#terminology-decisions)
- [Maintenance policy](#maintenance-policy)

## Scope

The audit covered every tracked Markdown file, inbound links, tables of
contents, source-path references, command ownership, architecture claims, and
historical evidence. Maintained prose was compared with the current Rust,
TypeScript, shell, Cargo, and package-script owners.

## Information architecture

`README.md` is the release-binary start path and product overview.
`docs/README.md` is the documentation map. Maintained pages are grouped by
reader need:

- operators: configuration, API, observability, logging;
- contributors: development, testing, architecture, media pipeline;
- advanced contributors: concurrency proofing, performance contracts,
  layering, agent-plane and MCP integration;
- agents: canonical skills and the quality program under
  `docs/agent-guidance/`.

Every maintained prose page has a local contents section. `SKILL.md` files are
procedural units and intentionally do not carry generated tables of contents.

## Executable owners

Documentation explains intent and boundaries while volatile inventories stay
with their executable owner:

| Concern | Owner |
|---|---|
| Build and test commands | `Makefile`, `package.json`, and `scripts/` |
| Runtime variables and defaults | `src/config.rs` |
| API routes and payloads | `src/api/` and contract checks |
| Harness modes, fixtures, cleanup, artifacts | `src/bin/test_harness/` and `test/harness/` |
| Benchmark inventory | `Cargo.toml` and `benches/` |
| Frontend source | `web/ts/` and `web/styles/input.css` |
| Generated frontend output | `public/js/` |
| Quality work and measurements | quality backlog, journal, and baseline ledger |

Docs link to these owners rather than copying line counts, route totals,
complete mode lists, or script bodies.

## Content retired

The pass removed completed migration designs, superseded implementation plans,
dated code-coverage snapshots, and performance narratives whose claims no
longer matched the source. Git history remains the archive for those records.

Maintained architecture, performance, matrix-resource, and MCP pages were
rewritten as current contracts. Historical evidence remains in-tree only when
it still names a live regression obligation or supplies reproducible proof.

## Terminology decisions

Node.js, npm, and TypeScript are current frontend build/test tooling. They must
not be described as the production server. MediaMTX is a current live-harness
peer and interoperability sink; it must not be described as a production
runtime dependency.

The MCP sidecar is Rust code in this repository. The term “sidecar” describes a
deployment boundary, not a separate implementation stack.

## Maintenance policy

The Markdown gate checks structure, links, diagram format, executable ownership,
and retired-reference regressions. The source audit supplies machine-derived
inventory without embedding volatile counts in prose.

When behavior changes:

1. update the executable owner and its tests;
2. update only the maintained page that explains the affected contract;
3. link rather than repeat another document's concern;
4. remove completed plans instead of converting them into status diaries;
5. run the Markdown and source-audit gates before commit.
