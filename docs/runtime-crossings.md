# Runtime Crossings Audit

Every place media or protocol work crosses between execution contexts
(Compio/io_uring owners, Tokio, FFmpeg threads), with its mechanism, rate,
bound, measured cost where available, and a verdict: **justified** (the
crossing buys ownership, isolation or backpressure worth its cost),
**fixed** (a cheaper form landed), or **measure** (plausibly material at
scale; the WI8 instrument that decides it is named). Updated as crossings
change; `docs/srt-compio-roadmap.md` WI8 owns the measurement programme.

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
| C1 | RTMP publish: Compio ingress owner → Tokio control session | decoded message `Bytes` (zero-copy) over a per-connection `mpsc` (16), gated by a shared 64 MiB byte semaphore | one channel op (and possible cross-runtime wake) per audio/video message, ~77/s per publisher | 16 messages per connection + 64 MiB shared | **Justified.** Parsing and socket I/O stay with the transport owner; ring publish, timestamp mapping, input gate and GOP cache are application state. Messages larger than a read rarely batch (a 4 KiB read seldom completes more than one message), so batching buys little. Cost not yet measured; see M1. |
| C2 | Direct RTMP play: Tokio control session ↔ Compio ingress owner | `PlayNext` request + oneshot reply carrying ≤ 32 `Arc<MediaPacket>` | one round trip per burst; bursts can be a single packet at the live edge | one outstanding burst per player | **Fixed + measure.** The per-burst `info!` log is gone, the burst `Vec` is recycled, and client commands are now read during playback. Whether to move the play `Reader` onto the owner is M2. |
| C3 | SRT publish: SRT ingress Owner → Tokio | one `SrtIngressEvent::Media` (1316-byte TS `Bytes`) per datagram over one shared `mpsc` (256) | ~760/s per publisher | 256 events shared by all SRT publishers | **Justified at current scale; measure at N publishers (M3).** ~10x C1's rate, and a single Tokio task (`SrtServer::run`) TS-demuxes and publishes for every SRT publisher and also drives SRT play readers. Candidate if M3 shows it: batch one `Vec<Bytes>` per Owner service pass per peer, and/or shard demux per publisher. |
| C4 | Ring → SRT egress: shared TS muxer | one Tokio task per pipeline stage reads the ring and writes `TsChunkRing`; shards read it | per packet, **once per feed** (not per output) | TS ring capacity | **Justified.** Muxing once per feed and sharing TS chunks is what makes SRT fan-out O(feed) in mux cost. |
| C5 | Ring → RTMP/RTMPS egress | fabric shards read `RingFeed` directly (atomic cursor); Raw→FLV converted once per shard (`egress_payload_cache`) | no per-packet crossing, only wakes (W1) | ring capacity, per-leaf cursors | **Justified** (already the cheap form). |
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

## Open measurement items

- **M1** RTMP publish handoff: permit/queue wait p50/p99, handoffs/s, Tokio
  CPU per publisher (direct-play/publish scaling mode, WI8).
- **M2** Direct RTMP play: `PlayNext` latency p50/p99, packets per burst,
  ingress-owner busy time with publishers and players mixed. Only then
  compare moving the play `Reader` onto the Compio owner.
- **M3** SRT publish at N publishers: `SrtServer::run` task CPU and event
  queue depth, owner→Tokio event rate; decides batching or per-publisher
  demux sharding.
- **M4** Ingress owners: single RTMP ingress owner and single SRT ingress
  Owner busy time versus publisher count (roadmap §36 WI8 question).
