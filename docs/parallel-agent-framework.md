# Parallel agent coordination

This guide describes the repository's current coordination contract for
parallel agents. Executable setup details belong to the helper scripts, not to
copied `git worktree`, cache, port, or harness recipes in this page.

## Contents

- [Isolation model](#isolation-model)
- [Create a worktree](#create-a-worktree)
- [Build coordination](#build-coordination)
- [Live correctness and measurement](#live-correctness-and-measurement)
- [Artifacts and long-lived sessions](#artifacts-and-long-lived-sessions)
- [Cleanup](#cleanup)
- [Ownership boundaries](#ownership-boundaries)

## Isolation model

Parallel work has four independent concerns:

| Concern | Owner | Rule |
|---|---|---|
| Source edits | `scripts/agent/worktree.sh` | One task and branch per worktree |
| Heavy builds | `scripts/build/resource-limit.sh` plus worktree `setup.env` | One host-global exclusive build lane |
| Live correctness | `scripts/harness/run.sh` and harness manifests | Use the wrapper and its isolation defaults |
| Measurements | Bench and measurement workflows | Run serially on an otherwise idle host |

A worktree protects source and per-tree build state. It does not by itself
isolate host processes or make concurrent measurements comparable.

## Create a worktree

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

## Build coordination

The generated `setup.env` is the source of truth for `WORK_ROOT`,
`RESTREAM_BUILD_LOCK_FILE`, and shared native state. Source it before running
worktree commands.

Prefix heavy Cargo and native-build commands with
`scripts/build/resource-limit.sh`. The wrapper owns lock behavior, timeout
handling, and job sizing. Do not duplicate those settings in agent-specific
wrappers or documentation.

Never compile while a live Restream, MediaMTX, or FFmpeg pipeline is running.
Do not kill processes owned by another task merely to acquire the build lane.

## Live correctness and measurement

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

## Artifacts and long-lived sessions

Keep each task's generated evidence under the `WORK_ROOT` or `WORK_DIR`
reported by its setup and harness tooling. Never share one writable artifact
directory between tasks.

A long-lived dashboard or debugging session must have a named owner, explicit
runtime directory, and an explicit cleanup path. Prefer repository harness or
demo tooling when it already owns the required services and ports. Ad hoc port
tables in documentation are not reservations and must not become a second
allocator.

## Cleanup

After preserving or publishing the task's work, remove the worktree through the
same helper:

```sh
scripts/agent/worktree.sh --cleanup <id>
```

The helper deliberately leaves branch deletion as a separate decision and
refuses unsafe cleanup unless its explicit force option is used.

## Ownership boundaries

- [AGENTS.md](../AGENTS.md) owns agent safety, gate selection, and model-tier
  guidance.
- `scripts/agent/worktree.sh` owns worktree creation and cache preparation.
- `scripts/build/resource-limit.sh` owns heavy-command serialization.
- [Testing](testing.md) and the harness catalog own live workflow selection.
- Dated performance evidence owns measured host limits; this guide does not
  copy machine-specific concurrency numbers.
