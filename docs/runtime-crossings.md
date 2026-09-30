# Runtime Crossings Audit

Every place media or protocol work crosses between execution contexts
(Compio/io_uring owners, Tokio, FFmpeg threads), with its mechanism, rate,
bound, measured cost where available, and a verdict: **justified** (the
crossing buys ownership, isolation or backpressure worth its cost),
**fixed** (a cheaper form landed), or **measure** (plausibly material at
scale; the WI8 instrument that decides it is named), or **interim** (bounded
and safe, but continuous media work on the Tokio control runtime: boundedness
proves safety, not that Tokio is the right steady-state executor; WI11 in
`docs/srt-compio-roadmap.md` moves it). Updated as crossings change;
`docs/srt-compio-roadmap.md` WI8 owns the measurement programme.

Execution contexts:

```text
Tokio CONTROL    API, DB, pipeline state, session lifecycle, HLS PUT uploads;
                 still media: lightweight audio routers, diagnostic RTMP play
Compio/io_uring  RTMP ingress owner (one) and SRT ingress Owner (one), each
                 running its ingest media to completion (parse/demux, gate,
                 timestamps, GOP, ring publish); direct SRT play on the SRT
                 Owner; egress fabric shards (RTMP/RTMPS/SRT), woken
                 directly by the publishing thread
Media executors  fixed pool (`restream-media`): shared SRT TS muxing, HLS
                 segmenting (TS and fMP4), recording feeder, external file demux
                 and ring publication
FFmpeg threads   transcoders: in-process stages pull the source ring from
                 their AVIO read; external stages use one stdin thread and
                 one stdout thread per child
Disk             blocking file writer threads
```

## Contents

