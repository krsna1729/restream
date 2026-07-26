# Restream API Reference

Base URL: `http://localhost:3030`

JSON uses camelCase. Unless noted otherwise, routes require the `session` cookie
returned by login.

## Contents

- [Request Limits](#request-limits)
- [Authentication](#authentication)
- [Configuration and Discovery](#configuration-and-discovery)
- [Pipelines](#pipelines)
- [Outputs](#outputs)
- [Process Logs](#process-logs)
- [Output Status](#output-status)
- [Probe, Graph, and Diagnostics](#probe-graph-and-diagnostics)
- [Optional Agent Plane](#optional-agent-plane)
- [Recording](#recording)
- [File Ingest](#file-ingest)
- [Media Files](#media-files)
- [Custom Encoding](#custom-encoding)
- [Health and Status](#health-and-status)
- [HLS Pull](#hls-pull)
- [Operator v1 Endpoints](#operator-v1-endpoints)
- [Engineer v1 Endpoints](#engineer-v1-endpoints)

## Request Limits

| Limit | Value |
|---|---|
| Maximum request body | 4 MiB |
| `name` / `serverName` / `label` fields | 256 bytes |
| `url` / output URL fields | 2048 bytes |
| output `config` JSON | 512 bytes |
| `streamKey` | 256 bytes |
| `ffmpegArgs` (custom encoding) | 4096 bytes |
| `password` | 1024 bytes |

Requests exceeding the body limit receive `413 Payload Too Large`. Fields
exceeding the per-field limits receive `400 Bad Request` with a descriptive
message.

## Authentication

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/auth/login` | Create a persisted session from `{ "password": "..." }` |
| `POST` | `/api/auth/logout` | Delete the current session |
| `POST` | `/api/auth/change-password` | Change the password; existing sessions remain valid |

Static pages/assets are served without an auth gate; protected API handlers
enforce the cookie themselves. `/healthz` and `/audio-caps` are public.
HLS pull routes require the dashboard session cookie.

All responses include `X-Content-Type-Options: nosniff` and
`X-Frame-Options: SAMEORIGIN` security headers. These are applied globally by
a `SetResponseHeaderLayer` on the main router.

## Configuration and Discovery

The canonical authenticated settings surface is `/api/v1/settings`.

### `GET /api/v1/settings`

Returns SQLite-backed settings plus configured pipelines, outputs, and jobs.
Query params:

| Param | Default | Notes |
| --- | --- | --- |
| `jobs` | `all` | `latest` returns only the newest job per `(pipelineId, outputId)` pair for consumers that need a slimmed job list. |
| `view` | `full` | `dashboard` trims admin-only settings fields (`ingestSecurity`, `recordingSettings`, `srtIngest`) and omits job rows from the dashboard runtime fetch while keeping editor/runtime fields such as `ingestHost`, `transcodeProfiles`, pipelines, and outputs. Settings mode upgrades itself back to `full` on entry. |

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
  },
  "pipelines": [],
  "outputs": [],
  "jobs": []
}
```

Each pipeline in this response includes the selected input's RTMP and SRT
ingest URLs. Use the pipeline-input endpoint to enumerate every configured
input and its credential.

### `PATCH /api/v1/settings`

Updates any supplied setting:

```json
{
  "serverName": "India Restream",
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

An empty `serverName` returns `400`.

When `transcodeProfiles` are included in `PATCH /api/v1/settings`, each profile
is validated before saving:

- `preset` must be one of: `ultrafast`, `superfast`, `veryfast`, `faster`, `fast`, `medium`, `slow`, `slower`, `veryslow`, `placebo`
- `tune` must be empty or one of: `film`, `animation`, `grain`, `stillimage`, `psnr`, `ssim`, `fastdecode`, `zerolatency`
- `crf` must be in `0..=51`

Invalid values return `400 Bad Request` with a descriptive error.

### `GET /api/v1/stream-keys`

Returns the selected stream key and native ingest URLs for each configured
pipeline. Use `GET /api/v1/pipelines/:pipelineId/inputs` when a client needs
all primary and backup credentials:

```json
[
  {
    "key": "stream-key",
    "label": "Stream 1",
    "ingestUrls": {
      "rtmp": "rtmp://stream.example.com:1935/live/stream-key",
      "srt": "srt://stream.example.com:10080?streamid=publish:stream-key"
    }
  }
]
```

Input credentials are managed through the pipeline-input routes. Stream keys
remain opaque random values with an `sk_` prefix; they do not encode pipeline,
input, role, or protocol information.

### `GET /audio-caps`

Returns the frontend's platform/protocol audio capability matrix.

## Pipelines

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/pipelines` | List pipelines |
| `POST` | `/api/v1/pipelines` | Create a pipeline |
| `PATCH` | `/api/v1/pipelines/:id` | Replace editable pipeline fields |
| `DELETE` | `/api/v1/pipelines/:id` | Delete a pipeline |
| `GET` | `/api/v1/pipelines/:pipelineId/inputs` | List configured inputs and runtime state |
| `POST` | `/api/v1/pipelines/:pipelineId/inputs` | Add a backup input |
| `PATCH` | `/api/v1/pipelines/:pipelineId/inputs/:inputId` | Rename or enable/disable an input |
| `DELETE` | `/api/v1/pipelines/:pipelineId/inputs/:inputId` | Delete an unselected backup input |
| `POST` | `/api/v1/pipelines/:pipelineId/inputs/:inputId/promote` | Select and promote an input |

Create/update body:

```json
{
  "name": "Main Feed",
  "streamKey": "stream-key",
  "inputSource": null,
  "fileIngest": {
    "filename": "recording-1.ts",
    "loopFlag": true,
    "startTime": "00:00:05",
    "liveOptimized": true,
    "targetGopSeconds": 3
  }
}
```

`name` is required for both create and update because the current update handler
uses the same payload type. If `streamKey` is omitted on create, an opaque
random primary-input key is generated.

`inputSource` is persisted for operator metadata only; it does not pull remote
media or transform the active native ingest path.

`fileIngest` is optional. When present, pipeline create/update persists or
replaces the pipeline's file-ingest config in the same mutation response; send
`"fileIngest": null` to clear the configured file source as part of the same
pipeline edit instead of issuing a follow-up `/file-ingest` request.

Create and update responses include a normalized `pipeline` object plus the
mutation message so clients can patch dashboard pipeline state locally without
an immediate follow-up settings fetch. That response now also includes the
normalized `fileIngest` state so the editor can avoid a separate read or write
round-trip when a pipeline switches between publisher and file-source modes.
Pipeline deletes intentionally return a simple acknowledgement because the
client can remove the matching pipeline, outputs, jobs, and cached health rows
locally.

Deletion cancels configured output tasks, the active ingest, and any
file-ingest FFmpeg subprocesses whose `streamKey` matches the pipeline's
stream key before removing the pipeline row. Shared transcoder, HLS, and
recording cleanup still follows their existing task lifecycle.

### Pipeline inputs

A pipeline starts with one primary input and accepts up to four configured
inputs. Additional inputs are explicit backup rows, each with its own opaque
stream key and RTMP/SRT URL. Exactly one enabled input is selected. Role records
how the input was created; promoting a backup changes selection without
rewriting its role.

Create body:

```json
{ "label": "Venue encoder B" }
```

Patch body:

```json
{ "label": "Venue encoder B", "enabled": true }
```

List and mutation responses expose each input's `id`, `label`, `streamKey`,
`role`, `enabled`, `selected`, `ingestUrls`, `previewUrl`, and `runtime`.
Runtime includes `connected`, `forwardingState`, protocol, uptime, bytes,
remote address, media metadata, and transport quality. A connected unselected
input reports `forwardingState: "standby"`; a promoted input may briefly report
`"awaitingKeyframe"` before becoming `"active"`.

Promotion returns the normalized input plus `connected`. When the target is
already connected, the current writer is demoted and drained before the target
replays its latest complete compressed GOP on its next packet arrival. If no
complete GOP is cached, it waits for its next video keyframe. Promotion of an
idle configured input changes selection and returns `connected: false`; its
next publisher session becomes the selected source.

## Outputs

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/v1/pipelines/:pipelineId/outputs` | Create an output |
| `PATCH` | `/api/v1/pipelines/:pipelineId/outputs/:outputId` | Update an output |
| `DELETE` | `/api/v1/pipelines/:pipelineId/outputs/:outputId` | Delete an output |
| `POST` | `/api/v1/pipelines/:pipelineId/outputs/:outputId/start` | Set `desiredState=running` |
| `POST` | `/api/v1/pipelines/:pipelineId/outputs/:outputId/stop` | Set `desiredState=stopped` |

Create/update body:

```json
{
  "name": "Primary CDN",
  "url": "rtmp://destination.example/live/key",
  "config": {
    "video": { "mode": "preset", "preset": "1080p", "codec": "auto" },
    "audio": { "mode": "selectTracks", "tracks": [0] },
    "protocol": { "type": "rtmp", "mode": "enhanced" }
  }
}
```

`config` is the output contract. Legacy top-level `encoding` is not accepted on
create/update. Output responses return the same typed object, and the runtime
derives any internal stage labels from it server-side.

The one-second reconciler starts and stops native egress tasks from
`desiredState`.

Create, update, start, and stop responses include a normalized `output` object
plus the mutation message so clients can patch local output config state without
an immediate follow-up settings fetch.

Output config accepts `source`, built-in video presets, `video.codec`
(`auto`, `h264`, `h265`), and typed audio-routing shapes (`all`,
`selectTracks`, `downmix`, `remap`). `custom` output video mode is rejected
with `400 Bad Request` because custom FFmpeg arguments are persisted for future
use but are not applied by the runtime yet.

For RTMP and RTMPS outputs, `config.protocol` may be
`{ "type": "rtmp", "mode": "legacy" | "enhanced" }` and defaults to legacy
RTMP when omitted. Enhanced RTMP advertises `avc1`, `hvc1`, and `mp4a`
capabilities during connect; H.264 outputs continue using AVC payloads, while
HEVC outputs use the Enhanced FLV `hvc1` packet format. Legacy RTMP converts
HEVC source video to H.264 before publish and resolves preset `codec:auto` to
H.264. Explicit H.265 is rejected for legacy RTMP and HLS outputs; Enhanced
RTMP and SRT support H.264 and H.265.

Deleting a running output cancels and unregisters its active egress before the
database row is removed.

URL behavior:

| URL prefix | Egress |
|---|---|
| `rtmp://` | RTMP |
| `rtmps://` | RTMPS with TLS before the RTMP handshake |
| `srt://` | SRT/MPEG-TS |
| `hls://` | Local in-memory HLS segmenter |
| `sink://` | Fabric sink output that discards media after egress accounting |
| `pipeline://` | In-process pipeline recirculation; candidate topology and target input are validated before the output starts |
| `http://` | HLS HTTP PUT upload |
| `https://` | HLS HTTP PUT upload |

Any other prefix is rejected during validation with a `400 Bad Request`.
Pipeline recirculation URLs are parsed and checked for cycles and target-input
ownership before the output can start.
HTTP/HTTPS HLS upload uses one shared local segmenter per pipeline, PUTs each
new `seg<N>.ts`, then PUTs the playlist URL.

## Process Logs

| Method | Route | Response |
|---|---|---|
| `GET` | `/api/v1/logs` | `{ logs, total, hasMore }` |
| `GET` | `/api/v1/logs/stream` | SSE stream (`event: log` frames) |

All process log entries are stored in the `app_logs` SQLite table and served
through these two endpoints. The frontend history UI calls `/api/v1/logs` with
`pipeline_id`/`output_id` filters instead of relying on the pipeline-scoped
history endpoints.

### `GET /api/v1/logs`

Query parameters:

| Parameter | Default | Description |
|---|---|---|
| `level` | `info` | Minimum level: `error`, `warn`, `info`, `debug` |
| `since` | — | RFC3339 lower bound (inclusive) |
| `until` | — | RFC3339 upper bound (exclusive) |
| `target` | — | Module prefix filter (`restream::media::srt`) |
| `pipeline_id` | — | Restrict to a single pipeline |
| `output_id` | — | Restrict to a single output (requires `pipeline_id`) |
| `event_class` | — | `lifecycle` to return only lifecycle transition events |
| `prefix` | — | Comma-separated message prefix filter (`stderr,exit`) |
| `limit` | `200` | 1–1000 |
| `order` | `desc` | `asc` or `desc` on `ts` for ordinary list snapshots |

Each log entry in the response includes `id`, `ts`, `level`, `target`,
`message`, `fields` (JSON), `pipelineId`, `outputId`, `eventType`.
Lifecycle-aware clients also receive `eventClass` (for example, `lifecycle`).

### `GET /api/v1/logs/stream`

SSE live tail. Accepts the same core filter parameters as `GET /api/v1/logs`,
plus `include_restream=true` when a `pipeline_id` subscription should also
receive restream-wide process lifecycle events on the same stream.
On connect, the handler backfills entries newer than the `Last-Event-ID`
header (or `?last_event_id=`) from the database, then streams new entries
from the broadcast channel. Live entries are broadcast only after persistence,
so their positive SSE IDs are stable and reconnect backfill preserves
`eventType` and `eventClass`. Reconnect backfill pages are ordered by ascending
persisted ID to match the resume cursor and continue until the gap is closed
rather than truncating after one page. A `": ping"` comment is sent every 20 seconds.
Lagging receivers are closed; the browser reconnects automatically using
`Last-Event-ID`.
The dashboard overview activity rail uses an initial `GET /api/v1/logs` snapshot
plus this SSE endpoint filtered with `scope=restream` for live restream-wide
activity updates. Overview also reuses that same restream-scoped stream to
wake `/api/v1/dashboard/runtime` summary refreshes on lifecycle events, avoiding a second
lifecycle-only SSE connection in that mode.
Pipeline, inspect, control-room, and publisher-health runtime surfaces
subscribe to this SSE endpoint with `event_class=lifecycle` so they refresh
immediately on process lifecycle transitions instead of waiting for the next
periodic poll.
Focused pipeline and inspect views now add `pipeline_id=<selected>` plus
`include_restream=true` on that lifecycle SSE feed so the browser receives only
the selected pipeline's lifecycle events alongside restream-wide process
transitions, instead of every sibling pipeline event.
Settings and media also keep a narrower restream-scoped `event_class=lifecycle`
feed open so the global Rust-process indicator can react to
shutdown/fault/ready events without waking the heavier runtime health polls in
those modes. Their successful `/metrics/system` refreshes also mark the Rust
process as reachable immediately instead of leaving the indicator on
"Connecting" until the next lifecycle event.
The output-history and pipeline-history "Live" views use the same SSE endpoint
with `pipeline_id`, `output_id`, and `event_class` filters plus `Last-Event-ID`
resume cursors instead of periodic history re-polls.
Status mode reuses its own `scope=restream` stream over the initial snapshot so
restream process activity can update live without repeated log GETs or a second
lifecycle-only SSE connection.
Hidden dashboard tabs now close these SSE feeds and resume from the last seen
event id when visible again, falling back to slower snapshot polling only while
the tab is backgrounded.

## Output Status

Dashboard output start/stop controls update their button/card state
optimistically as soon as the API request is accepted, then let the next
runtime refresh confirm the actual engine state. That keeps control feedback
immediate without requiring a dedicated per-output poller.

### `GET /api/v1/pipelines/:pipelineId/outputs/:outputId/status`

Returns live egress telemetry for a single output. While active, this is the
current runtime state. After teardown/cleanup, the endpoint preserves the most
recent classified output snapshot, including `status`, `phase`, `lastError`,
`failurePhase`, `endedAt`, and active retry-backoff fields such as
`retrying`/`nextRetryAt`, so failure cleanup does not erase operator context.
Returns `404` only when the output has no active or recent runtime state.

Recovered outputs also expose short-lived downstream instability signals:
`recentFailureCount` tracks recent egress failures still inside the flap
window, and `flapping` becomes `true` after repeated sink failures even if the
output is currently back to `status=running`.

`GET /api/v1/engine/health` carries the complementary ingest-side instability
signal. In addition to `disconnectGraceActive` / `disconnectGraceRemainingMs`,
the input snapshot now includes `recentDisconnectCount` and `flapping` so
clients can distinguish a single recent drop from repeated reconnect churn.

## Probe, Graph, and Diagnostics

### `GET /api/v1/pipelines/:pipelineId/probe`

Returns active native ingest metadata, bitrate, GOP observations, and ingest
identity. Video and audio metadata include MPEG-TS `pid`, `language`, and
`title` when the source descriptors provide them. `audioTracks` lists every
active audio track; `audio` remains the primary/first track for older clients.
Returns `404` without an active ingest.

### `GET /api/v1/pipelines/:pipelineId/graph`

Returns the current processing DAG: ingest, source ring, transcoder stages,
egresses, HLS, and recording nodes where present.

### `POST /api/v1/pipelines/:pipelineId/diagnostics/run`

Returns one JSON report with `protocol`, `totalDurationMs`, and ordered `checks`.
The server infers the protocol from the active ingest; the POST has no request
body. Returns `404` without an active ingest and `429` when another run is
active for the pipeline.

RTMP and SRT inputs run the transport-oriented checks documented in
[Observability](observability.md). File inputs switch to file-aware checks:
source-file presence and analysis, file-ingest runtime state, and preview /
recording readiness.

## Optional Agent Plane

The phase-4 agent read/planning plane is behind the `agent-plane` Cargo feature.
Normal core builds compile it out and return `404` from `/api/v1/agent/*`
routes with `compiledIn: false`.

When compiled with `--features agent-plane`, the routes are authenticated,
read-only, and do not mutate pipeline, output, or runtime state. Execution is
reserved for the separate `agent-execution` phase-6 feature.

The agent capability route catalog intentionally lists only agent-plane routes.
Core operator APIs may expose raw operator data such as target URLs; agents
should use `/api/v1/agent/context` and investigation responses for redacted
state.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/agent/capabilities` | Discover compiled-in read and planning tools |
| `GET` | `/api/v1/agent/context` | Return one redacted read-only state bundle for agent reasoning |
| `POST` | `/api/v1/agent/investigations` | Bundle health, graph, telemetry, alerts, and events for investigation workflows |
| `POST` | `/api/v1/agent/plans` | Convert intent plus structured proposed changes into a draft plan |
| `POST` | `/api/v1/agent/plans/validate` | Return only validation results for a draft plan |
| `POST` | `/api/v1/agent/graph-diff-preview` | Return graph/impact preview for a draft plan |

When compiled with `--features agent-execution`, the API also exposes
approval-gated operation routes. These routes are still authenticated, and
operation responses are redacted before they are returned.

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/v1/agent/operations` | Create an operation object from an intent, structured changes, and optional idempotency key |
| `GET` | `/api/v1/agent/operations/:operation_id` | Read operation status, audit log, progress, execution result, and verification result |
| `POST` | `/api/v1/agent/operations/:operation_id/approve` | Record explicit approval before mutation is allowed |
| `POST` | `/api/v1/agent/operations/:operation_id/apply` | Apply approved output add/update/remove/start/stop changes through the core DB/runtime primitives |
| `POST` | `/api/v1/agent/operations/:operation_id/verify` | Verify post-change health, graph convergence, and alert delta |
| `POST` | `/api/v1/agent/verify` | Verify by body: `{ "operationId": "op_..." }` |

Without `agent-execution`, these operation routes return an authenticated `404`
with `feature: "agent-execution"` and `compiledIn: false`.

Context responses include:

- route and lightweight schema metadata for agent clients
- build/runtime status, OS basics, native-library versions, and feature flags
- redacted pipelines, outputs, ingests, jobs, transcode profiles, and settings
- current desired-vs-actual summaries for inputs, outputs, recording, and HLS
- health, resource maps, engine telemetry, per-pipeline telemetry, processing
  graphs, alerts, and recent lifecycle events
- media inventory, storage summary, dependency summaries, and passive
  diagnostics findings plus active diagnostics route metadata
- redaction metadata describing which fields were removed

Raw stream keys and output URLs are never returned by this endpoint. They are
replaced with stable SHA-256 fingerprints plus URL scheme/host summaries.
The context endpoint does not open active diagnostics probes; agents can use the
advertised diagnostics run route when an explicit active report is needed.

Plan request:

```json
{
  "intent": "Attach a 720p RTMP output",
  "pipelineId": "pipeline_abc",
  "proposedChanges": [
    {
      "kind": "addOutput",
      "name": "Primary CDN",
      "url": "rtmp://destination/live/key",
      "config": {
        "video": { "mode": "preset", "preset": "720p" },
        "audio": { "mode": "selectTracks", "tracks": [0] }
      }
    }
  ]
}
```

Operation create request:

```json
{
  "intent": "Attach a stopped RTMP output",
  "pipelineId": "pipeline_abc",
  "idempotencyKey": "change-ticket-123",
  "actor": "agent",
  "agentId": "ops-agent",
  "toolIdentity": "agent-execution-api",
  "incidentId": "incident_123",
  "incidentLinks": ["alert:egress-stale"],
  "proposedChanges": [
    {
      "kind": "addOutput",
      "name": "Primary CDN",
      "url": "rtmp://cdn.example/live/key",
      "config": {
        "video": { "mode": "source" },
        "audio": { "mode": "all" }
      },
      "desiredState": "stopped"
    }
  ]
}
```

Operation records include `operationId`, `status`, `approval`, `request`,
`plan`, `proposedPlanHash`, `incidentId`, `incidentLinks`, `affectedObjects`,
`stateTransitions`,
`progressSnapshots`, `auditLog`, `executionResult`, and `verificationResult`.
`apply` is rejected until approval is recorded.

Plan responses include `planId`, validation errors/warnings, static graph
preview, and impact notes. `executionEnabled` is `true` only when
`agent-execution` is compiled in.

The current run contains nine checks; see [Observability](observability.md).

## Recording

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/v1/pipelines/:pipelineId/recording/start` | Persist enabled state and start immediately if ingest is active |
| `POST` | `/api/v1/pipelines/:pipelineId/recording/stop` | Disable and cancel recording |

Response:

```json
{ "enabled": true, "active": true }
```

The recording path writes raw MPEG-TS files in `media/`, and files whose task
lifetime is shorter than five seconds are deleted as transient artifacts. The
recording feeder uses the shared TS packet feeder before writing to the
MemoryQueue-backed file writer.

When a recording stops successfully and is at least five seconds long, the
runtime starts a one-off FFmpeg remux from the source `.ts` into a sibling
`.mp4`. The media library prefers the `.mp4` for browser playback, keeps the
original `.ts` available for download while it exists, and surfaces
`conversionStatus` as `converting`, `ready`, or `failed`.

The deployment-wide setting `recordingSettings.retainSourceTs` controls whether
the original `.ts` is kept after a successful remux:

- `false` (default): delete the source `.ts` only after the `.mp4` is created successfully
- `true`: keep both files

Failed remuxes keep the source `.ts` regardless of this setting.

## File Ingest

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/ingests` | List configured file ingests |
| `POST` | `/api/v1/ingests` | Create |
| `PUT` | `/api/v1/ingests/:id` | Update |
| `DELETE` | `/api/v1/ingests/:id` | Delete |
| `POST` | `/api/v1/ingests/:id/start` | Start file ingest via the configured backend |
| `POST` | `/api/v1/ingests/:id/stop` | Stop the active ingest task/process |

Create/update body:

```json
{
  "filename": "example.mp4",
  "streamKey": "stream-key",
  "loop": true,
  "startTime": "00:00:05",
  "liveOptimized": true,
  "targetGopSeconds": 2
}
```

Start returns `400` if `media/<filename>` does not exist, `400` if no pipeline
matches the configured stream key, and `409` if that ingest ID already has a
running file ingest or the target pipeline already has another active
publisher.

By default the backend is the embedded `public/bin/ffmpeg` subprocess. The
application service owns its argument construction; the API contract is the
requested loop, start-time, and optimization behavior rather than a copied
process command.

Set `RESTREAM_USE_INTERNAL_FILE_INGEST=1` to switch passthrough
`liveOptimized=false` starts to the in-process remux path instead.

When `liveOptimized=true`, start always uses the embedded FFmpeg subprocess and
re-encodes toward a live-friendly GOP cadence:

- video: H.264 (`libx264`)
- audio: AAC
- forced keyframes every `targetGopSeconds`
- scene-cut GOP drift disabled

Deleting an ingest definition terminates the running ingest regardless of backend.
Both stop and delete kill the child and call `wait()` to reap it immediately so
no zombie processes remain.

## Media Files

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/media` | List supported media files in `.restream/media/` by default |
| `POST` | `/api/v1/media/upload` | Upload one `.ts`, `.mkv`, `.mp4`, or `.mov` media-library file (multipart field `file`, max 8 GiB) |
| `GET` | `/api/v1/media/:filename/analysis` | Return source-file codec / duration / GOP analysis |
| `PATCH` | `/api/v1/media/:filename` | Rename a media file without changing its extension |
| `DELETE` | `/api/v1/media/:filename` | Delete an unreferenced file under `media/` |

Deletion returns `409` when a configured file ingest references the filename.
Deletion canonicalizes both the `media/` root and the requested target path.
Requests that resolve outside `media/` (path traversal) return `400`.
Missing files return `404`.

`GET /api/v1/media` returns entries for `.ts`, `.mkv`, `.mp4`, and `.mov`
files. Recording-backed entries may include:

- `sourceName` / `sourceSize`
- `convertedName` / `convertedSize`
- `playName`
- `conversionStatus`
- `conversionError`
- `conversionUpdatedAt`

For recordings with a successful `.mp4` remux, `playName` points at the `.mp4`
while `sourceName` still refers to the original recording `.ts`.

Renaming keeps the file extension fixed. For recording source `.ts` files, the
server also renames any sibling converted `.mp4` and conversion-state JSON, and
updates configured file-ingest rows that referenced the old filename.

## Custom Encoding

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/encodings/custom` | Return `{ "ffmpegArgs": "..." }` |
| `PUT` | `/api/v1/encodings/custom` | Persist `{ "ffmpegArgs": "..." }` |

The value is configuration-only today. The native transcoder does not interpret
the stored FFmpeg argument string.

## Health and Status

### `GET /api/v1/dashboard/runtime`

Authenticated dashboard-optimized runtime snapshot. Returns `health` and
`metrics` together so lifecycle-triggered dashboard refreshes can re-sync in a
single round trip instead of fetching `/api/v1/engine/health` and
`/metrics/system` separately.

Query params:

| Param | Default | Notes |
| --- | --- | --- |
| `health_view` | `full` | `summary` trims pipeline/output runtime detail for overview wakes and lighter dashboard refreshes. |
| `metrics_view` | `full` | `summary` trims the host metrics payload to aggregate percentages/rates while preserving `engine` totals. |
| `pipeline_id` | — | Optional selected-pipeline focus. With `health_view=summary`, the response keeps summary health for every pipeline while upgrading the selected pipeline entry to the full detail shape. With `health_view=full`, it scopes the response down to the selected pipeline. |

```json
{
  "health": {
    "status": "ready",
    "pipelines": {}
  },
  "metrics": {
    "generatedAt": "2026-06-30T00:00:00Z",
    "cpu": { "usagePercent": 12 },
    "memory": { "usedPercent": 20 },
    "disk": { "usedPercent": 40 },
    "network": {
      "downloadKbps": 1,
      "uploadKbps": 2
    },
    "engine": {
      "cpuPercent": 3,
      "totalMemoryBytes": 1234,
      "cpuSampleReady": true
    }
  }
}
```

The dashboard currently uses:

- `health_view=summary&metrics_view=full` on the first overview load so static
  disk/interface metadata can be cached client-side
- `health_view=summary&metrics_view=summary` for steady-state overview wakes and
  SSE-triggered refreshes
- `health_view=summary&pipeline_id=<selected>` for selected-pipeline detail
  paths so the dashboard keeps summary liveness for every pipeline while
  returning full runtime detail for the active pipeline in the same snapshot;
  focused pipeline and inspect modes pair that with
  `/api/v1/logs/stream?pipeline_id=<selected>&event_class=lifecycle&include_restream=true`
  so selected-pipeline and restream-wide lifecycle events wake the immediate
  refresh path while unrelated pipelines stay off the wire
- `health_view=full` without `pipeline_id` for publisher-health paths that need
  the full runtime view across pipelines
- lifecycle-SSE-driven output start/stop convergence in pipeline/control modes,
  with a short runtime-refresh fallback when no lifecycle wakeup arrives
- lifecycle-SSE-driven file-ingest start/stop convergence when the pipeline
  dashboard stream is already open, with direct runtime-refresh fallback
  otherwise
- direct local recording-state patching from `POST /recording/start|stop`
  mutation responses, because recording enable/active state is not carried by
  the lifecycle SSE feed
- standalone `/metrics/system` fetches only in modes that do not need runtime
  health in the same refresh

### `GET /api/v1/engine/health`

Authenticated native state snapshot:

Query params:

| Param | Default | Notes |
| --- | --- | --- |
| `view` | `full` | `summary` returns compact pipeline health. |

```json
{
  "generatedAt": "2026-06-20T12:00:00Z",
  "status": "ready",
  "pipelines": {
    "pipeline_id": {
      "input": {
        "status": "on",
        "publishStartedAt": "2026-06-20T11:59:00Z",
        "bytesReceived": 12000000,
        "bitrateKbps": 1600,
        "video": {},
        "audio": {},
        "publisher": {
          "protocol": "srt",
          "remoteAddr": "203.0.113.10:50000",
          "quality": {
            "srtBonded": true,
            "srtGroupMemberCount": 2,
            "srtGroupConnectedMembers": 2,
            "srtGroupActiveMembers": 1,
            "srtGroupBrokenMembers": 0
          }
        }
      },
      "outputs": {
        "output_id": {
          "status": "failed",
          "phase": "failed",
          "lastError": "connection reset by peer",
          "failurePhase": "send",
          "endedAt": "2026-06-20T12:00:05Z",
          "endedAgeMs": 250
        }
      },
      "recording": { "enabled": false, "active": false }
    }
  },
  "srtListener": {
    "bondingAvailable": false,
    "udpRxQueueBytes": 0,
    "udpRxQueuePeakBytes": 0,
    "udpDrops": 0
  },
  "rtmpListener": {
    "acceptErrors": 0,
    "fdExhaustionErrors": 0
  },
  "runtimeLimits": {
    "nofile": {
      "configured": 65536,
      "soft": 65536,
      "hard": 65536,
      "satisfied": true
    }
  },
  "hostSettings": [
    {
      "key": "runtime.nofile",
      "label": "Open file descriptors",
      "current": 65536,
      "required": 65536,
      "unit": "fds",
      "status": "ok",
      "detail": "hard limit 65536"
    },
    {
      "key": "net.core.rmem_max",
      "label": "Kernel receive buffer ceiling",
      "current": 26214400,
      "required": 26214400,
      "unit": "bytes",
      "status": "ok",
      "detail": "needed for SRT UDP receive buffers"
    },
    {
      "key": "net.core.wmem_max",
      "label": "Kernel send buffer ceiling",
      "current": 8388608,
      "required": 8388608,
      "unit": "bytes",
      "status": "ok",
      "detail": "needed for SRT UDP send buffers"
    },
    {
      "key": "runtime.tokio.worker_threads",
      "label": "Tokio async workers",
      "current": 2,
      "required": null,
      "unit": "threads",
      "status": "ok",
      "detail": "async scheduler worker count; too many workers increased migrations and cache misses in MSR profiling"
    },
    {
      "key": "runtime.tokio.max_blocking_threads",
      "label": "Tokio blocking thread cap",
      "current": 512,
      "required": null,
      "unit": "threads",
      "status": "ok",
      "detail": "upper bound for spawn_blocking work such as SRT handshakes and epoll waiters; protects ramp-up latency without unbounded idle thread footprint"
    },
    {
      "key": "runtime.cpu.available_parallelism",
      "label": "Available CPU parallelism",
      "current": 6,
      "required": null,
      "unit": "cpus",
      "status": "ok",
      "detail": "basis for default Tokio worker sizing before workload-specific tuning"
    },
    {
      "key": "runtime.cpu.allowed_list",
      "label": "Allowed CPU mask",
      "current": "0-5",
      "required": null,
      "unit": "cpuset",
      "status": "ok",
      "detail": "process scheduler affinity (6 CPUs); container cpusets can make this smaller than the host"
    },
    {
      "key": "runtime.cpu.cgroup_max",
      "label": "Cgroup CPU quota",
      "current": "max 100000",
      "required": null,
      "unit": "quota",
      "status": "ok",
      "detail": "no cgroup CPU quota; scheduling is cpuset/host limited"
    }
  ]
}
```

See [Observability](observability.md) for field derivation, publisher quality,
and diagnostic check details.

### `GET /healthz`

Public liveness response:

```json
{ "status": "ok" }
```

### `GET /metrics/system`

Authenticated JSON containing host CPU, host memory, disk, host-wide network
rates, and an `engine` object for restream self metrics. `engine.cpuPercent`
and `engine.totalMemoryBytes` include the restream process plus child FFmpeg
processes launched by restream; `restream*` and `externalFfmpeg*` fields provide
the breakdown. This is not Prometheus text format.
Query params:

| Param | Default | Notes |
| --- | --- | --- |
| `view` | `full` | `summary` trims steady-state dashboard polls down to aggregate percentages/rates plus engine totals. The first dashboard load still uses `full` so static disk/interface metadata can be cached client-side. |

This route remains the generic host-metrics surface. The dashboard now prefers
`/api/v1/dashboard/runtime` whenever it also needs engine health in the same
refresh.

### `GET /api/v1/engine`

Authenticated build/runtime information:

```json
{
  "restream": {
    "version": "0.1.0",
    "commit": "abc558b",
    "nativeBuildId": "..."
  },
  "toolchain": {
    "rustc": "1.96.0",
    "target": "x86_64-unknown-linux-gnu",
    "llvm": "22.1.2",
    "gccRuntime": "13.3.0"
  },
  "nativeLibraries": {
    "ffmpeg": {
      "version": "8.1.2",
      "configuration": "... --enable-x86asm ...",
      "license": "GPL version 2 or later",
      "x86Assembly": true
    },
    "srt": {
      "version": "1.5.5",
      "buildVersion": "1.5.5",
      "license": "MPL-2.0",
      "bondingAvailable": true
    },
    "mbedtls": {
      "version": "Mbed TLS 3.6.6",
      "buildVersion": "3.6.6",
      "license": "Apache-2.0"
    },
    "sqlite": { "version": "3.x", "sourceId": "...", "license": "blessing" },
    "x264": {
      "version": "0.164.x",
      "license": "GPL-2.0-or-later",
      "versionSource": "linked pkg-config metadata at build time"
    },
    "x265": {
      "version": "3.x",
      "license": "GPL-2.0-or-later",
      "versionSource": "linked pkg-config metadata at build time"
    }
  },
  "sbom": {
    "format": "CycloneDX",
    "specVersion": "1.5",
    "endpoint": "/api/v1/engine/sbom",
    "componentCount": 100,
    "rustComponentCount": 85,
    "nativeComponentCount": 16,
    "nativeComponents": ["libavcodec", "..."],
    "licensesIncluded": true
  },
  "os": {
    "platform": "linux",
    "arch": "x86_64",
    "hostname": "host",
    "kernelVersion": "6.x",
    "uptime": 12345,
    "totalMem": 17179869184,
    "cpu": {
      "modelName": "13th Gen Intel(R) Core(TM) i9-13900H",
      "logicalCpus": 20,
      "physicalCores": 10,
      "threadsPerCore": 2.0,
      "virtualization": "VT-x",
      "hypervisorDetected": true,
      "hypervisorVendor": "Microsoft",
      "flags": ["sse4_1", "sse4_2", "avx", "avx2", "fma", "aes", "vmx", "hypervisor"]
    }
  }
}
```

For native libraries that expose both `version` and `buildVersion`, `version` is
queried from the library loaded by the running process and `buildVersion` is the
version resolved by the build script at compile time. They should normally
match; a mismatch is a packaging/linking diagnostic.

Native versions are obtained from the running libraries where they expose a
runtime API. x264 and x265 have no public runtime version call, so their exact
linked pkg-config versions are embedded at build time and labeled accordingly.

The `os.cpu` object is intentionally a production-debug subset rather than an
`lscpu` clone. It identifies the CPU model, core/thread topology, virtualization
context, and acceleration features that can explain codec throughput, WSL/cloud
behavior, and native-library performance differences.

### `GET /api/v1/engine/sbom`

Authenticated CycloneDX 1.5 JSON software bill of materials. The response uses
content type `application/vnd.cyclonedx+json; version=1.5` and contains:

- the Restream application component and build identity;
- every resolved normal/runtime Rust crate from Cargo's locked dependency
  graph, including version, Cargo package URL, source, and declared license;
- FFmpeg component libraries, SRT, libmbedcrypto, SQLite, x264, x265, glibc
  when applicable, Rust's standard library, libstdc++, and libgcc;
- runtime-reported versions where an API exists, with explicit provenance for
  build-resolved versions;
- SPDX license expressions or `NOASSERTION` when upstream metadata does not
  declare a license.

The SBOM describes software present in the running artifact. It intentionally
does not include development-only or benchmark dependencies.

## HLS Pull

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/hls/:pipelineId` | Playlist alias |
| `GET` | `/hls/:pipelineId/index.m3u8` | Primary media playlist |
| `GET` | `/hls/:pipelineId/master.m3u8` | Master playlist with alternate audio |
| `GET` | `/hls/:pipelineId/seg<N>.m4s` | Video media segment alias |
| `GET` | `/hls/:pipelineId/video/index.m3u8` | Video-only media playlist |
| `GET` | `/hls/:pipelineId/video/init.mp4` | Video init segment |
| `GET` | `/hls/:pipelineId/video/seg<N>.m4s` | Video media segment |
| `GET` | `/hls/:pipelineId/audio/:trackIndex/index.m3u8` | Audio-only media playlist |
| `GET` | `/hls/:pipelineId/audio/:trackIndex/init.mp4` | Audio init segment |
| `GET` | `/hls/:pipelineId/audio/:trackIndex/seg<N>.m4s` | Audio media segment |
| `GET` | `/hls/inputs/:inputId/master.m3u8` | Input-scoped master playlist |
| `GET` | `/hls/inputs/:inputId/video/index.m3u8` | Input-scoped video playlist |
| `GET` | `/hls/inputs/:inputId/video/init.mp4` | Input-scoped video init segment |
| `GET` | `/hls/inputs/:inputId/video/seg<N>.m4s` | Input-scoped video media segment |
| `GET` | `/hls/inputs/:inputId/audio/:trackIndex/index.m3u8` | Input-scoped audio playlist |
| `GET` | `/hls/inputs/:inputId/audio/:trackIndex/init.mp4` | Input-scoped audio init segment |
| `GET` | `/hls/inputs/:inputId/audio/:trackIndex/seg<N>.m4s` | Input-scoped audio media segment |

Responses:

- playlist: `application/vnd.apple.mpegurl`
- video segment / init: `video/mp4`
- audio segment / init: `audio/mp4`
- `404`: no active store, no completed segments, or evicted segment
- `400`: invalid segment filename

These routes require the dashboard session cookie. Input-scoped stores are
created for connected inputs independently of selection, allowing an operator
to inspect a warm standby without forwarding it into pipeline outputs.

All HLS routes respond with `Access-Control-Allow-Origin: *` and allow `GET`,
`OPTIONS`, `Content-Type`, and `Range` so browser-based players on other
origins can pull segments and playlists without CORS preflight errors.

## Operator v1 Endpoints

All `/api/v1` routes require the session cookie.

### `GET /api/v1/engine`

Authenticated canonical engine/runtime status envelope for the frontend control
plane.

### `GET /api/v1/engine/health`

Authenticated engine health snapshot. Returns the same pipeline/ingest/output
health model documented above on the v1 authenticated control-plane surface.

Query params:

| Param | Default | Notes |
| --- | --- | --- |
| `view` | `full` | `summary` trims steady-state overview/control polls down to per-pipeline status, bitrate, uptime, recording state, and reconnect/grace flags. Pipeline, inspect, and publisher-quality flows continue using `full`. |

Summary response shape:

```json
{
  "status": "ready",
  "pipelines": {
    "pipeline_id": {
      "input": {
        "status": "on",
        "publishStartedAt": "2026-06-20T11:59:00Z",
        "probeReady": true,
        "probeStatus": "ready",
        "probePendingMs": null,
        "bytesReceived": 12000000,
        "bytesSent": 24000000,
        "readers": 2,
        "bitrateKbps": 1600,
        "publisher": {
          "protocol": "srt",
          "remoteAddr": "203.0.113.10:50000"
        },
        "disconnectGraceActive": false,
        "disconnectGraceRemainingMs": null
      },
      "outputs": {
        "output_id": {
          "status": "running",
          "uptimeSecs": 42.5,
          "totalSize": 16000000,
          "bitrateKbps": 1500,
          "retrying": false
        }
      },
      "recording": { "enabled": false, "active": false }
    }
  },
  "runtimeLimits": {
    "nofile": {
      "configured": 65536,
      "soft": 65536,
      "hard": 65536,
      "satisfied": true
    }
  },
  "hostSettings": [
    {
      "key": "runtime.nofile",
      "label": "Open file descriptors",
      "current": 65536,
      "required": 65536,
      "unit": "fds",
      "status": "ok",
      "detail": "hard limit 65536"
    },
    {
      "key": "runtime.cpu.allowed_list",
      "label": "Allowed CPU mask",
      "current": "0-5",
      "required": null,
      "unit": "cpuset",
      "status": "ok",
      "detail": "process scheduler affinity (6 CPUs); container cpusets can make this smaller than the host"
    }
  ]
}
```

### `GET /api/v1/overview`

Engine-wide operator summary: pipeline counts, alert rollup, and listener state.

```json
{
  "generatedAt": "...",
  "totalPipelines": 3,
  "activePipelines": 2,
  "degradedPipelines": 0,
  "failedOutputs": 0,
  "alertCount": { "critical": 0, "warning": 1 },
  "srtListener": { ... }
}
```

### `GET /api/v1/alerts`

Aggregate alerts across all pipelines. Each alert carries `id`, `severity`,
`scope`, `evidence`, `recommendedAction`, `firstSeen`, and `lastSeen` fields.
Sorted Critical-first. `firstSeen` is stamped on first observation;
`lastSeen` updates on every subsequent observation. Resolved alerts are
pruned automatically. Engine-level alerts include SRT UDP drops, RTMP listener
file-descriptor exhaustion, and a runtime nofile limit below the configured
target.

```json
{
  "generatedAt": "...",
  "alerts": [ { "id": "...", "severity": "Warning", ... } ]
}
```

### `GET /api/v1/events`

Lifecycle event log. Query params: `pipeline_id` (optional filter),
`limit` (default 100, max 1000).

```json
{
  "generatedAt": "...",
  "events": [ { "seq": 1, "kind": "IngestConnected", "pipelineId": "...", "timestamp": "..." } ]
}
```

### `GET /api/v1/pipelines/:pipelineId/summary`

Operator-focused pipeline view: source state, output rollup, recording,
HLS preview, alerts. Returns 404 for unknown pipeline IDs.

### `GET /api/v1/pipelines`

Authenticated pipeline list with ingest URLs and configured pipeline metadata.

### `GET /api/v1/pipelines/:pipelineId`

Authenticated pipeline detail endpoint. Returns one pipeline plus its
configured outputs. Returns 404 for unknown pipeline IDs.

### `GET /api/v1/pipelines/:pipelineId/alerts`

Alerts for a single pipeline. Same alert shape as the aggregate endpoint.

### `GET /api/v1/engine/resource-map`

Authenticated resource attribution snapshot for Inspect and agent workflows.
Without query params it returns `scope.kind = "runtime"` for the whole restream
runtime. With `pipeline_id=<id>` it returns `scope.kind = "pipeline"` and filters
stage/output nodes to that pipeline.

Query parameters:

- `view=grouped|summary|detail`: defaults to `grouped`. `summary` returns only
  summary counters and no nodes. `grouped` collapses high-cardinality resources
  such as outputs by kind/protocol/execution. `detail` returns top individual
  nodes and includes detailed memory-accounting payloads.
- `top_n=<n>`: caps returned nodes, default `25`, maximum `200`.

The summary contains measured process and child-process fields such as CPU,
RSS, thread count, file descriptor count, child FFmpeg count, and active SRT
sender thread permits. Nodes include execution ownership (`tokio_task`,
`os_thread`, `child_process`, `shared`, or `process`) plus memory attribution
with a confidence marker:

- `measured`: process RSS, child FFmpeg RSS, process thread/fd counts
- `derived`: ring payload stats, AVIO queue lengths, stage/egress counters
- `estimated`: overheads that are intentionally not assigned to exact nodes

Responses include `limits.totalNodeCount`, `limits.returnedNodeCount`, and
`limits.truncatedNodeCount` so large fleets can show that the view is grouped or
capped. Agent context uses summary mode by default; investigation responses use
grouped mode unless a future explicit drill-down tool asks for detail.

### `GET /api/v1/pipelines/:pipelineId/graph`

Authenticated pipeline graph endpoint. Returns 404 for unknown pipeline IDs.

### `POST /api/v1/pipelines/:pipelineId/diagnostics/run`

Authenticated JSON endpoint returning a complete, ordered diagnostics report.

### `GET /api/v1/settings`

Authenticated settings/configuration read endpoint.
Supports `?jobs=latest` for consumers that only need the newest job per output,
and `?view=dashboard` for the slim dashboard config shape used by runtime
overview/control flows. Responses include `backendPolicy`:

```json
{
  "backendPolicy": {
    "internalVideoPresets": false,
    "internalHevcToH264": false,
    "internalHlsPreview": false,
    "internalComplexAudio": false
  }
}
```

### `PATCH /api/v1/settings`

Authenticated settings/configuration update endpoint. The `backendPolicy`
object is optional; when supplied it is persisted and becomes the runtime policy
for newly started or reconciled transcoding stages.

## Engineer v1 Endpoints

All engineer endpoints require the session cookie.

### `GET /api/v1/engine/telemetry`

Engine-wide telemetry: all active ingests, processing stages with throughput
counters, egresses, and transcoder buffer count.

```json
{
  "generatedAt": "...",
  "ingests": [
    { "pipelineId": "...", "protocol": "rtmp", "uptimeSecs": 42.5, "bytesReceived": 12345678, "metrics": { ... } }
  ],
  "stages": [
    { "stageKey": "pipe1:video:720p", "pipelineId": "pipe1", "kind": "video:720p", "metrics": { "packetsIn": 100, "packetsOut": 100, "bytesIn": 50000, "bytesOut": 30000, "processingUs": 1200 }, "pipeMetrics": { ... } }
  ],
  "egresses": [
    {
      "outputId": "...",
      "pipelineId": "...",
      "protocol": "rtmp",
      "targetUrl": "rtmp://...",
      "targetAddr": "203.0.113.10:1935",
      "status": "running",
      "phase": "sending",
      "uptimeSecs": 42.0,
      "bytesOut": 9876543,
      "lastProgressAt": "...",
      "lastProgressAgeMs": 250,
      "lastError": null,
      "lastErrorAt": null,
      "failurePhase": null,
      "fabric": false,
      "shardId": null,
      "quality": {
        "tcpCongestionAlgorithm": "cubic",
        "tcpRttMs": 12.4,
        "tcpSendRateMbps": 4.8,
        "tcpNotsentBytes": 0,
        "tcpSndCwnd": 10,
        "tcpTotalRetrans": 0,
        "mbpsSendRate": null,
        "srtBonded": null
      },
      "metrics": { ... }
    }
  ],
  "activeTranscoderBuffers": 2
}
```

### `GET /api/v1/pipelines/:pipelineId/telemetry`

Pipeline-scoped telemetry: ingest, source ring buffer, processing stages,
and egresses for a single pipeline.

```json
{
  "generatedAt": "...",
  "pipelineId": "...",
  "ingest": { "protocol": "srt", "uptimeSecs": 10.0, "bytesReceived": 500000, "metrics": { ... } },
  "sourceRing": { "fill": 42, "capacity": 8192, "readers": [ { "name": "...", "lagSlots": 5, "overflowCount": 0, "packetAgeMs": 120 } ] },
  "stages": [ { "kind": "video:720p", "metrics": { ... } } ],
  "egresses": [ { "outputId": "...", "uptimeSecs": 10.0, "bytesOut": 400000 } ]
}
```

### `GET /api/v1/stages/:stageKey/telemetry`

Single-stage telemetry by stage key (e.g. `pipe1:video:720p`). Returns raw
throughput counters and subprocess pipe metrics (if present). Returns 404
if the stage is not currently active.

```json
{
  "generatedAt": "...",
  "stageKey": "pipe1:video:720p",
  "pipelineId": "pipe1",
  "kind": "video:720p",
  "metrics": { "packetsIn": 100, "packetsOut": 100, "bytesIn": 50000, "bytesOut": 30000, "processingUs": 1200 },
  "pipeMetrics": null
}
```
