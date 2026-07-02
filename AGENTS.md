# AGENTS.md

Instructions for AI coding agents in this repository.

## Principles

- Keep changes small, intentional, and consistent with existing Rust/TypeScript patterns.
- Read relevant code and docs before editing, especially for media-pipeline behavior.
- Preserve unrelated user or agent changes. Check `git status` before broad edits, staging, or commits.
- Assume parallel agents may be editing the same files. If `git status`, diffs, or file contents show
  overlapping work, use hunk-based file edits and hunk-based git operations; do not overwrite,
  reformat, stage, or revert whole files unless that exact whole-file operation was requested.
- Add or update tests for behavior changes. Benchmark before and after hot-path changes.
- Concurrency, lifecycle, and thread-hop changes must ship with proof: deterministic unit tests, loom/proptest where feasible, a live harness fault case for recovery behavior, and either a benchmark or an explicit note that the change is off the hot path.
- Update docs when changing commands, configuration, architecture, protocols, or user-visible behavior.
- Prefer targeted fixes over rewrites. Add abstractions only when they remove real complexity.
- For Rust or frontend layering/module-boundary refactors, use `docs/agent-guidance/skills/layering-audit/SKILL.md` and stop when the next split would add more indirection than ownership clarity.

## Repository Map

- Rust backend and control plane: `src/`
- Media engine and protocols: `src/media/`
- Frontend source: `public/ts/`
- Generated frontend output: `public/js/`
- Tests and validation: `test/`
- Benchmarks: `benches/`
- Docs: `docs/`
- Archived legacy implementation: `old/` (not production runtime)

## Standard Commands

Use the pinned Rust toolchain from `rust-toolchain.toml`.

Always prefix Cargo and heavy commands with `scripts/resource-limit` — it acquires
the build lock and sizes jobs to available RAM/CPU (parallel agents share resources).
Use `--profile bench` instead of `--release` for local/agent builds (same opt-level,
incremental, keeps debug symbols). Never use `--release` from agents.

When parallel agent work is needed, start from `docs/parallel-agent-framework.md`
and prefer `scripts/agent-worktree.sh <id>` over manually assembling a new
worktree. The helper creates `worktrees/<id>`, seeds a pruned high-value debug
cache by default, shares static native outputs by default, and writes
recommended `WORK_ROOT` defaults into `.agent-state/setup.env`.

```sh
scripts/resource-limit cargo build --profile bench
scripts/resource-limit cargo test
scripts/resource-limit cargo clippy
cargo fmt --all

# Parallel agent worktree setup
scripts/agent-worktree.sh <id>
source worktrees/<id>/.agent-state/setup.env
scripts/agent-worktree.sh --cleanup <id>   # when the worktree is no longer needed

# Frontend
npx tsc -p tsconfig.json
npx tailwindcss -i public/input.css -o public/output.css
npm run test:frontend
npm run test:frontend:coverage
npx playwright test

# Benchmarks and integration tests
scripts/resource-limit cargo bench --bench <name>
scripts/resource-limit target/debug/test_harness mixed-anchor   # also: ramp-family, bonding
```

Edit `public/ts/` and `public/input.css`. Do not hand-edit generated files in `public/js/`.
Use `npm run test:frontend` as the default frontend verification loop. The main
coverage gate is `npm run test:frontend:coverage`, which measures the
deterministic Node/fake-DOM TypeScript surface. `npm run test:frontend:coverage:all`
is a broader diagnostic report and `npm run test:frontend:js-smoke` keeps a
small direct guard on generated `public/js/`.
Integration tests use a private loopback namespace by default; use `--no-netns` only when required.

### Build Safety (WSL2)

**Never run `cargo build`, `cargo test`, or `cargo clippy` while a live pipeline is running.**
Static FFmpeg libraries push VSZ to ~3.9 GB; adding a rustc invocation on top can
exhaust the 8 GB WSL2 limit and kernel-panic the VM.

