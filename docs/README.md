# Documentation

Every page here describes Restream as it is now. History lives in git: a
measured change records its numbers in its commit message, and a removed
document is one `git log -- <path>` away.

## Contents

- [Start here](#start-here)
- [Operate and integrate](#operate-and-integrate)
- [Change the system](#change-the-system)
- [Measure](#measure)
- [Release](#release)
- [Agents](#agents)
- [Documentation rules](#documentation-rules)

## Start here

1. [Project README](../README.md): setup, the daily loop, the codebase map.
2. [Developer guide](development.md): prerequisites, scoped workflows,
   worktrees for parallel agents.
3. [Architecture](architecture.md): runtime and ownership boundaries,
   dataplane invariants, layering rules.
4. [Testing](testing.md): the narrowest proof gate for a change, proof maps,
   regression artifacts.
5. [Backlog](backlog.md): the one list of open work.

## Operate and integrate

- [Configuration](configuration.md): environment variables, persisted
  settings, ports and paths.
- [API reference](api-reference.md): HTTP contracts and examples.
- [Observability](observability.md): health, telemetry, alerts, diagnostics
  and logging.
- [Agent plane](agent-plane-integration.md): agent-plane and MCP contracts.

## Change the system

- [Media pipeline](media-pipeline.md): protocols, codecs and stages.
- [Egress architecture](egress-architecture.md): the fabric's ownership and
  concurrency contract.
- [Runtime crossings](runtime-crossings.md): every Compio/Tokio/FFmpeg
  crossing with its mechanism, rate, bound and verdict.
- [High-performance data path](high-performance-data-path.md): hot-path
  invariants and how to measure them.
- [Concurrency proofing](concurrency-proofing.md): which proof catches which
  bug, and the gates.
- [Isolation audit](isolation-audit.md): every resource entities share, its
  per-entity bound and its test; open findings.
- [Assurance roadmap](assurance-roadmap.md): which rung (types, tests, Kani,
  TLA+, Lean) should prove which invariant.
- [Frontend](frontend.md): design system, route contract, visual baseline
  (operator walkthrough: [operator baseline](../test/frontend/redesign/specs/operator-baseline.md)).

## Measure

- [Capacity ramp](capacity-ramp.md): egress fan-out capacity per protocol.
- [Resource sweep](resource-sweep.md): the measurement harness and its matrix
  constraints.
- [Mahashivratri scenario](mahashivratri-hero-scenario.md): the scale workload
  definition.

## Release

- [Release runbook](release-runbook.md), [release compliance](release-compliance.md)
  and [source distribution](source-distribution.md).
- [FFmpeg versions](ffmpeg-versions.md): native media dependency selection.
- [Third-party components](../distribution/THIRD_PARTY_COMPONENTS.md) and the
  [Docker seccomp profile](../distribution/docker/README.md).
- [MIT license](../LICENSE.md).

## Agents

- [AGENTS.md](../AGENTS.md) is the agent contract; [CLAUDE.md](../CLAUDE.md)
  points Claude Code at it.
- [`agent-guidance/skills/`](agent-guidance/skills/) holds the task skills:
  [concurrency proof](agent-guidance/skills/concurrency-proof/SKILL.md),
  [layering audit](agent-guidance/skills/layering-audit/SKILL.md),
  [perf sweep](agent-guidance/skills/perf-sweep/SKILL.md),
  [protocol test](agent-guidance/skills/protocol-test/SKILL.md),
  [respin](agent-guidance/skills/respin/SKILL.md) and
  [ops agent](agent-guidance/skills/restream-ops-agent/SKILL.md).
- References: [perf attribution](agent-guidance/skills/perf-sweep/references/advanced-attribution.md),
  [ops tool contract](agent-guidance/skills/restream-ops-agent/references/tool-contract.md).
- [Harness manifests](../test/harness/README.md).

## Documentation rules

- Every multi-section page has a local `Contents` section (`SKILL.md` files
  are exempt). One H1 title, sentence-case headings, `sh` for shell fences.
- Use Mermaid for conceptual diagrams (`flowchart LR` for pipelines,
  `flowchart TD` for layers and lifecycles), tables for comparisons, and
  `text` fences only for literal output, syntax and file trees. No checked-in
  SVG renderings.
- Describe current behavior with current source paths and runnable commands.
  Do not keep dated evidence, ledgers or completed plans here; put evidence in
  the commit message of the change it justifies.
- Let executable artifacts own executable detail: scripts own build and gate
  steps, manifests own versions, CLI help owns flags. Link to the entrypoint
  instead of copying it.
- When a source or command rename lands, update the owning page in the same
  change and run `node scripts/check/docs.mjs` (`cargo xtask gates` selects it
  for Markdown changes).
