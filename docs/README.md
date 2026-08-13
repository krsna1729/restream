# Documentation Guide

This page is the table of contents for Restream's maintained documentation.
Start with the reading path that matches your task; the dated reports and
design records are useful evidence, but they are not prerequisites for normal
development or operation.

## Contents

- [Newcomer path](#newcomer-path)
- [Operator and integrator path](#operator-and-integrator-path)
- [Advanced contributor path](#advanced-contributor-path)
- [Plans, decisions, and evidence](#plans-decisions-and-evidence)
- [Agent and quality-program documentation](#agent-and-quality-program-documentation)
- [Complete file index](#complete-file-index)
- [Documentation rules](#documentation-rules)

## Newcomer path

Read these in order:

1. [Project README](../README.md) for setup, the daily loop, and the codebase map.
2. [Developer guide](development.md) for prerequisites and scoped workflows.
3. [Architecture](architecture.md) for current runtime and ownership boundaries.
4. [Testing](testing.md) for the narrowest proof gate for a change.

Pull in [media pipeline](media-pipeline.md) and
[high-performance data path](high-performance-data-path.md) only when changing
media behavior or a hot path.

## Operator and integrator path

- [Configuration reference](configuration.md) — environment variables,
  persisted settings, ports, and runtime paths.
- [API reference](api-reference.md) — HTTP contracts and examples.
- [Observability and diagnostics](observability.md) — health, telemetry,
  alerts, and diagnostic endpoints.
- [Logging](logging.md) — levels, sinks, retention, and callsite policy.
- [Release runbook](release-runbook.md) — repeatable release procedure.
- [Release compliance](release-compliance.md) and
  [source distribution](source-distribution.md) — release evidence and source
  obligations.
- [FFmpeg versions](ffmpeg-versions.md) — native media dependency selection.

## Advanced contributor path

- [Media pipeline](media-pipeline.md) — protocol, codec, and stage behavior.
- [High-performance data path](high-performance-data-path.md) — hot-path
  invariants and the measurement workflow.
- [Concurrency proofing](concurrency-proofing.md) — proof ladder and gates.
- [Stage boundary proof map](stage-boundary-proof-map.md) — current invariant
  coverage by runtime boundary.
- [Frontend boundary proof map](frontend-boundary-proof-map.md) — current
  invariant coverage by UI contract boundary.
- [Testing decision record](testing-strategy.md) — why the repository uses
  unit and live correctness tiers.
- [Matrix resource constraints](matrix-resource-constraints.md) and
  [resource sweep](resource-sweep.md) — scale-model constraints and replay.
- [Agent plane integration](agent-plane-integration.md) and
  [MCP Rust architecture](mcp-rust-architecture.md) — agent-plane contracts.
- [Parallel agent framework](parallel-agent-framework.md) — isolated worktree,
  build, harness, and measurement policy.

## Plans, decisions, and evidence

These documents own active plans, durable decisions, workload definitions, or
dated measurements. Completed migration plans and superseded implementation
snapshots are intentionally left to Git history instead of remaining in the
active documentation set.

- [Current priorities](current-priorities.md) — maintained forward-looking
  priorities.
- [Layering roadmap](layering-roadmap.md) — maintained refactor sequence.
- [Frontend layering audit](../audits/frontend-layering-audit-2026-07-21.md) — three-lens frontend architecture audit.
- [Testing decision record](testing-strategy.md) — accepted rationale for the
  unit/live tier boundary. Use [testing.md](testing.md) for current commands.
- [Mahashivratri scenario](mahashivratri-hero-scenario.md) — durable scale
  workload definition.
- [Dashboard v2 live MSR operator review](ui-redesign/operator-msr-live-review-2026-07-16.md)
  — dated browser/CDP evidence for the v2 Overview and Pipeline / Operate
  readiness boundary.
- [Regression artifact index](regression-artifacts.md) — durable replay map for
  historical failures.

Performance experiment records live under
[`agent-guidance/quality/`](agent-guidance/quality/README.md). Their dates and
commit identifiers are part of the evidence boundary; do not treat old numbers
as current baselines without rerunning the documented command.

## Agent and quality-program documentation

- [AGENTS.md](../AGENTS.md) is the repository-wide agent contract.
- [Autonomous quality program](agent-guidance/quality/README.md) explains the
  backlog, journal, baseline ledger, and one-item loop.
- [`agent-guidance/skills/`](agent-guidance/skills/) contains canonical
  task-skill instructions. These are operational contracts, not newcomer
  product documentation.
- [Graphify local index](agent-guidance/graphify.md) documents the optional
  generated code-graph workflow introduced on the rebased master branch.

## Complete file index

This inventory makes every tracked Markdown document reachable. Reading paths
above remain the better way to learn the system.

### Root, legal, distribution, and local guidance

- [Project README](../README.md)
- [Console design system](../DESIGN.md)
- [Agent instructions](../AGENTS.md)
- [Architecture guardrails](../ARCHITECTURE_GUARDRAILS.md)
- [Claude compatibility shim](../CLAUDE.md)
- [Egress architecture](egress-architecture.md)
- [Egress implementation](egress-implementation.md)
- [MIT license](../LICENSE.md)
- [Third-party component manifest](../distribution/THIRD_PARTY_COMPONENTS.md)
- [Harness manifest README](../test/harness/README.md)
- [Phase 0 egress baseline workloads](../test/harness/baselines/egress-phase0/README.md)
- [RTMP fabric A/B baseline](../test/harness/baselines/rtmp-fabric-matrix/README.md)

### Product, contributor, and operator documents

- [Agent plane integration](agent-plane-integration.md)
- [API reference](api-reference.md)
- [Architecture](architecture.md)
- [Concurrency proofing](concurrency-proofing.md)
- [Configuration](configuration.md)
- [Current priorities](current-priorities.md)
- [Development](development.md)
- [FFmpeg versions](ffmpeg-versions.md)
- [Frontend boundary proof map](frontend-boundary-proof-map.md)
- [High-performance data path](high-performance-data-path.md)
- [Layering roadmap](layering-roadmap.md)
- [Logging](logging.md)
- [Matrix resource constraints](matrix-resource-constraints.md)
- [MCP Rust architecture](mcp-rust-architecture.md)
- [Media pipeline](media-pipeline.md)
- [Observability](observability.md)
- [Parallel agent framework](parallel-agent-framework.md)
- [Regression artifacts](regression-artifacts.md)
- [Release compliance](release-compliance.md)
- [Release runbook](release-runbook.md)
- [Resource sweep](resource-sweep.md)
- [Source distribution](source-distribution.md)
- [Stage boundary proof map](stage-boundary-proof-map.md)
- [Testing](testing.md)

### Plans, scenarios, and evidence

- [Mahashivratri hero scenario](mahashivratri-hero-scenario.md)
- [Testing decision record](testing-strategy.md)
- [UI redesign baseline](ui-redesign/brief.md)
- [UI redesign live MSR operator review](ui-redesign/operator-msr-live-review-2026-07-16.md)
- [UI redesign operator task model](ui-redesign/operator-task-model.md)
- [UI redesign state matrix](ui-redesign/state-matrix.yaml)
- [UI redesign route contract](ui-redesign/route-contract.md)
- [UI redesign migration map](ui-redesign/migration-map.md)
- [UI redesign framework decision](ui-redesign/decisions/0001-baseline-before-framework.md)
- [UI redesign visual and accessibility baseline](ui-redesign/visual-accessibility-baseline.md)
- [UI redesign Overview slice](ui-redesign/overview-slice.md)
- [UI redesign component build seam](ui-redesign/build-seam.md)
- [UI redesign operator baseline test plan](../test/frontend/redesign/specs/operator-baseline.md)
- [Quality program](agent-guidance/quality/README.md)
- [Quality backlog](agent-guidance/quality/backlog.md)
- [Performance and resource baselines](agent-guidance/quality/baselines.md)
- [Quality journal](agent-guidance/quality/journal.md)
- [MSR final report — 2026-07-12](agent-guidance/quality/msr-final-report-2026-07-12.md)
- [RTMP egress experiments — 2026-07-13](agent-guidance/quality/rtmp-egress-experiments-2026-07-13.md)
- [SRT egress correctness-at-scale investigation — 2026-08-10](agent-guidance/quality/srt-egress-scale-investigation-2026-08-10.md)
- [MSR 1,200-output resource attribution — 2026-08-13](agent-guidance/quality/msr-1200-resource-attribution-2026-08-13.md)

### Canonical agent skills and references

- [Graphify local index](agent-guidance/graphify.md)
- [Backlog groom](agent-guidance/skills/backlog-groom/SKILL.md)
- [Bench](agent-guidance/skills/bench/SKILL.md)
- [Check](agent-guidance/skills/check/SKILL.md)
- [Concurrency proof](agent-guidance/skills/concurrency-proof/SKILL.md)
- [Layering audit](agent-guidance/skills/layering-audit/SKILL.md)
- [Log audit](agent-guidance/skills/log-audit/SKILL.md)
- [Media test](agent-guidance/skills/media-test/SKILL.md)
- [Modularity sweep](agent-guidance/skills/modularity-sweep/SKILL.md)
- [Performance sweep](agent-guidance/skills/perf-sweep/SKILL.md)
- [Advanced performance attribution](agent-guidance/skills/perf-sweep/references/advanced-attribution.md)
- [Proof sweep](agent-guidance/skills/proof-sweep/SKILL.md)
- [Protocol test](agent-guidance/skills/protocol-test/SKILL.md)
- [Quality loop](agent-guidance/skills/quality-loop/SKILL.md)
- [Resilience sweep](agent-guidance/skills/resilience-sweep/SKILL.md)
- [Respin](agent-guidance/skills/respin/SKILL.md)
- [Restream ops agent](agent-guidance/skills/restream-ops-agent/SKILL.md)
- [Restream ops tool contract](agent-guidance/skills/restream-ops-agent/references/tool-contract.md)
- [Test guardrails](agent-guidance/skills/test-guardrails/SKILL.md)

## Documentation rules

- Every maintained multi-section Markdown document has a local `Contents`
  section. Operational `SKILL.md` packages are exempt so their instructions
  begin immediately. The central index on this page lists the whole
  documentation set.
- Start with one H1 title, use sentence-case headings, and use `sh` for shell
  command fences unless syntax specific to Bash is required.
- Use Mermaid for conceptual diagrams. Prefer `flowchart LR` for pipelines and
  `flowchart TD` for layers, ownership, or lifecycle flow. Keep labels plain,
  omit custom colors and theme-dependent styling, and summarize the important
  relationship in nearby prose.
- Use Markdown tables for comparisons and matrices. Reserve `text` fences for
  literal terminal output, protocol or command syntax, captured results, and
  file trees. Do not check in SVG renderings of documentation diagrams; they
  duplicate the Markdown source and are easy to leave stale.
- State whether a document is current guidance, a proposal, or dated evidence.
  Current behavior must point to current source paths and runnable commands.
- Prefer links over bare repository paths when the path is part of a reading
  sequence. Paths inside commands, tables, and implementation discussion may
  remain code-formatted.
- Keep tutorials and quick starts short. Put exhaustive contracts in reference
  documents and measurements in dated evidence records.
- Let executable artifacts own executable detail. Scripts own package lists,
  build steps, gate sequences, and release mechanics; manifests and lockfiles
  own versions and asset inventories; CLI help owns modes and flags; source
  code owns spawned-process arguments. Documentation should explain why and
  when to use those owners, then link to or invoke their public entrypoint.
- Include a command example only when it is itself the supported user or
  contributor entrypoint. Do not copy a helper script's internal commands into
  prose, and do not repeat the same multi-line shell recipe across maintained
  guides.
- When a source rename or command rename lands, update the relevant canonical
  doc in the same change and run `node scripts/check/docs.mjs`. The staged gate
  router selects this check automatically for Markdown and documentation-check
  changes.
