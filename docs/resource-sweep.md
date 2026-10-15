# Resource Sweep

The preferred integration entry point is the native Rust harness mode:

```sh
./scripts/harness/run.sh resource-sweep
```

The sweep measures `restream` CPU and memory across a fixed set of live
integration scenarios:

- empty baseline
- ingest-only for the 5 production ingest shapes
- ingest growth with identical pipelines
- ingest growth with mixed ingest types
- egress growth with source outputs only
- egress growth with mixed RTMP+SRT source outputs
- egress growth with 720p transcode outputs only
- egress growth with mixed RTMP+SRT 720p transcode outputs
- egress growth with mixed RTMP+SRT source plus 720p outputs
- egress growth with mixed RTMP+SRT 720p plus 1080p outputs
- egress growth with mixed RTMP+SRT source plus 720p plus 1080p outputs
- HEVC ingest to RTMP source-output growth (captures the internal HEVC bridge)

Artifacts are written to `.local/artifacts/resource-sweep/`:

- `resource-sweep-results.json`: stage aggregates
- `resource-sweep-results.csv`: spreadsheet-friendly summary
- `resource-sweep-samples.jsonl`: raw 1 Hz samples
- `packet-contract.json`: the WI3.4 packet-rate contract summary (mean/peak
  rates, peak gauges, cost proxies, and an `unavailable` block naming every
  metric the product cannot source yet)
- `packet-contract-samples.jsonl`: one packet-rate contract record per sample
- `media-executor-<scenario>-p<pipelines>-o<outputs>-{start,end}.json`: the
  full `/metrics/system` `mediaExecutor` object at the start and end of each
  rated window (cumulative per-class and per-service `polls`/`busyUs`/
  `maxPollUs`, per-worker `busyUs`/`parks`, queue depth). Subtract `start`
  from `end` for the window and divide by wall time × `configuredWorkers` for
  utilization; per-service rows give each feed's and stage's share.
  `maxPollUs` is a lifetime maximum, not a window value
- `restream.log`, `mediamtx.log`, and publisher logs

The packet-rate contract records per-second rates for the SRT Owner packet
path (TX datagrams split into DATA, retransmissions and control; RX
datagrams; service visits; Owner actions), scheduler wake rate, ready depth,
budget/exhaustion pressure, kernel UDP errors and NIC drops, and the
capacity/flow projection — sourced from `/metrics/system` and host counters,
so the numbers describe the shipped datapath. `packet-contract.json` carries
the run's git SHA/dirty state and workload/peer environment, one summary per
`(scenario, output count)` rung, and a `validity` verdict per sample and per
rung (`healthy`, `contaminated` when the host or peer dropped datagrams, or
`invalid` when a participant is missing, stalled, retrying or faulted).

The sweep combines:

- OS-level process memory from `/proc/<pid>/status` and `/proc/<pid>/smaps_rollup`
- CPU deltas from `/proc/<pid>/stat`, and per thread group
  (`threadCpuPct`) from `/proc/<pid>/task/*/stat`
- run-queue wait per thread group (`threadRunQueuePct`: time runnable but
  not running, % of one thread's wall time, from `/proc/<pid>/task/*/schedstat`;
  the delay `perf sched` reports, without its privileges)
- internal counters, payload accounting and Restream's own delivery view from
  `GET /api/v1/measure` (fixed shape, so sampling cost does not grow with
  output count)
- child FFmpeg RSS and CPU from `/proc`

For transcode scenarios the CSV/JSON now split CPU into:

- `restream_cpu_*`: CPU consumed inside the `restream` process
- `ffmpeg_cpu_*`: CPU consumed by child `ffmpeg` processes
- `total_cpu_*`: combined CPU for the scenario

Run it with the canonical bench harness wrapper:

```sh
./scripts/harness/run.sh resource-sweep
```

Useful env vars:

