# Quality Backlog

Prioritized work queue for the autonomous quality loop. Format and grooming
rules: `docs/agent-guidance/skills/backlog-groom/SKILL.md`. Execution protocol:
`docs/agent-guidance/skills/quality-loop/SKILL.md`. Top of file = highest
priority.

Dimensions: `proof` (correct/proven) · `resilience` (reliable/resilient) ·
`modularity` · `efficiency` · `performance` · `groom`.
Tiers: `haiku` (read-only audit) · `sonnet` (scoped code+test) · `opus`
(concurrency/hot-path architecture).

## Open

### Q-001 [proof] [sonnet] Establish the per-module coverage map
- Goal: a per-module line/branch coverage table for `src/` recorded in the
  journal, with follow-up `[proof]` items filed for the 3 weakest
  `src/media/` modules.
- Files: none modified (measurement only); output lands in
  `docs/agent-guidance/quality/journal.md` + new backlog items.
- Gates: `scripts/resource-limit cargo llvm-cov --summary-only` completes
  clean (kill-check media processes first; this is a heavy build).
- Context: cargo-llvm-cov is installed. The stale root `coverage.lcov` is from
  2026-06-24 and gitignored; a fresh map is the seed for invariant-coverage
  work per the proof-sweep skill.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-002 [resilience] [haiku] Inventory crafted-bytes fault-injection coverage
- Goal: a table (in the journal + filed items) of every demux/parse entry
  point in `src/media/` vs the malformed-input cases it has tests for
  (truncated header, oversized declared length, invalid tag/type,
  non-monotonic timestamps, mid-stream parameter change).
- Files: read-only over `src/media/`; no code changes.
- Gates: none (inventory); each gap filed as a `[resilience] [sonnet]` item
  with exact entry-point function named.
- Context: `docs/testing-strategy.md` designates crafted-bytes unit tests as
  the home of fault injection after the in-memory edge subsystem was dropped.
  "No failure path may crash the engine" needs per-parser proof.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-003 [performance] [sonnet] Seed the benchmark baseline ledger
- Goal: medians for `ring_buffer`, `avio_throughput`, and
  `high_performance_data_path` recorded in `baselines.md` with date, commit,
  and noise notes.
- Files: `docs/agent-guidance/quality/baselines.md`.
- Gates: three clean serial `scripts/resource-limit cargo bench --bench <name>`
  runs on an otherwise idle host (`pgrep -x restream/mediamtx/ffmpeg` all
  empty).
- Context: Criterion state in `target/criterion/` is scratch; the ledger is
  the durable regression guard perf-sweep Mode A depends on. Without it every
  future comparison is blind.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-004 [proof] [haiku] Panic-path inventory for src/media
- Goal: classified list of every `.unwrap()`, `.expect(`, `panic!`,
  `unreachable!` in non-test `src/media/` code: invariant-safe (with the
  one-line justification) vs fallible (filed as `[proof] [sonnet]` fix items).
- Files: read-only; output to journal + backlog.
- Gates: none (inventory).
- Context: proof-sweep discovery recipe; the engine no-crash contract makes
  every fallible panic path in the media runtime a latent broadcast outage.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-005 [resilience] [sonnet] Baseline the harness fault modes
- Goal: current pass/fail state of `fault-resilience`, `fault-egress-retry`,
  `fault-output-stall`, and `recovery` recorded in the journal; any failure or
  flake filed as its own item with output attached.
- Files: none modified (measurement only).
- Gates: `scripts/resource-limit cargo build --bin test_harness` then each
  mode via `scripts/resource-limit target/debug/test_harness <mode>`,
  serially, idle host.
- Context: these modes are the live resilience contract; the loop needs a
  known-green baseline before it can treat a failure as a regression signal.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-006 [efficiency] [sonnet] Seed the resource baseline table
- Goal: RSS, ring payload, and AVIO high-water marks from a
  `resource-sweep` harness run recorded in `baselines.md` next to the
  historical 2026-06-27 numbers.
- Files: `docs/agent-guidance/quality/baselines.md`.
- Gates: `scripts/build-bench-harness.sh` then
  `scripts/resource-limit target/bench/test_harness resource-sweep`, serial,
  idle host.
- Context: the 2026-06-27 memory-optimization pass cut ~205 MB RSS across 15
  scale cases; without a refreshed baseline, regressions of that work are
  invisible.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-007 [groom] [sonnet] Diff concurrency-proof coverage doc against the fast gate
- Goal: every rule claimed in `docs/concurrency-proof-coverage-2026-07-02.md`
  mapped to its enforcement in `scripts/check-concurrency-proof-fast.sh` /
  `scripts/check-concurrency-contract.sh`; uncovered rules filed as
  `[proof]` items (tier per rule complexity).
- Files: read-only; output to backlog + journal.
- Gates: none (grooming).
- Context: the coverage doc is a point-in-time claim (2026-07-02); the gates
  are what actually bind. Drift between them is unproven confidence.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-008 [modularity] [sonnet] Execute the topmost undone layering-roadmap step
- Goal: the first not-yet-done step in `docs/layering-roadmap.md` completed at
  the lightest ladder rung, or (if already done in code) the roadmap updated
  to reflect reality.
- Files: per the roadmap step; plus `docs/layering-roadmap.md`.
- Gates: scoped `cargo test` for the touched area;
  `./scripts/check-api-contract.sh` if contract surface moved; standard
  quality-loop gates.
- Context: known cross-layer flows still open: planner→media backend parsing,
  runtime core emitting API-shaped JSON, protocol handlers reading raw SQL
  (`docs/layering-roadmap.md` § Current Shape).
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-009 [performance] [opus] Eliminate one copy in the AVIO→TsMux path
- Goal: FFmpeg AVIO output written directly into a pre-sized `BytesMut`,
  removing the AVIO buffer → `ts_accum` copy, with before/after
  `avio_throughput` + `high_performance_data_path` numbers and green
  `correctness-srt` / `correctness-rtmp` harness modes.
- Files: `src/media/avio.rs`, `src/media/srt.rs` (TS mux accumulator), ledger.
- Gates: perf-sweep Mode C discipline (baseline first, serial measurement),
  protocol correctness modes, concurrency gates if thread-hops move.
- Context: 2026-06-27 CPU profile: `memmove` 3.28% + `VecDeque::extend` 0.43%
  self-time from the two-copy path — the top standing optimization. Requires
  AVIO→TsMux interface redesign: opus tier, do not attempt below it.
- Status: open (Filed: 2026-07-03 by bootstrap)

### Q-010 [efficiency] [opus] Evaluate pooling for per-packet Arc<MediaPacket> allocation
- Goal: a measured decision (implemented or explicitly rejected with numbers)
  on slab/pool allocation for `MediaPacket`, based on `_int_malloc` 0.87%
  self-time in the 2026-06-27 profile.
- Files: `src/media/ring_buffer.rs` and packet construction sites (survey
  first).
- Gates: perf-sweep Mode C; `ring_buffer` bench before/after; loom/unit proofs
  if ownership rules change.
- Context: pooling changes ownership semantics on the hot path — correctness
  risk outweighs the win unless proven; a documented rejection is a valid
  completion.
- Status: open (Filed: 2026-07-03 by bootstrap)

## Blocked

(none)

## Archive

(done items move here with their commit hashes)
