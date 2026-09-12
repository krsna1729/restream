# Advanced Performance Attribution

Use for profiling or experiment design beyond a single Criterion suite.

## Contents

- [Evidence boundary](#evidence-boundary)
- [Profiling branches](#profiling-branches)
- [Restream attribution](#restream-attribution)

## Evidence boundary

Record revision/dirty paths, host/kernel/CPU, CPU mask/quota, binary identity,
protocol/codec/bitrate/audio tracks, output mix/count, duration, settle time,
sampling, and run order. Establish receiver byte/packet growth and clean
runtime/teardown behavior.

For small expected wins, repeat comparable runs and vary their order to expose
noise. Report medians and spread; report latency tails from sufficient samples.
Normalize CPU, events, allocations, and syscalls by delivered bytes/packets
and output-seconds.

## Profiling branches

Probe `perf list` and a short `perf stat` run before choosing hardware events;
virtualization may expose only a subset.

| Evidence needed | Tool / constraint |
|---|---|
| Compute, branches, cache/TLB | Small non-multiplexed `perf stat` groups; reject poor time-running coverage |
| Hot code | Per-process/hot-TID user and kernel call graphs; bench debug info and `perf annotate` |
| Runnable delay, migrations | `perf sched record/timehist` or per-TID `/proc` deltas |
| Locks, false sharing, load sites | `perf lock contention`, `c2c`, or `mem` only with supported events |
| No PMU | `pidstat -t -u -w -r`, `/proc/<pid>/task/<tid>/`, short separate `strace -f -c` windows |
| Allocation/RSS | heaptrack on reduced workloads; `smaps_rollup`, thread/FD census, existing queue telemetry |

Group TIDs by `comm` and normalize their work. Scheduler counters do not prove
IPC, cache misses, false sharing, or stalled cycles. Do not compare raw virtual
PMU counts across hosts. Profiling overhead needs a separate calibration run.

## Restream attribution

Separate fixed ingest cost from output-scaled cost using RTMP-only, canonical
mixed, and bounded SRT workloads. Attribute hot Tokio TIDs before changing
worker placement. Measure allocations, syscall/copy cost, and latency tails
before proposing pooling, I/O redesign, batching, or affinity. Prove soak and
teardown behavior before calling RSS growth a leak.

Check [the baseline ledger](../../../quality/baselines.md) for prior negative
results; repeat a failed experiment only when relevant code, workload, or a
previously missing variable changed. Do not add diagnostic readers that change
production behavior.

Report established facts, unresolved attribution, and ranked experiments with
one variable, normalized metrics, and correctness/acceptance gates each.
Visible symbols alone do not establish the next optimization.
