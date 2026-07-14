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
| Transcoder backend | External FFmpeg subprocess | `RESTREAM_INTERNAL_VIDEO_PRESETS`, `RESTREAM_INTERNAL_HEVC_TO_H264`, `RESTREAM_INTERNAL_HLS_PREVIEW`, and `RESTREAM_INTERNAL_AUDIO_COMPLEX` (`1`/`true`/`yes`/`on` enable each in-process stage family independently) |
| File-ingest backend | External embedded FFmpeg subprocess | `RESTREAM_USE_INTERNAL_FILE_INGEST` (`1`/`true`/`yes`/`on` to enable in-process remux + demux for passthrough file ingest) |
| External transcoder and file-ingest executable | Embedded `public/bin/ffmpeg`, extracted to `.restream/runtime/ffmpeg/` at startup | `FFMPEG_BIN_PATH` |
| SQLite database | `.restream/data/restream.db` (with WAL/SHM sidecars) | `RESTREAM_DB_PATH` |
| Media directory | `.restream/media/` | `RESTREAM_MEDIA_DIR` |
| Text file log directory | `.restream/logs/` | `RESTREAM_LOG_DIR` |
| Media packet ring depth (source/ingest) | `1024` packets | `RESTREAM_RING_CAPACITY` |
| Media packet ring depth (transcoder output) | `512` packets | `RESTREAM_TRANSCODER_RING_CAPACITY` (720p30 output ≈ 80 pkt/s → 512 slots ≈ 6.4 s jitter headroom; lower than source ring because I-frame payloads are large) |
| Shared SRT TS ring depth | `256` chunks | `RESTREAM_TS_RING_CAPACITY` (SRT protocol's own send buffer absorbs network jitter; this ring only bridges muxer → socket write, typically sub-millisecond) |
| SRT egress muxer max outputs per shard | `0` | `RESTREAM_SRT_EGRESS_MUXER_MAX_OUTPUTS_PER_SHARD` (disabled at `0`; when set, SRT egress creates a new shared TS muxer shard as each pipeline+encoding cohort crosses this many outputs) |
| SRT egress muxer max shards | `64` | `RESTREAM_SRT_EGRESS_MUXER_MAX_SHARDS` (hard guardrail for dynamic SRT muxer sharding; once reached, new outputs are assigned to the least-loaded existing shard and a warning is emitted) |
| SRT egress local-port reuse | Enabled | `RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT` (`0`/`false` disables reuse) |
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

The Rust server does not currently read the old Node environment variables such
as `BASE_PATH`, `PUBLIC_INGEST_HOST`, `HEALTH_SNAPSHOT_INTERVAL_MS`,
or the old output-recovery knobs. Do not depend on those variables.

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
| Custom encoding | Stored through `/api/v1/encodings/custom` for future use; not offered as an output encoding and rejected by output create/update |
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
host. MSR profiling showed thread-family partitioning can help, but the runtime
does not currently pin individual thread families; keep fine-grained placement
experiments outside production defaults until they have host-specific proof.

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
    "audio": { "mode": "all" }
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
| `http://...`, `https://...` | Starts the local MPEG-TS segmenter and uploads segments/playlist with HTTP PUT |

Any other prefix is rejected during validation. The served preview HLS path is
fragmented MP4 (`init.mp4` + `.m4s`), but HTTP/HTTPS HLS upload intentionally
stays on MPEG-TS for ingest compatibility. For HTTP/HTTPS HLS upload,
segment upload URLs are derived from the playlist target: a `file=` query
parameter is replaced with `seg<N>.ts`, otherwise the playlist path filename is
replaced with the segment filename.

Encoding strings are compound values:

```text
<video-preset>[+<audio-routing>]
```

Examples:

```text
source
720p
1080p+atrack:0
720p+remap:0:1
source+downmix:1
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
`custom` remains stored configuration only. It is rejected by output
create/update so operators do not accidentally select a passthrough path that
looks like custom FFmpeg execution.

Audio routing accepts `atrack`, `remap`, and `downmix`. `atrack` stays on the
packet-only selector path; channel-level `remap` and `downmix` routes run
through an external FFmpeg audio stage and re-encode stereo AAC.

## File Ingest Configuration

File ingest is configured per pipeline or through the standalone ingest routes.
Each definition stores:

- `filename`
- `loop`
- `startTime`
- `liveOptimized`
- `targetGopSeconds`

`liveOptimized=false` keeps the default passthrough path. With the subprocess
backend that means:

```text
ffmpeg -re [-stream_loop -1] [-ss <start>] -i media/<file> -map 0 -c copy -f mpegts pipe:1
```

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

- 250 ms latency
- 256-packet loss/reorder tolerance
- 8 MiB UDP send/receive buffers
- 12 MiB SRT send/receive buffers
- 32768-packet flow-control window
- unlimited automatic maximum bandwidth

The code does not explicitly apply the helper to accepted sockets or bonded
egress groups. Do not assume those sockets have every requested value without
runtime verification.

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

Inbound bonding uses the same single listener. When the publisher initiates a
real SRT group, `srt_accept` returns one group ID and libsrt attaches later
links in the background. Merely opening two independent sockets with the same
StreamID does not create a bond.

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
