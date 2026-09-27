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
Tokio            control plane: API, DB, pipeline state, ring publish,
                 RTMP control sessions, SRT ingest demux, SRT/TS muxing
Compio/io_uring  RTMP ingress owner (one), SRT ingress Owner (one),
                 egress fabric shards (RTMP/RTMPS/SRT, per feed)
FFmpeg threads   transcoders (blocking AVIO callbacks)
```

## Contents

- [Media crossings](#media-crossings)
- [Control and wake crossings](#control-and-wake-crossings)
- [Observability crossings](#observability-crossings)
- [Open measurement items](#open-measurement-items)

## Media crossings

| # | Crossing | Mechanism | Rate (8 Mbit/s stream) | Bound | Verdict |
|---|---|---|---|---|---|
| C1 | RTMP publish: Compio ingress owner → Tokio control session | decoded message `Bytes` (zero-copy) over a per-connection `mpsc` (16), gated by a shared 64 MiB byte semaphore | one channel op (and possible cross-runtime wake) per audio/video message, ~77/s per publisher | 16 messages per connection + 64 MiB shared | **Interim (WI11).** Parsing and socket I/O correctly stay with the transport owner. Ring publish, timestamp mapping, input gate and GOP cache are per-packet media work that sits on Tokio only because the control session owns `RtmpIngestHandle`; `StandbyGopCache` and `InputTimestampMapper` are synchronous and `InputPacketGate` is atomics, so none needs Tokio. Messages larger than a read rarely batch (a 4 KiB read seldom completes more than one message), so batching buys little. Cost not yet measured; see M1. |
| C2 | Direct RTMP play: Tokio control session ↔ Compio ingress owner | `PlayNext` request + oneshot reply carrying ≤ 32 `Arc<MediaPacket>` | one round trip per burst; bursts can be a single packet at the live edge | one outstanding burst per player | **Fixed; no scaling work.** Direct play from ingest is a debug/diagnostic path, not a production fan-out path. Requirements: it works with a real player (ffplay/ffmpeg), and an attached player does not interfere with the hot path (with/without-player A/B). The per-burst `info!` log is gone, the burst `Vec` is recycled, and client commands are now read during playback. |
| C3 | SRT publish: SRT ingress Owner → Tokio | one `SrtIngressEvent::Media` (1316-byte TS `Bytes`) per datagram over one shared `mpsc` (256) | ~760/s per publisher | 256 events shared by all SRT publishers | **Interim (WI11); the most significant crossing.** ~10x C1's rate, and a single Tokio task (`SrtServer::run`) TS-demuxes and publishes for every SRT publisher and also drives SRT play readers. One task runs on one worker at a time, so it is a process-wide serial ceiling. Release profile at one 8 Mbit/s publisher (`symprof/now-rtmp100`, 43b68dc0): `SrtServer::run` 2.6% inclusive and TS demux 0.6% of Restream samples, ≈1% of a core per publisher, so the task would saturate near ~100 publishers (extrapolated; M3 confirms). Target: sharded media workers own demux and publication; the Owner hands off one batch per peer per service pass. |
| C4 | Ring → SRT egress: shared TS muxer | one Tokio task per pipeline stage reads the ring and writes `TsChunkRing`; shards read it | per packet, **once per feed** (not per output) | TS ring capacity | **Justified sharing, interim executor (WI11).** Muxing once per feed and sharing TS chunks is what makes SRT fan-out O(feed) in mux cost; keep that. The muxer is still a `tokio::spawn` per feed doing per-packet conversion and TS packaging, which at hundreds of feeds is continuous Tokio load: it moves to the media executors (one logical muxer per stage, not one thread each); metadata acquisition can stay on Tokio. |
| C5 | Ring → RTMP/RTMPS egress | fabric shards read `RingFeed` directly (atomic cursor); Raw→FLV converted once per shard (`egress_payload_cache`) | no per-packet crossing, only wakes (W1) | ring capacity, per-leaf cursors | **Justified** (already the cheap form). |
| C7 | Direct SRT play: Tokio → SRT ingress Owner | `SrtServer::run` advances each player's `TsChunkReader` and issues bounded send commands by `LogicalPeerId` | per TS chunk per player | bounded Owner command queue | **Accepted (diagnostic path).** Direct play from ingest is debug/diagnostic only. It shares C3's serial task with every SRT publisher, so the requirement is non-interference: an attached player must not change ingest or output delivery (with/without-player A/B), and it must keep working when SRT ingest moves off that task. |
| C8 | Ring → FFmpeg stage input | `StageInputPump::pump_to` (async, on Tokio) reads the ring and prepares TS for the transcoder's `MemoryQueue`/stdin | per packet per transcoded stage | queue capacity | **Interim (WI11).** The FFmpeg side is correctly on dedicated threads (C6); the pump that feeds it is per-packet work on Tokio. |
| C6 | Ring ↔ FFmpeg transcoder | `MemoryQueue` (`Mutex` + `Condvar` byte queue) into FFmpeg's blocking AVIO callbacks on dedicated threads | per muxed chunk | queue capacity with blocked-write accounting | **Justified.** FFmpeg's AVIO API is blocking; dedicated threads keep it off Tokio and Compio (AGENTS media rule). Benchmarked by `avio_throughput` / `transcoder_throughput`. |

## Control and wake crossings

| # | Crossing | Mechanism | Rate | Verdict |
|---|---|---|---|---|
| W1 | Feed wake: ring publish → egress shards | publish signals a ring `Notify`; a per-feed Tokio watcher task wakes and `try_send`s `FeedWake` to each shard, coalesced by a `WakeGate` (at most one in flight per shard) | one watcher wake per publish per feed; ≤ 1 queued wake per shard | **Justified.** Keeps the ring free of egress knowledge for one Tokio wake per publish per feed (scales with feeds, not outputs). The watcher clones its handle `Vec` on every publish, but its whole task is below 0.05% of Restream samples at RTMP×100 (release profile, `symprof/pre-o1-rtmp100`), so the clone is left alone. |
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

- **M1** RTMP publish handoff: permit/queue wait p50/p99, handoffs/s, Tokio
  CPU per publisher (publish scaling mode, WI8).
- **M2** (replaced) Direct play from ingest is a debug/diagnostic path. The
  player check runs in CI (`direct-play` shard: ffprobe structure plus an
  ffmpeg null-sink decode of SRT `read` and RTMP `play`); non-interference is
  a with/without-player A/B when WI11 changes these paths. No scaling study.
- **M3** SRT publish at N publishers: `SrtServer::run` task CPU and event
  queue depth, owner→Tokio event rate; decides batching or per-publisher
  demux sharding.
- **M4** Ingress owners: single RTMP ingress owner and single SRT ingress
  Owner busy time versus publisher count (roadmap §36 WI8 question).
