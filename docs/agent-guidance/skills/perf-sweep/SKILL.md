---
name: perf-sweep
description: Measure, attribute, guard, or improve ONE hot-path performance or resource-efficiency target with before/after evidence. Use for backlog items tagged [performance] or [efficiency], regression and CPU/RSS investigations, WSL or hardware-PMU profiling plans, scheduler/cache/allocation attribution, and experiment design.
---

# Skill: perf-sweep

Broadcast-scale means the data path must stay flat under load: no per-packet
allocation creep, no silent throughput regression, no RSS growth that OOMs a
long event. One invocation = one measured unit: a ledger check, one regression
chased, or one optimization with before/after proof.

## Non-negotiable measurement discipline

- **Serial only:** nothing else may build or run on the host during any
  measurement. Kill-check first: `pgrep -x restream; pgrep -x mediamtx; pgrep -x ffmpeg`.
- Bench profile only: use
  `scripts/build/resource-limit.sh cargo bench --bench <name>` for Criterion
  and `scripts/harness/run.sh <mode>` for measurement harness workflows. Never
  use `--release` or a `target/debug` harness binary.
- Every claim needs numbers from this machine, this session. No "should be
  faster".
- Durable results go to `docs/agent-guidance/quality/baselines.md`; Criterion's
  `target/criterion/` is scratch state that worktree churn can erase.

## Mode A — ledger check (performance guard)

1. Read the benchmark targets from `Cargo.toml` and compare them with the
   entries in `baselines.md`. Pick the least-recently-measured applicable
   target; do not maintain a duplicate suite list in this skill.
2. Run it; compare medians against the ledger.
3. Within noise (±5% for throughput suites unless the ledger row says
   otherwise) → update the "last verified" date, done.
4. Regression beyond threshold → do NOT optimize blindly. Bisect: check
   `git log` for hot-path commits since the ledger date, identify the suspect,
   and file a `[performance]` fix item with the numbers. Confirming and filing
   IS the completed item.

## Mode B — resource check (efficiency guard)

1. Run `scripts/harness/run.sh resource-sweep` serially. The wrapper owns the
   bench-harness build and lock handling.
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

## Mode D — advanced attribution or experiment plan

Use this when asked what currently dominates, what extra hardware `perf` would
show, or how to design experiments without PMU access.

1. Read [references/advanced-attribution.md](references/advanced-attribution.md).
2. Freeze the evidence boundary: commit/tree, dirty files, workload, host,
   protocol mix, bitrate, output count, and correctness proof.
3. Choose the platform branch:
   - PMU available: validate exposed events, then attribute per process and hot
     TID with non-multiplexed event groups and user/kernel call graphs.
   - PMU unavailable: use `/proc`, `pidstat`, short `strace -f -c`, heaptrack,
     and existing queue/ring/receiver telemetry. Do not claim IPC/cache causes.
4. Decompose one dimension at a time: ingest-only, RTMP-only, canonical mix,
   bounded SRT calibration, worker count, bitrate, or output count.
5. Return a ranked experiment plan with normalization and accept/reject gates;
   do not convert an attribution gap into an implementation recommendation.

## Discovery recipe (finding new [performance]/[efficiency] items)

- Read the CPU-profile table and jitter-headroom table in
  `docs/agent-guidance/quality/baselines.md` for known standing opportunities.
- Suites in the ledger not verified in >14 days → file a Mode A item each.
- `perf` on WSL2: hardware PMU counters are unavailable; use
  `perf record -e task-clock` when available, otherwise follow the `/proc`
  fallback in `references/advanced-attribution.md`.

## Rules

- Correctness outranks speed: a faster path that weakens a protocol test is a
  regression, full stop.
- One variable at a time — never mix a refactor with an optimization in the
  same measurement.
- Do not add diagnostic readers or metrics that alter production pipeline
  behavior to "help measure".
- Normalize live comparisons by delivered bytes or packets and output-seconds;
  path readiness alone is not equal work.
- `[opus]`-tagged architecture changes (e.g. AVIO→TsMux copy elimination) are
  off-limits below opus tier even if the numbers are tempting.
