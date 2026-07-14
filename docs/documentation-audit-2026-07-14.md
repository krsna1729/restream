# Documentation Audit — 2026-07-14

This audit covers all 61 tracked Markdown files at commit `b50d52f4`, plus
their links to current source, scripts, routes, tests, and generated assets.
It distinguishes maintained product guidance from design records, agent
contracts, measurement ledgers, and legal text so each audience has a clear
entry point without discarding useful history.

## Contents

- [Executive finding](#executive-finding)
- [Verified inconsistencies](#verified-inconsistencies)
- [Why the documentation feels sprawling](#why-the-documentation-feels-sprawling)
- [Target information architecture](#target-information-architecture)
- [Document disposition](#document-disposition)
- [Style and maintenance contract](#style-and-maintenance-contract)
- [Remove high-churn facts from maintained prose](#remove-high-churn-facts-from-maintained-prose)
- [Consolidation sequence](#consolidation-sequence)
- [Acceptance criteria](#acceptance-criteria)

## Executive finding

The repository has strong technical evidence but a weak information
architecture. Newcomer guidance, current reference, future architecture,
completed implementation plans, point-in-time audits, and append-only
experiment logs all sit at nearly the same level. The result was a large flat
documentation set with no previous central index or consistent local TOCs.

The right consolidation is not one giant manual. Keep four deliberately
different layers:

1. **Start and guides** for newcomers and routine tasks.
2. **Reference and concepts** for operators and advanced contributors.
3. **Decisions and plans** for maintained forward-looking work.
4. **Evidence archive** for dated audits, experiments, and completed plans.

This change establishes the navigation layer, local TOC convention, and fixes
the immediately verifiable broken links and current-source drift. Moving large
documents should be a separate mechanical change after link validation is
automated, because many agent instructions and historical records cite their
current paths.

## Verified inconsistencies

### Current code and command drift

- `README.md` and several architecture documents still named the removed
  monolith `src/api.rs`; the live router is `src/api/router.rs` and handlers
  are split across `src/api/`.
- HLS references still named removed `src/media/hls.rs`; the implementation is
  now split under `src/media/hls/`, with lifecycle integration in
  `src/media/engine_hls.rs`.
- The `docs/architecture.md` key-file table carried obsolete line counts and
  removed modules. Volatile line counts do not belong in a maintained
  architecture overview; `scripts/check/source-audit.sh` already produces the
  machine-readable inventory.
- `docs/testing.md` claimed a complete route audit using copied totals, but the
  live router and hand-maintained matrix had already diverged. Route
  completeness must come from router constants and generated checks.
- The source-audit narrative in `architecture/arch_gap_analysis.md` says the
  audit covers `public/ts`; authored frontend source moved to `web/ts`.
  The shell half of `scripts/check/source-audit.sh` uses `web/ts`, while its
  embedded JSON inventory still scans `public/ts`, so this is also a code-side
  audit inconsistency rather than a documentation-only typo.
- `ARCHITECTURE_GUARDRAILS.md` describes old per-file no-growth limits, while
  the current source audit applies one 2,000-line cap across Rust, authored
  TypeScript, and hand-written JavaScript tests.
- The configuration reference omitted four centralized runtime settings:
  ingest-disconnect grace, file-log retention, secure session cookies, and SRT
  egress local-port reuse.
- The developer guide's benchmark list omitted the current `hls_fmp4_cost` and
  `rtmp_serializer` suites declared in `Cargo.toml`.

### Broken navigation

The initial audit found 11 broken Markdown links:

- three skill links used too few `..` path components;
- `test-guardrails` linked root files as if they lived under `docs/`;
- `ffmpeg-versions.md` linked three sibling docs through the repository root;
- `matrix-resource-constraints.md` linked the removed HLS monolith.

There was also no central documentation index and no tracked multi-section
document had a consistent TOC marker.

### Content overlap

- `README.md` and `development.md` both own setup and the daily loop. The README
  should keep the ten-minute path; the developer guide should own detailed
  prerequisites and workflows.
- `testing.md` mixes maintained test policy, a route coverage matrix, live-mode
  reference, dated validation results, a future plan, resource measurements,
  and findings. `testing-strategy.md` separately explains the two-tier
  decision. Keep the decision record, but split dated results out of the
  maintained testing reference.
- `architecture.md`, `architecture/arch.md`, `architecture/impl.md`,
  `architecture/arch_gap_analysis.md`, and `layering-roadmap.md` overlap.
  `architecture.md` should be current state; `layering-roadmap.md` should be
  the active sequence; the three files under `architecture/` should become a
  clearly labeled design-record set.
- `high-performance-data-path.md`, `resource-sweep.md`,
  `matrix-resource-constraints.md`, the Mahashivratri scenario, and the quality
  baseline ledger repeat measurements. Keep rules and interpretation in the
  first three, workload definition in the scenario, and raw dated measurements
  in the ledger.
- `agent-plane-integration.md`, `mcp-rust-architecture.md`, and the
  `restream-ops-agent` skill repeat tool contracts. Product architecture should
  explain boundaries; the skill and its reference should own the executable
  workflow and exact tool mapping.

## Why the documentation feels sprawling

The issue is document role ambiguity more than raw file count:

| Symptom | Cause | Correction |
|---|---|---|
| Newcomers encounter thousand-line documents immediately | No audience-based entry point | Route readers through `docs/README.md` |
| Old plans read like current architecture | Status is implicit in filenames and prose | Add explicit current/proposal/evidence labels |
| The same command appears in several forms | No canonical owner per topic | Assign one canonical reference and link to it |
| Dated results dominate maintained guides | Evidence and contract live together | Move results to a dated evidence tree |
| Link and path drift is discovered manually | No docs gate | Add a link, TOC, and stale-path check |
| Heading and fence styles vary | No written style contract | Enforce the small rules in `docs/README.md` |

## Target information architecture

Use this destination structure. Preserve Git history with `git mv` and land
the migration by group, not as one repository-wide rewrite.

```text
docs/
  README.md                         # complete documentation index
  guides/
    development.md                 # detailed contributor workflow
    release.md                     # release procedure
  concepts/
    architecture.md                # current system and ownership
    media-pipeline.md
    high-performance-data-path.md
    concurrency-proofing.md
  reference/
    api.md
    configuration.md
    observability.md
    logging.md
    ffmpeg-versions.md
  testing/
    README.md                       # maintained testing reference
    strategy.md                     # accepted decision and rationale
    stage-boundary-proof-map.md
    regression-artifacts.md
    matrix-resource-constraints.md
  decisions/
    current-priorities.md
    layering-roadmap.md
    architecture/                   # target, implementation, gap records
  scenarios/
    mahashivratri.md
  evidence/
    2026-07-02-concurrency-coverage.md
    resource-sweeps.md
    run-to-completion-analysis.md
    quality/                        # baselines, journal, experiment reports
  agent-guidance/
    quality/
    skills/
```

Root-level `AGENTS.md`, `CLAUDE.md`, legal documents, distribution manifests,
and directory-local README files remain near the code or artifact they govern.

## Document disposition

### Entry points and maintained guides

| Document | Audience | Decision |
|---|---|---|
| `README.md` | Everyone | Keep short; setup and route to the docs index |
| `docs/README.md` | Everyone | New canonical documentation index |
| `docs/development.md` | Contributors | Keep; remove product-change diary material |
| `docs/release-runbook.md` | Release operators | Keep as task guide |
| `test/harness/README.md` | Harness contributors | Keep beside manifests; expand only with DSL changes |

### Current concepts and reference

| Document | Canonical role | Decision |
|---|---|---|
| `docs/architecture.md` | Current runtime and ownership | Keep; remove volatile file line counts |
| `docs/media-pipeline.md` | Current media behavior | Keep |
| `docs/high-performance-data-path.md` | Hot-path rules | Dated audit logs moved to `docs/evidence/`; keep rules and measurement workflow |
| `docs/concurrency-proofing.md` | Current proof policy | Keep |
| `docs/configuration.md` | Config reference | Keep; generate/check env inventory |
| `docs/api-reference.md` | HTTP reference | Keep; generate/check route inventory |
| `docs/observability.md` | Runtime diagnostics reference | Keep |
| `docs/logging.md` | Logging reference | Keep; retain callsite table only if checked |
| `docs/ffmpeg-versions.md` | Native version reference | Keep |
| `docs/source-distribution.md` | Distribution reference | Keep |
| `docs/release-compliance.md` | Compliance reference | Keep |
| `docs/agent-plane-integration.md` | Agent-plane boundary | Merge architectural overlap from MCP doc |
| `docs/mcp-rust-architecture.md` | Older MCP design | Merge current parts, then archive as a design record |

### Testing and proof

| Document | Canonical role | Decision |
|---|---|---|
| `docs/testing.md` | Maintained test reference | Dated results, manual route/coverage inventories, measurements, and rollout plan moved to `docs/evidence/` |
| `docs/testing-strategy.md` | Accepted test-tier decision | Keep as decision record |
| `docs/stage-boundary-proof-map.md` | Maintained proof coverage | Keep |
| `docs/regression-artifacts.md` | Maintained replay index | Keep |
| `docs/matrix-resource-constraints.md` | Scale constraints | Keep, deduplicate measurements |
| `docs/resource-sweep.md` | Sweep workflow and interpretation | Keep workflow; move snapshots to evidence |
| `docs/concurrency-proof-coverage-2026-07-02.md` | Dated evidence | Move to evidence |

### Plans and historical analysis

| Document | Role | Decision |
|---|---|---|
| `docs/current-priorities.md` | Maintained priority view | Keep and review on major releases |
| `docs/layering-roadmap.md` | Active refactor sequence | Keep until complete, then archive |
| `docs/architecture/arch.md` | Target design record | Label as design record; stop duplicating current maps |
| `docs/architecture/impl.md` | Completed migration plan | Freeze and archive after extracting open items |
| `docs/architecture/arch_gap_analysis.md` | Completion evidence | Freeze as dated evidence after final reconciliation |
| `docs/run-to-completion-analysis.md` | Historical analysis | Move to evidence; remove it from newcomer paths |
| `docs/mahashivratri-hero-scenario.md` | Durable scenario | Keep under scenarios; link measurements instead of copying |
| `docs/parallel-agent-framework.md` | Agent workflow design | Keep with agent guidance or decisions, not product concepts |

### Agent program and evidence ledgers

| Document family | Decision |
|---|---|
| `AGENTS.md`, `CLAUDE.md` | Keep at root; agent contract and compatibility shim |
| `ARCHITECTURE_GUARDRAILS.md` | Keep at root because CI cites it; update from the live gate |
| `docs/agent-guidance/skills/**` | Keep paths stable; these are executable contracts |
| `docs/agent-guidance/quality/README.md` | Keep as quality-program index |
| `backlog.md` | Keep active; archive completed items out of the hot view |
| `journal.md` | Keep append-only, but index by month when it becomes unwieldy |
| `baselines.md` | Split stable benchmark ledger from dated experiment narratives |
| dated MSR/RTMP reports | Keep as evidence; organize by date or scenario |
| `LICENSE.md`, `distribution/THIRD_PARTY_COMPONENTS.md` | Exempt from editorial restructuring; legal/release records |

## Style and maintenance contract

The minimal contract is intentionally small:

- one H1 title per prose document;
- sentence-case headings;
- a local H2 `Contents` section for every multi-section prose document;
- one blank line around headings, lists, tables, and fenced blocks;
- `sh` fences by default, `bash` only for Bash-specific syntax;
- relative Markdown links for reading paths;
- explicit `Current guidance`, `Proposal`, or `Evidence as of YYYY-MM-DD`
  status near the top of non-obvious documents;
- no hard-coded source line counts in maintained prose;
- no claim of complete route/config/test inventory unless a check derives it
  from source.

Legal texts, generated manifests, and one-section compatibility shims do not
need synthetic local TOCs. They remain listed in the central documentation
index or in the release/compliance chain that owns them.

## Remove high-churn facts from maintained prose

Maintained guides should explain ownership, behavior, invariants, and how to
obtain current evidence. They should not copy facts that change whenever code
is reorganized or another test is added.

Remove or generate these high-churn details:

- source-file and directory line counts;
- largest-file rankings and per-file growth snapshots;
- manually counted routes, tests, benchmarks, modes, assertions, or modules;
- exhaustive source-file inventories duplicated outside their owning module or
  generated report;
- current coverage percentages and benchmark/resource measurements without a
  dated evidence boundary;
- completed-phase progress logs embedded in current architecture or developer
  guides.

| High-churn fact | Durable replacement |
|---|---|
| File line counts and rankings | `target/source-audit.json` from `scripts/check/source-audit.sh` |
| API route totals | Router constants and a generated route inventory |
| Test or assertion totals | Test runner output or a generated CI artifact |
| Benchmark suite inventory | `Cargo.toml`, validated by a docs check when a curated list is useful |
| Coverage percentages | Dated coverage artifact linked from the evidence index |
| Performance and resource numbers | Dated baseline ledger with commit, host, and replay command |
| Migration completion details | Frozen gap-analysis/evidence record, not the current architecture guide |

Stable limits and defaults that are part of a user-visible contract still
belong in reference documentation. The distinction is whether changing the
fact represents a contract change or merely normal repository churn.

## Consolidation sequence

1. **Navigation and correctness** — land `docs/README.md`, local TOCs, broken
   link fixes, and obvious removed-path corrections. This is the current
   bounded change.
2. **Automated docs gate** — add a validator for
   relative links, TOC presence/freshness, and one H1. Extend it to flag known
   removed paths, compare router constants with the documented route inventory,
   and compare centralized production env keys with the configuration reference.
3. **Remove high-churn snapshots — complete in this audit branch.** Line counts,
   manual totals, benchmark inventories, callsite tables, and dated hot-path
   snapshots were removed from maintained prose or replaced with generated
   inventories and dated evidence.
4. **Testing split — complete for high-churn material.** Policy and live-mode
   reference remain in `testing.md`; route/coverage snapshots, dated results,
   measurements, and the end-to-end rollout plan moved to `docs/evidence/`.
5. **Architecture reconciliation** — make `architecture.md` the only current
   state map; label/freeze the target, implementation, and gap documents.
6. **Evidence move** — move dated reports and experiment narratives with link
   rewrites in one mechanical commit.
7. **Reference generation** — generate route/config indexes into checked docs
   or validate hand-written docs against source in CI.

Do not start with mass renames. Stable links and executable agent contracts are
more valuable than a perfect directory tree, and the first move should happen
only after the docs gate protects the rewrite.

## Acceptance criteria

The consolidation is complete when:

- a newcomer reaches a working build and the correct first test without
  opening a design record or evidence ledger;
- an operator can find every public configuration and API contract from the
  central index;
- an advanced contributor can find media, concurrency, performance, and proof
  invariants without reading dated results;
- every tracked prose document is centrally indexed or explicitly classified
  as legal, generated, local, agent-only, or historical;
- every maintained multi-section document has a local TOC;
- the docs gate reports no broken relative links or references to removed
  canonical source paths;
- route and configuration completeness claims are derived from source rather
  than manually counted;
- maintained prose contains no source line counts, file-size rankings, or
  manually maintained test/route totals;
- dated evidence states its commit, environment, and replay command.