- `RESTREAM_BIN=/path/to/restream`
- `WORK_DIR=.local/artifacts/resource-sweep-custom`
- `RESOURCE_SWEEP_SAMPLE_SECS=10`
- `RESOURCE_SWEEP_SETTLE_SECS=6`
- `RESOURCE_SWEEP_BITRATE=8M` (publisher fixture bitrate label; default `1.5M`)
- `RESOURCE_SWEEP_EGRESS_COUNTS=1,3,6`
- `RESOURCE_SWEEP_INGEST_COUNTS=1,2,4`
- `RESOURCE_SWEEP_INGEST_OUTPUTS=2` gives every `ingest-growth-same` feed
  that many outputs of kind `RESOURCE_SWEEP_INGEST_OUTPUT_KIND` (a catalog
  name, default `srtSource`), started as each feed goes live; every window
  waits for all of them to make progress. Default 0: publishers only. This
  is the many-feed matrix (feeds × few outputs)
- `MSR_PEER=sink` swaps MediaMTX for in-process `srt-rs`/RTMP sink listeners,
  which is what the packet-rate ladder uses so the peer side is not the
  bottleneck
- `RESOURCE_SWEEP_SRT_PEER_HOSTS=peer-a,peer-b` sends SRT outputs to sink
  peers on other hosts instead of `127.0.0.1` (spread by a stable hash of the
  output name). Run `scripts/harness/run.sh srt-sink -- --no-netns` on each
  peer host; it binds the SRT sink pool (default port 8891, override with
  `SRT_SINK_PORTS`) and prints its own counters plus that host's kernel UDP
  error/receive-buffer counters every `SRT_SINK_REPORT_SECS` (default 5 s).
- `RESOURCE_SWEEP_HLS_TLS=1` serves the HLS PUT counting sink over HTTPS with
  the checked-in fixture certificate (SAN `127.0.0.1`) and points HLS outputs
  at `https://`; the sink handshakes connections concurrently, as an ingest
  does. The sweep always passes that fixture to Restream as
  `RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM` (MediaMTX's RTMPS listener presents
  it too); RTMPS scenarios need the MediaMTX peer, not `MSR_PEER=sink`
- `RESOURCE_SWEEP_SCENARIOS=baseline-empty,ingest-only,ingest-growth-same,ingest-growth-mixed,egress-growth-source-same,egress-growth-source-srt,egress-growth-source-mixed,egress-growth-transcode-same,egress-growth-transcode-srt,egress-growth-transcode-mixed,egress-growth-source-plus-transcode-mixed,egress-growth-transcode-dual-mixed,egress-growth-source-plus-transcode-dual-mixed,egress-growth-hevc-bridge,egress-growth-source-hls,egress-growth-source-rtmps`
- `RESOURCE_SWEEP_LIFECYCLE=isolated|continuous|cumulative`
- `RESOURCE_SWEEP_NO_CLEANUP=1` to leave the final scenario running

The scenario filter is the main "cheap loop" knob. It lets you rerun only the
slice you care about instead of paying for the whole sweep.

If you specifically want the current-code passthrough-vs-transcode-family
baseline, use the dedicated harness mode instead:

```sh
./scripts/harness/run.sh branch-matrix
```

Examples:

```sh
RESOURCE_SWEEP_SCENARIOS=egress-growth-transcode-mixed \
RESOURCE_SWEEP_EGRESS_COUNTS=10 \
./scripts/harness/run.sh resource-sweep

RESOURCE_SWEEP_SCENARIOS=egress-growth-source-same,egress-growth-source-srt \
RESOURCE_SWEEP_EGRESS_COUNTS=50,100,200 \
./scripts/harness/run.sh resource-sweep

RESOURCE_SWEEP_SCENARIOS=egress-growth-hevc-bridge \
RESOURCE_SWEEP_EGRESS_COUNTS=10 \
./scripts/harness/run.sh resource-sweep

RESOURCE_SWEEP_SCENARIOS=egress-growth-transcode-dual-mixed,egress-growth-source-plus-transcode-dual-mixed \
RESOURCE_SWEEP_EGRESS_COUNTS=10 \
./scripts/harness/run.sh resource-sweep

HARNESS_SRT_PASSPHRASE=0123456789abcd \
HARNESS_SRT_PBKEYLEN=16 \
RESOURCE_SWEEP_SCENARIOS=ingest-only,egress-growth-source-mixed \
./scripts/harness/run.sh resource-sweep

RESTREAM_BIN=target/bench/restream \
SRT_CRYPTO_MATRIX_VARIANTS=plaintext,enc16,enc24,enc32 \
BRANCH_MATRIX_SCENARIOS=egress-growth-source-mixed \
./scripts/harness/run.sh srt-crypto-matrix
```