For multi-worktree agent sessions, export a host-global lock file before heavy
builds so separate worktrees do not compile concurrently by accident:

```sh
export RESTREAM_BUILD_LOCK_FILE=/tmp/restream-build.lock
```

```sh
pkill -x restream; pkill -x mediamtx; pkill -x ffmpeg
```

### Parallel Agent Setup

- Use one worktree per agent/task. Prefer `scripts/agent-worktree.sh <id>` so
  cache and artifact layout stay consistent.
- When the task is complete and the worktree is no longer needed, run
  `scripts/agent-worktree.sh --cleanup <id>`. Use `--force-cleanup` only for a
  dirty or locked disposable worktree you explicitly want to discard.
- Treat `target/`, `.cargo/`, and `node_modules/` as copied seed caches owned
  by the destination worktree after setup. The default target warmup is a
  pruned debug subset, not a full `target/` clone. Do not point multiple
  worktrees at the same live `target/` directory.
- Use `scripts/agent-worktree.sh --full-target-cache <id>` only when you
  explicitly want a full `target/` copy despite the size and time cost.
- Use `scripts/agent-worktree.sh --with-incremental <id>` only when you know a
  small warm incremental slice is worth the extra disk cost for that task.
- Treat `.build/static/` and `public/bin/` as shared warm artifacts by default
  when the task does not modify the native/static build layer.
- Disable static sharing with `scripts/agent-worktree.sh --no-share-static <id>`
  when touching `scripts/setup-static-build.sh`, `scripts/build-static.sh`,
  `scripts/bootstrap-dev.sh`, `Dockerfile`, `build.rs`, native `test/*.c`
  helpers, or other native-linkage inputs.
- Use the `.agent-state/setup.env` emitted by the helper as the source of truth
  for `WORK_ROOT`, `RESTREAM_BUILD_LOCK_FILE`, and the shared static root.

## Configuration Defaults

- HTTP: `3030` (`RESTREAM_HTTP_PORT`)
- RTMP: `1935` (`RESTREAM_RTMP_PORT`)
- SRT: `10080` (`RESTREAM_SRT_PORT`)
- SQLite DB: `data.db` (`RESTREAM_DB_PATH`)
- Media directory: `media/` (`RESTREAM_MEDIA_DIR`)

Frontend assets are embedded with `rust-embed`, with a disk-first fallback during development.

## Media Pipeline Rules

Before changing `src/media/`, read `docs/architecture.md`, `docs/media-pipeline.md`,
`docs/high-performance-data-path.md`, and `docs/testing.md`.

Core invariants:

- Tokio tasks own sockets, API handlers, timers, and inline mux/demux work.
- Blocking FFmpeg calls and blocking `srt_send()` belong on dedicated OS threads.
- Wrap FFmpeg/libsrt OS-thread entry points with `catch_unwind(AssertUnwindSafe(...))`.
- Write defensive, resilient engine code: no internal or external failure path may crash the engine; isolate faults and surface errors instead.
- Keep media timestamps separate from wall-clock/application time.
- Respect `MediaPacket.format`: consumers must handle `Flv` and `Raw` explicitly.
- RTMP video timestamps are DTS; signed FLV composition offset derives PTS.
- SRT Stream IDs must be normalized before lookup.
- Duplicate SRT publishers are not bonded ingest. Only libsrt group connections are bonds.
- HLS storage is in-memory; do not introduce segment disk I/O without an explicit design change.

## Hot-Path Rules

Hot paths include `src/media/`, ring buffers, mux/demux loops, AVIO queues,
SRT/RTMP packet loops, HLS segmenting, and transcoder data paths.

