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
- [Autonomous Quality Loops](#autonomous-quality-loops)
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

- Prefix Cargo and other heavy commands with `scripts/build/resource-limit.sh`.
- Use `--profile bench` instead of `--release` for local or agent builds.
- Cargo hardcodes `target/release` as the output dir for a profile named
  `bench` (long-standing `cargo bench` compatibility quirk); `cargo build
  --profile bench` alone does not populate `target/bench/`. For measurement
  harness modes that require binaries at `target/bench/`, build with
  `scripts/build/bench-harness.sh` instead — the one canonical path to those
  binaries.
- Edit `web/ts/` and `web/styles/input.css`; do not hand-edit generated files in `public/js/`.
- Default frontend verification is `npm run test:frontend`; use Playwright when browser-only behavior is touched.

```sh
scripts/build/resource-limit.sh cargo build --profile bench
scripts/build/resource-limit.sh cargo test
scripts/build/resource-limit.sh cargo clippy
cargo fmt --all

scripts/agent/worktree.sh <id>
source .local/worktrees/<id>/.agent-state/setup.env
scripts/agent/worktree.sh --cleanup <id>

scripts/agent/setup-skills.sh

npm run test:frontend
npm run test:frontend:coverage
npx playwright test

scripts/build/resource-limit.sh cargo bench --bench <name>
scripts/build/resource-limit.sh target/debug/test_harness mixed-anchor
```

Integration tests use a private loopback namespace by default; use `--no-netns` only when required.

## Inner Loop

Maximize useful work per token: act on the files in front of you, pull docs
in on demand, and verify with the narrowest gate first.

- Do not preload architecture docs for scoped fixes; read the doc a task
  touches when it touches it (see Key References).
- Skill bodies load on invocation; the canonical versions live in
  `docs/agent-guidance/skills/`. Claude Code shims are generated locally by
  `scripts/agent/setup-skills.sh` (`.claude/` is gitignored;
  `scripts/agent/worktree.sh` runs it automatically in new worktrees). After
  adding or editing a canonical skill, run `scripts/agent/setup-skills.sh` to
  refresh local shims.
- Pick the first gate by files touched, then broaden to full
  `scripts/build/resource-limit.sh cargo test` only when the change crosses module
  boundaries or shared contracts. Treat unrelated full-suite failures as
  separate findings.

| Files touched | First gate |
|---|---|
| one backend module | `scripts/build/resource-limit.sh cargo test <module>` |
| timestamp/DTS/PTS logic | `scripts/build/resource-limit.sh cargo test av_sync` |
| lifecycle in `engine.rs`, `srt.rs`, `ts_chunk_ring.rs`, `avio.rs`, `recording.rs`, `file_ingest.rs`, `external_transcoder.rs` | `scripts/check/concurrency/contract.sh` |
| concurrency primitives or thread hops | `scripts/check/concurrency/fast.sh` |
| frontend/backend contract surface | `scripts/check/api-contract.sh` |
| test media, fixtures, bench/harness setup | `scripts/check/fixture-discipline.sh` |
| Markdown documentation | `node scripts/check/docs.mjs` |
| `web/ts/`, `web/styles/input.css` | `npm run test:frontend` (plus Playwright for browser-only behavior) |
| hot-path code | relevant `benches/` suite before and after |
| RTMP/SRT/HLS protocol behavior | `test_harness` `correctness*` modes (protocol-test skill) |

## Build and Worktree Safety

**Never run `cargo build`, `cargo test`, or `cargo clippy` while a live pipeline is running.**
Static FFmpeg libraries can push WSL2 into OOM territory.

Before heavy builds in multi-worktree sessions:

```sh
export RESTREAM_BUILD_LOCK_FILE=/tmp/restream-build.lock
pkill -x restream; pkill -x mediamtx; pkill -x ffmpeg
```

- Prefer `scripts/agent/worktree.sh <id>` over manual setup.
- Use one worktree per agent or task.
- Treat `target/`, `.cargo/`, and `node_modules/` as copied caches owned by the destination worktree; do not point multiple worktrees at one live `target/`.
- Use `--no-share-static` when touching native or linkage-related inputs such as `build.rs`, Docker/static build scripts, or native `test/*.c` helpers.
- Use `.agent-state/setup.env` as the source of truth for `WORK_ROOT`, `RESTREAM_BUILD_LOCK_FILE`, and the shared static root.

## Media Rules

Before changing `src/media/`, read:

- `docs/architecture.md`
- `docs/media-pipeline.md`
- `docs/high-performance-data-path.md`
- `docs/testing.md`

Core invariants:

- Tokio tasks own sockets, API handlers, timers, and inline mux/demux work.
- Blocking FFmpeg calls and blocking `srt_send()` belong on dedicated OS threads.
- Wrap FFmpeg/libsrt OS-thread entry points with `catch_unwind(AssertUnwindSafe(...))`.
- No internal or external failure path may crash the engine; isolate faults and surface errors.
- Keep media timestamps separate from wall-clock/application time.
- Respect `MediaPacket.format`; consumers must handle `Flv` and `Raw` explicitly.
- RTMP video timestamps are DTS; signed FLV composition offset derives PTS.
- Normalize SRT Stream IDs before lookup.
- Duplicate SRT publishers are not bonded ingest; only libsrt group connections are bonds.
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
- For concurrency or thread-hop changes, extend `scripts/check/concurrency/fast.sh` or explain why the existing proof gate already covers the change.
- If teardown or recovery semantics change, update the live harness assertion and the operator-visible status contract in the same change.
- Gate selection by files touched: see the Inner Loop table above.
- Keep libtest parallel for correctness work, but run it through `scripts/build/resource-limit.sh`; the wrapper bounds `RUST_TEST_THREADS` with a separate memory/CPU-derived test budget unless explicitly overridden.
- For scale or integration checks, use `scripts/build/resource-limit.sh target/debug/test_harness mixed-anchor`.

## Autonomous Quality Loops

- The autonomous quality program lives in `docs/agent-guidance/quality/`
  (README, backlog, journal, baselines) with agent-neutral skills under
  `docs/agent-guidance/skills/` (quality-loop, proof-sweep, resilience-sweep,
  modularity-sweep, perf-sweep, backlog-groom, plus supporting task skills).
- The `docs/agent-guidance/skills/<name>/SKILL.md` files are canonical for
  every agent. Claude Code registration shims in `.claude/skills/` are
  generated locally by `scripts/agent/setup-skills.sh` (not checked in);
  agents without a skill system follow the canonical files directly.
- One loop iteration = one backlog item, verified by gates, journaled, and
  committed on its own. Loops never push.
- One quality loop per host. Multi-agent work goes through
  `scripts/agent/worktree.sh` with a host-global
  `RESTREAM_BUILD_LOCK_FILE=/tmp/restream-build.lock`.
- Loops skip (never kill) media processes they did not start.
- Backlog items are tier-tagged; do not attempt items above your model tier.

## Operational Guidance

- If the user starts a clearly new, unrelated task, suggest a fresh session to keep context costs down.
- Do not suggest that mid-task or for follow-up questions on the same topic.
- Use the lowest model class that can reliably do the work, and do not use a higher tier for helpers than the main session already has.
- `haiku` / `gpt-5.4-mini` / `gpt-5.4-nano`: retrieval, repo navigation, simple explanations, tiny wording edits.
- `sonnet` / `gpt-5.4`: default for scoped fixes, features, tests, and medium repo edits.
- `opus` / `gpt-5.5`: concurrency or lifecycle redesign, hot-path architecture, benchmark-driven decisions, or novel protocol behavior.

## Key References

- Overview/setup: `README.md`
- Current priorities: `docs/current-priorities.md`
- Architecture: `docs/architecture.md`
- Media pipeline: `docs/media-pipeline.md`
- Performance: `docs/high-performance-data-path.md`
- Testing: `docs/testing.md`
- Concurrency proofing: `docs/concurrency-proofing.md`
- Agent skills (canonical, agent-neutral): `docs/agent-guidance/skills/`
- Autonomous quality program: `docs/agent-guidance/quality/README.md`
- Layering audit skill: `docs/agent-guidance/skills/layering-audit/SKILL.md`
- Configuration: `docs/configuration.md`
- Observability: `docs/observability.md`
- Logging: `docs/logging.md`
- API: `docs/api-reference.md`
