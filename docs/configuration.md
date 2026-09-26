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
- [SRT transport configuration](#srt-transport-configuration)
- [HLS Pull and Authorization](#hls-pull-and-authorization)

## Fixed Runtime Values and Environment Variables

| Value | Default setting | Environment Variable Override |
|---|---|---|
| Dashboard/API listener | `127.0.0.1:3030` | `RESTREAM_HTTP_BIND_ADDR`, `RESTREAM_HTTP_PORT` |
| RTMP listener | `0.0.0.0:1935` | `RESTREAM_RTMP_PORT` |
| SRT listener | `0.0.0.0:10080` | `RESTREAM_SRT_PORT` |
| Tokio scheduler workers | Derived from the effective CPU mask/quota | `RESTREAM_TOKIO_WORKER_THREADS` |
| glibc malloc arenas | `2` (set at startup with `mallopt`); keeps freed per-thread memory from staying resident: −34 to −47 MB RSS at 50 SRT / 500 RTMP outputs, frozen-SRT-destination surge 66–72 MB → 37 MB, no measurable CPU change | `MALLOC_ARENA_MAX` (when set, Restream leaves glibc's own handling alone) |
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
| Egress fabric shard count | Derived from the effective CPU count (clamped `2..=8`); RTMP/sink/pipeline feeds then scale down live to match output count (128 outputs per shard), while SRT feeds always keep the CPU-derived ceiling (SRT shard count is a shared UDP socket / `CallerTable` parallelism budget under `srt-rs` — see [egress architecture](egress-architecture.md) § Readiness backends) | `RESTREAM_EGRESS_SHARDS` (clamped to `1..=1024`; overrides the initial shard count every feed starts with) |
| Egress fabric command capacity | `1024` commands per shard | `RESTREAM_EGRESS_COMMAND_CAPACITY` |
| Egress fabric command batch | `32` commands per loop | `RESTREAM_EGRESS_COMMAND_BATCH` |
| Egress fabric readiness batch | `64` ready leaves per loop | `RESTREAM_EGRESS_READY_BATCH` |
| Egress fabric timer batch | `64` timers per loop | `RESTREAM_EGRESS_TIMER_BATCH` |
| Egress native leaf capacity | `4096` reusable leaf slots per shard | `RESTREAM_EGRESS_MAX_LEAVES_PER_SHARD` (clamped to `1..=1000000`; output creation is rejected after the per-shard slab is full) |
| Egress fabric idle wait | `1` ms | `RESTREAM_EGRESS_IDLE_WAIT_MS` |
| RTMP/RTMPS TCP event capacity | `1024` entries per shard | `RESTREAM_EGRESS_TCP_POLLER_MAX_EVENTS` (bounds completion events and each poll's ready batch) |
| Egress fabric visit units | `32` units per visit | `RESTREAM_EGRESS_VISIT_MAX_UNITS` |
| Egress fabric visit bytes | `262144` bytes per visit | `RESTREAM_EGRESS_VISIT_MAX_BYTES` |
| Egress fabric visit time | `2000` µs per visit | `RESTREAM_EGRESS_VISIT_MAX_US` |
| Egress pending write limit | `262144` bytes per output | `RESTREAM_EGRESS_MAX_PENDING_BYTES` (application-owned protocol bytes; distinct from `RESTREAM_RTMP_STREAM_BUFFER_BYTES`, which configures the TCP socket buffers) |
| Egress fabric drain timeout | `3000` ms | `RESTREAM_EGRESS_DRAIN_TIMEOUT_MS` (clamped `1..=60000`; on shutdown, how long a shard keeps running to let leaves with queued bytes flush before force-closing — currently drives real per-leaf draining for RTMP and SRT, see `docs/archive/egress/implementation.md` Phase 6) |

`EgressFabricConfig::validate` runs once at startup after per-field clamping and logs non-fatal `restream.config.warning` events for cross-field issues among the egress fabric settings above — e.g. `RESTREAM_EGRESS_MAX_PENDING_BYTES` smaller than `RESTREAM_EGRESS_VISIT_MAX_BYTES`, `RESTREAM_EGRESS_SHARDS` more than 4x the effective CPU count, `RESTREAM_EGRESS_DRAIN_TIMEOUT_MS` under 50ms, or `RESTREAM_EGRESS_COMMAND_BATCH` exceeding `RESTREAM_EGRESS_COMMAND_CAPACITY`. See `docs/archive/egress/implementation.md` Phase 6.
| Capacity service-center limits | Derived from available CPUs plus conservative NIC, memory, and disk defaults | `RESTREAM_CAPACITY_INGRESS_PPS`, `RESTREAM_CAPACITY_EGRESS_PPS`, `RESTREAM_CAPACITY_NIC_BPS`, `RESTREAM_CAPACITY_MEMORY_BYTES`, `RESTREAM_CAPACITY_FFMPEG_STAGES`, and `RESTREAM_CAPACITY_DISK_BPS`; set from host-specific benchmark calibration. Values are observe-only and do not admit/reject work. |
| SRT egress muxer max outputs per shard | `0` | `RESTREAM_SRT_EGRESS_MUXER_MAX_OUTPUTS_PER_SHARD` (disabled at `0`; when set, SRT egress creates a new shared TS muxer shard as each pipeline+encoding cohort crosses this many outputs) |
| SRT egress muxer max shards | `64` | `RESTREAM_SRT_EGRESS_MUXER_MAX_SHARDS` (hard guardrail for dynamic SRT muxer sharding; once reached, new outputs are assigned to the least-loaded existing shard and a warning is emitted) |
| SRT egress connect timeout | `10000` ms | `RESTREAM_SRT_CONNECT_TIMEOUT_MS` (each output's request-local handshake attempt duration: it becomes that output's `LeafPolicy.connect_timeout` and its `CallerConfig` attempt deadline, whose clock starts when the pool ADMITS the request, so time spent queued behind the concurrency bound is excluded; raised from a 3s default: a live scale run showed a burst of 600+ simultaneous handshakes to one peer still completing the SRT handshake when the old 3s timeout tore the socket down first, surfacing as `SRT_ENOCONN` on the next send — see `docs/archive/quality/srt-egress-scale-investigation-2026-08-10.md`) |
| SRT egress connect concurrency | `64` | `RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY` (clamped to `1..=4096`; each SRT egress Owner's caller-pool `max_in_flight` — the transport's per-`(shard, address family)` handshake capacity, with an equally sized bounded queue behind it; capacity only, it never sets a timeout or changes the Owner's service budget; a request beyond both is refused and its output fails and retries. Decouples handshake concurrency from output count) |
| SRT encryption | Disabled | `RESTREAM_SRT_PASSPHRASE`; `RESTREAM_SRT_PBKEYLEN` selects the key length and defaults to `16` |
| SRT UDP socket buffer | `8388608` bytes requested by SRT egress; ingress uses the transport default unless overridden | `RESTREAM_SRT_UDP_BUF_BYTES` (optional positive byte count applied to ingress and egress Owner socket configuration; Linux may clamp the effective size to its UDP buffer ceilings) |
| SRT receive budget | `512` datagrams for the measurement sink | `RESTREAM_SRT_RECV_BUDGET_DATAGRAMS` (A/B knob for the test-harness SRT sinks only; the production Owner's receive work is bounded by `OwnerServiceBudget`. The sink uses `RecvBudget::new(8, 512)` unless this env is set.) |
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
| RTMP maximum inbound message size (larger declared messages are rejected before any payload is buffered; clamped 64 KiB–16 MiB) | `8388608` bytes | `RESTREAM_RTMP_MAX_MESSAGE_BYTES` |
| RTMP ingest parser budget (bytes all ingest parsers may hold before messages complete; the connection that would exceed it is rejected; never below the maximum message size) | `268435456` bytes | `RESTREAM_RTMP_INGEST_PARSER_BUDGET_BYTES` |
| RTMP pre-auth socket buffers | `131072` bytes | `RESTREAM_RTMP_PREAUTH_BUFFER_BYTES` |
| RTMP streaming socket buffers | `8388608` bytes | `RESTREAM_RTMP_STREAM_BUFFER_BYTES` |
| RTMP egress chunk size | `16384` bytes | `RESTREAM_RTMP_EGRESS_CHUNK_SIZE` (sent with the RTMP `SetChunkSize` message; 16 KiB was the best measured loopback fanout point in the RTMP-only MSR chunk-size sweep) |
| RTMPS extra trust roots | Public WebPKI roots | `RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM` (path to PEM CA certificates added to the default trust roots) |
| HLS minimum segment length | 1 second | `RESTREAM_HLS_MIN_SEGMENT_MS` |
| HLS live window length | 20 segments | `RESTREAM_HLS_MAX_SEGMENTS` |
| HLS segment accumulator capacity | 8 MiB | `RESTREAM_HLS_SEGMENT_CAPACITY_BYTES` |

RTMPS outputs use the RTMP Compio TCP path and require Linux kTLS after the
Rustls handshake. If the host cannot install kTLS for the negotiated suite, the
output fails; there is no implicit userspace-TLS fallback. Configure
`RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM` only when the destination uses a
private CA.

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
do not cap native helper threads created by FFmpeg or SQLite. Restream
names its Tokio runtime threads `restream-tokio` so process tools can separate
them from the `srt-in-<port>` ingress owner thread, `sqlx-sqlite-*`, and other native helper threads; that label
covers Tokio scheduler, blocking, and replacement worker threads.

## SQLite Performance Settings

`db::create_pool` already applied foreign keys, `synchronous=NORMAL`, a
5s `busy_timeout`, and the cache/temp/mmap PRAGMAs. WAL used to be set
only later in `setup_database_schema` on one pool checkout. Connect
options now also set `journal_mode=WAL` and raise `busy_timeout` to 30s
so every pooled connection — including the first — gets the same tuning.
Schema setup still re-asserts WAL for files created before that
connect-option path existed.

| PRAGMA | Value | Effect |
|---|---|---|
| `journal_mode` | `WAL` | Concurrent readers during a writer; set on connect, not only at schema setup |
| `synchronous` | `NORMAL` | fsync only at WAL checkpoints; safe with WAL |
| `busy_timeout` | 30000 ms | Wait on a locked database before returning SQLITE_BUSY |
| `journal_size_limit` | 64 MiB | Caps WAL file growth; excess is reclaimed at checkpoint |
| `cache_size` | -16384 (16 MiB) | Page cache kept in process memory |
| `temp_store` | `MEMORY` | Temporary tables and indices use memory, not disk |
| `mmap_size` | 128 MiB | Read pages via memory-mapped I/O on supported platforms |

Write transactions that still lose a SQLITE_BUSY race after that wait
(typically a deferred-transaction upgrade deadlock, which SQLite returns
immediately) are retried at the repository boundary.

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
| `rtmp://...` | Compio TCP RTMP egress; IPv6 addresses in bracket notation (`[::1]`) are supported |
| `rtmps://...` | Compio TCP RTMPS egress using Linux kTLS for record processing |
| `srt://...` | SRT egress through the `srt-rs` Compio Owner; percent-encoded `streamid` characters are decoded automatically |
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
in-process backend; audio streams are copied. HEVC-to-H.264 bridge stages
and complex audio stages are controlled separately by
`RESTREAM_INTERNAL_HEVC_TO_H264` and `RESTREAM_INTERNAL_AUDIO_COMPLEX`.
HEVC HLS preview reuses the shared `hevc_to_h264` bridge (that HEVC-to-H.264
flag), not a dedicated preview stage. `RESTREAM_INTERNAL_HLS_PREVIEW` remains
a backend-family toggle for `StageKind::Preview`, but the current planner does
not create that kind. The Admin → Backend checkbox for HLS preview is therefore
inert until a dedicated preview stage exists again.
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

## SRT transport configuration

SRT is implemented with `srt-rs` (`srt-proto` and `srt-transport`) and Compio
`Owner` threads. The transport owns SRT protocol state and its UDP sockets.
The production path uses this transport directly; no alternate SRT transport is wired into runtime configuration.

### Ingest latency and encryption

The global `srtIngest.latencyMs` setting defaults to `250` ms and accepts
`20–8000` ms. A pipeline's `srtIngestPolicy.latencyMs` overrides the global
value; an omitted per-pipeline value inherits it. Configure the global value
through `PATCH /api/v1/settings` and the per-pipeline value through the
pipeline's SRT ingest policy. Both settings are also exposed in the dashboard.

Restream supplies this value as the SRT listener's peer latency policy during
admission. The caller can propose a larger receive delay, so the delay
negotiated for a connection can exceed the configured listener value. This
setting does not calculate or set per-caller receive-buffer or flow-control
values; transport buffering is owned by `srt-transport`.

SRT ingest encryption is configured through the global or per-pipeline API
policy, not through caller-supplied StreamID text. Plaintext is the default.
Encrypted mode requires a valid passphrase and supports key lengths of 16, 24,
or 32 bytes.

The `RESTREAM_SRT_UDP_BUF_BYTES` environment variable optionally requests an
OS UDP socket-buffer size for SRT listener and caller sockets. When unset, the
listener uses the transport's default buffer policy and egress requests 8 MiB.
Linux may clamp the effective value to `net.core.rmem_max` and
`net.core.wmem_max`; inspect runtime health and host limits when tuning this
setting. `RESTREAM_SRT_RECV_BUDGET_DATAGRAMS` applies only to the harness's
measurement sink; production Owner work is bounded by `OwnerServiceBudget`.

### Recognized SRT egress URL parameters

Restream reads only these query parameters from an `srt://` output URL.
Unrecognized parameters are ignored; in particular, `sndbuf`, `rcvbuf`,
`fc`, `latency`, and `maxbw` are not egress URL settings:

| Parameter | Purpose |
|---|---|
| `streamid` | Stream ID presented to the destination; percent-decoded |
| `passphrase` | AES passphrase for an encrypted link; percent-decoded |
| `pbkeylen` | AES key length in bytes (`16`, `24`, or `32`) |
| `bond` | Comma-separated additional peer addresses |
| `type` | Bond mode: `backup` (default) or `broadcast` |

For a bond, the URL authority is the primary peer and `bond=` contains the
additional legs. `backup` prefers the authority and uses other legs as
standbys; `broadcast` sends over each healthy leg. All legs must use the same
address family because one SRT Owner socket serves the group. The peers must
also participate in the same receiving group.

Example:

```text
srt://primary.example:10080?streamid=publish:key&bond=backup1.example:10080,backup2.example:10080&type=backup
```

Caller-controlled ingest buffer settings are not accepted from the SRT URL.
The caller's own socket options configure that caller's socket; Restream's
listener buffers are controlled by the operator and transport configuration.
The caller's SRT latency proposal is negotiated by the protocol, independently
of URL parsing by Restream.

Inbound bonding uses the SRT listener's explicit group-input policy. A
publisher-created Broadcast or Backup group is authenticated as one logical
SRT input; matching independent sockets remain independent publishers. The
logical input retains its identity when a physical leg fails and exposes both
per-leg and deduplicated aggregate telemetry.


### Linux host and harness capacity

For a fresh Linux host, `scripts/dev/bootstrap.sh` and
`scripts/dev/bootstrap-runtime.sh` report whether private user/network
namespaces and the required SRT UDP buffer ceilings are available. To persist
the harness host settings, run:

```sh
scripts/dev/bootstrap.sh --configure-harness-host
# or, for a runtime-only host:
scripts/dev/bootstrap-runtime.sh --configure-harness-host
```

Both bootstrappers delegate to `scripts/dev/harness-host-prereqs.sh`. They
configure `kernel.unprivileged_userns_clone=1`,
`user.max_user_namespaces=28633`, `net.core.rmem_max=26214400`, and
`net.core.wmem_max=8388608` in
`/etc/sysctl.d/99-restream-harness.conf`. They do not disable AppArmor or other
host security policy; use `--no-netns` only as a temporary fallback when the
host administrator has not approved unprivileged namespaces.

Run the prerequisite script before a scale capture:

```sh
scripts/dev/harness-host-prereqs.sh
```

| Setting | Harness baseline | Why it matters |
|---|---:|---|
| `RLIMIT_NOFILE` hard limit | at least `65536` | Restream requests this at startup; the MSR harness raises its soft limit based on the largest requested checkpoint but cannot exceed the inherited hard limit. |
| `net.core.rmem_max` | `26214400` | Conservative receive-buffer ceiling for SRT UDP workloads; the effective request depends on the transport default and `RESTREAM_SRT_UDP_BUF_BYTES`. |
| `net.core.wmem_max` | `8388608` | Supports the default 8 MiB SRT egress UDP socket-buffer request; raise it when using a larger override. |
| `net.core.somaxconn` | at least `4096` for the 1,200-output MSR | Bounds pending TCP accepts during the RTMP connection burst. |
| `net.ipv4.ip_local_port_range` | at least `4096` ports | Bounds concurrent outbound loopback/egress connections; the normal Linux range is ample. |
| `fs.file-max` | above aggregate process demand | Host-wide descriptor ceiling; leave room for Restream, harness peers, MediaMTX, and publishers. |
| `kernel.unprivileged_userns_clone`, `user.max_user_namespaces` | `1`, `28633` | Enables the private user/network namespace used by default integration tests. |

`net.core.rmem_default`, `net.core.wmem_default`, `net.core.netdev_max_backlog`,
`net.ipv4.udp_mem`, CPU affinity/quota, cgroup limits, and available RAM have
workload-dependent requirements. Record them with scale artifacts; runtime
health exposes process limits, affinity, cgroup quota, and memory.

The host-specific SRT buffer and receive-budget A/B measurements are preserved
in [`srt-tokio-ab-knobs-2026-09-07.md`](archive/quality/srt-tokio-ab-knobs-2026-09-07.md);
they are diagnostic evidence, not portable defaults or shard-policy inputs.


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