- Benchmark before and after hot-path changes with the relevant `benches/` suite.
- Avoid per-packet allocation, logging, serialization, locks, async channel sends, and system calls.
- Do not add logging inside packet-level loops in `src/media/ring_buffer.rs` or `src/media/avio.rs` (push, pull, read). Control operations such as creation, resize, or reader registration are not in the hot path and may log at `debug!` or `info!`.
- Use burst APIs such as `push_batch`, `pull_burst`, and `write_batch` where available.
- Hoist reusable buffers outside loops and call `.clear()` inside the loop.
- Prefer `Bytes`/`BytesMut` ownership transfer and ref-counting over payload copies.
- Do not add diagnostic readers or metrics that alter production pipeline behavior.
- Keep protocol correctness tests at least as strong as performance validation.

For SIMD/vectorization: benchmark scalar first; add SIMD only for measured bottlenecks;
keep a scalar fallback; use runtime feature detection; minimize `unsafe` and document invariants.

## Testing Expectations

- Successful test runs must stay quiet: no compiler warnings, panic text, FFmpeg probe chatter, or stale-binary drift in the passing log. If a test expects noisy stderr, suppress it in the helper instead of teaching CI to ignore it.
- Standardize on `cargo fmt --all` and `cargo fmt --all --check` from the pinned toolchain. Do not run `rustfmt` directly; it can miss workspace and edition context.
- If a test or bench needs media, resolve it through `src/test_fixtures.rs`; when adding a new committed asset, register it in `REQUIRED_CHECKED_IN_FIXTURES` so missing files fail loudly.
- Do not add inline media generators to tests, benches, or harness modes when an existing checked-in fixture can cover the case; measurement and correctness runs should consume committed assets, not synthesize them at runtime.
- Any concurrency/thread-hop change must either extend `scripts/check-concurrency-proof-fast.sh` or explain why the existing proof gate already covers it.
- For changes in `src/media/engine.rs`, `srt.rs`, `ts_chunk_ring.rs`, `avio.rs`, `recording.rs`, `file_ingest.rs`, or `external_transcoder.rs` that affect lifecycle, cancellation, stage sharing, or thread-hop behavior, run `scripts/check-concurrency-contract.sh` before sign-off.
- If teardown or recovery semantics change, update the live harness assertion and the operator-visible status contract in the same change.
- When touching test media, benchmark fixtures, or harness measurement setup, run `scripts/check-fixture-discipline.sh`. When touching frontend/backend contract code, run `scripts/check-api-contract.sh`.
- Run scoped tests first (filtered unit/Criterion for the touched path), then broaden only
  if the change crosses module boundaries or alters shared contracts.
- Treat unrelated full-suite failures as separate findings — don't let them obscure scoped results.
- For ad hoc testing and benchmarks, use existing checked-in assets from `test/fixtures/` and `media/` first.
  Only generate inline media when the test case genuinely cannot be covered by the existing fixture set.
- Let Cargo keep its normal test parallelism for correctness work; do not shard multiple heavy
  `cargo test` invocations across the same tree unless there is explicit process/resource isolation.
- Only correctness-oriented harness slices may parallelize by default. Criterion runs and
  measurement-oriented harness modes stay serial unless the run is intentionally resource-isolated.
- `cargo test av_sync` for timestamp/DTS/PTS changes; protocol-matched probes for RTMP/SRT.
- UI changes: `npm run test:frontend` plus relevant Playwright tests when browser-only behavior is touched.
- Scale/integration: `scripts/resource-limit target/debug/test_harness mixed-anchor` (ramp-family, bonding).

## Session Hygiene

Token costs grow with context length. To keep sessions efficient:
- If the user's request is clearly a new, unrelated task, say so in one sentence and suggest
  starting a fresh session (e.g. "This looks like a new topic — a new session would keep costs low").
- Do not suggest this mid-task or for follow-up questions on the same topic.

## Model Selection

Pick the lowest model class that can reliably complete the work. If your
environment supports helpers, subagents, or delegated tasks, never use a higher
model tier for those helpers than the main session already has available.

Model tiers (lowest to highest): `haiku` → `sonnet` → `opus`

Rough cross-provider mapping for portable guidance in Claude, Codex, and
GitHub Copilot environments:

