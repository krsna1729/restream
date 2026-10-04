# AGENTS.md

Instructions for AI coding agents in this repository.

## Contents

- [Core Rules](#core-rules)
- [Repository Map](#repository-map)
- [Commands](#commands)
- [Inner Loop](#inner-loop)
- [Build and Worktree Safety](#build-and-worktree-safety)
- [Media Rules](#media-rules)
- [Hot-Path Rules](#hot-path-rules)
- [Testing](#testing)
- [Rigor and Evidence](#rigor-and-evidence)
- [Merge Strategy](#merge-strategy)
- [Operational Guidance](#operational-guidance)
- [Key References](#key-references)

## Core Rules

- Keep changes small, intentional, and consistent with existing Rust/TypeScript patterns.
- Read the relevant code and docs before editing, especially for media-pipeline behavior.
- Preserve unrelated user or agent changes. Check `git status` before broad edits, staging, or commits.
- If overlapping work is visible in `git status`, diffs, or file contents, use hunk-based edits and hunk-based git operations. Do not overwrite, reformat, stage, or revert whole files unless explicitly asked.
- Add or update tests for behavior changes. Benchmark before and after hot-path changes.
- Concurrency, lifecycle, and thread-hop changes need proof: deterministic unit tests, loom/proptest where feasible, a live harness fault case for recovery behavior, and either a benchmark or an explicit note that the change is off the hot path.
- Update docs when changing commands, configuration, architecture, protocols, or user-visible behavior.
- Prefer targeted fixes over rewrites. Add abstractions only when they remove real complexity.
- For Rust or frontend layering/module-boundary refactors, use `docs/agent-guidance/skills/layering-audit/SKILL.md` and stop when the next split would add more indirection than ownership clarity.

## Repository Map

- Backend: `src/`
- Media engine: `src/media/`
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
  `cargo xtask build-release` (`target/qual-release/`); keep the bench
  profile for the day-to-day inner loop.
- Cargo hardcodes `target/release` as the output dir for a profile named
  `bench` (long-standing `cargo bench` compatibility quirk); `cargo build
  --profile bench` alone does not populate `target/bench/`. For measurement
  harness modes that require binaries at `target/bench/`, build with
  `cargo xtask build-bench` instead — the one canonical path to those
  binaries.
- Edit `web/ts/` and `web/styles/input.css`; do not hand-edit generated files in `public/js/`.
- Default frontend verification is `npm run test:frontend`; use Playwright when browser-only behavior is touched.

```sh
cargo build --profile bench
cargo test
cargo clippy
cargo fmt --all

scripts/agent/worktree.sh <id>
source .local/worktrees/<id>/.agent-state/setup.env
scripts/agent/worktree.sh --cleanup <id>

cargo xtask setup-skills

npm run test:frontend
npm run test:frontend:coverage
npx playwright test

cargo bench --bench <name>
scripts/harness/run.sh <mode>   # modes: target/bench/test_harness catalog list-modes
```

Integration tests use a private loopback namespace by default; use `--no-netns` only when required.

## Inner Loop

Maximize useful work per token: act on the files in front of you, pull docs
in on demand, and verify with the narrowest gate first.

- Do not preload architecture docs for scoped fixes; read the doc a task
  touches when it touches it (see Key References).
- Skill bodies load on invocation; the canonical versions live in
  `docs/agent-guidance/skills/`. Claude Code shims are generated locally by
  `cargo xtask setup-skills` (`.claude/` is gitignored;
  `scripts/agent/worktree.sh` runs it automatically in new worktrees). After
  adding or editing a canonical skill, run `cargo xtask setup-skills` to
  refresh local shims.
- Skills supply task-specific guidance within the user's scope and existing
  authorization. Do not turn reviews into fixes or add approval stops merely
  because a skill is relevant.
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
| lifecycle, concurrency primitives, thread hops | Concurrency fast; Concurrency live on master pushes | the new test or loom case only |
| frontend/backend contract surface | API contract | — |
| test media, fixtures, bench/harness setup | Rust unit hygiene and fixtures | — |
| Markdown documentation | Source architecture audit (`scripts/check/docs.mjs`) | — |
| `web/ts/`, `web/styles/input.css` | Frontend unit, Frontend app, HLS browser | the new test only |
| RTMP/SRT/HLS protocol behavior | Transport live-harness; master push transport matrix | one harness mode for a new scenario (`test_harness catalog list-modes`) |
| hot-path code | — | relevant `benches/` suite before and after |

## Build and Worktree Safety

**Never run Cargo builds, tests, checks, clippy, or benchmarks while a live pipeline is running.**
Static FFmpeg libraries can push a small host into OOM territory.

Before heavy builds, check for live media; nothing else serializes builds
against live runs or other worktrees:

```sh
pgrep -a -x restream
pgrep -a -x mediamtx
pgrep -a -x ffmpeg
pgrep -a -x cargo
```

- Stop only processes owned by this task or covered by the user's restart
  request, after checking their PIDs and command lines. A matching process
  name does not establish ownership. Defer heavy work while other media runs.
- Prefer `scripts/agent/worktree.sh <id>` over manual setup.
- Use one worktree per agent or task.
- Treat `target/`, `.cargo/`, and `node_modules/` as copied caches owned by the destination worktree; do not point multiple worktrees at one live `target/`.
- Use `--no-share-static` when touching native or linkage-related inputs such as `build.rs`, Docker/static build scripts, or native `test/*.c` helpers.
- Use `.agent-state/setup.env` as the source of truth for `WORK_ROOT` and the shared static root.

## Media Rules

Before changing `src/media/`, read:

- `docs/architecture.md`
- `docs/media-pipeline.md`
- `docs/high-performance-data-path.md`
- `docs/testing.md`

Core invariants:

- Tokio owns the control plane: API handlers, reconciliation, timers and application session state. Per-packet media work does not belong on Tokio: SRT and RTMP ingest media run to completion on their ingress owners; the shared TS mux and the HLS segmenter are still on Tokio and are moving to the owner that publishes the feed.
- SRT transport sockets and protocol state (`PeerTable`, timers, ACK/NAK, TX) are owned by dedicated Compio `Owner` threads: one ingress owner thread per listener member (`RESTREAM_SRT_INGRESS_OWNERS`, default 1; more than one share the port as a `SO_REUSEPORT` group and pass srt-rs `ListenerTransfer`s between their threads, never through Tokio), and one Compio runtime with at most one `Owner` per address family per egress shard. Never move SRT sockets or `PeerTable` state onto Tokio; address SRT ingress sessions from Tokio only by `IngressPeer` (Owner index + `LogicalPeerId`) through bounded commands and events.
- RTMP/RTMPS sockets stay on the native TCP/io_uring shard workers until the Compio TCP migration.
- Blocking FFmpeg and other blocking calls belong on dedicated OS threads or the blocking pool, never on Tokio workers or an Owner thread.
- Wrap OS-thread entry points that cross native FFmpeg/FFI boundaries with `catch_unwind(AssertUnwindSafe(...))`; the Compio Owner threads report an `Owner` fault as a bridge event, and a panic there closes the bridge, which Tokio treats as a listener stop.
- No internal or external failure path may crash the engine; isolate faults and surface errors.
- Keep media timestamps separate from wall-clock/application time.
- Respect `MediaPacket.format`; consumers must handle `Flv` and `Raw` explicitly.
- RTMP video timestamps are DTS; signed FLV composition offset derives PTS.
- Normalize SRT Stream IDs before lookup.
- Duplicate SRT publishers are not bonded ingest; bonding is an `srt-rs` group connection (one `LogicalPeerId`), never independent publishers sharing a StreamID.
- HLS storage is in-memory unless an explicit design change says otherwise.

Frontend assets are embedded with `rust-embed`, with a disk-first fallback during development.

## Hot-Path Rules

Hot paths include `src/media/`, ring buffers, mux/demux loops, AVIO queues, SRT/RTMP packet loops, HLS segmenting, and transcoder data paths.

- Benchmark before and after hot-path changes with the relevant `benches/` suite.
- Avoid per-packet allocation, logging, serialization, locks, async channel sends, and system calls.
- Do not add logging inside packet-level loops in `src/media/ring_buffer.rs` or `src/media/avio.rs`.
- Use burst APIs where available.
- Hoist reusable buffers outside loops and clear them inside the loop.
- Prefer `Bytes` and `BytesMut` ownership transfer over payload copies.
- Do not add diagnostic readers or metrics that alter production pipeline behavior.
- Keep protocol correctness tests at least as strong as performance validation.
- For SIMD, benchmark scalar first, keep a scalar fallback, use runtime feature detection, and minimize `unsafe`.

## Testing

- Passing test logs should stay quiet: no warnings, panic text, FFmpeg probe chatter, or stale-binary drift.
- Use `cargo fmt --all` and `cargo fmt --all --check`; do not run `rustfmt` directly.
- Resolve media through `src/test_fixtures.rs`; add new committed assets to `REQUIRED_CHECKED_IN_FIXTURES`.
- Prefer checked-in fixtures over inline media generation for tests, benches, and harness runs.
- Test-only code may adapt or observe production code, never re-implement it: inject fakes through an existing type parameter or constructor and call the production function, rather than adding a `#[cfg(test)]` sibling that repeats its logic.
- Route dashboard API calls through `web/ts/core/api.ts`; update contract tests when routes or payloads change.
- `cargo xtask test-hygiene` runs in CI (Rust unit hygiene and fixtures); suppress expected noise at the test helper, not in CI.
- For concurrency or thread-hop changes, extend the step tables in `crates/xtask/src/concurrency.rs` (`cargo xtask concurrency fast`) or explain why the existing proof gate already covers the change.
- If teardown or recovery semantics change, update the live harness assertion and the operator-visible status contract in the same change.
- Gate selection by files touched: see the Inner Loop table above.
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

- Default to squash-and-merge for ordinary feature, cleanup, UI, CI-fix, and
  agent-authored PRs where the branch history mostly records how the work
  converged.
- Use rebase-and-merge when the PR commits are intentionally curated,
  independently reviewable, and useful as future history, such as phased
  architecture or media-pipeline migrations.
- Use a merge commit only when preserving branch topology matters, such as a
  long-running integration branch or coordinated subsystem branch.
- Rule of thumb: squash when the PR history is "how we got there"; rebase when
  the commits are "the design of the change"; merge when the branch shape itself
  is important.

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
- Agent skills (canonical, agent-neutral): `docs/agent-guidance/skills/`
- Configuration: `docs/configuration.md`
- Observability and logging: `docs/observability.md`
- API: `docs/api-reference.md`
