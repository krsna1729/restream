# Configuration Reference

The Rust runtime has a small environment configuration surface for deployment
paths, listener ports, and operational tuning. User-facing settings are stored
in SQLite.

## Contents

- [Fixed Runtime Values and Environment Variables](#fixed-runtime-values-and-environment-variables)
- [SQLite-Backed Settings](#sqlite-backed-settings)
- [Linux Service Placement](#linux-service-placement)
- [SQLite Performance Settings](#sqlite-performance-settings)
- [Ingest URLs](#ingest-urls)
- [Output Configuration](#output-configuration)
- [File Ingest Configuration](#file-ingest-configuration)
- [SRT Socket Policy](#srt-socket-policy)
- [HLS Pull and Authorization](#hls-pull-and-authorization)

## Fixed Runtime Values and Environment Variables

| Value | Default setting | Environment Variable Override |
|---|---|---|
| Dashboard/API listener | `127.0.0.1:3030` | `RESTREAM_HTTP_BIND_ADDR`, `RESTREAM_HTTP_PORT` |
| RTMP listener | `0.0.0.0:1935` | `RESTREAM_RTMP_PORT` |
| SRT listener | `0.0.0.0:10080` | `RESTREAM_SRT_PORT` |
| SRT protocol backend | `libsrt` | `RESTREAM_SRT_BACKEND=rust` or `srt-rust` selects the pure-Rust SRT Core for non-bonded publish ingest and SRT egress; unset or any other value keeps the complete libsrt path |
| Rust SRT ingest workers | Derived from available parallelism, clamped to `1..=64` | `RESTREAM_SRT_INGEST_WORKERS` (one `SO_REUSEPORT` UDP socket and one OS worker per selected worker; in `connected` scaling this is the number of connected-socket workers behind one public listener) |
| Rust SRT ingest scaling | One `SO_REUSEPORT` socket per worker | `RESTREAM_SRT_INGEST_SCALING=connected` selects one public listener that completes handshake admission, then hands each tuple to a worker-owned connected UDP socket; `RESTREAM_SRT_INGEST_ROUTING=least-tuples` is the default and `round-robin` is available for first-owner selection |
| Tokio scheduler workers | Derived from the effective CPU mask/quota | `RESTREAM_TOKIO_WORKER_THREADS` |
| Tokio blocking-thread ceiling | `512` | `RESTREAM_TOKIO_MAX_BLOCKING_THREADS` |
| Transcoder backend | External FFmpeg subprocess | `RESTREAM_INTERNAL_VIDEO_PRESETS`, `RESTREAM_INTERNAL_HEVC_TO_H264`, `RESTREAM_INTERNAL_HLS_PREVIEW`, and `RESTREAM_INTERNAL_AUDIO_COMPLEX` (`1`/`true`/`yes`/`on` enable each in-process stage family independently) |
| File-ingest backend | External embedded FFmpeg subprocess | `RESTREAM_USE_INTERNAL_FILE_INGEST` (`1`/`true`/`yes`/`on` to enable in-process remux + demux for passthrough file ingest) |
| External transcoder and file-ingest executable | Embedded `public/bin/ffmpeg`, extracted to `.restream/runtime/ffmpeg/` at startup | `FFMPEG_BIN_PATH` |
| External FFmpeg codec threads | FFmpeg-selected | `RESTREAM_EXTERNAL_FFMPEG_THREADS` |
| Recording remux FFmpeg threads | FFmpeg-selected | `RESTREAM_RECORDING_FFMPEG_THREADS` |
| Concurrent external FFmpeg stages | Derived from available CPUs | `RESTREAM_EXTERNAL_FFMPEG_PERMITS`; derivation can be tuned with `RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE`, `RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD`, and `RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN` |
| SQLite database | `.restream/data/restream.db` (with WAL/SHM sidecars) | `RESTREAM_DB_PATH` |
| Media directory | `.restream/media/` | `RESTREAM_MEDIA_DIR` |
| Text file log directory | `.restream/logs/` | `RESTREAM_LOG_DIR` |
| Media packet ring depth (source/ingest) | `1024` packets | `RESTREAM_RING_CAPACITY` |
| Media packet ring depth (transcoder output) | `512` packets | `RESTREAM_TRANSCODER_RING_CAPACITY` (720p30 output ≈ 80 pkt/s → 512 slots ≈ 6.4 s jitter headroom; lower than source ring because I-frame payloads are large) |
| Shared SRT TS ring depth | `256` chunks | `RESTREAM_TS_RING_CAPACITY` (SRT protocol's own send buffer absorbs network jitter; this ring only bridges muxer → socket write, typically sub-millisecond) |
| Egress fabric shard count | Derived from the effective CPU count (clamped `2..=8`); RTMP/sink/pipeline feeds then scale down live to match output count (128 outputs per shard), while SRT feeds always keep the CPU-derived ceiling (SRT shard count is a libsrt-multiplexer parallelism budget — see `docs/egress-implementation.md`'s "Dynamic shard scaling" section) | `RESTREAM_EGRESS_SHARDS` (clamped to `1..=1024`; overrides the initial shard count every feed starts with) |
| Egress fabric command capacity | `1024` commands per shard | `RESTREAM_EGRESS_COMMAND_CAPACITY` |
| Egress fabric command batch | `32` commands per loop | `RESTREAM_EGRESS_COMMAND_BATCH` |
| Egress fabric readiness batch | `64` ready leaves per loop | `RESTREAM_EGRESS_READY_BATCH` |
| Egress fabric timer batch | `64` timers per loop | `RESTREAM_EGRESS_TIMER_BATCH` |
| Egress fabric idle wait | `1` ms | `RESTREAM_EGRESS_IDLE_WAIT_MS` |
| SRT fabric poll events | `1024` events per shard poller | `RESTREAM_EGRESS_SRT_POLLER_MAX_EVENTS` |
| Egress fabric visit units | `32` units per visit | `RESTREAM_EGRESS_VISIT_MAX_UNITS` |
| Egress fabric visit bytes | `262144` bytes per visit | `RESTREAM_EGRESS_VISIT_MAX_BYTES` |
| Egress fabric visit time | `2000` µs per visit | `RESTREAM_EGRESS_VISIT_MAX_US` |
| Egress pending write limit | `262144` bytes per output | `RESTREAM_EGRESS_MAX_PENDING_BYTES` (application-owned protocol bytes; distinct from `RESTREAM_RTMP_STREAM_BUFFER_BYTES`, which configures the TCP socket buffers) |
| Egress fabric drain timeout | `3000` ms | `RESTREAM_EGRESS_DRAIN_TIMEOUT_MS` (clamped `1..=60000`; on shutdown, how long a shard keeps running to let leaves with queued bytes flush before force-closing — currently drives real per-leaf draining for RTMP and SRT, see `docs/egress-implementation.md` Phase 6) |

`EgressFabricConfig::validate` runs once at startup after per-field clamping and logs non-fatal `restream.config.warning` events for cross-field issues among the egress fabric settings above — e.g. `RESTREAM_EGRESS_MAX_PENDING_BYTES` smaller than `RESTREAM_EGRESS_VISIT_MAX_BYTES`, `RESTREAM_EGRESS_SHARDS` more than 4x the effective CPU count, `RESTREAM_EGRESS_DRAIN_TIMEOUT_MS` under 50ms, or `RESTREAM_EGRESS_COMMAND_BATCH` exceeding `RESTREAM_EGRESS_COMMAND_CAPACITY`. See `docs/egress-implementation.md` Phase 6.
| SRT egress muxer max outputs per shard | `0` | `RESTREAM_SRT_EGRESS_MUXER_MAX_OUTPUTS_PER_SHARD` (disabled at `0`; when set, SRT egress creates a new shared TS muxer shard as each pipeline+encoding cohort crosses this many outputs) |
| SRT egress muxer max shards | `64` | `RESTREAM_SRT_EGRESS_MUXER_MAX_SHARDS` (hard guardrail for dynamic SRT muxer sharding; once reached, new outputs are assigned to the least-loaded existing shard and a warning is emitted) |
| SRT egress local-port reuse | Enabled | `RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT` (`0`/`false` disables reuse; when enabled the reused local UDP port is scoped per egress-fabric shard, so libsrt creates one multiplexer — and one `SndQ`/`RcvQ` worker thread pair — per shard rather than one for the whole process. Unrelated to `RESTREAM_SRT_EGRESS_MUXER_MAX_OUTPUTS_PER_SHARD`, which shards the shared TS muxer stage) |
| SRT egress local-port reuse pipeline scoping | Enabled | `RESTREAM_SRT_EGRESS_MUXER_PORT_PIPELINE_SCOPED` (`0`/`false` reverts to the pre-2026-08-14 engine-wide-shared behavior, where every pipeline's shard *N* shares one libsrt multiplexer; enabled by default so two unrelated pipelines' shard *N* never share a `CSndQueue` worker thread purely because their shard-assignment formulas produced the same numeric shard id. Multiplexer count scales with `shard_count x active_pipeline_count` when enabled, a flat `shard_count` when disabled) |
| SRT egress connect timeout | `10000` ms | `RESTREAM_SRT_CONNECT_TIMEOUT_MS` (raised from a 3s default: a live scale run showed a burst of 600+ simultaneous handshakes to one peer still completing the SRT handshake when the old 3s timeout tore the socket down first, surfacing as `SRT_ENOCONN` on the next send — see `docs/agent-guidance/quality/srt-egress-scale-investigation-2026-08-10.md`) |
| SRT egress connect concurrency | `64` | `RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY` (clamped to `1..=4096`; engine-wide bound on concurrent in-flight SRT egress handshakes, held per leaf from connect initiation until its first poller visit resolves the handshake — decouples connection-*establishment* concurrency from shard count, since SRT's `connect()` is non-blocking-initiate and returns before the handshake completes) |
| Require libsrt bonding support | Disabled | `RESTREAM_REQUIRE_SRT_BONDING` (presence makes unavailable bonding support a startup/test prerequisite) |
| SRT encryption | Disabled | `RESTREAM_SRT_PASSPHRASE`; `RESTREAM_SRT_PBKEYLEN` selects the key length and defaults to `16` |
| AVIO queue capacity (async↔OS-thread bridge) | `524288` bytes (512 KiB) | `RESTREAM_AVIO_QUEUE_CAPACITY` (measured peak HWM = 398 KiB at 8 Mb/s RTMP with zero blocked writes; raise only for very high-latency SRT links) |
| File descriptor limit | `65536` | `RESTREAM_NOFILE_LIMIT` |
| Output reconciliation interval | 1 second | `RESTREAM_RECONCILE_INTERVAL_MS` |
| Failed-output max retries | `10` | `RESTREAM_OUTPUT_MAX_RETRIES` |
| Failed-output restart base backoff | 5 seconds | `RESTREAM_OUTPUT_RETRY_BASE_MS` |
| Failed-output restart max backoff | 300 seconds | `RESTREAM_OUTPUT_RETRY_MAX_MS` |
| Ingest disconnect grace period | 5000 ms | `RESTREAM_INGEST_DISCONNECT_GRACE_MS` |
| Idle HLS segmenter timeout | 60 seconds | `RESTREAM_HLS_IDLE_TIMEOUT_MS` |
| SQLite log-history retention | 7 days | `RESTREAM_LOG_RETENTION_DAYS` |
| Secure-only session cookies | Disabled | `RESTREAM_SECURE_SESSION_COOKIES` (`1`/`true` enables the `Secure` cookie attribute for HTTPS deployments) |
| RTMP accept backlog | `1024` | `RESTREAM_RTMP_LISTENER_BACKLOG` |
| RTMP concurrent connection cap | `512` | `RESTREAM_RTMP_MAX_CONNECTIONS` |
| RTMP handshake timeout | `10000` ms | `RESTREAM_RTMP_HANDSHAKE_TIMEOUT_MS` |
| RTMP pre-auth socket buffers | `131072` bytes | `RESTREAM_RTMP_PREAUTH_BUFFER_BYTES` |
| RTMP streaming socket buffers | `8388608` bytes | `RESTREAM_RTMP_STREAM_BUFFER_BYTES` |
| RTMP egress chunk size | `16384` bytes | `RESTREAM_RTMP_EGRESS_CHUNK_SIZE` (sent with the RTMP `SetChunkSize` message; 16 KiB was the best measured loopback fanout point in the RTMP-only MSR chunk-size sweep) |
| HLS minimum segment length | 1 second | `RESTREAM_HLS_MIN_SEGMENT_MS` |
| HLS live window length | 20 segments | `RESTREAM_HLS_MAX_SEGMENTS` |
| HLS segment accumulator capacity | 8 MiB | `RESTREAM_HLS_SEGMENT_CAPACITY_BYTES` |

`FFMPEG_BIN_PATH` overrides the shared subprocess FFmpeg path used by the
external transcoder, the default file-ingest backend, and post-recording
`.ts` → `.mp4` remux. The recording remux path requires that binary to expose
the `mov/mp4` muxer.

The default working-directory layout is deliberately small and hidden under
`.restream/`: `data/` owns only SQLite state and its sidecars, `media/` owns
uploads and recordings, `logs/` owns rotated file logs, and `runtime/` is an
internal disposable executable cache. Database and media paths can be
overridden independently for host-service conventions.

## SQLite-Backed Settings

`GET /api/v1/settings` returns the current values. `PATCH /api/v1/settings`
updates any supplied field.

```json
{
  "serverName": "Name",
  "ingestHost": "stream.example.com",
  "ingestSecurity": {
    "failureLimit": 10,
    "failureWindowMs": 60000,
    "banMs": 600000,
    "trackedIpLimit": 10000
  },
  "recordingSettings": {
    "retainSourceTs": false
  }
}
```

| Setting | Behavior |
|---|---|
| `serverName` | Dashboard display name; must be non-empty |
| `ingestHost` | Hostname used when generating RTMP/SRT publisher URLs; blank falls back to `localhost` |
| `ingestSecurity` | In-memory failed-publish tracking and temporary IP bans; changes are persisted |
| `recordingSettings.retainSourceTs` | Deployment-wide recording retention policy. Default `false`: after a successful `.mp4` remux, the source recording `.ts` is deleted. Failed remuxes always keep the source `.ts`. |
| Dashboard password | Scrypt hash stored in SQLite. On first startup, `RESTREAM_INITIAL_ADMIN_PASSWORD` is used when set; otherwise a high-entropy password is generated and written next to the SQLite database as `restream-initial-admin-password.txt` with owner-only permissions. |
| Custom output video mode | Stored through `/api/v1/encodings/custom` for future use; not offered as an output video mode and rejected by output create/update |
| Recording enabled | Stored per pipeline as `recording_enabled:<pipelineId>` |

Sessions are persisted in SQLite and reloaded at startup. Expired sessions are
pruned during initialization and then once per hour while the server is running
(reconciler tick 3600).

The dashboard/API HTTP listener binds to `127.0.0.1` by default. Override that
with `RESTREAM_HTTP_BIND_ADDR` only when another component, such as a reverse
proxy or tunnel, is expected to reach the service on a different interface.

## Linux Service Placement

For Linux hosts managed by systemd, prefer systemd for coarse process-level CPU
and NUMA placement. The helper below installs a `restream.service` unit and can
add `CPUAffinity`, `NUMAPolicy`, and `NUMAMask` when those are known-good for
the host:

```sh
sudo RESTREAM_CPU_AFFINITY=0-5 \
  RESTREAM_NUMA_POLICY=local \
  scripts/deploy/install-systemd-service.sh
```

Use systemd placement only after validating the CPU/NUMA set on the deployment
host. Process/cgroup-level CPU placement (systemd `CPUAffinity`, Docker
`--cpuset-cpus`, or a Kubernetes CPU manager policy) is the supported mechanism
for CPU partitioning: the kernel enforces it over the whole process lifetime,
including threads the runtime spawns later, and it is container-aware by
construction. The runtime deliberately does not pin individual thread families
itself. An in-process affinity scanner was prototyped and rejected — it did not
reproduce the external-partition win even with masks proven applied, because it
cannot hold a partition against the runtime's continuous thread turnover the way
a process-level cpuset does (see
`docs/agent-guidance/quality/baselines.md` § Q-012 decision).

The runtime also exposes its resolved Tokio sizing in `/api/v1/engine/health`
and the engineer telemetry host-settings table. `RESTREAM_TOKIO_WORKER_THREADS`
controls async scheduler workers; `RESTREAM_TOKIO_MAX_BLOCKING_THREADS` controls
Tokio `spawn_blocking` capacity for blocking handshakes and waiters. Those knobs
do not cap native helper threads created by FFmpeg, SQLite, or libsrt. Restream
names its Tokio runtime threads `restream-tokio` so process tools can separate
them from `SRT:*`, `sqlx-sqlite-*`, and other native helper threads; that label
covers Tokio scheduler, blocking, and replacement worker threads.

## SQLite Performance Settings

The following PRAGMAs are applied at startup after WAL mode is enabled:

| PRAGMA | Value | Effect |
|---|---|---|
| `synchronous` | `NORMAL` | fsync only at WAL checkpoints; safe with WAL |
| `busy_timeout` | 5000 ms | Retry on locked database before returning SQLITE_BUSY |
| `journal_size_limit` | 64 MiB | Caps WAL file growth; excess is reclaimed at checkpoint |
| `cache_size` | -16384 (16 MiB) | Page cache kept in process memory |
| `temp_store` | `MEMORY` | Temporary tables and indices use memory, not disk |
| `mmap_size` | 128 MiB | Read pages via memory-mapped I/O on supported platforms |

## Ingest URLs

Generated publisher URLs use the configured ingest host and fixed native ports:

```text
rtmp://<ingestHost>:1935/live/<streamKey>
srt://<ingestHost>:10080?streamid=publish:<streamKey>
```

Pipelines may supply an explicit stream key, or omit it and let the API generate
a high-entropy key. `GET /api/v1/stream-keys` returns only keys already assigned
to configured pipelines; it does not enumerate unused credentials.

## Output Configuration

Each output stores:

```json
{
  "name": "Primary CDN",
  "url": "rtmp://destination.example/live/key",
  "config": {
    "video": { "mode": "source" },
    "audio": { "mode": "all" },
    "protocol": { "type": "rtmp", "mode": "legacy" }
  }
}
```

Supported routing behavior:

| URL | Runtime behavior |
|---|---|
| `rtmp://...` | Native RTMP egress; IPv6 addresses in bracket notation (`[::1]`) are supported |
| `rtmps://...` | Native RTMPS egress through the RTMP path with TLS before handshake |
| `srt://...` | Native SRT MPEG-TS egress; percent-encoded characters in the `streamid` query parameter are decoded automatically |
| `hls://...` | Starts the pipeline's local in-memory HLS segmenter |
| `sink://...` | Discards media through the egress fabric for diagnostics, soak tests, and capacity measurement |
| `pipeline://...` | In-process pipeline recirculation; candidate topology and target input are validated before backend ownership starts |
| `http://...`, `https://...` | Starts the local MPEG-TS segmenter and uploads segments/playlist with HTTP PUT |

Any other prefix is rejected during validation. Pipeline recirculation URLs are
recognized and checked for cycles and target-input ownership before runtime
backend ownership starts. The served preview HLS path is
fragmented MP4 (`init.mp4` + `.m4s`), but HTTP/HTTPS HLS upload intentionally
stays on MPEG-TS for ingest compatibility. For HTTP/HTTPS HLS upload,
segment upload URLs are derived from the playlist target: a `file=` query
parameter is replaced with `seg<N>.ts`, otherwise the playlist path filename is
replaced with the segment filename.

Output config describes video and audio separately:

```json
{
  "video": { "mode": "preset", "preset": "720p" },
  "audio": { "mode": "selectTracks", "tracks": [0] }
}
```

Built-in video profiles are `source`, `720p`, `1080p`, and the internal `h264`
conversion profile. `source` is passthrough and bypasses the video transcoder.
For non-source built-in video profiles, the default backend is an external
FFmpeg subprocess that performs decode/scale/encode. Set
`RESTREAM_INTERNAL_VIDEO_PRESETS=1` to opt those video-preset stages into the
in-process backend; audio streams are copied. HEVC-to-H.264 bridge stages,
HLS preview transcode stages, and complex audio stages are controlled
separately by `RESTREAM_INTERNAL_HEVC_TO_H264`,
`RESTREAM_INTERNAL_HLS_PREVIEW`, and `RESTREAM_INTERNAL_AUDIO_COMPLEX`.
These environment variables are startup defaults. Operators can override the
same four backend-family choices from Admin -> Backend or by patching
`backendPolicy` through `/api/v1/settings`; persisted settings take precedence
on restart and apply to newly started or reconciled stages.
`custom` remains stored configuration only. It is rejected by output create/update
so operators do not accidentally select a passthrough path that looks like custom
FFmpeg execution.

RTMP and RTMPS outputs also accept
`protocol: { "type": "rtmp", "mode": "legacy" | "enhanced" }` inside
`config`. Omitting protocol settings keeps legacy behavior. Enhanced RTMP
advertises `avc1`, `hvc1`, and `mp4a` capabilities during connect; H.264
outputs keep normal AVC payloads, while HEVC outputs use the Enhanced FLV
`hvc1` packet format. With HEVC ingest, legacy RTMP adds the shared
`hevc_to_h264` conversion edge before publish and Enhanced RTMP skips it.

Typed audio routing accepts `all`, `selectTracks`, `remap`, and `downmix`.
Track selection stays on the packet-only selector path; channel-level `remap`
and `downmix` routes run through an external FFmpeg audio stage and re-encode
stereo AAC.

## File Ingest Configuration

File ingest is configured per pipeline or through the standalone ingest routes.
Each definition stores:

- `filename`
- `loop`
- `startTime`
- `liveOptimized`
- `targetGopSeconds`

`liveOptimized=false` keeps the default passthrough path. The application
service owns the exact subprocess arguments; this reference documents the
user-visible settings and resulting behavior.

`liveOptimized=true` forces the subprocess backend even when
`RESTREAM_USE_INTERNAL_FILE_INGEST=1`. In that mode the embedded FFmpeg binary
re-encodes video to H.264, audio to AAC, disables scene-cut GOP drift, and
forces keyframes at the configured `targetGopSeconds` cadence for steadier HLS
preview and recording from sparse-GOP source files.

## SRT Socket Policy

Both SRT play (subscriber) and SRT egress connections wait up to 200 ms per
poll for the ingest probe to complete before creating the MPEG-TS muxer.
If no video metadata is available the server polls every 200 ms; if the ingest
disappears during the wait the connection is closed gracefully.

The runtime calls its high-bitrate helper for the SRT listener and single-link
egress sockets:

- 250 ms latency (default; see "SRT ingest latency" below for the
  global/per-pipeline override)
- 256-packet loss/reorder tolerance
- 8 MiB UDP send/receive buffers
- 12 MiB SRT send/receive buffers (default; scales up for a higher
  configured latency — see below)
- 32768-packet flow-control window (default; scales with the buffer above)
- unlimited automatic maximum bandwidth

The code does not explicitly apply the UDP-buffer/loss-tolerance/maxbw
portion of the helper to accepted sockets or bonded egress groups. Do not
assume those sockets have every requested value without runtime
verification. Latency/RCVBUF/FC, in contrast, are explicitly re-applied to
every accepted socket in the accept-hook (see below) — those three are not
just the listener's inherited default.

### SRT ingest latency

Every ingest connection's `SRTO_RCVLATENCY` — and, derived from it, its
`SRTO_RCVBUF`/`SRTO_FC` — is resolved from `SrtGlobalIngestConfig::latencyMs`
(global default, 250 ms) or a per-pipeline
`SrtPipelineIngestConfig::latencyMs` override, the same inherit/override
shape already used for SRT ingest encryption. Configurable via
`PATCH /api/v1/settings` (`srtIngest.latencyMs`) for the global default, or
per pipeline through its `srtIngestPolicy.latencyMs` field — both also have
dashboard fields (Settings → Global SRT Ingest; the pipeline editor's SRT
Ingest Policy section). Valid range: `20–8000` ms, the SRT wire protocol's
own documented range for the negotiated TSBPD delay field
(`docs/features/handshake.md`'s `TsbPdDelay`/`RcvTsbPdDelay`/`SndTsbPdDelay`
in the vendored libsrt source).

`RCVBUF`/`FC` scale with the resolved latency using the same formula
egress's `SNDBUF` ceiling uses (worst-case assumed bitrate × latency ×
margin), floored at the historical flat 12 MiB/32768-packet preset so the
default-latency case is unchanged. This can only ever be sized from the
value configured here, never the value actually negotiated with the caller
(`max(this value, the caller's own PEERLATENCY)`) — `SRTO_RCVBUF` is a
PREBIND option, locked before libsrt processes the caller's proposed
latency at all (confirmed directly against the vendored libsrt source:
`acceptAndRespond` in `srtcore/core.cpp` calls `interpretSrtHandshake`,
which negotiates latency, before `prepareBuffers`, which allocates the
receive buffer — but `SRTO_RCVBUF` was already locked well before either
call, in the accept-hook). A caller who proposes a higher latency than
configured here can still push the negotiated result above what the
buffer was sized for; nothing on either end can close that gap, since
libsrt does not validate the peer's proposed latency at all.

Linux startup checks warn when `net.core.rmem_max` or `net.core.wmem_max` cannot
support the requested UDP buffers. The listener's `/proc/net/udp` receive queue
and drop count are exported in `/api/v1/engine/health`.

For a fresh Linux host, both `scripts/dev/bootstrap.sh` and
`scripts/dev/bootstrap-runtime.sh` report whether private user/network
namespaces and the required SRT UDP buffer ceilings are available. To persist
the known-good live-harness values deliberately from either bootstrap path, run:

```sh
scripts/dev/bootstrap.sh --configure-harness-host
# or, for a runtime-only host:
scripts/dev/bootstrap-runtime.sh --configure-harness-host
```

This writes `kernel.unprivileged_userns_clone=1`,
`user.max_user_namespaces=28633`, `net.core.rmem_max=26214400`, and
`net.core.wmem_max=8388608` to `/etc/sysctl.d/99-restream-harness.conf`.
Both bootstrappers delegate to `scripts/dev/harness-host-prereqs.sh`, so the
sysctl policy cannot drift. They do not disable AppArmor or other host security
policy; use `--no-netns` as a temporary fallback when the host administrator
has not approved unprivileged namespaces.

SRT egress backup links can be supplied with:

```text
srt://primary.example:10080?streamid=publish:key&bond=backup1.example:10080,backup2.example:10080
```

This code path is unit-tested for URL parsing and socket-option constants, but
still needs live multi-link interoperability validation.

### Recognized SRT egress URL parameters

Only these query parameters are read from an `srt://` output URL. Anything
else (including `mss`, `oheadbw`, `tlpktdrop`, `nakreport`, and other names
used by ffmpeg or libsrt's own tools) is **silently ignored** — it is not an
error, it simply has no effect:

| Parameter | Purpose | Default when omitted | Clamped range |
|---|---|---|---|
| `streamid` | Stream ID presented to the destination | — | — |
| `passphrase` | AES passphrase for an encrypted link | — | — |
| `pbkeylen` | AES key length (`16`, `24`, `32`) | — | — |
| `bond` | Comma-separated backup links (see above) | — | — |
| `sndbuf` | SRT send-buffer ceiling, in bytes (`SRTO_SNDBUF`) | `bitrate x latency x 4` formula, ~6.25 MB at the worst-case bitrate assumption | 2 MB – 12 MB |
| `rcvbuf` | SRT receive-buffer ceiling, in bytes (`SRTO_RCVBUF`) | 1 MB — egress only ever receives small ACK/NAK control traffic, never media | 64 KB – 4 MB |
| `latency` | Timestamp-based-delivery latency window, in ms (`SRTO_LATENCY`) | 250 ms | 20 ms – 8000 ms |
| `maxbw` | Bandwidth ceiling, in **bytes/sec** — libsrt's own unit, not bits/sec (`SRTO_MAXBW`) | `-1` (unlimited/input-relative) | unclamped beyond `>= -1` — a pacing rate, not a preallocated buffer |
| `fc` | Flow-control window, in packets (`SRTO_FC`) | 32768 | 256 – 32768 |

`sndbuf`'s formula default is in `srt_egress_sndbuf_bytes` (`src/media/srt/socket.rs`).
Raise it for a destination that legitimately needs more in-flight headroom;
lower it to cut per-output memory on many-destination fan-outs. Every
allocation-sized field is clamped in `EgressBufferOpts::with_overrides`
(`src/media/srt/socket.rs`) regardless of what the URL asks for — an output
URL is operator/API-configured rather than anonymous wire input, but nothing
stops a typo or an untrusted upstream config source from asking for gigabytes
per destination, and that cost multiplies by output count.

Example combining several overrides on one destination:

```text
srt://dest.example:9000?streamid=publish:key&sndbuf=3000000&latency=400&maxbw=6250000
```

All five are **pre-connect** settings: libsrt marks every one of them `PRE` or
`PREBIND`, so none can be changed after the connection is established (see
`EgressBufferOpts` in `src/media/srt/socket.rs` for the full rationale,
including why this rules out true post-connect/adaptive resizing). The
effective `sndbuf` value actually in force is read back from libsrt at connect
time and reported as `srtSndbufConfiguredBytes` in output quality telemetry
(and in the dashboard's publisher-quality panel as "Send buffer ceiling
(configured)"), so what is in force can always be confirmed rather than
inferred. The negotiated `latency` is already visible the same way through the
existing `msSendTsbPdDelay`/`msReceiveTsbPdDelay` quality fields.

### No per-caller SRT ingest buffer/FC parameters

Ingest intentionally does **not** offer `rcvbuf=`/`fc=`/`latency=`-style
per-caller overrides, unlike egress's URL parameters above. This isn't
missing scope — it was implemented and then removed once the standard SRT
URL convention (libsrt's own reference option table,
`.local/build/static/src/srt/apps/socketoptions.hpp`) made clear there is
no standard mechanism for it, and building a non-standard one is worse than
not having the feature:

- `rcvbuf`/`sndbuf`/`fc` are real, standard SRT URL query parameters — but
  standard usage always configures the *local* socket of whoever's URL it
  is. A caller's own `srt://ourserver:port?rcvbuf=...` connect URL sets
  *their* `SRTO_RCVBUF`, never ours. These options are never wire-negotiated
  (confirmed against the vendored libsrt source: no `SRTO_RCVBUF`/`SRTO_FC`
  field exists anywhere in the handshake extension blocks), so there is no
  standard — or even physically possible — way for a caller's URL to reach
  across and configure the listener's own buffers. The only way to attempt
  it would be inventing a non-standard convention (e.g. smuggling query
  params inside the `streamid` field's text content, which no real SRT tool
  does or interprets), which was tried here and reverted for exactly that
  reason.
- `latency` is different, and needs no code at all: it genuinely is
  wire-negotiated (`SRTO_PEERLATENCY`, sent in the real HSREQ/HSRSP
  extension — see `docs/features/handshake.md`). A caller who sets their
  own standard `srt://ourserver:port?streamid=...&latency=400` on their own
  connect call already gets that value carried onto the wire by libsrt
  automatically, and this repo's existing `SRTO_RCVLATENCY` setting on the
  listener already participates in that negotiation
  (`max(local RCVLATENCY, peer PEERLATENCY)`, per
  `docs/API/API-socket-options.md`). Nothing needs to be parsed or applied
  on our side for a caller's latency preference to take effect.

If per-caller ingest buffer sizing becomes a real need later, the only
correct lever is an *operator*-controlled one (e.g. per-pipeline listener
config, not caller-supplied input) — see `EgressBufferOpts` in
`src/media/srt/buffer_sizing.rs` for the equivalent egress-side reasoning
about who benefits from, and who should control, this kind of override.

Ingest encryption is configured through the API/pipeline settings, not the
streamid.

Inbound bonding uses the same single listener. When the publisher initiates a
real SRT group, `srt_accept` returns one group ID and libsrt attaches later
links in the background. Merely opening two independent sockets with the same
StreamID does not create a bond.

The Rust connected ingest path follows the same lifecycle boundary. It uses
the first handshake packets only for provisional tuple ownership, learns the
peer GROUP and StreamID before processing CONCLUSION, and installs a
libsrt-compatible local mirror GROUP extension in its response. Only after the
Core reaches `Connected` does the listener transfer the complete connection
state to the worker's connected socket. All legs with the same peer GROUP stay
on one worker-owned `SrtGroup`; each leg retains its own tuple, timers, and
socket ID.

The linked libsrt must be built with `ENABLE_BONDING=ON`. The listener checks
`SRTO_GROUPCONNECT` at startup and logs a warning when the linked binary does
not expose working bonding support; ordinary single-link SRT remains enabled.
The repo-managed native prefix builds pinned SRT 1.5.5 with bonding enabled and
runs separate-process broadcast and backup/failover tests before packaging.
This does not require a second ingest endpoint: all member tuples join the
group accepted from the shared listener.

Practical note: if you validate bonded ingest or egress across multiple NICs or
WAN paths with one wildcard listener, upstream SRT recommends a build with
`ENABLE_PKTINFO=ON`. Without packet-info support, replies from a wildcard
listener can leave from the wrong source IP, which breaks real multi-interface
bonding even though same-host or single-interface tests may still pass.

## HLS Pull and Authorization

The in-memory HLS store is served at:

```text
/hls/<pipelineId>
/hls/<pipelineId>/index.m3u8
/hls/<pipelineId>/seg<N>.m4s
/hls/<pipelineId>/video/init.mp4
/hls/<pipelineId>/audio/<trackIndex>/index.m3u8
/hls/<pipelineId>/audio/<trackIndex>/init.mp4
/hls/<pipelineId>/audio/<trackIndex>/seg<N>.m4s
```

Live preview generation uses one shared native fMP4 segmenter per pipeline and
exposes separate video/audio rendition playlists from memory. The HTTP/HTTPS
upload path still uses the native inline MPEG-TS segmenter. The preview
segmenter is kept alive while at
least one persistent HLS output is active; its reference count is adjusted
correctly even when an HLS egress task panics (refcount is decremented in
an always-runs cleanup path outside the panic-catching closure).

These routes require the dashboard session cookie. They still respond with
HLS CORS headers, but unauthenticated playlist and segment requests return
`401`.

Before exposing HLS outside authenticated dashboard sessions, add signed URLs
or short-lived bearer tokens covering both playlists and segments, plus expiry,
revocation, rate limits, cache policy, and token-safe audit logs.