| Model class | Claude-style label | OpenAI-style label | When to prefer it here |
|-----------|-------|-------|-------|
| Lightweight | `haiku` | `gpt-5.4-mini` or `gpt-5.4-nano` | Search, grep, doc lookup, simple explanation, tiny wording edits |
| Workhorse | `sonnet` | `gpt-5.4` | Most bug fixes, small features, tests, API/UI gap closure, medium repo edits |
| Deep reasoning | `opus` | `gpt-5.5` | Concurrency redesign, hot-path architecture, novel pipeline/protocol design, benchmark-driven decisions |

These are task-class mappings, not exact capability equivalence claims. If an
environment exposes only generic labels such as "fast", "balanced", or
"powerful", map them to lightweight, workhorse, and deep-reasoning classes in
that order.

Preliminary workspace-specific heuristics, based on the task mix visible in
active worktrees (`API-UI-gaps`, `docs-archify`, `hls-fmp4`), current
priorities, and the repo's dedicated skills:

| Query shape in this workspace | Model | Notes |
|-----------|-------|-------|
| Search, grep, file lookup, single-file read, command lookup, test discovery | `haiku` | Retrieval-first work with little synthesis |
| Explain one subsystem, summarize docs, review one file, small docs/AGENTS wording updates, rename/format | `haiku` | Bounded context, low-risk judgment |
| Scoped bug fix, small feature, test writing, API/UI gap closure, contract sync, harness triage in one area | `sonnet` | Usually touches a few files and needs validation |
| Multi-file refactor, runtime/view separation, planner or stage-sharing cleanup, media-pipeline behavior analysis | `sonnet` | Cross-file reasoning, but still mostly within established architecture |
| Concurrency/lifecycle redesign, task-thread-hop changes, hot-path packet-flow changes, HLS/fMP4 design, novel protocol behavior, benchmark-driven architecture decisions | `opus` | High-risk or performance-sensitive decisions with non-obvious tradeoffs |

Escalate to `opus` when the task:

- changes Tokio task ↔ OS thread ownership or cancellation/lifecycle behavior
- introduces or reshapes abstractions in `src/media/` hot paths
- requires benchmark interpretation, failure-mode design, or a new protocol/design direction
- coordinates overlapping multi-agent work on the same runtime boundary

Stay on `haiku` when the task is mostly:

- repo navigation, grep, command lookup, or finding the right test/doc entrypoint
- explaining existing code without changing behavior
- making tiny wording, formatting, or rename-only edits

Default to `sonnet` for implementation work in this repo, especially when the
request implies code edits plus tests but does not clearly require novel design.

Apply this across Claude, Codex, and GitHub Copilot desktop/CLI workflows. For
helper agents, delegated tasks, or side sessions, pick the lowest tier
sufficient for the work and do not exceed the main session's available model
class. For the main task itself, if it clearly fits a lower-cost model, say so.
Examples:

- docs lookup, grep-heavy scouting, AGENTS wording, and simple explanations: use a lightweight model class such as `haiku` or `gpt-5.4-mini`
- scoped bug fixes, tests, and medium-sized repo edits: use a workhorse model class such as `sonnet` or `gpt-5.4`
- only keep the highest-tier model class for the uncommon cross-cutting design/perf/concurrency cases above, such as `opus` or `gpt-5.5`

## Key References

- Overview/setup: `README.md`
- Current priorities: `docs/current-priorities.md`
- Architecture: `docs/architecture.md`
- Media pipeline: `docs/media-pipeline.md`
- Performance: `docs/high-performance-data-path.md`
- Testing: `docs/testing.md`
- Concurrency proofing: `docs/concurrency-proofing.md`
- Layering audit skill: `docs/agent-guidance/skills/layering-audit/SKILL.md` — use for Rust or frontend layering/module-boundary refactors so abstractions stay justified
- Configuration: `docs/configuration.md`
- Observability: `docs/observability.md`
- Logging: `docs/logging.md` (level policy, callsite audit, sink architecture)
- API: `docs/api-reference.md`
