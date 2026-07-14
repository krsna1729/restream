# Resource-Constraint Audit: `mixed.matrix` and `mixed.fast-breadth`

This document records the CPU and memory constraints that apply to the live
protocol/input matrix runners. It complements [testing.md](testing.md), which
describes how to run the matrix, by explaining *what is bounded* and *what is
not*.

## Contents

- [Executive summary](#executive-summary)
- [Build-time constraints only](#build-time-constraints-only)
- [Runtime constraints that exist](#runtime-constraints-that-exist)
- [Matrix and fast-breadth concurrency model](#matrix-and-fast-breadth-concurrency-model)
- [What is NOT constrained](#what-is-not-constrained)
- [Harness/runtime knobs status](#harnessruntime-knobs-status)
- [Measured resource baselines](#measured-resource-baselines)
- [Recommendations](#recommendations)
- [Bottom line](#bottom-line)

## Executive summary

The full matrix and fast-breadth modes are **partially constrained**. Every
internal buffer, queue, ring, socket, and semaphore that the engine uses has an
explicit bound, and the harness caps concurrent pipelines per shared stack.
However, there is **no process-level runtime CPU or memory limit** on the
`restream` process, MediaMTX, or spawned FFmpeg children. `scripts/build/resource-limit.sh`
only constrains build parallelism.

On a resource-constrained host, the biggest risks are:

1. **H.265→H.264 codec-edge stages** — in-process, CPU-heavy, unbounded by any
   cgroup or affinity.
2. **External FFmpeg transcode children** — one per `(pipeline, preset)`; memory
   grows with preset count and output fan-out.
3. **Fast-breadth parallel families** — three independent stacks run
   concurrently on the host, tripling peak load versus one stack.
4. **Documented-but-unimplemented env vars** — `KEEP_ARTIFACTS`,
   `RSS_BASELINE`, `SAVE_RSS_BASELINE`, and `ALLOW_GLOBAL_PROCESS_CLEANUP` appear
   in [testing.md](testing.md) but have no source implementation.

Conclusion: the components are **structurally bounded** at the data-structure
level, but the test runner is **not environmentally sandboxed** at runtime. For
CI or shared machines, add explicit runtime limits (cgroups, `systemd-run`, or
container memory/CPU limits) rather than relying on env knobs alone.

## Build-time constraints only

[scripts/build/resource-limit.sh](../scripts/build/resource-limit.sh) is the only place that sizes
work from available CPU and memory. It computes `BUILD_JOBS`,
`CARGO_BUILD_JOBS`, `CMAKE_BUILD_PARALLEL_LEVEL`, and `MAKEFLAGS` from:

| Knob | Default | Meaning |
|---|---|---|
| `RESTREAM_MB_PER_JOB` | 500 | Memory budget per compiler job |
| `RESTREAM_CPU_RESERVE` | 1 | CPUs to leave free |
| `RESTREAM_MIN_JOBS` | 1 | Lower bound |
| `RESTREAM_MAX_JOBS` | unset | Optional hard cap |

It also serializes heavy commands behind a flock. **It does not constrain the
running `restream`, MediaMTX, or FFmpeg processes.**

## Runtime constraints that exist

### Ring buffers

| Ring | Default | Range | Env knob | Location |
|---|---|---|---|---|
| Source/pipeline ring | 1024 slots | 64–16384 | `RESTREAM_RING_CAPACITY` | [src/media/ring_buffer.rs](../src/media/ring_buffer.rs) |
| Transcoder output ring | 512 slots | 64–16384 | `RESTREAM_TRANSCODER_RING_CAPACITY` | [src/media/ring_buffer.rs](../src/media/ring_buffer.rs) |
| TS chunk ring (SRT shared muxer) | 256 slots | 32–16384 | `RESTREAM_TS_RING_CAPACITY` | [src/media/srt.rs](../src/media/srt.rs) |

The source ring is also **adaptively resized** after stream probe based on
`video_fps + audio_track_count * 50 pkt/s` with a 6-second headroom, capped at
16384 ([src/media/engine.rs](../src/media/engine.rs)). Slots hold
`Arc<MediaPacket>`; payload memory scales with compressed frame size, not slot
count.

### AVIO / MemoryQueue

- Default capacity: **512 KiB**, clamped **64 KiB–16 MiB**.
- Knob: `RESTREAM_AVIO_QUEUE_CAPACITY`.
- Location: [src/media/avio.rs](../src/media/avio.rs).
- Used by SRT play sender threads, H.264 transcoder input, recording writer,
  file ingest, and external transcoder pipes.

### SRT socket and protocol limits

| Limit | Value | Location |
|---|---|---|
| SRT send buffer | 12 MB | [src/media/srt.rs](../src/media/srt.rs) |
| SRT recv buffer | 12 MB | [src/media/srt.rs](../src/media/srt.rs) |
| UDP send buffer | 8 MB | [src/media/srt.rs](../src/media/srt.rs) |
| UDP recv buffer | 8 MB | [src/media/srt.rs](../src/media/srt.rs) |
| Flow-control window | 32768 packets | [src/media/srt.rs](../src/media/srt.rs) |
| Latency | 250 ms | [src/media/srt.rs](../src/media/srt.rs) |
| Loss max TTL | 256 packets | [src/media/srt.rs](../src/media/srt.rs) |
| Listener backlog | 1024 | [src/media/srt.rs](../src/media/srt.rs) |
| Accept→tokio channel | 1024 | [src/media/srt.rs](../src/media/srt.rs) |
| Concurrent SRT sender OS threads | 512 permits | [src/media/engine_registries.rs](../src/media/engine_registries.rs) |

The 512-permit `sender_semaphore` is the only explicit **thread-count** bound on
the SRT egress path.

### HLS in-memory store

| Limit | Default | Env knob | Location |
|---|---|---|---|
| Max segments | 20 | `RESTREAM_HLS_MAX_SEGMENTS` | [src/config.rs](../src/config.rs) |
| Segment accumulator capacity | 8 MiB | `RESTREAM_HLS_SEGMENT_CAPACITY_BYTES` | [src/config.rs](../src/config.rs) |
| Min segment length | 1 s | `RESTREAM_HLS_MIN_SEGMENT_MS` | [src/config.rs](../src/config.rs) |

The fMP4 preview store uses the same `HlsConfig` bounds.

### File descriptor limit

- Default: **65536**.
- Knob: `RESTREAM_NOFILE_LIMIT`.
- Location: [src/lib.rs](../src/lib.rs).

### Transcode profiles

Built-in presets use fixed, low-latency settings:

| Preset | Encoder | Preset | Tune | GOP | B-frames | CRF | Location |
|---|---|---|---|---|---|---|---|
| `h264` | libx264 | ultrafast | zerolatency | 60 | 0 | 23 | [src/media/profiles.rs](../src/media/profiles.rs) |
| `720p` | libx264 | ultrafast | zerolatency | 60 | 0 | 23 | [src/media/profiles.rs](../src/media/profiles.rs) |
| `1080p` | libx264 | ultrafast | zerolatency | 60 | 0 | 23 | [src/media/profiles.rs](../src/media/profiles.rs) |

These are **quality/latency bounds**, not CPU bounds. `ultrafast` trades bitrate
for CPU; it still consumes significant CPU at high resolutions.

### Startup probe budgets

External FFmpeg transcode stages get a bounded probe budget:

- H.264: `analyzeduration=0`, `probesize=32 KiB`
- HEVC: `analyzeduration=1_000_000`, `probesize=512 KiB`

Location: [src/media/startup_policy.rs](../src/media/startup_policy.rs).

### Batch sizes

- `MEDIA_PULL_BURST_PACKETS = 32`
- `MEDIA_PRODUCER_BATCH_PACKETS = 32`
- `MEDIA_TS_BATCH_TARGET_BYTES = 1316 * 32 = 42 112 bytes`

Location: [src/media/mod.rs](../src/media/mod.rs). These bound per-loop work
but do not limit total throughput.

## Matrix and fast-breadth concurrency model

### Full matrix (`mixed.matrix`)

- 18 input cases across 3 shared-batch groups: `live-rtmp`, `live-srt`,
  `file-ingest`.
- One Restream + one MediaMTX stack per group.
- Within each stack, cases run in waves of up to **2 concurrent pipelines**
  (`tokio::join!` on case A and case B).
- Default `N_PER_GROUP = 2`: each output row is created twice to verify stage
  sharing.
- On wave failure, the stack is stopped and restarted for the remaining cases.

Source: [src/bin/test_harness/mixed_runner.rs](../src/bin/test_harness/mixed_runner.rs).

### Fast breadth (`mixed.fast-breadth`)

- 6 selected input cases, same 3 groups.
- Default `N_PER_GROUP = 1`, `SKIP_LOAD = 1`, `COLLECT_FAILURES = 1`.
- [scripts/harness/parallel-fast-breadth.sh](../scripts/harness/parallel-fast-breadth.sh)
  launches **all three groups concurrently** on the host with isolated port
  bundles and work directories.
- Each group still runs up to 2 concurrent pipelines internally.

### Peak process count (fast-breadth parallel)

Per group:

- 1 `restream`
- 1 `mediamtx`
- Up to 2 FFmpeg publishers (one per concurrent pipeline)
- External transcode FFmpeg children per active `(pipeline, preset)` pair
- SRT play sender OS threads (capped at 512 globally per `restream`, but in
  practice one per SRT output)

Tripled across 3 parallel groups. There is **no global limit** on how many
groups run at once beyond the shell script’s hard-coded 3.

## What is NOT constrained

### Runtime CPU / memory per process

No cgroup, `systemd-run`, `ulimit -v`, `nice`, or CPU-affinity limits are
applied to `restream`, MediaMTX, or FFmpeg children. The OS scheduler and OOM
killer are the only backstops.

### Tokio runtime sizing

- `restream`: `tokio::runtime::Builder::new_multi_thread()` with a
  conservative worker count derived from effective CPUs (Rust available
  parallelism, process CPU mask, and cgroup v2 CPU quota) and
  `RESTREAM_TOKIO_WORKER_THREADS` as an explicit override
  ([src/main.rs](../src/main.rs)).
- `test_harness`: `#[tokio::main(flavor = "multi_thread")]` with default counts
  ([src/bin/test_harness.rs](../src/bin/test_harness.rs)).

The default favors fewer, busier scheduler workers for high-fanout I/O:
effective CPUs divided by three, rounded up, clamped to `1..8`. The blocking
pool remains capped separately by `RESTREAM_TOKIO_MAX_BLOCKING_THREADS`
(`512` by default).

### RTMP listener backlog

`TcpListener::bind` is followed by `listener.accept().await` without setting an
explicit backlog; the kernel default (`SOMAXCONN`, typically 4096 on Linux)
applies. Location: [src/media/rtmp.rs](../src/media/rtmp.rs).

### OS thread accumulation

The engine registers OS thread `JoinHandle`s in `RuntimeInfra::os_threads` and
prunes finished ones opportunistically, but there is no hard cap on live
threads. Location: [src/media/engine.rs](../src/media/engine.rs).

### External FFmpeg children

`start_external_transcoder_stage` spawns one FFmpeg subprocess per
`(pipeline_id, encoding)` key. The number of keys is bounded by the output
matrix, but there is no global semaphore or cgroup for child processes.
Location: [src/media/external_transcoder.rs](../src/media/external_transcoder.rs).

### File ingest pacing

File ingest paces packets by timestamp but can still consume one OS thread per
active file ingest and one `MemoryQueue`. No global file-ingest count limit
exists. Location: [src/media/file_ingest.rs](../src/media/file_ingest.rs).

## Harness/runtime knobs status

The following previously documented knobs are now implemented in the harness:

| Var | Documented use | Implemented? |
|---|---|---|
| `KEEP_ARTIFACTS` | Retain old `.local/artifacts/` directories | Yes |
| `RSS_BASELINE` | Compare mixed-input RSS against saved CSV | Yes |
| `SAVE_RSS_BASELINE` | Save current RSS summary as baseline | Yes |
| `ALLOW_GLOBAL_PROCESS_CLEANUP` | Legacy host-wide cleanup before run | Yes |

Implemented knobs:

| Var | Where |
|---|---|
| `RESTREAM_ARTIFACT_MIN_FREE_MB` | Preflight disk check only ([src/bin/test_harness.rs](../src/bin/test_harness.rs)) |
| `MIXED_MATRIX_FAIL_FAST` | Yes ([src/bin/test_harness/mixed_runner.rs](../src/bin/test_harness/mixed_runner.rs)) |
| `MIXED_MATRIX_SERIAL` | Yes ([src/bin/test_harness/mixed_runner.rs](../src/bin/test_harness/mixed_runner.rs)) |
| `ONLY_CHECKS` | Yes ([src/bin/test_harness/mixed_runner.rs](../src/bin/test_harness/mixed_runner.rs)) |
| `N_PER_GROUP` | Yes ([src/bin/test_harness/mixed_runner.rs](../src/bin/test_harness/mixed_runner.rs)) |
| `SKIP_LOAD` / `COLLECT_FAILURES` / `ASSERTION_LOG` | Yes ([src/bin/test_harness/mixed_runner.rs](../src/bin/test_harness/mixed_runner.rs)) |
| `MIXED_FAST_BREADTH_GROUPS` | Yes ([src/bin/test_harness/mixed_manifest.rs](../src/bin/test_harness/mixed_manifest.rs)) |

## Measured resource baselines

Authoritative current-code measurements from [testing.md](testing.md):

| Scenario | Restream MB | Child FFmpeg MB | Combined MB | Total CPU % |
|---|---:|---:|---:|---:|
| Empty baseline | 72.8 | 0.0 | 72.8 | 1.15 |
| 5× H.264 SRT ingest | 82.6 | 0.0 | 82.6 | 7.27 |
| Mixed 720p transcode, 20 outputs | 120.3 | 166.5 | 286.8 | 51.65 |
| HEVC bridge, 10 RTMP source outputs | 158.7 | 0.0 | 158.7 | 71.82 |
| H.265 SRT 8M, 4 outputs | 278.4 | 303.3 | 581.6 | 310.75 |

Implication: a single H.265 multi-audio matrix row with several transcode
outputs can approach **0.5–1 GB combined** and **multiple CPU cores**.
Fast-breadth parallel runs three stacks, so a worst-case overlap could exceed
that.

## Recommendations

### Immediate doc fixes

Update [testing.md](testing.md) to:

1. State clearly that `scripts/build/resource-limit.sh` constrains **build parallelism
   only**.
2. Keep `KEEP_ARTIFACTS`, `RSS_BASELINE`, `SAVE_RSS_BASELINE`, and
  `ALLOW_GLOBAL_PROCESS_CLEANUP` documented as implemented harness controls.
3. Add a “Runtime resource limits” table listing the bounded knobs that
   actually exist.

### Runtime safety for CI/shared hosts

Add an optional wrapper (e.g., `scripts/run-matrix-cgroup.sh`) that uses
`systemd-run --scope -p MemoryMax=... -p CPUQuota=...` or `cgexec` to cap each
`restream` + MediaMTX stack. Suggested starting points:

- Per-stack memory: **2 GB soft, 4 GB hard** for full matrix; **1.5 GB hard**
  for fast-breadth groups.
- Per-stack CPU: **400%** (4 cores) to prevent one stack from starving others.
- Overall fast-breadth parallel: **8 GB memory, 800% CPU** for the 3-group run.

### Implement missing knobs (optional)

The documented RSS baseline and artifact-retention knobs are implemented.
Optional future work is stricter baseline schema/versioning and richer diff
reporting on baseline mismatches.

### Add a global child-FFmpeg cap (optional)

Implemented: runtime now gates external transcoder children behind a semaphore
with derived sizing and env overrides (`RESTREAM_EXTERNAL_FFMPEG_*`).

## Bottom line

`mixed.matrix` and `mixed.fast-breadth` are **correctly bounded at the
data-structure and harness-concurrency level**, but they are **not
environmentally constrained at the OS/process level**. The existing env knobs
give operators control over buffer sizes and build parallelism, but they will
not prevent a runaway H.265 transcode or a memory-heavy FFmpeg child from
impacting the host. For unattended or shared runners, add cgroup-level limits
around the launch scripts.