For MSR full certification, `MSR_FULL=1` runs an A/V marker signal-calibration
phase ahead of the canonical 30-audio MSR phase. The calibration phase keeps the
same checkpoints, Zipf output plan, and protocol mix, but uses the checked-in
two-audio A/V marker fixture so sampled outputs can be validated for marker
sync, audio PTS gaps, decode errors, clipping, and impulse/click artifacts.
Every MSR checkpoint also validates the MediaMTX path API before and after the
resource sample. Expected paths must be ready, expose tracks, show
`bytesReceived` growth, and report zero `inboundFramesInError`. A deterministic
random output sample is then probed with `ffprobe` using explicit
`MSR_FFPROBE_PROBESIZE`, `MSR_FFPROBE_ANALYZEDURATION`, and
`MSR_FFPROBE_SAMPLE_SECS`; the emitted JSON includes the sample size, seed, and
without-replacement detection confidence for the configured defect-rate
assumption.

To leave the last scenario up for interactive inspection:

```sh
RESOURCE_SWEEP_NO_CLEANUP=1 ./scripts/harness/run.sh resource-sweep
```

Lifecycle modes:

```sh
RESOURCE_SWEEP_LIFECYCLE=isolated ./scripts/harness/run.sh resource-sweep
RESOURCE_SWEEP_LIFECYCLE=continuous ./scripts/harness/run.sh resource-sweep
RESOURCE_SWEEP_LIFECYCLE=cumulative ./scripts/harness/run.sh resource-sweep
```

- `isolated`: restart `restream` and `mediamtx` between scenarios for cleaner attribution
- `continuous`: keep the same processes alive across the whole sweep and only rotate pipelines/publishers
- `cumulative`: keep the same processes and leave prior publishers/pipelines running so later rows show additive growth

## Contents

