# Advanced performance attribution

Use this reference for read-only performance analysis, experiment design, and
profiling beyond a single Criterion suite.

## Contents

- [Establish the evidence boundary](#establish-the-evidence-boundary)
- [Hardware-PMU branch](#hardware-pmu-branch)
- [No-PMU / WSL branch](#no-pmu-wsl-branch)
- [Attribution order for this repository](#attribution-order-for-this-repository)
- [Required analysis output](#required-analysis-output)

## Establish the evidence boundary

Record before interpreting results:

- commit and dirty paths;
- host/kernel/CPU topology, CPU mask, cgroup quota, and virtualization;
- bench-profile binary identity;
- ingest protocol, codec, bitrate, audio-track count, egress mix, and output count;
- measurement duration, settle time, sample interval, and run order;
- receiver byte/packet growth, ready paths, warnings, panics, overflows, and
  queue high-water marks.

Use at least three cold repetitions for small expected wins. Randomize variant
order. Report median, p95, min/max, and coefficient of variation. Normalize CPU,
events, allocations, and syscalls by delivered GiB or packet count and by
output-seconds.

## Hardware-PMU branch

1. Run `perf list` and a short `perf stat` probe. Virtual PMUs may omit precise
   sampling, LBR, PEBS/IBS, cache-to-cache, or raw events.
2. Use separate event groups small enough to avoid multiplexing:
   - cycles, instructions, branches, branch misses;
   - cache references/misses, L1d loads/misses, LLC loads/misses;
   - dTLB loads/misses and frontend/backend stalls when supported.
3. Capture user and kernel call graphs separately for the process and hot TIDs.
   Use the bench profile's debug information and `perf annotate` on the top
   functions.
4. Use `perf sched record/timehist` for runnable delay, wakeup placement, and
   migrations. Use `perf lock contention` when the required trace/BPF support
   exists.
5. Use `perf c2c` only when cache-to-cache events are exposed; investigate
   ring indexes, reader metrics, stage counters, Arc refcounts, and queue state.
6. Use `perf mem` only when precise load sampling is exposed; identify the
   owning data structure before proposing layout or padding changes.
7. Reject counter samples with poor time-running coverage. Do not compare raw
   virtual-PMU counts across different hosts.

Hardware counters answer whether the owner is compute, branches, locality/TLB,
kernel networking, scheduler/locks, or page activity. They do not replace
receiver correctness, latency tails, or resource telemetry.

## No-PMU / WSL branch

Use:

- `pidstat -t -u -w -r -p <pid> 1 <count>`;
- per-TID deltas from `/proc/<pid>/task/<tid>/{stat,status,sched,schedstat,wchan}`;
- short, separate `strace -f -c -p <pid>` calibration windows;
- heaptrack on reduced stable workloads for allocation stacks;
- `/proc/<pid>/smaps_rollup`, thread/FD census, and existing ring/queue telemetry;
- endpoint, convergence, recovery, and receiver-byte latency distributions.

Group TIDs by `comm`; calculate CPU seconds, voluntary/involuntary switches,
migrations, runnable delay when available, and events per delivered GiB.

Do not infer IPC, cache misses, false sharing, branch-miss causes, or stalled
cycles from WSL scheduler counters. Use WSL to reject ideas and identify likely
owners; require a PMU-capable production-like host for locality/affinity claims.

## Attribution order for this repository

1. Re-prove the current exact binary and canonical receiver health.
2. Separate fixed ingest cost from output-scaled cost.
3. Compare RTMP-only, canonical mixed, and bounded SRT calibration shapes.
4. Attribute hot Tokio TIDs by call graph before changing worker placement.
5. Measure allocation sites before pooling or ownership redesign.
6. Measure syscall and kernel-copy cost before changing an I/O boundary.
7. Measure latency tails before batching, delayed wakeups, affinity, or pooling.
8. Run a current-code soak and teardown proof before diagnosing RSS as a leak.

Preserve negative results already recorded in the ledger. Do not repeat RTMP
burst coalescing, payload-ownership transfer, allocator arena caps, Tokio
blocking-cap/keepalive tuning, or in-process affinity unless the new experiment
changes the previously missing variable.

## Required analysis output

Lead with:

1. current conclusion and confidence;
2. facts directly established by current code or measurements;
3. unresolved attribution gaps;
4. ranked experiments, one variable each;
5. metrics, normalization, and correctness gates;
6. explicit ideas to defer or reject.

Do not report a cache, scheduler, allocation, or I/O change as the next
optimization merely because its symbols are visible.
