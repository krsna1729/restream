---
name: perf-sweep
description: Measure, guard, or improve ONE hot-path performance or resource-efficiency target with before/after evidence — bench ledger comparison, allocation/lock/copy hunt, or RSS/ring telemetry check. Use for backlog items tagged [performance] or [efficiency], or when asked to find regressions, cut memory, or speed up the data path.
---

# Skill: perf-sweep

Broadcast-scale means the data path must stay flat under load: no per-packet
allocation creep, no silent throughput regression, no RSS growth that OOMs a
long event. One invocation = one measured unit: a ledger check, one regression
chased, or one optimization with before/after proof.

## Non-negotiable measurement discipline

- **Serial only:** nothing else may build or run on the host during any
  measurement. Kill-check first: `pgrep -x restream; pgrep -x mediamtx; pgrep -x ffmpeg`.
- Bench profile only: `scripts/build/resource-limit.sh cargo build --profile bench`,
  `scripts/build/resource-limit.sh cargo bench --bench <name>`. Never `--release`,
  never `target/debug` for measurement harness modes
  (`scripts/build/bench-harness.sh` → `target/bench/test_harness`).
- Every claim needs numbers from this machine, this session. No "should be
  faster".
- Durable results go to `docs/agent-guidance/quality/baselines.md`; Criterion's
  `target/criterion/` is scratch state that worktree churn can erase.

## Mode A — ledger check (performance guard)

1. Pick the least-recently-measured suite in `baselines.md`
   (`ring_buffer`, `avio_throughput`, `high_performance_data_path`,
   `matrix_throughput`, `srt_ingest_latency`, `transcoder_throughput`,
   `hls_cost`, `hls_fmp4_cost`, `stage_feeder`, `stage_metrics`,
   `codec_conversions`, `simd_alternatives`, `alert_tracker`).
2. Run it; compare medians against the ledger.
3. Within noise (±5% for throughput suites unless the ledger row says
   otherwise) → update the "last verified" date, done.
4. Regression beyond threshold → do NOT optimize blindly. Bisect: check
   `git log` for hot-path commits since the ledger date, identify the suspect,
   and file a `[performance]` fix item with the numbers. Confirming and filing
   IS the completed item.

## Mode B — resource check (efficiency guard)

1. Run `scripts/build/resource-limit.sh target/bench/test_harness resource-sweep`
   (serial, bench harness).
2. Compare RSS, ring payload, and AVIO high-water marks against the resource
   table in `baselines.md`.
3. Record; regressions become filed items with numbers, same as Mode A.

## Mode C — targeted optimization (item names the target)

1. Baseline: run the relevant bench suite(s) BEFORE touching code. Record.
2. Make the narrowest change. Hot-path rules (AGENTS.md) bind:
   - no per-packet allocation, logging, locks, async sends, or syscalls
   - no logging in `ring_buffer.rs` / `avio.rs` packet loops
   - hoist buffers out of loops, clear inside; prefer `Bytes`/`BytesMut`
     ownership transfer over copies; use burst APIs
   - SIMD: benchmark scalar first, keep scalar fallback, runtime feature
     detection, minimal `unsafe`
3. Re-run the same suite(s). Improvement must be outside noise; protocol
   correctness tests must stay green (`cargo test` scoped + the relevant
   correctness harness mode for the touched protocol).
4. Update `baselines.md` with the new medians and the commit reference.
5. No measurable win after two attempts → revert fully, journal the numbers
   and the hypothesis that failed (negative results save the next agent time).

## Discovery recipe (finding new [performance]/[efficiency] items)

- Read the CPU-profile table and jitter-headroom table in
  `docs/agent-guidance/quality/baselines.md` for known standing opportunities.
- Suites in the ledger not verified in >14 days → file a Mode A item each.
- `perf` on WSL2: hardware PMU counters are unavailable; use
  `perf record -e task-clock` (software sampling) with `perf_event_paranoid=-1`.

## Rules

- Correctness outranks speed: a faster path that weakens a protocol test is a
  regression, full stop.
- One variable at a time — never mix a refactor with an optimization in the
  same measurement.
- Do not add diagnostic readers or metrics that alter production pipeline
  behavior to "help measure".
- `[opus]`-tagged architecture changes (e.g. AVIO→TsMux copy elimination) are
  off-limits below opus tier even if the numbers are tempting.