- [Current Authoritative Snapshot (June 28, 2026)](#current-authoritative-snapshot-june-28-2026)
- [Profiling Workflow](#profiling-workflow)
- [Matrix resource constraints](#matrix-resource-constraints)

## Current Authoritative Snapshot (June 28, 2026)

Source artifacts:

- `.local/artifacts/resource-sweep-authoritative/resource-sweep-results.csv`
- `.local/artifacts/bitrate-sweep-authoritative/bitrate-sweep-results.csv`

Peak memory and average CPU for the default isolated sweep:

| Scenario | Restream MB | Child FFmpeg MB | Combined MB | Total CPU % |
|---|---:|---:|---:|---:|
| Empty baseline | 72.8 | 0.0 | 72.8 | 1.15 |
| Ingest-only, H.264 RTMP | 59.0 | 0.0 | 59.0 | 1.65 |
| Ingest-only, H.264 SRT | 59.2 | 0.0 | 59.2 | 2.31 |
| Ingest-only, H.265 SRT | 60.9 | 0.0 | 60.9 | 3.31 |
| Same ingest growth, 5 pipelines | 82.6 | 0.0 | 82.6 | 7.27 |
| Mixed ingest growth, 5 pipelines | 75.9 | 0.0 | 75.9 | 8.92 |
| Mixed source egress, 20 outputs | 83.8 | 0.0 | 83.8 | 11.86 |
| Mixed 720p transcode egress, 20 outputs | 120.3 | 166.5 | 286.8 | 51.65 |
| HEVC bridge, 10 RTMP outputs | 158.7 | 0.0 | 158.7 | 71.82 |

Peak internal buffers for the most relevant scaling rows (the AVIO HWM column
measured the stage input queue, which the external-only FFmpeg cutover removed;
current harness output no longer reports it):

| Scenario | Source Ring MB | Transcoder Ring MB | TsMux Ring MB | AVIO HWM MB |
|---|---:|---:|---:|---:|
| Empty baseline | 0.1 | 0.0 | 0.0 | 0.0 |
| Same ingest growth, 5 pipelines | 19.0 | 0.0 | 0.0 | 0.0 |
| Mixed ingest growth, 5 pipelines | 15.6 | 0.0 | 0.0 | 0.0 |
| Mixed source egress, 20 outputs | 5.8 | 0.0 | 1.5 | 0.5 |
| Mixed 720p transcode egress, 20 outputs | 5.7 | 8.3 | 4.3 | 4.6 |
| HEVC bridge, 10 RTMP outputs | 5.8 | 8.2 | 0.0 | 0.0 |

What these numbers say:

- Baseline process cost is about `55-73 MB` depending on whether a pipeline is
  live. The fully empty isolated baseline peaked at `72.8 MB`.
- Ingest fan-in is cheap. Five live ingest pipelines stay under `83 MB` parent
  RSS with single-digit CPU.
- Source egress is cheap. Twenty mixed source outputs stay under `84 MB` parent
  RSS and about `12%` total CPU.
- External 720p transcode is mostly a child-process memory problem. The parent
  grows to `120.3 MB`, but the shared FFmpeg child adds another `166.5 MB`.
- Historical (in-process backend, since removed): the HEVC bridge reached
  `158.7 MB` and `71.82%` CPU without any child FFmpeg process.

### Per-rendition FFmpeg child cost (2026-10-15)

Release binaries at `1f4e0a34` (`cargo xtask build-release`, `target/qual-release/`
provenance matches HEAD), `scripts/harness/run.sh resource-sweep -- --no-netns`
(this host has no `unshare`), `MSR_PEER=sink`, `RESOURCE_SWEEP_EGRESS_COUNTS=1,5,10`,
20 s samples, 6 s settle. The child count is the sampler's FFmpeg child PID set
(`ffmpegCount` in `resource-sweep-samples.jsonl`), not the stage count, which
also covers TsMuxer and audio-router stages.

| Scenario | Outputs | FFmpeg children | Child RSS MB | Child CPU % (avg/peak) | Restream RSS MB | Threads | Delivered |
|---|---:|---:|---:|---:|---:|---:|---:|
| Empty baseline | 0 | 0 | 0.0 | 0.0 / 0.0 | 43.9 | 10 | — |
| Ingest only (RTMP, SRT, HEVC) | 0 | 0 | 0.0 | 0.0 / 0.0 | 44.9–51.0 | 10–11 | — |
| 720p rendition, one family | 1 / 5 / 10 | 1 | 168.2 / 168.3 / 186.3 | 81.5 / 77.3 / 108.5 | 61.4 / 64.3 / 65.1 | 16 / 15 / 15 | 1 / 5 / 10 |
| 720p rendition, mixed egress | 2 / 10 / 20 | 1 | 186.6 / 186.7 / 186.7 | 89.6 / 68.9 / 69.5 | 85.1 / 97.3 / 99.6 | 22 / 23 / 23 | 2 / 10 / 20 |
| 720p + 1080p renditions | 4 / 20 / 40 | 2 | 422.3 / 422.7 / 422.9 | 162.9 / 139.6 / 152.8 | 182.1 / 197.9 / 214.7 | 29 / 29 / 36 | 4 / 20 / 40 |
| Source + 720p + 1080p | 6 / 30 / 60 | 2 | 417.0 / 417.4 / 417.5 | 122.2 / 129.6 / 114.1 | 196.2 / 212.1 / 242.4 | 33 / 32 / 36 | 6 / 30 / 60 |
| HEVC bridge | 1 / 5 / 10 | 0 | 0.0 | 0.0 / 0.0 | 58.3 / 59.8 / 60.5 | 12 / 11 / 12 | 1 / 5 / 10 |

What these numbers say:

- The child count is a property of the pipeline's FFmpeg-backed stages, not of
  its destinations: one child carried 1 to 10 outputs, two carried 4 to 60, and
  the `egress-growth-hevc-bridge` scenario ran 10 outputs with none.
- Per-child RSS is flat as destinations grow: 186.6 to 186.7 MB across 2, 10 and
  20 outputs, 422.3 to 422.9 MB across 4, 20 and 40, 417.0 to 417.5 MB across 6,
  30 and 60. The 1080p child costs about 235 MB against the 720p child's 186 MB.
- The parent, not the children, grows with destinations: 85.1 to 99.6 MB at 2 to
  20 outputs and 182.1 to 214.7 MB at 4 to 40.
- Every row delivered every destination.
- The 20-output mixed row agrees with the 2026-06-28 snapshot: 286.3 MB combined
  here against 286.8 MB there.

## Profiling Workflow

For targeted profiling, avoid a full sweep. Run one scenario with a long sample
window and attach a profiler during the live run:

```sh
WORK_DIR=.local/artifacts/profile-external \
RESOURCE_SWEEP_SCENARIOS=egress-growth-transcode-mixed \
RESOURCE_SWEEP_EGRESS_COUNTS=10 \
RESOURCE_SWEEP_SAMPLE_SECS=30 \
./scripts/harness/run.sh resource-sweep
```

On Linux, attach `perf` to the live pid:

```sh
perf stat -e task-clock,context-switches,cpu-migrations,page-faults,minor-faults,major-faults -p <pid> -- sleep 10
perf record -F 99 -e cpu-clock -g -p <pid> -- sleep 10
perf report --stdio --no-children
```

On this WSL setup the `/usr/bin/perf` wrapper did not match the running kernel,
but `/usr/lib/linux-tools/6.8.0-124-generic/perf` worked for software-counter
sampling.

Current profiling takeaways from June 28, 2026:

- External `720p` mixed egress: `restream` itself is light (`282 ms`
  task-clock over a 10 second sample), while child `ffmpeg` carried the load
  (`4.65 s` task-clock). Hot symbols were `x264_8_trellis_coefn`,
  `__memmove_avx_unaligned_erms`, and heavy `pthread_cond_broadcast` wakeups.
- Internal `720p` mixed egress: hot symbols moved into `restream` and included
  `x264_8_encoder_encode`, `slicetype_frame_cost`, `ff_hscale8to15_4_avx2`,
  and `ff_h264_decode_mb_cavlc`.
- HEVC bridge: hot symbols were `x264_8_encoder_encode`,
  `ff_hevc_deblocking_boundary_strengths`, `ff_hevc_hls_filter`,
  `ff_hevc_hls_residual_coding`, plus the same futex/condvar wakeup signature.

Optimization direction from those profiles:

- For external `720p`, optimizing parent Rust code will not move the main CPU
  needle much; the child encoder dominates.
- For internal `720p` and the HEVC bridge, the codec work is real, but the
  wakeup/futex footprint suggests there is still room to reduce handoff churn
  between queueing stages and worker threads.
- Avoiding transcode remains the largest operational win. Source passthrough
  scales cleanly in both CPU and memory.

Then watch:

- Restream UI: `http://127.0.0.1:3030`
- mediamtx API: `http://127.0.0.1:9997/v3/paths/list`

Manual shutdown:

```sh
pkill -9 -x ffmpeg
pkill -9 -x restream
pkill -9 -x mediamtx
```

## Matrix resource constraints

This page defines the resource boundary for live matrix runs. It intentionally
does not duplicate current harness modes, queue values, or measured totals.

### Scope

Live matrices exercise the compiled Rust server with publishers, destinations,
and optional FFmpeg/MediaMTX peers. They are integration and measurement work,
not production capacity certification. The harness owns scenario construction;
the production runtime owns admission and bounded media storage.

### Resource owners

| Resource | Current owner |
|---|---|
| Cargo/build parallelism | `.cargo/config.toml` (`jobs = -1`); no host-wide build serialization |
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

### Harness boundary

MediaMTX is used only where a scenario needs an independent protocol peer or
sink. FFmpeg may act as a fixture publisher, reader, or external transform
child. These are test topology components; neither replaces the Rust server's
production transport ownership.

Fixture resolution, process cleanup, artifact retention, and supported modes
are executable behavior. Consult the harness entry point and
[Testing](testing.md) instead of copying their lists here.

The harness deliberately avoids killing unrelated media processes. In a shared
host or worktree session, establish process ownership and check for running builds
(`pgrep -a -x cargo`) before starting a heavy run.

### Running a bounded matrix

1. Build the required profile. If the mode consumes `target/bench/`, use
   `cargo xtask build-bench`.
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

### Interpreting results

Record the workload shape, build identity, fixture, host limit, and whether
publisher/sink resources are included. Separate these conclusions:

- protocol correctness under the exercised topology;
- recovery behavior under the injected fault;
- directional CPU, memory, queue, or throughput evidence;
- a deployment recommendation supported across representative hosts.

Only the first two can usually be established by a single bounded run. Record
repeatable results in the commit that makes the change, not in this
contract.
