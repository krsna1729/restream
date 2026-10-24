# Documentation

Every page here describes Restream as it is now. History lives in git: a
measured change records its numbers in its commit message, and a removed
page is one `git log -- <path>` away.

## Contents

- [Start here](#start-here)
- [Operate and integrate](#operate-and-integrate)
- [Change the system](#change-the-system)
- [Measure](#measure)
- [Release](#release)
- [Agents](#agents)
- [Documentation rules](#documentation-rules)

## Start here

1. [Project README](../README.md): what Restream is, quick start, container
   launch.
2. [Developer guide](development.md): prerequisites, first build, daily
   loop, worktrees, shared-host rules.
3. [Architecture](architecture.md): the system at a glance, thread
   placement, input selection, stages, recovery, dataplane invariants,
   layering rules.
4. [Testing](testing.md): the narrowest gate for a change, proof maps,
   regression artifacts.
5. [Backlog](backlog.md): the one list of open work.

## Operate and integrate

- [Configuration](configuration.md): environment variables, persisted
  settings, ports and paths.
- [API reference](api-reference.md): HTTP routes, request and response
  contracts, examples.
- [Observability](observability.md): health, telemetry, alerts, diagnostics
  and logging.
- [Agent plane](agent-plane-integration.md): agent-plane and MCP contracts.

## Change the system

- [Media pipeline](media-pipeline.md): RTMP, SRT, HLS push and pull, MoQ,
  codecs and stages.
- [Egress architecture](egress-architecture.md): the egress fabric's
  ownership and concurrency contract.
- [Runtime crossings](runtime-crossings.md): every Compio, Tokio and FFmpeg
  crossing with its mechanism, rate, bound and verdict.
- [High-performance data path](high-performance-data-path.md): hot-path
  invariants and how to measure them.
- [Concurrency proofing](concurrency-proofing.md): which proof catches which
  bug, and the gates.
- [Isolation audit](isolation-audit.md): every resource entities share, its
  per-entity bound and its test; open findings.
- [Assurance roadmap](assurance-roadmap.md): which rung (types, tests, Kani,
  TLA+, Lean) should prove which invariant.
- [Frontend](frontend.md): design system and the control room.

## Measure

- [Capacity ramp](capacity-ramp.md): egress fan-out capacity per protocol.
- [Resource sweep](resource-sweep.md): the measurement harness and its matrix
  constraints.
- [Mahashivratri scenario](mahashivratri-hero-scenario.md): the scale workload
  definition.

## Release

- [Release runbook](release-runbook.md): local checks, dry runs and gated tag
  publishing.
- [Release compliance](release-compliance.md) and
  [source distribution](source-distribution.md): license and source
  obligations of the shipped bytes.
- [FFmpeg versions](ffmpeg-versions.md): the external FFmpeg executable and its version.
- [Third-party components](../distribution/THIRD_PARTY_COMPONENTS.md) and the
  [Docker seccomp profile](../distribution/docker/README.md).
- [MIT license](../LICENSE.md).

## Agents

- [AGENTS.md](../AGENTS.md) is the agent contract; [CLAUDE.md](../CLAUDE.md)
  points Claude Code at it.
- [`agent-guidance/skills/`](agent-guidance/skills/) holds the task skills:
  [media pipeline](agent-guidance/skills/media-pipeline/SKILL.md),
  [frontend](agent-guidance/skills/frontend/SKILL.md),
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

- Every multi-section page has a local `Contents` section that links every
  H2 (`SKILL.md` files are exempt). One H1 title, sentence-case headings,
  `sh` for shell fences.
- Use Mermaid for diagrams: `flowchart LR` for pipelines, `flowchart TD` for
  layers and lifecycles, `sequenceDiagram` for protocol exchanges and
  `stateDiagram-v2` for state machines. Name real components in node labels,
  keep each diagram small, and say in one sentence what it shows. Use `text`
  fences only for literal output, syntax and file trees. No checked-in SVG
  renderings or box-drawing diagrams.
- Describe current behavior with current source paths and runnable commands.
  Do not keep dated evidence, ledgers or completed plans here; put evidence in
  the commit message of the change it justifies.
- Let executable artifacts own executable detail: scripts own build and gate
  steps, manifests own versions, CLI help owns flags. Link to the entrypoint
  instead of copying it.
- Link every page from this index.
- When a source or command rename lands, update the owning page in the same
  change and run `node scripts/check/docs.mjs` (`cargo xtask gates` selects it
  for Markdown changes).
