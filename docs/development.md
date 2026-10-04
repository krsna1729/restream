# Developer guide

Use this guide to understand the supported contributor workflows and where
their executable definitions live. The top-level [README](../README.md) owns
the shortest clone-to-running path.

## Contents

- [Host setup](#host-setup)
- [Clean-checkout preparation](#clean-checkout-preparation)
- [Running a source build](#running-a-source-build)
- [Daily development](#daily-development)
- [Frontend work](#frontend-work)
- [Testing and benchmarks](#testing-and-benchmarks)
- [Static engineering build](#static-engineering-build)
- [Read next](#read-next)
- [Parallel agents and worktrees](#parallel-agents-and-worktrees)

## Host setup

On Debian or Ubuntu, use the developer bootstrap:

```sh
scripts/dev/bootstrap.sh
```

The bootstrap owns system-package installation, the pinned Rust toolchain,
frontend dependencies, MediaMTX, host readiness checks, Git hooks, and the
repo-managed native prefix. Its current options are documented by
`scripts/dev/bootstrap.sh --help`.

Do not copy its package list into setup documentation. The canonical Debian
package groups and installation behavior live in
[scripts/lib/debian-packages.sh](../scripts/lib/debian-packages.sh). For a host
that only runs the live harness, use
`scripts/dev/bootstrap-runtime.sh --help`; that script deliberately excludes
compiler and frontend setup.

Host-level namespace and SRT buffer changes are opt-in. Use the bootstrap's
`--configure-harness-host` option only when those settings should persist on
that machine.

## Clean-checkout preparation

After host setup, prepare generated build inputs with:

```sh
scripts/dev/prepare.sh
```

This script is the clean-checkout owner for the native prefix and embedded
frontend assets. It fails with a focused instruction when frontend dependencies
have not been installed instead of silently installing a second toolchain.

## Running a source build

Follow the [README daily loop](../README.md#daily-loop) to build and run the
service. A source-built process uses the hidden `.restream/` tree in its
working directory for SQLite state, media, logs, and disposable runtime files.
See [Configuration](configuration.md) for path overrides, listeners,
authentication bootstrap, and deployment settings.

The portable distribution contracts are the released Linux archive and runtime
container. `scripts/build/app-static.sh` is an engineering build path, not a
single-file release contract.

## Daily development

The [README](../README.md#daily-loop) owns the normal backend and frontend
command loop so it does not drift between two newcomer guides.

Cargo job sizing lives in `.cargo/config.toml` (`jobs = -1`).
`scripts/build/app-native.sh` verifies that
the development binary uses the expected native linkage.

## Frontend work

Edit authored files under `web/ts/` and `web/styles/input.css`; never edit
generated `public/js/` or `public/output.css` by hand. The current frontend
commands and their composition live in [package.json](../package.json). Run the
scoped frontend test entrypoint for ordinary TypeScript work and the browser-DOM
or Playwright workflow only when behavior depends on real browser facilities.

Layer ownership belongs in [Architecture](architecture.md), API behavior in
[API reference](api-reference.md), and runtime diagnostics in
[Observability](observability.md). This guide intentionally does not duplicate
those maps.

## Testing and benchmarks

[Testing](testing.md) owns gate selection, harness preparation, catalog
inspection, fixtures, and artifact handling. Prefer the narrowest proof that
crosses the changed boundary.

Benchmark target names are declared in `Cargo.toml`, implementations live
under `benches/`, and a measured change records its numbers in its commit
message. The [high-performance data-path guide](high-performance-data-path.md)
explains when a benchmark is required without copying the target inventory.

## Static engineering build

To exercise the fully static engineering path, run:

```sh
scripts/build/app-static.sh
```

The build script creates the native prefix when needed, verifies the resulting
binary, and emits its SBOM. Native source pins and build behavior belong to
[FFmpeg version configuration](ffmpeg-versions.md) and the scripts under
`scripts/build/`; do not repeat their internal sequence here.

Release candidates must use the [release runbook](release-runbook.md), which
certifies the packaged bytes rather than treating this engineering build as a
release artifact.

## Read next

1. [Architecture](architecture.md)
2. [Testing](testing.md)
3. [Configuration](configuration.md)
4. Area-specific documents from the [documentation guide](README.md)

## Parallel agents and worktrees

This guide describes the repository's current coordination contract for
parallel agents. Executable setup details belong to the helper scripts, not to
copied `git worktree`, cache, port, or harness recipes in this page.

### Isolation model

Parallel work has four independent concerns:

| Concern | Owner | Rule |
|---|---|---|
| Source edits | `scripts/agent/worktree.sh` | One task and branch per worktree |
| Heavy builds | `.cargo/config.toml` job count plus worktree `setup.env` | One heavy build at a time per host; check `pgrep -a -x cargo` |
| Live correctness | `scripts/harness/run.sh` and harness manifests | Use the wrapper and its isolation defaults |
| Measurements | Bench and measurement workflows | Run serially on an otherwise idle host |

A worktree protects source and per-tree build state. It does not by itself
isolate host processes or make concurrent measurements comparable.

### Create a worktree

Use the repository helper:

```sh
scripts/agent/worktree.sh <id>
source .local/worktrees/<id>/.agent-state/setup.env
```

The helper owns path and branch defaults, cache seeding, frontend dependency
hydration, static-prefix handling, skill-shim setup, and the generated
`.agent-state/setup.env`. Inspect `scripts/agent/worktree.sh --help` for
current options rather than reproducing them here.

Use the native-isolation option described by the helper when changing native
inputs, linkage, Docker build stages, or native test helpers. Do not manually
share a writable `target/` tree between worktrees.

### Build coordination

The generated `setup.env` is the source of truth for `WORK_ROOT` and
shared native state. Source it before running
worktree commands.

Cargo job sizing lives in `.cargo/config.toml`; nothing serializes builds
across worktrees, so check `pgrep -a -x cargo` before a heavy build.

Never compile while a live Restream, MediaMTX, or FFmpeg pipeline is running.
Do not kill processes owned by another task merely to acquire the build lane.

### Live correctness and measurement

[Testing](testing.md) owns harness selection, catalog inspection, network
namespace behavior, fixture rules, and artifact interpretation. Run live modes
through `scripts/harness/run.sh` so stale bench binaries and build-lock
coordination are handled consistently.

Correctness runs may overlap only when their selected workflow provides
independent network, process, port, and artifact isolation. Use
`scripts/harness/parallel-fast-breadth.sh` for the repository-owned parallel
breadth workflow instead of copying its port allocation.

Benchmarks, bitrate/resource sweeps, and other capacity measurements remain
serial. Their result is invalid when another build, live pipeline, or
measurement competes for the host.

### Artifacts and long-lived sessions

Keep each task's generated evidence under the `WORK_ROOT` or `WORK_DIR`
reported by its setup and harness tooling. Never share one writable artifact
directory between tasks.

A long-lived dashboard or debugging session must have a named owner, explicit
runtime directory, and an explicit cleanup path. Prefer repository harness or
demo tooling when it already owns the required services and ports. Ad hoc port
tables in documentation are not reservations and must not become a second
allocator.

### Cleanup

After preserving or publishing the task's work, remove the worktree through the
same helper:

```sh
scripts/agent/worktree.sh --cleanup <id>
```

The helper deliberately leaves branch deletion as a separate decision and
refuses unsafe cleanup unless its explicit force option is used.

### Ownership boundaries

- [AGENTS.md](../AGENTS.md) owns agent safety, gate selection, and model-tier
  guidance.
- `scripts/agent/worktree.sh` owns worktree creation and cache preparation.
- [Testing](testing.md) and the harness catalog own live workflow selection.
- Dated performance evidence owns measured host limits; this guide does not
  copy machine-specific concurrency numbers.
