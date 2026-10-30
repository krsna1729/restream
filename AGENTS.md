# AGENTS.md

Root contract for AI coding agents in this repository. Area rules live in
skills; load the skill for the area you touch.

## Contents

- [Core Rules](#core-rules)
- [Skills](#skills)
- [Repository Map](#repository-map)
- [Commands](#commands)
- [Inner Loop](#inner-loop)
- [Branches and Build Safety](#branches-and-build-safety)
- [Testing](#testing)
- [Rigor and Evidence](#rigor-and-evidence)
- [Merge Strategy](#merge-strategy)
- [Operational Guidance](#operational-guidance)
- [Key References](#key-references)

## Core Rules

- Keep changes small, intentional, and consistent with existing Rust/TypeScript patterns.
- Preserve unrelated user or agent changes. Check `git status` before broad edits, staging, or commits.
- If overlapping work is visible in `git status`, diffs, or file contents, use hunk-based edits and hunk-based git operations. Do not overwrite, reformat, stage, or revert whole files unless explicitly asked.
- Add or update tests for behavior changes.
- No internal or external failure path may crash the engine; isolate faults and surface errors.
- Anywhere in `src/` or `crates/restream-*/src/`: no blocking or per-packet work on Tokio workers or Compio Owner threads; OS-thread entry points `catch_unwind`; codecs run only in FFmpeg child processes; SRT sockets and `PeerTable` stay on Owner threads (details: [media-pipeline](docs/agent-guidance/skills/media-pipeline/SKILL.md)).
- Update docs when changing commands, configuration, architecture, protocols, or user-visible behavior.
- Prefer targeted fixes over rewrites. Add abstractions only when they remove real complexity.

## Skills

Canonical skill bodies live in `docs/agent-guidance/skills/<name>/SKILL.md`.
They supply task-specific guidance within the user's scope and existing
authorization; do not turn reviews into fixes or add approval stops merely
because a skill is relevant.

| Touching | Skill |
|---|---|
| `src/media/`, `crates/restream-*/src/`, protocol paths, stages, ring buffers, any hot path, thread/runtime placement | [media-pipeline](docs/agent-guidance/skills/media-pipeline/SKILL.md) |
| `web/ts/`, `web/styles/`, frontend build, API contract | [frontend](docs/agent-guidance/skills/frontend/SKILL.md) |
| RTMP/SRT/HLS behavior, codecs, live fault recovery | [protocol-test](docs/agent-guidance/skills/protocol-test/SKILL.md) |
| synchronization, thread hops, cancellation, teardown/recovery | [concurrency-proof](docs/agent-guidance/skills/concurrency-proof/SKILL.md) |
| benchmarks and resource regressions | [perf-sweep](docs/agent-guidance/skills/perf-sweep/SKILL.md) |
| Rust or frontend layering and module boundaries | [layering-audit](docs/agent-guidance/skills/layering-audit/SKILL.md) |
| the local MediaMTX demo | [respin](docs/agent-guidance/skills/respin/SKILL.md) |
| live pipeline operations | [restream-ops-agent](docs/agent-guidance/skills/restream-ops-agent/SKILL.md) |

Claude Code shims are generated locally by `cargo xtask setup-skills`
(`.claude/` is gitignored). After adding or editing a canonical skill, rerun it.

## Repository Map

- Backend: `src/`
- Media engine and lifecycle: `src/media/`; shared codec, synchronization and protocol utilities: `crates/restream-*/src/`
- Frontend source: `web/ts/`
- Generated frontend output: `public/js/`
- Tests: `test/`
- Benchmarks: `benches/`
- Docs: `docs/`

## Commands

Use the pinned Rust toolchain from `rust-toolchain.toml`.

- Cargo's job count comes from `.cargo/config.toml` (`jobs = -1`: all CPUs but one); override with `CARGO_BUILD_JOBS`.
- Use `--profile bench` instead of `--release` for local or agent builds.
  Exception: performance evidence that gets committed (capacity ramps, A/B
  runs, profiles) uses real release binaries from
  `cargo xtask build-release` (`target/qual-release/`).
- Cargo writes a profile named `bench` to `target/release`, so
  `cargo build --profile bench` alone does not populate `target/bench/`.
  Harness modes that need binaries at `target/bench/` require
  `cargo xtask build-bench`, the one canonical path to those binaries.

```sh
cargo build --profile bench
cargo test
cargo clippy
cargo fmt --all

cargo xtask setup-skills

npm run test:frontend

cargo bench --bench <name>
cargo xtask harness <mode>   # modes: target/bench/test_harness catalog list-modes
```

Integration tests use a private loopback namespace by default; use `--no-netns` only when required.

## Inner Loop

Maximize useful work per token: act on the files in front of you, pull docs
in on demand, and verify with the narrowest gate first.

- Do not preload architecture docs for scoped fixes; read the doc a task
  touches when it touches it (see Key References).
- Correctness gates run in GitHub PR CI, not locally: push the branch, open
  the PR, and read the failing job. Locally, run only (a) performance
  measurement and (b) the targeted new or changed test or feature you are
  writing (`cargo test <name>`, one harness mode for a new scenario). Do not
  rerun full suites, concurrency contracts, or harness matrices locally to
  confirm what CI already runs on quieter runners. Treat unrelated CI
  failures as separate findings.

| Files touched | CI job that gates it | Local, when adding/changing it |
|---|---|---|
| backend module, timestamp/DTS/PTS logic | Rust unit hygiene and fixtures | `cargo test <new test name>` |
| lifecycle, concurrency primitives, thread hops | Rust unit hygiene and fixtures; Concurrency fast (loom); Concurrency live on master pushes | the new test or loom case only |
| frontend/backend contract surface | API contract | — |
| test media, fixtures, bench/harness setup | Rust unit hygiene and fixtures | — |
| Markdown documentation | Source architecture audit (`cargo xtask docs-audit`) | — |
| `web/ts/`, `web/styles/input.css` | Frontend unit, Frontend app E2E | the new test only |
| RTMP/SRT/HLS protocol behavior | Transport live-harness; master push transport matrix | one harness mode for a new scenario (`test_harness catalog list-modes`) |
| hot-path code | — | relevant `benches/` suite before and after |

## Branches and Build Safety

Work on a branch in the primary checkout. Do not create Git worktrees.
Several agents may share the checkout, so check `git status` before builds
and edits, and keep to your own hunks.

**Never run Cargo builds, tests, checks, clippy, or benchmarks while a live pipeline is running.**
Builds are memory heavy and can push a small host into OOM territory.

Before heavy builds, check for live media and other builds; nothing else
serializes them:

```sh
pgrep -a -x restream
pgrep -a -x mediamtx
pgrep -a -x ffmpeg
pgrep -a -x cargo
```

- Stop only processes owned by this task or covered by the user's restart
  request, after checking their PIDs and command lines. A matching process
  name does not establish ownership. Defer heavy work while other media runs.

## Testing

- Passing test logs should stay quiet: no warnings, panic text, FFmpeg probe chatter, or stale-binary drift.
- Use `cargo fmt --all` and `cargo fmt --all --check`; do not run `rustfmt` directly.
- Resolve media through `src/test_fixtures.rs`; add new committed assets to `REQUIRED_CHECKED_IN_FIXTURES`. Prefer checked-in fixtures over inline media generation.
- Test-only code may adapt or observe production code, never re-implement it: inject fakes through an existing type parameter or constructor and call the production function, rather than adding a `#[cfg(test)]` sibling that repeats its logic.
- `cargo xtask test-hygiene` runs in CI (Rust unit hygiene and fixtures); suppress expected noise at the test helper, not in CI.
- Scale and capacity runs are performance measurement and stay local: `cargo xtask capacity-ramp`, serially, on an idle host.

## Rigor and Evidence

Every change carries the proof its risk calls for, written with the code, and
its evidence in the commit message.

- Put each invariant at the lowest rung that can enforce it: types and
  ownership first, then one checked primitive, then tests of composition
  ([assurance roadmap](docs/assurance-roadmap.md)). Delete a test only when
  the API can no longer express what it defends.
- New behavior: unit tests for the contract, a proptest against a simple model
  for data structures and state machines, a fuzz target for parsers of
  external bytes, a loom model for a cross-thread primitive, and a benchmark
  before and after for hot paths.
- A fix: a test that fails before and passes after.
- A new guard: mutation-check it (disable or weaken it; a test must fail).
- Never test wiring, copies, mock echoes or source text; never re-pin
  incidental behavior.
- Commit messages state the problem, the change, and the evidence: tests
  added, mutation results, measured before/after numbers with host and
  command. That is where measurements live; do not add ledgers or dated
  evidence pages to `docs/`.
- Open work goes in [`docs/backlog.md`](docs/backlog.md); close an item by
  deleting it in the change that does the work.

## Merge Strategy

- Squash-and-merge by default: the branch history records how the work converged.
- Rebase-and-merge when the commits are curated and are the design of the
  change, such as phased architecture or media-pipeline migrations.
- A merge commit only when the branch topology itself matters.

## Operational Guidance

- If the user starts a clearly new, unrelated task, suggest a fresh session to keep context costs down; not mid-task.
- Use the lowest model class that can reliably do the work, and do not use a higher tier for helpers than the main session already has.

## Key References

- Documentation index: `docs/README.md`
- Backlog: `docs/backlog.md`
- Architecture: `docs/architecture.md`
- Media pipeline: `docs/media-pipeline.md`
- Performance: `docs/high-performance-data-path.md`
- Testing: `docs/testing.md`
- Concurrency proofing: `docs/concurrency-proofing.md`
- Assurance roadmap: `docs/assurance-roadmap.md`
- Frontend: `docs/frontend.md`
- Configuration: `docs/configuration.md`
- Observability and logging: `docs/observability.md`
- API: `docs/api-reference.md`
