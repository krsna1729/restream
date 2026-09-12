---
name: perf-sweep
description: Benchmark, investigate resource regressions, or optimize restream hot paths with comparable before/after evidence; includes performance/efficiency backlog items.
---

# Performance Sweep

Choose the requested measurement or hypothesis. Discover Criterion targets in
`Cargo.toml` and inspect the matching `benches/` implementation; infer the
target from the changed path when possible.

## Measurement contract

Follow [AGENTS.md](../../../../AGENTS.md) for host/build safety. Measurements
run serially on an otherwise idle host; only the measurement workload may run.
Do not stop processes belonging to another task.

```sh
scripts/build/resource-limit.sh cargo bench --bench <name>
scripts/harness/run.sh <mode>
```

Cargo owns Criterion builds. The harness wrapper builds canonical
`target/bench/` binaries when needed; debug harness results are not performance
evidence. Inspect the current harness catalog before choosing a workload.

Record the revision/dirty tree, host, command, workload, and comparable
before/after numbers. Use Criterion's saved baseline for local comparisons;
put durable results in [baselines](../../quality/baselines.md) when maintaining
the ledger or landing a measured change. Historical numbers are context, not
a current control run.

## Choose the work

- **Benchmark or ledger check:** run the relevant target and compare medians
  with the recorded threshold (default ±5% for throughput ledger rows).
  Confirm a suspected regression with comparable runs before attributing it.
- **Resource check:** run `scripts/harness/run.sh resource-sweep`; compare
  delivered work, RSS, ring payload, and AVIO high-water marks.
- **Optimization:** measure before editing, change one variable, then repeat
  the measurement and relevant correctness gates. If there is no demonstrated
  benefit, discard only the experimental hunks and record the negative result.
- **Attribution or experiment plan:** read
  [advanced attribution](references/advanced-attribution.md). A planning request
  does not authorize an implementation or a long benchmark campaign.

Preserve protocol correctness and latency tails alongside throughput. Normalize
live comparisons by delivered bytes/packets and output-seconds; ready paths alone
do not establish equal work. Do not add readers or instrumentation that alter
the production pipeline being measured. Check available profiling events;
virtualized hosts may lack PMU support, so scheduler data cannot establish
cache misses or IPC.