- [Media crossings](#media-crossings)
- [Control and wake crossings](#control-and-wake-crossings)
- [Observability crossings](#observability-crossings)
- [Open measurement items](#open-measurement-items)

## Media crossings

| # | Crossing | Mechanism | Rate (8 Mbit/s stream) | Bound | Verdict |
|---|---|---|---|---|---|
| C1 | RTMP publish: RTMP ingress owner → ring | none per packet: the owner parses each message and runs the publisher's media (`RtmpPublisherMedia`: sequence-header cache, timestamp mapping, input gate, GOP cache, ring publish) inline; the control session receives only lifecycle commands and the one-time media probe (`RtmpControlCommand::MediaProbe`) | lifecycle per session | unchanged control channel, no media on it | **Fixed (WI11 step 2).** Interleaved release A/B vs 8d037400, RTMP ingest only, 2 reps, ~7.65 Mbit/s per pipeline in both: Restream CPU 26/41–48/72–73% → 17/25–31/53–54% at 16/32/64 publishers, Tokio 11/17–21/29–31% → 5/6–8/13–14% (API polling and control), RTMP owner thread 14/22–26/39–40% → 11/17–21/36–37% (the per-message channel send and cross-runtime wake cost more than the media work that replaced them). The 64 MiB media handoff semaphore is gone. |
| C2 | Direct RTMP play: Tokio control session ↔ Compio ingress owner | `PlayNext` request + oneshot reply carrying ≤ 32 `Arc<MediaPacket>` | one round trip per burst; bursts can be a single packet at the live edge | one outstanding burst per player | **Fixed; no scaling work.** Direct play from ingest is a debug/diagnostic path, not a production fan-out path. Requirements: it works with a real player (ffplay/ffmpeg), and an attached player does not interfere with the hot path (with/without-player A/B). The per-burst `info!` log is gone, the burst `Vec` is recycled, and client commands are now read during playback. |
| C3 | SRT publish: SRT ingress Owner → ring | none per packet: every received payload is demuxed, gated, timestamp-mapped, GOP-cached and published on the Owner thread (`ingress_media`); the Owner↔Tokio bridges carry session lifecycle only (connect/auth, the one-time stream probe, disconnect) | lifecycle events per session | unchanged bridges, no media on them | **Fixed (WI11 step 1).** Interleaved release A/B vs 763ba5d7, SRT ingest only, 2 reps: at 32 publishers Restream CPU 101–111% → 71–80%, Tokio 41–43% → 10–11% (API polling and control), SRT Owner thread 60–67% → 60–69%, bridge-full visits 1,458–1,540 → 0 per window, ingest ~7.8 Mbit/s per pipeline throughout. Owner media cost ~4–5 µs per payload. The 20–128 ms single-payload stalls (`mediaSlowPasses20ms`) are the VPS, not Restream: a per-phase timing build showed them landing in whatever phase was running (a no-op drain took 30 ms; the thread CPU clock jumped 40 ms across 1.5 µs of wall time), and an empty spin loop pinned to an idle Restream CPU lost 43 gaps over 5 ms in 30 s (worst 122 ms, ~3% of wall time) with zero reported steal: hypervisor descheduling of the vCPU. Also ruled out by interleaved A/B: allocator (glibc 2 or default arenas, mimalloc, jemalloc) and isolating the Owner thread on its own CPU. SRT latency and receive buffers absorb gaps of this size (ingest held ~7.9 Mbit/s). `scripts/harness/host-jitter.py` measures it and the capacity ramp records it per Restream CPU. Next: Owner sharding for publishers per core. |
| C4 | Ring → SRT egress: shared TS muxer | one task per unique (pipeline, preset) on the media executor pool (`restream-media`) reads the ring and writes `TsChunkRing`; shards read it | per packet, **once per feed** (not per output) | TS ring capacity | **Fixed (WI11 step 3b).** Muxing once per feed and sharing TS chunks keeps SRT mux cost O(feed). The media task waits for engine metadata, then packages bounded bursts and yields; it never runs on the control scheduler. |
| C5 | Ring → RTMP/RTMPS egress | fabric shards read `RingFeed` directly (atomic cursor); Raw→FLV converted once per shard (`egress_payload_cache`) | no per-packet crossing, only wakes (W1) | ring capacity, per-leaf cursors | **Justified** (already the cheap form). |
| C7 | Direct SRT play: SRT ingress Owner | the Owner pulls each reader's TS chunks and sends through the peer (window deferral and overload disconnect unchanged) | per TS chunk per player, on the Owner | per-peer deferral bound | **Fixed (WI11 step 1).** No Tokio in the path. Diagnostic only; `srt.policy` in the `direct-play` CI shard checks it plays (plain and encrypted reads decode cleanly). |
| C8 | Ring → FFmpeg stage input (and external FFmpeg output) | in-process stages: FFmpeg's AVIO read callback pulls the ring and encodes TS on the FFmpeg thread when its `MemoryQueue` runs dry (`MemoryQueue::set_refill` + `StageInputPump::into_queue_refill`); external stages: one dedicated thread pulls the ring and writes the child's stdin, one demuxes its stdout into the output ring | none: no producer task or queue hop | FFmpeg read cadence; pipe back-pressure (external) | **Fixed (WI11).** No Tokio in the stage data path. The Tokio stage task only waits for cancellation or the stage ending, then cleans up. Refilled batches are served straight from the pump's TS buffer, so the queue buffer copy is gone for in-process stages. |
| C6 | Ring ↔ FFmpeg transcoder | `MemoryQueue` (`Mutex` + `Condvar` byte queue) into FFmpeg's blocking AVIO callbacks on dedicated threads | per muxed chunk | queue capacity with blocked-write accounting | **Justified.** FFmpeg's AVIO API is blocking; dedicated threads keep it off Tokio and Compio (AGENTS media rule). Benchmarked by `avio_throughput` / `transcoder_throughput`. |
| C9 | Ring → HLS segmenter | TS/fMP4 muxing, keyframe detection, and segment accumulation run on media workers; CONTROL owns lifecycle registration and observes executor failure | per packet, once per pipeline / rendition | packet bursts bounded; accumulator grows until a segment boundary; store limits retained segments | **Fixed (WI11 step 3b).** Dropping CONTROL requests cancellation but lets media flush its final segment. Cleanup and failure reporting are lifecycle-generation qualified. HTTP serving and snapshots remain on CONTROL. |
| C10 | Ring → Recording feeder | `feed_recording` runs on the media executor pool (`restream-media`), pulling the ring and writing to `MemoryQueue` with a cooperative yield per burst; guarded OS thread owns disk writes | per packet, once per recording | AVIO queue capacity | **Fixed (WI11).** Continuous recording TS preparation offloaded from the Tokio control runtime. |
| C11 | External file ingest: child stdout → Ring | Child stdout pipe detached from the control reactor and pumped on the media executor pool (`restream-media`); `TsDemuxer`, continuous timestamps, input gate, and ring publish stay off Tokio | per stdout read burst | 64 KiB read buffer, ring capacity | **Fixed (WI11).** Continuous file demux and publication offloaded from the Tokio control runtime; timestamp continuity preserved across loop restarts. |

## Control and wake crossings

| # | Crossing | Mechanism | Rate | Verdict |
|---|---|---|---|---|
| W1 | Feed wake: ring publish → egress shards | the producing thread calls the ring's publication subscribers after each publish, seal and end of stream; each fabric subscribes one waker per feed that sets every shard's `WakeGate` and `try_send`s `FeedWake` only on its clear-to-set transition (`subscribe_fabric_wakes`). A replacement ring shares the subscriber set, and dropping the subscription unsubscribes | per publish: two `ArcSwap` loads plus one gate swap per shard; ≤ 1 queued wake per shard | **Fixed (WI11 step 4).** No Tokio task between publish and shard: the per-feed watcher task, its `Notify` round trip and its per-publish handle `Vec` clone are gone. The ring still knows nothing about egress (it calls an opaque `PublishWake`). Release A/B vs 7bccb734 (one ingest, 2 reps): RTMP×100 Restream CPU 44–49% → 37–38%, shards 23–28% → 18.5–19%, Tokio 11–12% → 9%. |
| W2 | Egress commands: Tokio → shard | bounded flume (`RESTREAM_EGRESS_COMMAND_CAPACITY`, 1024) | control rate (add/remove/update) | **Justified.** |
| W3 | Egress progress/quality: shard → Tokio/API | atomics for counters, `Mutex<PublisherQuality>` written once per second per leaf by the stall sweep | ≤ 1 Hz per leaf | **Justified.** |
| W4 | SRT egress Owner events → shard → Tokio | bounded event queues drained per ready batch | connection lifecycle rate | **Justified.** |
| W5 | Egress DNS resolution | resolver worker threads, bounded completion queue | per connect | **Justified** (blocking `getaddrinfo` off the owners). |

## Observability crossings

| # | Crossing | Finding | Verdict |
|---|---|---|---|
| O1 | API telemetry → host/process sampling (`/metrics/system`, status, resource map, agent context) | Each request built a fresh `sysinfo::System::new_all()` and `refresh_all()`: every process on the host, with command lines, environments and per-thread task lists, on a Tokio worker. The capacity harness requests `/metrics/system` on every sampling tick and the dashboard polls it. In the RTMP×100 fan-out profile `restream-tokio` was 48.9% of Restream samples; 39.6% was axum request handling and 24.5% `build_system_metrics_snapshot` (22.4% `sysinfo::refresh_procs`). Media work on Tokio was small: the SRT ingest loop ~2% and TS demux < 1%. The host CPU% it reported was also wrong: `global_cpu_usage()` read from a just-created `System` has no previous sample to diff against. | **Fixed.** One long-lived sampler (`system_sampling::sampled_system`) refreshes host CPU usage (a real delta between calls), memory, and only Restream plus its children (`/proc/self/task/*/children`). Symbol-resolved release profiles, SRT → 100 RTMP outputs: `build_system_metrics_snapshot` 24.5% → 1.2% of Restream samples (`sysinfo::refresh_procs` 22.4% → gone). Interleaved A/B (5 reps, release binaries): Restream CPU median 49.1% → 40.1% (mean 49.6 → 37.9), delivery 100/100 throughout. The next Tokio item is the health snapshot at 9.5% of samples, which the harness also polls every second. |
| O2 | API observation cost (health, telemetry) | Release profile RTMP×100 at 43b68dc0 (`symprof/now-rtmp100`, Restream ≈0.39 cores): API handling ~21% of Restream samples; health snapshot 7–10%, pipeline/engine telemetry ~5.5%, host sampling (sysinfo + `sample_host_settings`) ~2.5%. Inside the health snapshot, allocator calls are ~45% of its samples and `serde_json::Value` serialization ~36%; the per-output projection itself (`egress_runtime_json`) ~4%. The cost is building a `Value` tree per request and grows with output count; the capacity harness polls health and telemetry every second, so ramp CPU includes it. `sample_host_settings` rereads ~6 `/proc` and cgroup files per call (~1%). | **Measure → fix (accounting, not throughput).** The API only reads what shards and leaves already publish (atomics, small state-change mutexes, the 1 Hz quality snapshot); it sends no shard commands and never waits on a shard thread, so it does not interrupt egress. It costs Tokio CPU on Restream's cores (which inflates ramp CPU and competes only when a rung is CPU-bound) and can delay ingest media work that still runs on Tokio until WI11. Candidates: serialize typed structs instead of `Value` trees, cache host settings with a short TTL, and a lighter delivery endpoint for the harness. Quantify at 1000 outputs before choosing. |

## Open measurement items

- **M1** RTMP publish handoff. Measured (2026-09-27, release, ingest only,
  `ingest-growth-same` with `RESOURCE_SWEEP_INGEST_GROWTH_CONFIG=h264-rtmp`):
  64 RTMP publishers at 8 Mbit/s held 7.5 Mbit/s each; per publisher the RTMP
  ingress owner costs ~0.66% of a core and Tokio ~0.45% (API polling
  included). Open: permit/queue wait p50/p99 is not instrumented.
- **M2** (replaced) Direct play from ingest is a debug/diagnostic path. The
  player check runs in CI (`direct-play` shard: ffprobe structure plus an
  ffmpeg null-sink decode of SRT `read` and RTMP `play`); non-interference is
  a with/without-player A/B when WI11 changes these paths. No scaling study.
- **M3** SRT publish at N publishers. Measured (2026-09-27, release, ingest
  only, Restream on 3 CPUs; window-only counter deltas from the harness's
  `engine-health-*-start/end.json`): 8/16/24/32 publishers at 8 Mbit/s held
  ~7.9 Mbit/s each, Restream 34/50–66/66–90/85–112% CPU. The Owner→Tokio
  event bridge (256 shared) was full on a growing share of Owner passes:
  74–100, 220–970, 590–1,140 and 1,330–2,030 `eventBridgeFullVisits` per
  30 s window. Timing the `SrtServer::run` loop showed 1–5 passes per 5 s
  over 20 ms (worst 86–215 ms), mostly waiting rather than on CPU
  (`SrtServer::run` is ~0.7% of a core per publisher on CPU: TS demux 42%,
  forwarding 21%). Ruled out by interleaved A/B: harness API polling rate
  (1 s vs 5 s), running `SrtServer::run` on its own current-thread runtime
  (steady state unchanged; it only helps while pipelines are being created),
  and the malloc arena cap (2 vs glibc default). A 4096-slot bridge
  (whole-run counters, growth included) had ~0 bridge-full visits but a
  cumulative high-water of 4096, so it absorbs stalls without removing them;
  the stall cause remains open. The harness cannot drive more than ~40 SRT
  publishers on this host (libsrt publishers cost 6% of a harness core each).
- **M4** Ingress owners. The SRT ingress Owner thread (`srt-in`) costs ~1.8%
  of a core per 8 Mbit/s publisher (50–68% at 32), so one Owner saturates
  near ~55 publishers: the first SRT ingest ceiling, before `SrtServer::run`
  (~140 by its on-CPU cost). Moving demux off Tokio (WI11) does not lift it;
  more Owners on the port (SO_REUSEPORT) would. The RTMP ingress owner is
  ~0.66% per publisher (~150 per core).
