# Mahashivratri Hero Scenario: SRT Multi-Language Fan-Out

## Contents

- [Status](#status)
- [Canonical Mahashivratri Workload](#canonical-mahashivratri-workload)
- [Expected Current Runtime Graph](#expected-current-runtime-graph)
- [Capacity Model](#capacity-model)
- [Measured Baselines](#measured-baselines)
- [Risks To Track](#risks-to-track)
- [Representative Harness Plan](#representative-harness-plan)
- [Harness Implementation Shape](#harness-implementation-shape)
- [Proposed Controls](#proposed-controls)
- [Acceptance Contract](#acceptance-contract)
- [Tracking Checklist](#tracking-checklist)
- [Related Documentation](#related-documentation)

## Status

- **Scenario status:** canonical Mahashivratri capacity and correctness target
- **Architecture status:** model and test the current implementation; no media-path
  redesign is implied by this document
- **Harness status:** `msr` mode implemented, including an owned sink-mode
  verification peer (`MSR_PEER=sink`) as an alternative to mediamtx for
  raw-connection-count runs. Measured at full scale on a dedicated 6-core
  VPS at real 1080p60/8Mbps bitrate on 2026-08-13 for all three canonical
  protocol mixes (pure RTMP, pure SRT, canonical 95/5); 4K, HEVC, and
  event-hardware/external-link runs still pending
- **Baseline status:** all three protocol mixes clean at the full
  1,200-output target with real 1080p60/8Mbps media, recorded 2026-08-13 —
  see
  [1,200-output resource attribution](agent-guidance/quality/msr-1200-resource-attribution-2026-08-13.md)
  for the measured CPU/RSS/thread footprint and
  [the SRT egress scale investigation](agent-guidance/quality/srt-egress-scale-investigation-2026-08-10.md)
  for the correctness fixes that made this run clean. The earlier
  2026-07-11 connection-scale baseline (synthetic low-bitrate fixture) is
  superseded by this real-bitrate result; see
  `docs/agent-guidance/quality/baselines.md` § "Mahashivratri msr
  full-scale ramp" for the historical entry.

This document tracks the Mahashivratri production scenario for the current
backend: one high-resolution SRT contribution carrying one video stream and 30
language audio tracks, fanned out to mostly RTMP push outputs for the event.
Each RTMP output selects exactly one audio track. SRT outputs may select one
track, a subset, or all tracks.

The purpose is to keep the Mahashivratri assumptions, expected runtime graph,
proof plan, and eventual event-capacity measurements in one durable place. It
is not an architecture proposal.

## Canonical Mahashivratri Workload

### Ingest

| Dimension | Canonical value |
|---|---|
| Protocol | SRT / MPEG-TS |
| Video streams | 1 |
| Video resolution | Run separately at 1080p and 4K |
| Video frame rate | 30 fps initially; 60 fps as an explicit extension |
| Video codecs | H.264 primary; HEVC compatibility variant |
| Audio tracks | 30 AAC tracks at 48 kHz |
| Audio layouts | 29 stereo, 1 5.1 |
| Track identity | Stable track index plus language metadata |
| Output video encoding | `source` unless a test explicitly selects a preset |

The canonical language ranks are:

```text
English, தமிழ், हिन्दी, తెలుగు, ಕನ್ನಡ, मराठी, नेपाली, Bengali,
മലയാളം, Gujarati, Odia, Italian, Spanish, French, German, Russian,
Portuguese, Arabic, Simplified Chinese, Traditional Chinese, Indonesian,
Japanese, Korean, Urdu, Turkish, Vietnamese, Thai, Punjabi, Dutch, Polish
```

Simplified and Traditional Chinese both use the MPEG-TS ISO 639 language code
`zho`; their distinct stream titles preserve the requested identity.

The primary run uses source-resolution passthrough so it measures the actual
fan-out and routing path rather than conflating it with a resolution transcode.
The HEVC variant includes the current shared HEVC-to-H.264 compatibility stage
for RTMP outputs.

### Output population

The canonical Zipf distribution uses exponent `s = 1`, rank-one population
300, and rounded counts `N(rank) = round(300 / rank)`. This deliberately sums
to exactly 1,200 outputs:

```text
300, 150, 100, 75, 60, 50, 43, 38, 33, 30,
27, 25, 23, 21, 20, 19, 18, 17, 16, 15,
14, 14, 13, 13, 12, 12, 11, 11, 10, 10
```

| Protocol | Share | Output count |
|---|---:|---:|
| RTMP | 95% | 1,140 |
| SRT | 5% | 60 |
| **Total** | **100%** | **1,200** |

Protocol assignment must preserve the rank distribution as closely as
possible while producing exactly 1,140 RTMP and 60 SRT outputs. A deterministic
assignment is required so repeated runs are comparable.

Every RTMP output uses `source+atrack:N`. The primary SRT run also uses one
selected track per output so protocol costs can be compared against the same
media shape. Two SRT correctness variants are required:

1. selected subsets, including at least 2-, 3-, and 6-track subsets;
2. `source` with all 30 audio tracks.

The subset/all variants need not carry the full 60-output population during
the first proof. Their purpose is to validate correctness and measure the
incremental package cost of those current supported shapes.

## Expected Current Runtime Graph

### H.264 source variant

With all 30 languages represented:

- one SRT ingest task and inline MPEG-TS demuxer;
- one adaptive source ring;
- 30 shared `audio:atrack:N:from:source` router stages;
- 1,140 independent RTMP egress tasks and TCP connections;
- shared SRT MPEG-TS mux stages keyed by distinct final encoding;
- 60 independent SRT feeder tasks, sender threads, sockets, and AVIO queues.

The Zipf distribution changes the number of last-hop senders per language. It
does not create additional audio routers for a hot language: the 300 rank-one
outputs share the same selected-audio ring.

### HEVC source variant

The current RTMP compatibility edge adds:

- one shared `hevc_to_h264:from:source` stage for all RTMP source outputs;
- RTMP audio routers downstream of the H.264 compatibility ring;
- separate native-HEVC audio routers for SRT selected-track outputs.

If all 30 languages are used by both protocols, up to 60 selected-audio router
stages are therefore expected. This is current stage-key behavior, not a target
for consolidation.

### High-track-count packet rate

The source-ring sizing model assumes approximately 50 AAC packets per second
per audio track:

```text
30 tracks * 50 audio packets/s + 30 to 60 video packets/s
= approximately 1,530 to 1,560 source packets/s
```

Six seconds of adaptive source-ring headroom requires approximately 9,180 to
9,360 slots, below the current 16,384-slot maximum.

With 30 distinct selected-track routes, the audio routers collectively inspect
approximately 46,000 source packets per second. Payload bytes are shared, but
packet objects, ring writes, metrics, and notifications remain per route.

## Capacity Model

The following figures are planning estimates, not measured baselines. They
assume approximately 192 kbps per selected stereo AAC track. The one 5.1 track
does not materially change the aggregate unless it is assigned a much higher
bitrate.

| Source video bitrate | Approximate media payload egress at 1,200 outputs |
|---:|---:|
| 8 Mbps | 9.8 Gbps |
| 12 Mbps | 14.6 Gbps |
| 25 Mbps | 30.2 Gbps |
| 40 Mbps | 48.2 Gbps |

Wire overhead, retransmission, and operational headroom are additional. A
localhost harness can measure backend CPU, memory, task, queue, and socket
behavior, but cannot certify physical NIC capacity from these figures.

At 30 fps, one selected AAC track produces roughly 80 media messages per
output per second. At 60 fps it produces roughly 110. The canonical population
therefore drives approximately 96,000 to 132,000 per-output media-message sends
per second.

## Measured Baselines

**Real-bitrate full-scale run (2026-08-13, dedicated 6-core VPS, 1080p60
H.264 source passthrough at 8 Mbps, 30 audio tracks, sink-mode verification
peer with real RTMP/SRT protocol negotiation)**: all three canonical
protocol mixes clean at every checkpoint through 1,200 outputs —

| Mix | Outputs | CPU (of 6 cores) | RSS | Threads |
|---|---:|---:|---:|---:|
| pure SRT | 1200/1200 | ~2.9 | 4.11 GB | 214 |
| pure RTMP | 1200/1200 | ~2.4 | 1.51 GB | 55 |
| canonical 95/5 | 1200/1200 | ~3.2 | 1.97 GB | 223 |

Full thread/memory/CPU attribution, including why SRT's footprint differs
so much from RTMP's, lives in
[1,200-output resource attribution](agent-guidance/quality/msr-1200-resource-attribution-2026-08-13.md).
This closes connection-scale evidence for MSR-02, MSR-03, and MSR-07 at
real 1080p60 bitrate (superseding the synthetic-bitrate run below) and
covers the 1080p slice of Phase 3's bitrate envelope. MSR-01 (external-link
certification), the 4K/HEVC slice of Phase 3, and Phase 4 (degradation
slices) remain open.

First full-scale Phase 2 ramp (2026-07-11, commit 6fc2f254, dedicated 6-vCPU
EPYC gen1 VPS, 1080p30 H.264 passthrough, loopback sink, synthetic
low-bitrate fixture): PASS at every checkpoint through 1,200 outputs with no
capacity knee — ~2.4 cores average / 2.8 peak and 447 MB RSS at 1,200
outputs, sublinear CPU scaling, zero warnings or errors. Full per-checkpoint
table and caveats live in `docs/agent-guidance/quality/baselines.md`
§ "Mahashivratri msr full-scale ramp — 2026-07-11 (VPS)".

Hardware-counter profiling during that 2026-07-11 soak (same host, same
commit) attributed two structural CPU costs inside the ~2.4-core total:
the SRT ingest epoll waiter busy-spins for ~1 core per ingest
(`src/media/srt.rs:1536`; fix filed), and libsrt allocates one multiplexer
(2 OS threads) per SRT egress — 122 threads and ~1 core of RcvQ work at 60
SRT outputs. Details and the tokio locality dataset live in
`docs/agent-guidance/quality/baselines.md` § "Profiling notes (VPS)". The
per-SRT-egress-connection multiplexer cost this profiling described was
since fixed to be per-*shard* instead (see the SRT egress scale
investigation); the 2026-08-13 run above reflects that fix.

## Risks To Track

| ID | Risk | Evidence required |
|---|---|---|
| MSR-01 | Aggregate network bandwidth exceeds the host link | Measured bytes/s plus external-link certification |
| MSR-02 | Per-output RTMP packetization/socket work saturates Tokio workers | CPU, scheduler, progress, and latency samples across the ramp |
| MSR-03 | Thirty audio routers cause allocation/cache pressure | Stage CPU, RSS, retained payload, allocation profile |
| MSR-04 | Adaptive ring replacement disrupts mass startup | Startup timeline, cancellations, retries, time-to-progress |
| MSR-05 | Correlated sink failure creates a retry/log/DB storm | Fault slice with bounded recovery and reconciler timing |
| MSR-06 | Slow outputs overflow and repeatedly restart | One and many slow-sink fault slices; overflow and retry counters |
| MSR-07 | SRT per-output threads and buffers consume excessive resources | Thread count, AVIO HWM, RSS/PSS, kernel socket memory |
| MSR-08 | 5.1 AAC is not accepted by an RTMP destination | Interop probe of passthrough 5.1 and optional downmix route |
| MSR-09 | The shared 4K HEVC compatibility stage misses real time | Stage CPU, output progress, decode probe, dropped/overflow counters |
| MSR-10 | Teardown leaks tasks, stages, sockets, jobs, or memory | Post-stop convergence and resource return-to-baseline assertions |

## Representative Harness Plan

The Mahashivratri scenario is implemented using the existing live harness philosophy:
run the production binary, configure it through the API, publish real media
over SRT, and receive real RTMP/SRT egress over localhost. It is a dedicated
bench-profile measurement mode rather than another row multiplied through
`mixed.matrix`.

Mode name:

```text
msr
```

The mode command is lowercase `msr`, consistent with the other harness modes.
MSR stands for **Mahashivratri**. It is an additive,
bench-profile-only harness mode and is not part of the default suite. Its safe
default runs the first 30 outputs; set `MSR_FULL=1` for the canonical
30/120/300/600/900/1,200 ramp or override `MSR_OUTPUT_COUNTS` directly.
Each checkpoint must now prove both Restream output progress and sink-side
MediaMTX health by querying `/v3/paths/list`: every expected sink path must be
`ready=true`, and aggregate `bytesReceived` must grow across the sample window.
The harness writes the machine-readable rollup to `msr-results.json` and a
human report to `msr-report.md`.

Run the deterministic plan smoke, bounded default, or full Mahashivratri ramp:

```sh
MSR_PLAN_ONLY=1 ./scripts/harness/run.sh msr
./scripts/harness/run.sh msr
MSR_FULL=1 ./scripts/harness/run.sh msr
```

The implementation must follow the repository's two-tier testing strategy:
pure topology/distribution rules in unit tests, and media/runtime behavior in
the live binary-plus-API harness. It must also follow fixture discipline and
write artifacts under a run-specific `.local/artifacts/` directory.

### Phase 0: deterministic topology unit tests

Test without sockets or media:

- Zipf rank vector has 30 entries and totals 1,200;
- protocol allocation totals exactly 1,140 RTMP and 60 SRT;
- every RTMP output resolves to exactly one `atrack:N` selection;
- requested SRT single/subset/all encodings are valid;
- expected distinct stage keys are calculated for H.264 and HEVC;
- output IDs, sink paths, and ports are deterministic and collision-free.

### Phase 1: 30-track correctness fixture

Add a checked-in, compact MPEG-TS fixture registered in
`src/test_fixtures.rs` with:

- one H.264 video stream;
- 30 AAC audio streams at 48 kHz;
- 29 stereo tracks and one 5.1 track;
- stable language descriptors;
- distinguishable audio markers so track selection proves identity, not only
  stream count.

The existing two-audio transport fixtures prove the routing mechanism, and the
`2v16a` media-library fixture proves higher track-count probing, but neither is
an exact transport-level oracle for this scenario.

The first live correctness run should create one RTMP and one SRT output for
each language, plus representative SRT subset and all-track outputs. Assert:

- ingest reports 30 audio tracks with the expected language/channel metadata;
- every RTMP sink receives H.264 plus exactly one correct AAC language;
- every selected SRT sink receives the requested tracks only;
- the all-track SRT sink receives all 30 tracks;
- selected tracks are reindexed correctly at the output edge;
- shared stage counts match the expected graph;
- no output reports retry, overflow, or failure.

### Phase 2: bounded Zipf fan-out ramp

Use the real production binary and real localhost sinks. Ramp through
deterministic prefixes of the same canonical population, for example:

```text
30, 120, 300, 600, 900, 1,200 outputs
```

Keep the source bitrate moderate for this phase so it isolates connection and
per-output backend cost from physical-link saturation. At each step record:

- active/healthy/progressing output counts by protocol and language rank;
- startup p50/p95/p99 and total convergence time;
- restream RSS/PSS, CPU, file descriptors, task count, and OS-thread count;
- child FFmpeg count/RSS/CPU;
- source, selected-audio, and TS-mux ring fill/HWM/overflow counters;
- AVIO queue length, HWM, and blocked writes;
- egress bytes/s and messages/s by protocol;
- retry, failure, and log-event rates;
- SQLite/reconciler tick duration if exposed by current telemetry.

Correctness probing should occur once per distinct language/subset shape plus a
deterministic sample of duplicate hot-language outputs. Spawning an `ffprobe`
process for all 1,200 outputs would measure probe-process fan-out more than the
backend. The scale checkpoint should use a generic receiver-health signal for
every output path, currently MediaMTX `/v3/paths/list` readiness plus
`bytesReceived` growth, and reserve full decode/probe checks for representative
routes.

### Phase 3: bitrate and codec envelope

Repeat selected stable population points with:

- H.264 1080p source;
- H.264 4K source;
- HEVC 4K source with the shared RTMP compatibility stage;
- 30 fps and, separately, 60 fps;
- passthrough 5.1 AAC and a shared stereo downmix variant.

Do not require the local host to run every bitrate/population cross-product.
Use a small matrix chosen from Phase 2's knee points. The full 1,200-output 4K
run is a certification case, not a routine developer gate.

### Phase 4: degradation slices

Run separately from clean capacity measurements:

1. one stalled RTMP sink in the hottest language;
2. one stalled SRT sink;
3. a bounded percentage of sinks disconnecting together;
4. restart of the shared sink service;
5. stop and recreate the full output population;
6. publisher disconnect and reconnect while all outputs are desired-running.

Assert sibling progress, bounded retry behavior, causal failure phases, and
complete teardown. Fault runs must not be mixed into performance baselines.

### Phase 5: external-link certification

Loopback runs cannot validate 10–50 Gbps wire behavior. A full certification
run requires remote or distributed sinks and records:

- NIC throughput, drops, errors, queue depth, and retransmits;
- host softirq/system CPU;
- RTMP and SRT delivery progress at the receivers;
- end-to-end packet loss, continuity, and decode success;
- backend telemetry and harness artifacts from the same time window.

This phase may be unavailable on a development workstation. Its absence must
be reported as an unproven network envelope rather than inferred from localhost
success.

## Harness Implementation Shape

The lowest-risk implementation is an extension of the current declarative
resource/ramp infrastructure:

- a typed `HeroLanguageFanoutConfig` owning rank counts, protocol assignment,
  codec, resolution, frame rate, and SRT subset/all variants;
- a declarative scenario file rather than 1,200 hand-authored output rows;
- generic receiver-health adapters, starting with MediaMTX path-health polling,
  avoiding one helper process per output;
- production output creation through the existing HTTP API;
- incremental `scenario.json`, JSONL assertions, raw 1 Hz samples, CSV summary,
  and final JSON result;
- bench-profile execution through `scripts/harness/run.sh`;
- serial measurement discipline and explicit detection of pre-existing media
  processes.

Do not use one FFmpeg or ffprobe child per output during the scale phase. That
would make helper-process cost dominate the system under test. Use full media
probes only for distinct output shapes and sampled duplicates.

## Proposed Controls

Names are provisional until the mode is implemented:

| Variable | Default | Purpose |
|---|---:|---|
| `MSR_OUTPUT_COUNTS` | `30` | Ramp checkpoints; overrides `MSR_FULL` |
| `MSR_FULL` | unset | Use `30,120,300,600,900,1200` checkpoints when set to `1` |
| `MSR_PLAN_ONLY` | unset | Emit the deterministic 1,200-output plan without starting media |
| `MSR_SAMPLE_SECS` | `6` | Stable sampling window per checkpoint |
| `MSR_SAMPLE_INTERVAL_MS` | `1000` | Raw resource sample interval |
| `MSR_SETTLE_SECS` | `4` | Settle time before sampling |
| `MSR_PROGRESS_TIMEOUT_BASE_SECS` | `60` | Initial output-progress allowance |
| `MSR_PROGRESS_TIMEOUT_PER_OUTPUT_SECS` | `2` | Additional progress allowance per output |
| `MSR_PROGRESS_TIMEOUT_CAP_SECS` | `900` | Maximum progress wait |
| `MSR_NO_CLEANUP` | unset | Leave the final stack for inspection |
| `PEER_COUNT` | `1` | Number of peer instances (`MTX_RTMP`/`MTX_SRT`/`MTX_API` + instance offset each); outputs distribute round-robin by ordinal (`ordinal % PEER_COUNT`) |
| `PEER_SKIP_START` | unset | Meaningful only for `MSR_PEER=mediamtx`: peer instances are pre-started externally, and the harness verifies all `PEER_COUNT` instances are live instead of spawning them. No effect on `MSR_PEER=sink` — an in-process listener has no external-process equivalent to skip-start; it is always bound fresh by the harness. |
| `MSR_PEER` | `mediamtx` | `mediamtx` (default) or `sink` — see "Peer modes" below |
| `HARNESS_SRT_SINK_BACKEND` | `libsrt` (or `RESTREAM_SRT_BACKEND`) | `MSR_PEER=sink` only: `libsrt` for the native control stack or `rust` for the pure-Rust Core receiver. The Rust MSR path must set this to `rust` so the sink cannot bottleneck Rust egress with libsrt |
| `HARNESS_SRT_SINK_THREADS` | `PEER_COUNT` | `MSR_PEER=sink` only: **libsrt** discard-thread count for the shared SRT sink pool, partitioned into exclusively-owned port chunks. The Rust backend uses one mio loop and ignores this value |
| `HARNESS_SRT_SINK_UDP_BUFFER` | `8388608` (8MB) | `MSR_PEER=sink` only: native libsrt sink `SRTO_UDP_RCVBUF`/`SRTO_UDP_SNDBUF`; the Rust backend does not use this native option |
| `HARNESS_SRT_SINK_FC_PACKETS` | `32768` | `MSR_PEER=sink` only: SRT flow-control ceiling used by both the libsrt and Rust sink backends |
| `HARNESS_SRT_SINK_RCVBUF_BYTES` | `12582912` (12MiB) | `MSR_PEER=sink` only: SRT send/receive buffer policy used by both sink backends; Rust converts it to packets using 1472 bytes per packet and caps it at FC |
| `MSR_SRT_BOND` | unset | `MSR_PEER=sink` test-only switch: adds a second SRT leg to the same sink endpoint so the Rust or libsrt bonding receiver is exercised without changing ordinary MSR output URLs |
| `MSR_SRT_BOND_MODE` | unset (`backup`) | `MSR_SRT_BOND=1` only: `backup` or `broadcast`; appends `bondmode=` to the output URL so the native and Rust egress paths can be tested against the matching sink group mode |
| `MSR_SKIP_FFPROBE` | unset | Skip ffprobe read-back checks (always forced on when `MSR_PEER=sink`) |
| `MSR_SINK_SAMPLE_SECS` | `3` | mediamtx path-health sample window before the resource-window sample |
| `MSR_SINK_POST_SAMPLE_SECS` | `2` | mediamtx path-health sample window after the resource-window sample |
| `MSR_SINK_TIMEOUT_SECS` | `60` | Timeout for mediamtx path-health verification (both windows above) |
| `MSR_SINK_ENGINE_SAMPLE_SECS` | `2` | Engine-health bytesOut sample window used only when `MSR_PEER=sink` |

### Peer modes (`MSR_PEER`)

Every egress output publishes into a peer process the harness itself owns —
this is the only supported way to run the 1,200-output scale test; peers are
never expected to be started by hand outside `PEER_SKIP_START`.

- **`mediamtx`** (default): `PEER_COUNT` mediamtx instances, each with its own
  config/log file (instance 0 keeps the pre-existing `msr-mediamtx.yml`/`.log`
  names; instance N>0 is suffixed `-N`). Each checkpoint's expected paths are
  grouped by the instance the output actually published to and verified
  against that instance's `/v3/paths/list`, then merged into one
  `mediamtxPathHealth`/`mediamtxPostSamplePathHealth` aggregate in the
  checkpoint JSON (with `PEER_COUNT=1`, the default, this is byte-identical to
  the pre-multi-instance shape).
- **`sink`**: `PEER_COUNT` in-process, harness-native accept-and-discard
  listeners for RTMP (`sinks.rs`'s `GeneralizedSinkServer`, one per
  instance) in place of mediamtx, bound directly by the `test_harness`
  process itself — far lower per-connection memory than a real mediamtx
  path, for runs where raw connection count matters more than a readable
  path. As of `docs/agent-guidance/quality/srt-scaling-investigation.md`'s
  sink-mode extraction, this replaced spawning a separate `restream
  --sink-mode` process per instance; `RESTREAM_SINK_MODE` no longer exists
  in production restream (see that doc for why the two "sink" concepts —
  this receiver and the unrelated `sink://` egress output type — needed to
  be disambiguated). Because binds are synchronous and in-process, there is
  no readiness-polling step to wait on, and `PEER_SKIP_START` has no effect
  on `sink` peers (it exists only for pre-started external processes, i.e.
  `mediamtx`).

  The SRT side is a single shared sink pool spanning every `PEER_COUNT` port,
  not one listener per instance. `HARNESS_SRT_SINK_BACKEND=libsrt` selects
  `harness_srt_sink.rs::HarnessSrtSinkPool`; `HARNESS_SRT_SINK_BACKEND=rust`
  selects the pure-Rust `SrtConnection` receiver and its one mio readiness
  loop. Rust-stack measurements must use the latter.
  `HARNESS_SRT_SINK_THREADS` is a **total** thread budget for that pool
  (default: `PEER_COUNT`, i.e. one thread per port), partitioned into
  contiguous, **exclusively owned** port chunks — `ports.len() /
  HARNESS_SRT_SINK_THREADS` ports per thread, remainder spread one-per-thread
  to the first few. No two threads ever call anything on the same listener
  socket. This replaced an earlier design where every thread shared one
  listener per instance via concurrent `srt_accept()`/`srt_recv()`, which
  the tunable sweep in `srt-scaling-investigation.md` found to be a severe
  regression (a listening port in unpatched libsrt has one shared
  multiplexer; multiple threads hammering it concurrently contends against
  its internal locking rather than adding capacity). Exclusive ownership
  fixed that and, per that doc's follow-up sweep, 2 ports/thread
  outperforms 1 port/thread at every port count tested, with a measured
  local optimum around 8 ports / 4 threads on a 6-core host — pushing
  either axis further can get worse again once total discard-thread count
  starts competing with the sender for CPU.

  Because a sink peer discards data below the RTMP/SRT protocol layer, mediamtx
  path-health and ffprobe read-back are skipped entirely; each checkpoint
  is instead verified from restream's own `/api/v1/engine/health`: every
  expected output must be present and its `bytesOut` must grow across
  `MSR_SINK_ENGINE_SAMPLE_SECS`. The checkpoint JSON carries a
  `sinkVerification` object with `outputsExpected`/`outputsPresent`/
  `bytesOutBefore`/`bytesOutAfter`/`bytesOutDelta`/`packetsSentDrop` totals
  in place of `mediamtxPathHealth`. **Read `bytesOutDelta` against the
  target aggregate, not just its presence** — `outputsPresent`/`PASS` alone
  does not mean sustained throughput; see the srt-scaling-investigation.md
  doc's `srt-only` correction for a case where it badly did not. The sink
  RTMP listener completes real `connect`/`createStream`/`publish`
  negotiation (`rml_rtmp::sessions::ServerSession`, the same state machine
  real ingest drives) before discarding media, so a genuine RTMP egress
  connection proceeds past its handshake and delivers real `bytesOut`
  growth — `sink` mode is a clean pass for `rtmp-only`, `srt-only`, and the
  canonical mix alike.

## Acceptance Contract

The first implementation is complete when it can produce repeatable artifacts
for Phases 0–2. It must not invent pass thresholds before a clean baseline has
been captured. Initial runs establish the curve and identify the first
capacity knee.

Eventually, a passing canonical run should require:

- exact requested output count and 95/5 protocol mix;
- every connection making sustained forward progress;
- correct audio identity for every distinct route and sampled duplicate;
- no unexpected stage multiplication;
- zero source/audio-router overflows during the clean run;
- no unexplained retries or failed outputs;
- bounded RSS/CPU/FD/thread growth relative to the recorded baseline;
- complete shutdown and return near the pre-run resource baseline;
- an explicit statement of whether network capacity was measured externally or
  remains unproven.

## Tracking Checklist

- [x] Canonical workload and Zipf population documented
- [x] Expected H.264 and HEVC runtime graphs documented
- [x] Representative harness approach defined
- [x] Exact 30-track transport mapping built from the checked-in `2v16a` fixture
- [x] Deterministic topology unit tests added
- [x] `msr` mode registered
- [x] Generic receiver-health proof recorded at the required connection counts
      (mediamtx path-health and, since 2026-08-12, sink-mode's real
      RTMP/SRT protocol-level verification)
- [ ] Phase 1 correctness run passing (per-language audio identity at scale;
      distinct from the connection-scale/bytes-out proof recorded so far)
- [x] Phase 2 bounded Zipf ramp baseline recorded (all three protocol mixes,
      1,200 outputs, real 1080p60/8Mbps bitrate — 2026-08-13)
- [ ] Phase 3 1080p/4K and H.264/HEVC envelope recorded (1080p H.264 slice
      done 2026-08-13; 4K and HEVC still open)
- [ ] Phase 4 degradation behavior recorded
- [ ] Phase 5 external-link certification recorded or explicitly waived

## Related Documentation

- [Media pipeline](media-pipeline.md)
- [Testing](testing.md)
- [Testing strategy](testing-strategy.md)
- [Resource sweep](resource-sweep.md)
- [Matrix resource constraints](matrix-resource-constraints.md)
- [Performance and resource baselines](agent-guidance/quality/baselines.md)
- [SRT egress scale investigation](agent-guidance/quality/srt-egress-scale-investigation-2026-08-10.md)
- [1,200-output resource attribution (thread/memory/CPU by mix)](agent-guidance/quality/msr-1200-resource-attribution-2026-08-13.md)
- [netns confound investigation (4-worktree controlled campaign)](agent-guidance/quality/msr-1200-netns-confound-investigation-2026-08-14.md)
