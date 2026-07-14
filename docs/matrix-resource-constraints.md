# Matrix resource constraints

This page defines the resource boundary for live matrix runs. It intentionally
does not duplicate current harness modes, queue values, or measured totals.

## Contents

- [Scope](#scope)
- [Resource owners](#resource-owners)
- [Harness boundary](#harness-boundary)
- [Running a bounded matrix](#running-a-bounded-matrix)
- [Interpreting results](#interpreting-results)

## Scope

Live matrices exercise the compiled Rust server with publishers, destinations,
and optional FFmpeg/MediaMTX peers. They are integration and measurement work,
not production capacity certification. The harness owns scenario construction;
the production runtime owns admission and bounded media storage.

## Resource owners

| Resource | Current owner |
|---|---|
| Cargo/build memory and host-wide build serialization | `scripts/build/resource-limit.sh` and the worktree build lock |
| Tokio scheduler sizing | `src/main.rs` using values parsed by `src/config.rs` |
| Packet, MPEG-TS, and AVIO queue bounds | `src/config.rs` and their structures under `src/media/` |
| RTMP listener and connection admission | `src/media/rtmp.rs` and runtime configuration |
| SRT sender admission and muxer sharding | `src/media/srt*.rs` and runtime configuration |
| External FFmpeg child admission/thread hints | `src/config.rs` and `src/media/external_transcoder.rs` |
| Scenario processes, fixtures, artifacts, and cleanup | `src/bin/test_harness/` and `test/harness/` |
| Host/container cgroup, CPU, NUMA, and memory limits | The environment that launches the harness |

The build limiter does not impose runtime cgroups on an already-built harness.
Likewise, runtime semaphores bound specific server resources but do not cap
MediaMTX, publisher FFmpeg processes, or the entire process tree.

## Harness boundary

MediaMTX is used only where a scenario needs an independent protocol peer or
sink. FFmpeg may act as a fixture publisher, reader, or external transform
child. These are test topology components; neither replaces the Rust server's
production transport ownership.

Fixture resolution, process cleanup, artifact retention, and supported modes
are executable behavior. Consult the harness entry point and
[Testing](testing.md) instead of copying their lists here.

The harness deliberately avoids killing unrelated media processes. In a shared
host or worktree session, establish process ownership and the build lock before
starting a heavy run.

## Running a bounded matrix

1. Build the required profile through the repository's resource-limited build
   path. If the mode consumes `target/bench/`, use
   `scripts/build/bench-harness.sh`.
2. Choose the smallest harness mode that proves the changed protocol or
   lifecycle boundary.
3. Apply whole-process limits outside the harness when the experiment requires
   a cgroup or container boundary.
4. Capture the harness summary and retained artifacts needed to explain a
   failure; do not retain routine high-volume output by default.
5. Escalate to a broader matrix only after the focused mode passes.

Environment variables recognized by the harness and server are parsed by their
respective source owners. Avoid undocumented combinations in automation: a
misspelled variable otherwise creates a false sense of constraint.

## Interpreting results

Record the workload shape, build identity, fixture, host limit, and whether
publisher/sink resources are included. Separate these conclusions:

- protocol correctness under the exercised topology;
- recovery behavior under the injected fault;
- directional CPU, memory, queue, or throughput evidence;
- a deployment recommendation supported across representative hosts.

Only the first two can usually be established by a single bounded run. Store
repeatable performance baselines in the
[quality baseline ledger](agent-guidance/quality/baselines.md), not in this
contract.
