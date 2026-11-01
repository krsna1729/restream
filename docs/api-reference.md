# Restream API reference

Every HTTP route the dashboard server registers in `src/api/router.rs`,
grouped by resource. Base URL: `http://localhost:3030` (the HTTP port is
`RESTREAM_HTTP_PORT`, see [Configuration](configuration.md)).

## Contents

- [Conventions](#conventions)
- [Route map](#route-map)
- [Authentication](#authentication)
- [Configuration and discovery](#configuration-and-discovery)
- [Pipelines](#pipelines)
- [Outputs](#outputs)
- [Process logs](#process-logs)
- [Probe, graph, and diagnostics](#probe-graph-and-diagnostics)
- [Agent plane](#agent-plane)
- [Recording](#recording)
- [File ingest](#file-ingest)
- [Media files](#media-files)
- [Custom encoding](#custom-encoding)
- [Runtime state](#runtime-state)
- [Picture and sound checks](#picture-and-sound-checks)
- [Health and status](#health-and-status)
- [Operator views](#operator-views)
- [Notifications](#notifications)
- [Schedules](#schedules)
- [Share links](#share-links)
- [Telemetry](#telemetry)
- [HLS pull](#hls-pull)
- [HLS push ingest](#hls-push-ingest)
- [Static pages](#static-pages)

## Conventions

- JSON bodies and responses use camelCase. The one exception is
  `GET /api/v1/monitoring/youtube-status`, which answers in snake_case.
- Path parameters are written `:name` here; the router writes `{name}`.
- Every `/api/*`, `/hls/*` and `/media/*` route needs the `session` cookie
  from login, except `POST /api/v1/auth/login`. Each handler checks the cookie
  itself (`require_authenticated` in `src/api/state.rs`); there is no
  router-level gate. A missing or expired session gets `401`.
- There is no bearer-token or API-key auth on this server: only the cookie.
- An unknown `/api/*` path gets a JSON `404`
  (`{"error": "API route not found", "path", "status": 404}`).
- Every response carries `X-Content-Type-Options: nosniff` and
  `X-Frame-Options: SAMEORIGIN` unless the handler set them.
- Only the static documents are gzip-compressed. JSON APIs and media are not.
- A successful request with any method other than `GET`, `HEAD` or `OPTIONS`
  wakes the reconciler and marks the runtime state changed
  (`wake_reconciler_after_mutation`). HLS push uploads do not.

Request limits:

| Limit | Value | Source |
|---|---|---|
| Request body (default) | 4 MiB, else `413` | `DEFAULT_API_BODY_LIMIT_BYTES` |
| `POST /api/v1/agent/operations` body | 256 KiB | `AGENT_OPERATION_BODY_LIMIT_BYTES` |
| `POST /api/v1/media/upload` body | 8 GiB | `MAX_MEDIA_UPLOAD_BYTES` |
| `name`, `label`, `filename` fields | 256 bytes | `MAX_NAME_LEN` |
| `url`, `sourceUrl`, `monitoringUrl` | 2048 bytes | `MAX_URL_LEN` |
| Output `config` JSON | 512 bytes | `MAX_OUTPUT_CONFIG_LEN` |
| `streamKey` | 256 bytes | `MAX_STREAM_KEY_LEN` |
| `password` fields | 1024 bytes | `MAX_PASSWORD_LEN` |
| File-ingest `startTime` | 64 bytes | `src/api/file_ingest.rs` |

A field over its limit gets `400` with
`{"error": "<field> exceeds maximum length of <n> bytes"}`.

## Route map

How the router is built: one listener serves the session-gated app router and
the HLS push ingest router side by side.

```mermaid
flowchart TD
    serve["http_server::serve<br/>(half-close only for /ingest/hls/)"] --> app["create_router"]
    serve --> ingest["create_hls_ingest_router"]
    app --> layers["apply_standard_layers<br/>4 MiB body limit + security headers"]
    app --> roles["authorize_and_audit<br/>route role policy + audit log"]
    roles --> wake["wake_reconciler_after_mutation"]
    layers --> staticDocs["create_compressed_static_router<br/>/login, /output.css, SPA fallback"]
    layers --> api["/api/v1/* JSON handlers"]
    layers --> hls["create_hls_router<br/>/hls/*"]
    layers --> media["/media/:filename"]
    layers --> notfound["/api/* → api_not_found_handler"]
    ingest --> push["hls_ingest_handler<br/>own body bounds, stream-key auth"]
```

| Group | Routes | Section |
|---|---|---|
| Auth, team and security | `/api/v1/auth/*`, `/api/v1/users*`, `/api/v1/audit`, `/api/v1/security/rate-limits*` | [Authentication](#authentication) |
| Settings and discovery | `/api/v1/settings`, `/stream-keys`, `/audio-caps`, `/monitoring/youtube-status` | [Configuration and discovery](#configuration-and-discovery) |
| Pipelines and inputs | `/api/v1/pipelines`, `…/inputs*` | [Pipelines](#pipelines) |
| Outputs | `…/outputs*` | [Outputs](#outputs) |
| Logs | `/api/v1/logs`, `/api/v1/logs/stream` | [Process logs](#process-logs) |
| Probe, graph, diagnostics, MoQ token | `…/probe`, `…/graph`, `…/diagnostics/*`, `…/moq-token` | [Probe, graph, and diagnostics](#probe-graph-and-diagnostics) |
| Agent plane | `/api/v1/agent/*` | [Agent plane](#agent-plane) |
| Recording | `…/recording/start`, `…/recording/stop` | [Recording](#recording) |
| Picture and sound checks | `…/stream-qa` | [Picture and sound checks](#picture-and-sound-checks) |
| File ingest | `…/file-ingest`, `/api/v1/ingests*` | [File ingest](#file-ingest) |
| Media library | `/api/v1/media*`, `/media/:filename` | [Media files](#media-files) |
| Custom encoding | `/api/v1/encodings/custom` | [Custom encoding](#custom-encoding) |
| Runtime state | `/api/v1/state`, `/state/stream`, `/fleet/history` | [Runtime state](#runtime-state) |
| Health and host | `/healthz`, `/metrics/system`, `/api/v1/engine*`, `/dashboard/runtime`, `/capacity/scenario` | [Health and status](#health-and-status) |
| Operator views | `/overview`, `/alerts`, `/events`, `…/summary`, `…/alerts`, `/engine/resource-map` | [Operator views](#operator-views) |
| Notifications | `/api/v1/notifications/*` | [Notifications](#notifications) |
| Schedules | `/api/v1/schedules*` | [Schedules](#schedules) |
| Share links | `…/share-links`, `/api/v1/share-links/:id`, `/watch/:token` | [Share links](#share-links) |
| Telemetry | `/measure`, `/engine/telemetry`, `…/telemetry`, `/stages/:stageKey/telemetry` | [Telemetry](#telemetry) |
| HLS pull | `/hls/*` | [HLS pull](#hls-pull) |
| HLS push | `/ingest/hls/:streamKey/*path` | [HLS push ingest](#hls-push-ingest) |
| Static | `/login`, `/logo.png`, `/output.css`, redirects, SPA fallback | [Static pages](#static-pages) |

## Authentication

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/v1/auth/login` | Check `{ "username"?, "password" }`, create a session, set the cookie |
| `POST` | `/api/v1/auth/logout` | Delete the current session and clear the cookie |
| `POST` | `/api/v1/auth/change-password` | Change the password from `{ "currentPassword", "newPassword" }` |
| `POST` | `/api/v1/auth/dismiss-password-change` | Hide the bootstrap "change the password" reminder |
| `GET` | `/api/v1/auth/me` | The session's member, role, view and effective role |
| `POST` | `/api/v1/auth/view-as` | Act as a lower role, `{ "role": "viewer" \| "operator" \| null }` |
| `GET`, `POST` | `/api/v1/users` | List members (each with `lastActiveMs`, the later of its newest sign-in and audited change, when one is kept); add one from `{ "username", "password", "role" }` (admin) |
| `PATCH`, `DELETE` | `/api/v1/users/:id` | Change `{ "role"?, "disabled"?, "password"? }`; remove (admin) |
| `GET` | `/api/v1/audit?limit&before` | Change requests, newest first (admin) |
| `GET` | `/api/v1/security/rate-limits` | List tracked failure counters and bans |
| `POST` | `/api/v1/security/rate-limits/reset` | Clear counters, optionally by `scope` and `ip` |

How a session is created and then checked on each request:

```mermaid
sequenceDiagram
    participant B as Browser
    participant L as login_post_handler
    participant S as AppState
    participant DB as auth_service (SQLite)
    B->>L: POST /api/v1/auth/login {password}
    L->>S: login_ban_remaining(dashboard-login, ip)
    alt client banned
        L-->>B: 429 "Try again in N seconds"
    end
    L->>DB: get_password_hash
    L->>L: verify_password (scrypt, spawn_blocking)
    alt wrong password
        L->>S: record_security_failure
        L-->>B: 401 Incorrect password
    end
    L->>DB: create_session(sha256(token))
    L->>S: add_session_hash
    L-->>B: 200 {ok:true} + Set-Cookie session=token
    B->>S: later request with Cookie: session=token
    S->>S: is_authenticated: hash in memory set?
    S->>DB: get_session_created_at
    alt missing or older than 30 days
        S-->>B: 401 Unauthorized
    else valid
        S-->>B: handler runs
    end
```

- The cookie is `session=<64 hex>; HttpOnly; Path=/; SameSite=Strict;
  Max-Age=2592000`, plus `; Secure` when `RESTREAM_SECURE_SESSION_COOKIES` is
  true. The server stores only the SHA-256 of the token.
- A session lives 30 days from creation (`SESSION_MAX_AGE_SECONDS`). Expired
  rows are pruned by the reconciler's housekeeping.
- Failed logins count under the `dashboard-login` rate-limit scope. The
  limits are the `ingestSecurity` settings (failure limit, window, ban).
- Change password: the new password must be at least 12 bytes
  (`MIN_DASHBOARD_PASSWORD_LEN`), else `400`. A wrong current password gets
  `403`. On success every other session is revoked; the caller's stays, and
  the bootstrap reminder is dismissed.
- Logout, change password and dismiss return `{ "ok": true }`.

### Team members and roles

Each member signs in with `{ "username", "password" }`. An absent username,
or `admin`, is the built-in admin: its password is the dashboard password
above, its role is always admin, and it cannot be disabled or removed. Other
members live in the `users` table with a scrypt hash and a role. A session
row carries the member (`user_id`, null for the built-in admin) and the role
it views as (`view_as`).

Roles are ordered: `viewer` < `operator` < `admin`.

| Role | May |
|---|---|
| `viewer` | Read every dashboard route except settings, stream keys, security, the team and the audit log |
| `operator` | Also change the plant (pipelines, inputs, outputs, media, file ingest, recording, the agent plane) and read settings and stream keys |
| `admin` | Also change settings, security and custom encodings, and manage the team and read the audit log |

One layer in front of every `/api/v1/` handler (`authorize_and_audit` in
`src/api/team.rs`) enforces `required_role(method, path)`
(`src/application/team.rs`). A request without a valid session passes
through to its handler, which answers `401` as before. A session whose role
is too low gets `403 { "error", "requiredRole", "effectiveRole" }` before
any handler runs. Reads any session may make skip the lookup.

- **View as.** `POST /api/v1/auth/view-as` stores a lower role on the
  session; every check then uses `min(role, viewAs)`, so the preview is what
  that role can do, not a hidden-button approximation. `null`, or the
  member's own role, ends it. It returns the `GET /auth/me` body:
  `{ "userId", "username", "role", "viewAs", "effectiveRole" }`.
- **Audit.** Every request that is not a read (`GET`/`HEAD`/`OPTIONS`) is
  recorded with the member, the effective role, the method, the path and
  the response status, including the `403`s. The log keeps the newest
  10,000 entries (`AUDIT_LOG_CAP`). `GET /api/v1/audit` returns
  `{ "entries": [{ "id", "atMs", "username", "role", "method", "path", "status" }] }`;
  `limit` is clamped to 1–500 (default 100), and `before` pages back from an
  entry id.
- **Members.** Usernames are 1–64 letters, digits, `.`, `_`, `-` or `@`; a
  taken one is `409`. Passwords need 12 bytes. A role change applies on the
  member's next request. Disabling a member, or an admin resetting their
  password, ends their sessions. A member's own password change
  (`/auth/change-password`) ends their other sessions only.

Rate limits: `GET` returns
`{ "attempts": [{ "scope", "ip", "failureCount", "banned", "banRemainingMs" }] }`.
`POST …/reset` takes `{ "scope"?, "ip"? }` and returns
`{ "ok": true, "removed": n }`; an unknown scope is `400`. Scopes
(`RateLimitScope` in `crates/restream-engine/src/security.rs`): `dashboard-login`,
`rtmp-publish`, `srt-publish`, `srt-read`, `hls-publish`, `moq-publish`.

## Configuration and discovery

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/settings` | Settings plus pipelines, outputs and jobs |
| `PATCH` | `/api/v1/settings` | Update any supplied setting |
| `GET` | `/api/v1/stream-keys` | Selected stream key and RTMP/SRT URLs per pipeline |
| `GET` | `/api/v1/audio-caps` | Platform/protocol audio capability matrix |
| `GET` | `/api/v1/monitoring/youtube-status` | Fetch a YouTube watch page and report its live flags |

### `GET /api/v1/settings`

| Param | Default | Notes |
|---|---|---|
| `jobs` | all | `latest` keeps only the newest job per output |
| `view` | full | `dashboard` drops `ingestSecurity`, `recordingSettings`, `srtIngest` and all jobs |

```json
{
  "serverName": "Name",
  "ingestHost": "stream.example.com",
  "dashboardPasswordChangeRecommended": false,
  "transcodeProfiles": {},
  "ingestSecurity": { "failureLimit": 10, "failureWindowMs": 60000, "banMs": 600000, "trackedIpLimit": 10000 },
  "recordingSettings": { "retainSourceTs": false },
  "srtIngest": { "mode": "plaintext", "pbkeylen": 16, "latencyMs": 250 },
  "pipelines": [],
  "outputs": [],
  "jobs": []
}
```

Each pipeline carries its selected input's RTMP and SRT ingest URLs. Use
`GET /api/v1/pipelines/:pipelineId/inputs` for every input.

### `PATCH /api/v1/settings`

Accepted fields: `serverName`, `ingestHost`, `ingestSecurity`,
`recordingSettings`, `srtIngest`, `transcodeProfiles`. Unknown fields are
ignored. The response is the refreshed settings (the six fields above).

| Field | Rule (else `400`) |
|---|---|
| `serverName` | Not blank |
| `ingestSecurity.*` | Each of `failureLimit`, `failureWindowMs`, `banMs`, `trackedIpLimit` ≥ 1 |
| `srtIngest.mode` | `plaintext` or `encrypted`; `encrypted` needs a 10–79 byte `passphrase` |
| `srtIngest.pbkeylen` | 16, 24 or 32 (default 16) |
| `srtIngest.latencyMs` | 20–8000 (default 250) |
| `transcodeProfiles.*.preset` | `ultrafast` … `placebo` (the ten x264 presets) |
| `transcodeProfiles.*.tune` | Empty or `film`, `animation`, `grain`, `stillimage`, `psnr`, `ssim`, `fastdecode`, `zerolatency` |
| `transcodeProfiles.*.crf` | 0–51 |

### `GET /api/v1/stream-keys`

```json
[
  {
    "key": "sk_…",
    "label": "Stream 1",
    "ingestUrls": {
      "rtmp": "rtmp://stream.example.com:1935/live/sk_…",
      "srt": "srt://stream.example.com:10080?streamid=publish:sk_…"
    }
  }
]
```

Generated stream keys are `sk_` plus 64 random hex characters. They encode
no pipeline, input, role or protocol.

### `GET /api/v1/audio-caps`

Returns `{ "caps": { "<platform>:<protocol>": { "codecs", "maxChannels",
"maxTracks" } }, "platformLabels": { … } }` for `facebook`, `generic`,
`vdocipher` and `youtube` over `hls`, `rtmp`, `rtmps` and `srt`
(`audio_caps_handler` in `src/api/auth.rs`).

### `GET /api/v1/monitoring/youtube-status`

Query `url` (a YouTube watch, share or live URL; at most 2048 bytes). The
server fetches the canonical watch page (5 s timeout, 512 KiB cap) and returns
`{ "canonical_watch_url", "live_now", "live_content", "upcoming", "title" }`.
Only HTTP(S) YouTube or `youtu.be` URLs are accepted. Requests use canonical
`www.youtube.com` URLs and follow only HTTPS redirects within `youtube.com`.
An unsupported URL is `400`; a failed or oversized fetch is `502`.

## Pipelines

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/pipelines` | List pipelines: `{ "pipelines": [...] }` |
| `POST` | `/api/v1/pipelines` | Create a pipeline (`201`) |
| `GET` | `/api/v1/pipelines/:id` | One pipeline plus its outputs: `{ "pipeline", "outputs" }` |
| `PATCH` | `/api/v1/pipelines/:id` | Update a pipeline |
| `DELETE` | `/api/v1/pipelines/:id` | Delete a pipeline |
| `GET` | `/api/v1/pipelines/:pipelineId/inputs` | List inputs with runtime state |
| `POST` | `/api/v1/pipelines/:pipelineId/inputs` | Add a backup or HLS pull input (`201`) |
| `PATCH` | `/api/v1/pipelines/:pipelineId/inputs/:inputId` | Rename, enable/disable or reconfigure an input |
| `DELETE` | `/api/v1/pipelines/:pipelineId/inputs/:inputId` | Delete an unselected backup input |
| `POST` | `/api/v1/pipelines/:pipelineId/inputs/:inputId/promote` | Select and promote an input |

Create and update body:

```json
{
  "name": "Main Feed",
  "streamKey": "sk_…",
  "inputSource": null,
  "srtIngestPolicy": { "mode": "inherit", "latencyMs": 400 },
  "fileIngest": {
    "filename": "recording-1.ts",
    "loopFlag": true,
    "startTime": "00:00:05",
    "liveOptimized": true,
    "targetGopSeconds": 3
  }
}
```

- `name` is required on create and update (one payload type) and must not
  be blank.
- `streamKey`: omitted on create, a random key is generated. On update,
  omitted keeps the key and an empty or blank value issues a new one. A key
  another input already uses is `409`.
- `inputSource` is operator metadata only; it pulls nothing.
- `srtIngestPolicy`: per-pipeline SRT ingest policy. `mode` is `inherit`
  (use the global `srtIngest`), `plaintext` or `encrypted` (10–79 byte
  `passphrase`, `pbkeylen` 16/24/32). `latencyMs` 20–8000 overrides the
  global latency.
- `inputSource` and `fileIngest`: omitted keeps the stored value, `null`
  clears it. A `fileIngest` object creates or replaces the pipeline's file
  source in the same request. Clearing or replacing it stops only the file
  publisher; a live RTMP/SRT publisher on another input keeps its session.

Create (`201`) and update return `{ "message", "pipeline" }`; `pipeline`
includes the normalized `fileIngest` state. Delete returns
`{ "message": "Pipeline <id> deleted" }` or `404`.

Delete removes the row first (outputs and inputs cascade), then wakes the
reconciler. The reconciler stops the pipeline's egresses, ingest sessions,
file-ingest children, stages and HLS runtime. A recirculation output that
still targets the deleted pipeline recreates that runtime on each attempt and
each pass removes it again, so the output retries until its retries are
exhausted.

### Pipeline inputs

A pipeline starts with one primary input and holds at most four
(`MAX_PIPELINE_INPUTS`; a fifth is `409`). Each backup has its own stream key
and URLs. Exactly one enabled input is selected. `role` records how the input
was created; promoting a backup changes selection, not role.

`kind` is `keyed` (default: waits for a publisher), `hlsPull` (the engine
fetches a remote live HLS origin; no listener) or `srtCall` (the engine dials
a remote SRT listener in caller mode; see [SRT call inputs](#srt-call-inputs)). A pull input has `sourceUrl`
and a `variantRule` of `highestBandwidth` (default) or `lowestBandwidth`. The
rule picks the master-playlist variant with the highest or lowest `BANDWIDTH`
among those whose `CODECS` the ingest publishes (H.264 or HEVC video, AAC-LC
audio), or among all variants when none declares such a set.

Keyed create: `{ "label": "Venue encoder B" }`. Patch:
`{ "label": "Venue encoder B", "enabled": true }`. HLS pull create:

```json
{
  "label": "Origin",
  "kind": "hlsPull",
  "sourceUrl": "https://origin.example/live/master.m3u8",
  "variantRule": "highestBandwidth",
  "auth": { "kind": "bearer", "token": "…" }
}
```

Validation:

- `hlsPull` needs an absolute `http`/`https` `sourceUrl` of at most 2048
  bytes, without a `user:password@` part.
- `keyed` must not carry `sourceUrl` or `auth`.
- An unknown `kind`, `variantRule` or `auth.kind` is `400`, not defaulted.
- An empty `label` is `400`.
- `hlsPull` must not carry `pushAuth` other than `{"kind":"none"}`.
- `srtCall` needs `srt://host:port` (options after `?`, at most 2048 bytes)
  and carries no `auth` or `pushAuth`.

Changing an input's `kind` ends the session the old kind held (a keyed
input's pusher is disconnected; a pull input's puller stops), so the new kind
starts clean instead of being refused as `PublisherActive`.

#### Pull credentials

`auth` is one tagged object. Its secrets are stored and used but never
returned.

| `auth.kind` | Payload | Limits |
|---|---|---|
| `none` | none (clears the credential) | must carry no payload |
| `bearer` | `token` | 1–4096 bytes |
| `cookies` | `cookies`, sent as one `Cookie` header | 1–4096 bytes, one line |
| `basic` | `username`, `password` | 1–256 and 1–4096 bytes |
| `headers` | `headers: [{ "name", "value" }]` | ≤ 8 entries; names ≤ 64, values ≤ 1024 bytes |

- Every value must be a valid HTTP header value: no CR, LF, NUL, other
  control byte or non-ASCII byte. Header names must be valid, and `Host`,
  `Content-Length`, `Transfer-Encoding` and `Connection` are refused.
- `basic` is sent as Basic first. If the origin answers `401` with a
  `WWW-Authenticate: Digest` challenge, the puller answers with HTTP Digest
  (`MD5`, `MD5-sess`, `SHA-256`, `SHA-256-sess`; `qop=auth` or none) from then
  on, and once more on `stale=true`. A challenge offering only `auth-int`,
  `userhash` or another algorithm ends the pull with that reason
  (`crates/restream-engine/src/hls_pull/digest.rs`). Use `https` if the origin must not see
  the password in the first request.
- A signed URL or cookies the origin sets itself need no `auth`: the URL
  carries the token and the fetcher keeps a cookie jar.
- On patch, absent `auth` keeps the credential, `none` clears it, any other
  variant replaces it. Disabling a pull input keeps `sourceUrl` and `auth`.

`sourceUrl` is configuration and is returned in full, query included, so a
signed URL's token is visible to any authenticated reader. Use an `auth` kind
for anything that must stay write-only.

#### SRT call inputs

```json
{
  "label": "Venue gateway",
  "kind": "srtCall",
  "sourceUrl": "srt://gateway.example:9000?streamid=cam1&passphrase=…&latency=200"
}
```

The engine dials the listener from ingress Owner 0, over IPv4 when every
listed host has an IPv4 address and otherwise over IPv6 (the Owner binds a
caller socket per family as each is first used). Once the call connects, it publishes to this input
like an SRT publisher; there is no stream-key check. URL options are those of SRT
outputs: `streamid`, `passphrase`, `pbkeylen`, `latency` (milliseconds,
range-checked) and `bond=host:port,…` with `type=backup|broadcast`. A failed
or dropped call is redialled after 1 s, doubling to 30 s while it keeps
failing; disabling, deleting or changing the input hangs the call up. The
`sourceUrl`, passphrase included, is returned in full like an SRT output URL;
the dashboard masks the passphrase on its input cards.

#### HLS push credentials

Keyed inputs may add `pushAuth: {"kind":"basic","username":"encoder",
"password":"…"}` on create or patch. The username is 1–256 UTF-8 bytes,
without a colon; the password is 1–4096 UTF-8 bytes. Passwords are stored
only as salted scrypt hashes, never returned or added to generated URLs.
On patch, omitted `pushAuth` preserves the credential, `{"kind":"none"}`
clears it, and `basic` replaces both fields. Explicitly clear a stored push
credential when changing the source kind to `hlsPull`.

The encoder must send HTTP Basic authorization on every PUT, POST and
DELETE, in addition to the stream key. Missing, malformed or wrong
credentials receive `401` with a Basic challenge before the handler reads
any body byte. No configured credential preserves key-only behavior.
Credential checks run on a bounded blocking lane; saturation returns `503`
with `Retry-After: 1`. Configuration writes can also return `503` while that
lane is busy. Changes apply to the next object, including existing sessions.
Use HTTPS through a TLS proxy: Basic auth is not encrypted on plain HTTP.
Encoders supporting URL userinfo may use
`https://USER:PASSWORD@HOST/ingest/hls/KEY/index.m3u8`, with credentials
percent-encoded as needed; do not paste that URL into logs or shared fields.

#### Input responses

Each input has `id`, `label`, `streamKey`, `role`, `kind`, `sourceUrl`,
`variantRule`, `authKind`, `authUsername`, `pushAuthKind`, `pushAuthUsername`,
`enabled`, `selected`,
`ingestUrls`, `previewUrl` and `runtime`. A keyed input has
`sourceUrl: null`, `variantRule: "highestBandwidth"` and `authKind: "none"`.

`ingestUrls` (`IngestUrls` in `src/application/runtime_state/inputs.rs`) is
`null` for a pull input. For a keyed input:

| Field | Value |
|---|---|
| `rtmp` | `rtmp://HOST:RTMP_PORT/live/KEY` |
| `srt` | `srt://HOST:SRT_PORT?streamid=publish:KEY` |
| `hls` | `http://HOST:HTTP_PORT/ingest/hls/KEY/index.m3u8` |
| `moq` | `https://HOST:MOQ_PORT/KEY` while `RESTREAM_MOQ_INGEST_LISTEN` is set, else `null` |

`HOST` is the `ingestHost` setting. The `hls` URL assumes the listener is
reached directly; behind a TLS proxy or path prefix, use the proxy's scheme,
host and prefix. MoQ publishes authenticate the key under the `moq-publish`
rate-limit scope. `previewUrl` is `/hls/inputs/:inputId/master.m3u8`.

`runtime` holds `connected`, `forwardingState`, protocol, uptime, bytes,
remote address, media metadata and transport quality. A connected unselected
input reports `forwardingState: "standby"`; a promoted one may report
`"awaitingKeyframe"` briefly before `"active"`.

#### Promotion

Promotion persists the selection and returns the input plus `connected`
(whether the target had a live session at request time). The reconciler
applies the switch first in its next output pass. If the target is
connected, the current writer is demoted and drained, then the target replays
its latest complete compressed GOP on its next packet, or waits for its next
keyframe if no complete GOP is cached. Promoting an idle input returns
`connected: false`; its next publisher becomes the source.

## Outputs

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/v1/pipelines/:pipelineId/outputs` | Create an output, stopped (`201`) |
| `PATCH` | `/api/v1/pipelines/:pipelineId/outputs/:outputId` | Update an output |
| `DELETE` | `/api/v1/pipelines/:pipelineId/outputs/:outputId` | Delete an output |
| `POST` | `/api/v1/pipelines/:pipelineId/outputs/:outputId/start` | Set `desiredState=running` |
| `POST` | `/api/v1/pipelines/:pipelineId/outputs/:outputId/stop` | Set `desiredState=stopped` |
| `GET` | `/api/v1/pipelines/:pipelineId/outputs/:outputId/status` | Live or last egress status |

Create and update body (unknown fields are `400`):

```json
{
  "name": "Primary CDN",
  "url": "rtmp://destination.example/live/key",
  "monitoringUrl": "https://www.youtube.com/watch?v=…",
  "config": {
    "video": { "mode": "preset", "preset": "1080p", "codec": "auto" },
    "audio": { "mode": "selectTracks", "tracks": [0] },
    "protocol": { "type": "rtmp", "mode": "enhanced" }
  }
}
```

- Create and update return `{ "message", "output" }`; start and stop return
  `{ "message", "desiredState", "output" }`; delete returns
  `{ "message": "Output deleted" }` or `404`.
- Updating the URL or `config` of an output that wants to run
  (`desiredState: running`, retries not exhausted) is `409`. Stop it first,
  or edit it once its retries are exhausted.
- `monitoringUrl` is optional (`http`, `https` or `srt`); blank means none.
- `config.video.mode`: `source` or `preset` (built-in presets);
  `video.codec`: `auto`, `h264`, `h265`. `custom` is `400`: custom FFmpeg
  arguments are not applied.
- `config.audio.mode`: `all`, `selectTracks`, `downmix` or `remap`.
- `config.protocol` `{ "type": "rtmp", "mode": "legacy" | "enhanced" }` is
  valid only for `rtmp://`/`rtmps://` URLs; omitted means legacy. Enhanced
  RTMP advertises `avc1`, `hvc1` and `mp4a` and sends HEVC as Enhanced FLV
  `hvc1`. Legacy RTMP converts HEVC to H.264 and resolves `codec: auto` to
  H.264.
- Explicit H.265 is `400` for legacy RTMP and HLS outputs. Enhanced RTMP,
  SRT and MoQ carry H.264 and H.265; sink and pipeline outputs follow the
  source. A preset with `codec: auto` encodes H.264 for legacy RTMP, HLS and
  MoQ (`ProtocolCapabilities` in `crates/restream-domain/src/domain/output_spec/config.rs`).
- A `pipeline://` output must use source video and passthrough audio; its
  target and cycle freedom are checked before it is saved.

URL schemes (`OutputUrlScheme` in `crates/restream-domain/src/domain/output_spec/protocol.rs`); any
other scheme is `400`:

| Scheme | Egress | `protocol` label |
|---|---|---|
| `rtmp://` | RTMP; URL must be `rtmp://host[:port]/app/stream-key` | `rtmp` |
| `rtmps://` | RTMP over TLS; same URL rule | `rtmp` |
| `srt://` | SRT, MPEG-TS | `srt` |
| `hls://` | Local in-memory HLS segmenter | `hls` |
| `http://`, `https://` | HLS HTTP PUT upload | `hls` |
| `moqt://` | MoQ publish to a relay (moq-lite or IETF MOQT) | `moq` |
| `sink://` | Fabric sink: counts and discards media | `sink` |
| `pipeline://` | In-process recirculation into another pipeline's input | `pipeline` |

- RTMP keys: when the query holds a `/`, the key is what follows the last
  `/` and the app keeps the query
  (`rtmp://host/live2?backup=1/stream-key`). An RTMP output stored without a
  key fails at every start until its retries are exhausted.
- SRT `latency` is the proposed TSBPD delay in milliseconds. Values outside
  `20..=8000` or non-integers are `400`; see
  [SRT egress URL parameters](configuration.md#recognized-srt-egress-url-parameters).
- `moqt://host[:port][/path][?broadcast=&ca=&insecure=&version=]` (`parse_moq_url`
  in `crates/restream-moq/src/url.rs`): the port defaults to 443. `broadcast`
  names the broadcast (default: pipeline id), `ca` names a PEM file of extra
  trusted CAs, and `insecure=1|true|0|false` skips certificate checks.
  `version=moqt-19` or another supported MoQ ALPN offers only that version;
  omission offers every supported version. Those four keys stay local;
  other query pairs (for example `?jwt=`) remain in the relay session path.
  Userinfo, an empty `broadcast` or `ca`, an invalid `insecure` value or
  an unknown `version` is `400`.
  A `ca` file must be relative to the operator's `RESTREAM_MOQ_CA_DIR`
  (for example `ca=relay.pem`); absolute paths, `..` components and symlinks
  escaping that directory fail output startup. Unset/empty configuration
  denies all `ca` reads, even with `insecure=1`. These filesystem-policy
  failures appear as bounded output-start errors, never file contents or
  OS path errors. Without `ca`, normal public trust roots are unchanged.
- HTTP/HTTPS HLS upload shares one local segmenter per pipeline. Each output
  starts at the newest segment, PUTs each new segment, then PUTs a playlist
  of the last three acknowledged segments (media sequence from 0 per
  attempt). On stop it PUTs a final playlist ending in `#EXT-X-ENDLIST`.
  The rules follow the YouTube HLS ingestion guide and the Akamai MSL HTTP
  ingest requirements (`crates/restream-engine/src/hls/upload_policy.rs` module doc).

### Output lifecycle

`desiredState` is only intent (`running` or `stopped`). The reconciler turns
it into egress tasks. How a create and start become a running egress, and how
a failing one retries:

```mermaid
sequenceDiagram
    participant C as Client
    participant H as outputs handlers
    participant DB as SQLite outputs
    participant R as Reconciler::output_pass
    participant E as EgressReconciler
    participant T as EgressTask
    C->>H: POST …/outputs
    H->>DB: create_output (desired stopped)
    H-->>C: 201 {message, output}
    C->>H: POST …/outputs/:id/start
    H->>DB: request_start (desired running, retries_exhausted = 0)
    H-->>C: 200 {desiredState: running}
    Note over H,R: wake_reconciler_after_mutation → ReconcileRequests::wake
    R->>E: reconcile(outputs)
    E->>E: decide_output_start_action
    alt no ingest / probe pending
        E-->>R: skip (probe pending re-checks next interval)
    else StartNow
        E->>T: start → tokio::spawn(EgressTask::run)
    end
    T-->>E: attempt fails: last_failed = (now, retries)
    Note over T,R: the egress ending emits EgressStopped, which wakes a pass
    R->>E: reconcile(outputs)
    alt retries below RESTREAM_OUTPUT_MAX_RETRIES
        E-->>R: WaitRetry → wake_at(backoff end)
        R->>E: reconcile at deadline → StartNow
    else retries spent
        E->>DB: mark_output_retries_exhausted
    end
```

- Output passes run when woken and as a backstop every 10 reconcile
  intervals. Wakes: a successful mutating request; the engine events
  `IngestConnected`, `IngestDisconnected` (again when its grace ends),
  `IngestProbed` and `EgressStopped` (`request_pass_for_event`); and retry
  deadlines. Passes are at least one interval apart
  (`RESTREAM_RECONCILE_INTERVAL_MS`, default 1000 ms).
- An attempt that made progress before failing resets its count to 1
  (`next_output_retry_count`).
- Retries exhausted: `desiredState` stays `running` and `retriesExhausted`
  becomes `true`. Restarts stop; the URL and config can be edited; the next
  start or stop clears the flag.
- Stop sets `desiredState: stopped`; the next pass unregisters the egress.
  Losing the ingest also stops a running egress; it starts again when an
  ingest returns.
- Delete removes the row; the next pass stops the egress.
- The dashboard updates start/stop buttons optimistically and lets the next
  runtime refresh confirm.

### `GET /api/v1/pipelines/:pipelineId/outputs/:outputId/status`

Live egress status while the egress runs. After it ends, the last classified
snapshot stays: `status`, `phase`, `lastError`, `failurePhase`, `endedAt` and
retry fields (`retrying`, `nextRetryAt`). `recentFailureCount` counts
failures inside the flap window, and `flapping` turns `true` after repeated
failures even if the output is running again. Returns `404`
`{"error": "output not active"}` when there is no active or recent state.

## Process logs

| Method | Route | Response |
|---|---|---|
| `GET` | `/api/v1/logs` | `{ logs, total, hasMore }` |
| `GET` | `/api/v1/logs/stream` | SSE stream of `event: log` frames |

Both read the `app_logs` SQLite table (`src/api/logs.rs`). Field semantics
and the logging pipeline are in [Observability](observability.md#logging).

### `GET /api/v1/logs`

| Parameter | Default | Description |
|---|---|---|
| `level` | `info` | Minimum level: `error`, `warn`, `info`, `debug` |
| `since` | — | RFC 3339 lower bound (inclusive) |
| `until` | — | RFC 3339 upper bound (exclusive) |
| `target` | — | Module prefix (`restream_engine::srt`) |
| `scope` | — | `restream` (no pipeline/output), `pipeline` (pipeline, no output) or `output` |
| `pipeline_id` | — | One pipeline |
| `output_id` | — | One output (with `pipeline_id`) |
| `event_class` | — | `lifecycle` returns only lifecycle events |
| `prefix` | — | Comma-separated message prefixes (`stderr,exit`) |
| `after_id` | — | Rows with a larger id, ordered by id ascending |
| `limit` | `200` | Clamped to 1–1000 |
| `order` | `desc` | `asc` or `desc` on `ts` |

Each entry has `id`, `ts`, `level`, `target`, `message`, `fields` (JSON),
`pipelineId`, `outputId`, `eventType` and `eventClass`.

### `GET /api/v1/logs/stream`

- Takes `level`, `target`, `scope`, `pipeline_id`, `output_id`,
  `event_class` and `prefix`, plus `include_restream=true`: with a
  `pipeline_id` and no `output_id`, also deliver restream-wide events (no
  pipeline, no output).
- On connect it backfills rows newer than `Last-Event-ID` (or
  `?last_event_id=`) from SQLite in id order, page by page (200 rows) until
  the gap is closed, then streams live rows from the broadcast channel.
- Live rows are broadcast after they are stored, so their SSE ids are stable
  database ids.
- A `: ping` comment is sent every 20 s. A lagging receiver is closed; the
  browser reconnects with `Last-Event-ID`.

## Probe, graph, and diagnostics

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/pipelines/:pipelineId/probe` | Active ingest metadata |
| `GET` | `/api/v1/pipelines/:pipelineId/graph` | Runtime and desired processing graph |
| `GET` | `/api/v1/pipelines/:pipelineId/diagnostics/context` | One troubleshooting bundle |
| `POST` | `/api/v1/pipelines/:pipelineId/diagnostics/run` | Run the active diagnostic checks |
| `POST` | `/api/v1/pipelines/:pipelineId/moq-token` | Mint a MoQ viewer token |

### `GET /api/v1/pipelines/:pipelineId/probe`

Active ingest metadata: bitrate, GOP observations, ingest identity, `video`,
`audio` and `audioTracks` (every active audio track; `audio` is the first).
Track metadata includes MPEG-TS `pid`, `language` and `title` when the
source descriptors carry them. `encoder` is what an RTMP encoder declared in
`onMetaData` (absent for other protocols, or when it sent none): `encoder`
(its name, control characters removed, at most 128 characters), `width`,
`height`, `frameRate`, `videoKbps`, `audioKbps`, each only when present and
plausible. It is declared, not measured. `404` without an active ingest.

### `GET /api/v1/pipelines/:pipelineId/graph`

The runtime processing graph (`nodes`, `edges`: ingest, demux, source ring,
stages, egresses, HLS, recording) plus `desiredGraph`, `desiredOutputGraphs`
(the planner's view for the live ingest codec) and `runtimeGraph` (the same
nodes and edges). `404` for an unknown pipeline.

- Every `egress` node has `outputId`; its `details` exist only while the
  egress runs.
- The source ring node's `details.readers` lists readers registered on the
  ring (HLS segmenters, recordings, RTMP playback, transcoder and SRT TS muxer
  inputs) with lag and overflow counts. Fabric outputs read the ring in place
  without registering, so a pipeline whose only consumers are fabric outputs
  shows `readers: []`. HLS and HLS PUT outputs appear through their
  segmenter. Per-output progress is `flow` in `GET /api/v1/state` and
  `egresses` in pipeline telemetry.

### `GET /api/v1/pipelines/:pipelineId/diagnostics/context`

Returns `{ generatedAt, pipelineId, health, graph: { desired,
desiredOutputs, runtime }, alerts, recentEvents, recentLogs,
backendStderrTail }`: the immediate health snapshot, the pipeline's alerts,
the last 100 events and log rows, and the last 20 FFmpeg stderr rows. It runs
no active probes. `404` for an unknown pipeline.

### `POST /api/v1/pipelines/:pipelineId/diagnostics/run`

No body. Returns one report with `protocol`, `totalDurationMs` and ordered
`checks`. The check set follows the active ingest protocol; see
[Observability](observability.md#diagnostic-checks). `404` without an active
ingest, `429` while another run for the pipeline is in progress. Unauthorized
callers get a JSON `401`.

### `POST /api/v1/pipelines/:pipelineId/moq-token`

Mints a viewer token for the pipeline's broadcast on the
[MoQ origin](media-pipeline.md#moq-origin). No body. Returns
`{ token, url, broadcast, expiresAt, certificateSha256 }`:

- `url`: the MoQ listener's WebTransport address,
  `https://HOST:MOQ_PORT/?token=…`.
- `broadcast`: the pipeline id.
- `expiresAt` (RFC 3339): 60 s after minting. The token admits new sessions
  until then; an admitted session keeps playing.
- `certificateSha256`: hex SHA-256 of the listener's leaf certificate, read
  at startup (`null` if unreadable). A browser pins it
  (`serverCertificateHashes`) when normal certificate checks fail.

A token admits only its own pipeline. The signing key is generated at process
start, so a restart invalidates outstanding tokens. `404` for an unknown
pipeline, `409` when the MoQ listener is off.

## Agent plane

The agent read/planning plane (`agent-plane`) and approval-gated execution
(`agent-execution`) are Cargo features; the default build has both
(`default = ["agent-execution"]`, which enables `agent-plane`). Without a
feature, its routes answer an authenticated `404`
`{"error", "feature", "compiledIn": false}`. The flow, operation states,
approval rules and verification gates are in
[Agent plane integration](agent-plane-integration.md).

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/agent/capabilities` | Compiled-in tools and route catalog |
| `GET` | `/api/v1/agent/context` | One redacted read-only state bundle |
| `POST` | `/api/v1/agent/investigations` | Health, graph, telemetry, alerts and events for an investigation |
| `GET` | `/api/v1/agent/incidents/explanation?alertId=…[&firstSeen=…]` | Why an incident happened and the plan for its fix |
| `POST` | `/api/v1/agent/plans` | Draft plan from intent plus proposed changes |
| `POST` | `/api/v1/agent/plans/validate` | Only the validation result of a draft plan |
| `POST` | `/api/v1/agent/graph-diff-preview` | Graph and impact preview of a draft plan |
| `POST` | `/api/v1/agent/operations` | Create an operation (256 KiB body limit) |
| `GET` | `/api/v1/agent/operations/:operationId` | Status, audit log, progress, results |
| `POST` | `/api/v1/agent/operations/:operationId/approve` | Approve every change (needs the most demanding change's role) |
| `POST` | `/api/v1/agent/operations/:operationId/actions/:index/approve` | Approve one change from its minimum role up |
| `POST` | `/api/v1/agent/operations/:operationId/apply` | Apply an approved operation |
| `POST` | `/api/v1/agent/operations/:operationId/verify` | Verify an applied operation |
| `POST` | `/api/v1/agent/verify` | Verify by body `{ "operationId": "op_…" }` |

The read and planning routes mutate nothing. The capability catalog lists only
agent routes; agents should read state through `context` and investigations,
which redact it. Raw stream keys and output URLs never appear there: they
become SHA-256 fingerprints plus scheme/host summaries.

The incident explanation reads one alert (the open one with that id, else
the latest resolved; `firstSeen` picks an episode) and the pipeline's flight
record from 60 s before it was first seen to its end
(`application::incident_explain`). It answers `{alert, explanation, plan}`:
`explanation` names the broken `layer` (`ingest`, `feed`, `shard`,
`destination`), a `summary`, measured `evidence`, the upstream layers
`ruledOut` with why, a `recommendation`, and a `proposal` (a plan request)
when a plant change fixes the cause; `plan` is that proposal judged like
`POST /api/v1/agent/plans`. Nothing is created. An unknown alert is `404`
`incidentNotFound`.

Context includes:

- route and schema metadata for agent clients;
- build and runtime status, OS basics, native-library versions, features;
- redacted pipelines, outputs, ingests, jobs, transcode profiles, settings;
- desired-vs-actual summaries for inputs, outputs, recording and HLS;
- health, resource map, engine and pipeline telemetry, graphs, alerts,
  recent lifecycle events;
- media inventory, storage summary, dependency summaries, passive
  diagnostics findings and the active diagnostics route;
- redaction metadata naming the removed fields.

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

Plan responses include `planId`, validation errors and warnings, a static
graph preview (`graphPreview.topology`, `null` without `pipelineId`), impact
notes and `executionEnabled` (`true` only with `agent-execution`).

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
      "config": { "video": { "mode": "source" }, "audio": { "mode": "all" } },
      "desiredState": "stopped"
    }
  ]
}
```

Operation records include `operationId`, `status`, `approval`, `actions`,
`request`, `plan`, `proposedPlanHash`, `incidentId`, `incidentLinks`,
`affectedObjects`, `stateTransitions`, `progressSnapshots`, `auditLog`,
`executionResult` and `verificationResult`. Responses are redacted.

- Apply before approval is refused. Each change in `actions` names its
  `requiredRole` (`removeOutput`: `admin`; other kinds: `operator`) and its
  `approval` once given. A session approves at its effective role; below the
  minimum it gets `403` (`requiredRole`, `effectiveRole`), recorded in the
  audit log. `approval.approvedBy` is the member that completed the
  approval; `approval.claimedBy` is the request's unverified `approvedBy`.
  See [approval invariants](agent-plane-integration.md#invariants).
- Operations live in memory: at most 1024, lost on restart. An unknown id
  answers every operation route with `404`
  `{"code": "operationNotFound", "error": "Operation not found"}`.
- A refused apply is `400` with the failed record; `executionResult` is
  `{"success": false, "error"}`, and changes before the refused one stay
  applied.
- `verificationResult` has four `gates` (`desiredState`, `outputsRunning`,
  `newCriticalAlerts`, `capacityState`), each `{ gate, passed, detail }`,
  plus `checks`, `graphConvergence` and `incidentDelta` (reported, not
  judged). See
  [verification gates](agent-plane-integration.md#verification-gates).

## Recording

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/api/v1/pipelines/:pipelineId/recording/start` | Persist enabled and start now if an ingest is live |
| `POST` | `/api/v1/pipelines/:pipelineId/recording/stop` | Disable and cancel recording |

Both return `{ "enabled": bool, "active": bool }`.

- The recorder writes raw MPEG-TS into the media directory
  (`RingBuffer → TsMuxer → MemoryQueue → file writer`, `crates/restream-engine/src/recording`).
- A recording shorter than 5 s is deleted (`MIN_DURATION_SECS`).
- A recording of 5 s or more is remuxed by FFmpeg into a sibling `.mp4`. The
  media library prefers the `.mp4` for playback, keeps the `.ts` for download
  while it exists, and reports `conversionStatus` (`converting`, `ready`,
  `failed`).
- `recordingSettings.retainSourceTs`: `false` (default) deletes the `.ts`
  after a successful remux; `true` keeps both. A failed remux always keeps
  the `.ts`.

## File ingest

A file ingest plays a media-library file into a pipeline through an FFmpeg
subprocess (binary resolution: `FFMPEG_BIN_PATH`, see
[Configuration](configuration.md)). There are two route families.

Per pipeline:

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/pipelines/:pipelineId/file-ingest` | The pipeline's file source and whether it runs |
| `PUT` | `/api/v1/pipelines/:pipelineId/file-ingest` | Create or replace it |
| `DELETE` | `/api/v1/pipelines/:pipelineId/file-ingest` | Clear it: `{ "deleted": true }` |

`PUT` body (`loopFlag` also accepts `loop`):

```json
{ "filename": "clip.mp4", "loopFlag": true, "startTime": "00:00:05", "liveOptimized": true, "targetGopSeconds": 2 }
```

By stream key:

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/ingests` | List file ingests |
| `POST` | `/api/v1/ingests` | Create |
| `PUT` | `/api/v1/ingests/:id` | Update |
| `DELETE` | `/api/v1/ingests/:id` | Delete (stops it if running) |
| `POST` | `/api/v1/ingests/:id/start` | Start |
| `POST` | `/api/v1/ingests/:id/stop` | Stop |

```json
{ "filename": "example.mp4", "streamKey": "sk_…", "loop": true, "startTime": "00:00:05", "liveOptimized": true, "targetGopSeconds": 2 }
```

Responses carry `id`, `filename`, `streamKey`, `loop`, `startTime`,
`liveOptimized`, `targetGopSeconds` and `running`.

- `filename` must be a relative path under the media directory: not blank,
  not absolute, no `.` or `..` components (else `400`).
- `loop` and `liveOptimized` default to `false`; `targetGopSeconds` defaults
  to 2 and is at least 1.
- Start errors: `400` when the media file is missing or no pipeline has the
  stream key; `409` when that ingest already runs or the pipeline already has
  an active publisher; `404` for an unknown ingest.
- `liveOptimized: true` re-encodes to H.264 (`libx264`) and AAC with a
  keyframe every `targetGopSeconds` and scene-cut GOP drift disabled.
- Stop and delete kill the child and reap it.

### Playlists

The per-pipeline `PUT` (and a pipeline's `fileIngest` on create or update)
also takes `playlist`: the files to play in order, at most 200
(`MAX_PLAYLIST_ITEMS`). Each entry is checked like `filename`. The first
entry becomes `filename`; a list of one file is that file. The per-pipeline
responses carry `playlist` (every file in order, one entry for a single
file) and `nowPlaying` (while running, the index of the playing file, else
`null`).

```json
{ "filename": "intro.mp4", "playlist": ["intro.mp4", "show/part1.mp4", "outro.mp4"], "loopFlag": true, "liveOptimized": true }
```

- The engine plays each file with its own FFmpeg run and keeps the
  pipeline's timestamps continuous across files, as it does when a looping
  file restarts. The start time applies to the first file of the first pass
  only.
- `loop` starts the list over after its last file; without it the ingest
  ends after the last file. One file still loops inside FFmpeg
  (`-stream_loop -1`).
- A file that fails is skipped. The ingest stops once every file has failed
  in a row, so a list of unreadable files never respawns FFmpeg forever
  (`next_item` in `crates/restream-engine/src/external_file_ingest.rs`).
- Files should share codecs and resolution: with `-c copy` a change
  between files reaches every destination. `liveOptimized` re-encodes each
  file to the same H.264/AAC.
- Renaming a media file renames it in every playlist, and a file in any
  playlist counts as in use.

## Media files

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/media` | List library files: `{ "files": [...] }` |
| `POST` | `/api/v1/media/upload` | Upload one file (multipart field `file`) |
| `GET` | `/api/v1/media/:filename/analysis` | Codec, duration and GOP analysis |
| `GET` | `/api/v1/media/:filename/poster` | A PNG still (320 px wide) from about 5 s in, decoded on the blocking pool two at a time; `ETag` from size and time, browser-cached for a day; `404` when the file has no picture |
| `PATCH` | `/api/v1/media/:filename` | Rename from `{ "newName" }` |
| `DELETE` | `/api/v1/media/:filename` | Delete an unreferenced file |
| `GET`, `HEAD` | `/media/:filename` | Download or stream a file, with byte ranges |

- Supported extensions: `.ts`, `.mkv`, `.mp4`, `.mov`. The media directory
  defaults to `.restream/media/` (`RESTREAM_MEDIA_DIR`).
- Upload: exactly one `file` field with a plain supported filename, at most
  8 GiB (`413` above). An existing name is `409`. Success returns
  `{ "uploaded": true, "name", "size" }`. The file is published with a
  create-only hard link, so a concurrent upload cannot replace it.
- Rename keeps the extension (`400` otherwise; `409` if taken). Renaming a
  recording `.ts` also renames its converted `.mp4` and conversion-state
  file, and updates file ingests that referenced the old name.
- Delete is `409` while a file ingest references the file, `404` for a
  missing file and `400` for a path that resolves outside the media
  directory.
- `/media/:filename` answers one `Range: bytes=` span with `206`
  (`Accept-Ranges: bytes`); an unsatisfiable or multi-range header is `416`.
- Recording entries may carry `sourceName`/`sourceSize`,
  `convertedName`/`convertedSize`, `playName`, `conversionStatus`,
  `conversionError` and `conversionUpdatedAt`. After a successful remux,
  `playName` is the `.mp4` and `sourceName` the `.ts`.

## Custom encoding

| Method | Route | Response |
|---|---|---|
| `GET` | `/api/v1/encodings/custom` | `410 Gone` |
| `PUT` | `/api/v1/encodings/custom` | `410 Gone` (body `{ "ffmpegArgs" }` is read and dropped) |

Both answer `{"error": "Custom encoding is not available yet; choose source
or a preset encoding"}` after authentication (`src/api/file_ingest.rs`).

## Runtime state

One typed, revisioned view of every pipeline, output, input and graph
(`application::runtime_state`). The TypeScript contract is generated from the
Rust types into `web/ts/core/generated/runtime-state.ts`; a unit test fails
when it is stale. Regenerate it with:

```sh
cargo test --lib runtime_state::tests::write_generated -- --ignored
```

`web/ts/core/state-stream.ts` is the client.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/state` | The current projection |
| `GET` | `/api/v1/state/stream` | SSE snapshot plus deltas |
| `GET` | `/api/v1/fleet/history` | 24 h of 5 s fleet samples |

### `GET /api/v1/state`

Returns the projection. It is rebuilt first unless the last rebuild is under
500 ms old (`ON_DEMAND_REUSE`) and nothing was marked changed since (engine
lifecycle event, mutating request, or a reconciler write such as retries
exhausted).

```json
{
  "epoch": "18c9f0a1b2c3d4e5",
  "revision": 42,
  "pipelines": [
    {
      "id": "...", "name": "...",
      "ingest": { "inputId": "...", "protocol": "srt", "videoCodec": "h264", "bonded": false },
      "standbyInputs": 1,
      "recording": false
    }
  ],
  "outputs": [
    {
      "id": "...", "pipelineId": "...", "name": "...",
      "protocol": "rtmp", "shard": 0, "stage": "pipe:video:720p",
      "desired": "running", "actual": "sending",
      "convergence": "converged", "flow": "flowing",
      "backpressure": null, "retry": null, "evidence": null, "diagnosis": null
    }
  ],
  "inputs": [],
  "graphs": []
}
```

Pipelines:

- `ingest.bonded`: the selected ingest is an SRT group connection.
- `standbyInputs`: connected, unselected inputs (a file source running beside
  the live input counts).
- `recording`: a recording file is being written.

Outputs:

- `protocol`: `rtmp` (also `rtmps`), `srt`, `hls` (also `http`/`https`),
  `moq`, `sink`, `pipeline` or `unknown`.
- `shard`: the egress shard of the running attempt; `null` off the fabric or
  when none runs.
- `stage`: the last stage the running attempt reads (`<pipeline>:<kind>`);
  outputs with the same `stage` share its work.
- `desired`: `running` or `stopped`. `actual`: the running attempt's phase,
  or `null`.
- `convergence`: `converged` (delivering, or stopped as asked), `starting`,
  `waitingForIngest`, `retrying` (`retry` holds `attempts` and
  `nextAttemptAtMs`), `retriesExhausted` or `stopping`.
- `flow`: `flowing`, `stalled` (no progress past the stall threshold) or
  `idle`. A delivering output that stalls stays `converged`: a stall is a
  flow problem, not a desired/actual mismatch.
- `evidence`: the latest failure (`error`, `phase`, `atMs`) of the running
  attempt, else of the last one.
- `diagnosis` (`restream_health::diagnosis`): why a desired-running output is
  degraded, or `null`. The media path has four layers, upstream first:
  `ingest`, `feed` (shared stages), `shard` (fabric shard) and `destination`.
  `layer` is the first unhealthy one; `ruledOut` lists the healthy layers
  checked before it. `cause.code`:
  - `ingest`: `noPublisher`;
  - `feed`: `stageNotProducing` (`stage`, `phase`);
  - `shard`: `shardPanicked`, `shardStalled`, `shardStopped` (`shard`), and
    `capacityRefused` (`error`): admission refused the output because the
    least-loaded shard of the feed's pool would pass its load ceiling or
    has no free leaf (failure phase `egress_capacity`);
  - `destination`: `retriesExhausted`, `retryBudgetNearlySpent` (`attempts`,
    `max`; at 80% of the budget), `rejected` (`phase`, `error`),
    `backpressured` (`stalled`), `noProgress`.

  Output alerts and the agent context's `desiredVsActual` read the same
  diagnosis.

Inputs and graphs:

- `inputs`: every input with the stored fields of
  `GET /api/v1/pipelines/:id/inputs` plus `position` (primary first, then
  backups by age). `runtime` holds only state: `connected`,
  `forwardingState`, `protocol`, `remoteAddr`, `video` (`codec`, `width`,
  `height`) and `audio` (`codec`, `sampleRate`, `channels`). Uptime, bytes
  and quality change on every read, so they are in
  `GET /api/v1/engine/telemetry` `inputs` instead.
- `graphs`: one graph per pipeline, the structure of
  `GET /api/v1/pipelines/:id/graph` without `details` and `metrics`: nodes
  (`id`, `type`, `label`, `active`, `stageKey`, `outputId`, `metricsStage`)
  and edges (`from`, `to`, `label`). `metricsStage` names the node's row in
  telemetry `stages`.

`revision` rises by 1 per change and restarts at 0 with the process;
`epoch` identifies the server run.

### `GET /api/v1/state/stream`

Server-sent events. Each event id is `<epoch>.<revision>`.

- `event: snapshot`: a whole state, as `GET /api/v1/state` returns it.
- `event: delta`: changes since the previous revision, with `epoch`,
  `pipelines`, `outputs`, `inputs`, `graphs` (whole entities to insert or
  replace) and `removedPipelines`, `removedOutputs`, `removedInputs`,
  `removedGraphs` (ids; a graph's id is its pipeline id).
- `event: heartbeat` (data `{}`): every 20 s. The dashboard client treats 50
  s of silence as a dead connection and reconnects.

A new connection starts with a snapshot. To resume, send the last id as
`?since=<epoch>.<revision>` or `Last-Event-ID`. The server replays the deltas
after it if they are among the last 256 (`DELTA_HISTORY`); otherwise, or for
another epoch, or when the client falls behind, it sends a snapshot. A client
that gets a delta that does not follow its revision, or with another epoch,
reconnects with `since`.

The projection is rebuilt only while a stream client is connected: every
reconcile interval, and soon after a lifecycle event or mutating request, at
most once per interval. With no client connected, only `GET /api/v1/state`, a
new stream connection and the fleet-history sampler rebuild it, reusing a
projection younger than 500 ms when nothing changed. Rebuilds run one at a
time. The resource map is process measurement and is not projected.

### `GET /api/v1/fleet/history`

Fleet signals sampled every 5 s whether or not anyone watches, kept 24 h (at
most 17 280 samples, oldest dropped first; `src/application/fleet_history.rs`).
`?since=` and `?until=` (RFC 3339, inclusive, optional) bound the window; a
malformed value is `400` after authentication. A sample is stamped at least 1
ms after the previous one, so a clock step back does not reorder history.

```json
{
  "samples": [
    { "atMs": 1792152000000, "headroomPercent": 74.5, "deliveringPercent": 50.0,
      "degraded": { "ingest": 1, "feed": 0, "shard": 0, "destination": 0 },
      "sentBytes": 28125000 }
  ],
  "completeFromMs": 1792151000000,
  "intervalMs": 5000,
  "newestMs": 1792152005000
}
```

- `headroomPercent`: 100 minus the capacity projection's utilization,
  floored at 0.
- `deliveringPercent`: wanted outputs that are converged and flowing; `null`
  when none is wanted.
- `degraded`: wanted outputs with a diagnosis, by `diagnosis.layer`.
- `sentBytes`: bytes every output sent since the previous sample (an output
  new or restarted since then counts from its start); a span's traffic is
  the sum of its samples. Fleet shows today's.
- `completeFromMs`: history is complete only after this instant (server
  start or the latest dropped sample).
- `newestMs`: the newest sample kept (`null` before the first). Once it is at
  or past a window's `until`, that window is final.

### `GET /api/v1/pipelines/:pipelineId/flight-recorder`

The pipeline's flight recorder (`src/application/flight_recorder.rs`).
Every 2 s, whether or not anyone watches, it samples three things:

- the selected ingest;
- the source ring;
- the pipeline's outputs.

It keeps the last 10 minutes in memory, and every 10 s also writes each
pipeline's newest sample to SQLite, kept for `RESTREAM_FLIGHT_HISTORY_HOURS`
(default 24; 0 keeps only memory). A deleted pipeline's history is dropped
with it. `?since=` and `?until=` (RFC 3339, optional) bound the window, so a
page polls incrementally or asks for any span the history holds;
`?points=` (default and cap 2,000) thins a long window evenly, first and last
samples kept. `detailedFromMs` says where the 2 s samples begin; older ones
are `persistedIntervalMs` apart. Returns `404` until the recorder has sampled
the pipeline. The values come from counters the engine
already keeps, read once per interval on the recorder's own task.

```json
{
  "schema": "restream.trace.v1",
  "pipelineId": "pipeline_…",
  "intervalMs": 2000,
  "completeFromMs": 1792742000000,
  "detailedFromMs": 1792742000000,
  "persistedIntervalMs": 10000,
  "samples": [
    {
      "atMs": 1792742537902,
      "ingest": { "inputId": "…", "protocol": "srt", "bitrateBps": 3903327.6,
        "packetsPerSec": 147.6, "videoCodec": "h264", "width": 1920,
        "height": 1080, "fps": 30.0, "gopSec": 2.0, "maxGopSec": 2.0,
        "rttMs": 41.2, "lossPerSec": 0.0, "retransPerSec": 0.4,
        "receiveBuffer": 0.08 },
      "ring": { "fill": 812, "capacity": 4096, "worstLagSlots": 3, "overflows": 0 },
      "outputs": { "wanted": 5, "delivering": 3, "degraded": 2, "retrying": 2,
        "stalled": 0, "retryAttempts": 5, "offeredBps": 3159609.7,
        "deliveredBps": 3171125.6, "resyncs": 0, "worstFeedLag": 0 }
    }
  ]
}
```

- `ingest` is `null` while no input is selected and connected.
  - `bitrateBps` is the byte counter's change over the interval within one
    session; it is `null` on a session's first sample and across a
    failover.
  - `gopSec` and `maxGopSec` are the mean and longest gaps between the
    session's last keyframes; a timestamp reset is skipped.
  - A value the protocol does not report (`rttMs`, `lossPerSec` on TCP) is
    `null`, never `0`.
  - `qa` carries the [picture and sound checks'](#picture-and-sound-checks)
    findings while they run; it is absent otherwise.
  - `timeline` is the stream inspector's last minute, per track, read from
    the source ring once a second on its own thread
    (`crates/restream-engine/src/stream_inspector.rs`); absent until it has read the ring:
    - `tracks[]`: `media` (`video` first, then `audio`), `track`,
      `packetsPerSec`; `backwards` (DTS that went back), `jumps` (DTS that
      moved more than 1 s apart from arrival time; a network stall moves
      both and is not one), `ptsBeforeDts`, `largestJumpMs` (signed);
    - audio only: `avOffsetMs`, how much later than the first video track
      its media arrives against its timestamps (smallest over 5 s): how far
      apart the publisher interleaves them;
    - AAC only: `clockDriftMsPerMin`, how far the track's timestamps move
      from its sample count (1024 samples a frame at its sample rate) each
      minute, positive when they run fast; absent until 20 s of unbroken
      clock, or when the two disagree by over 5% (a frame size the engine
      cannot know, such as HE-AAC declared at its core rate). Arrival times
      cannot show drift: muxers send packets in timestamp order;
    - video only: `captions`, present once the source sent closed captions
      (CEA-608/708 in SEI, read up to each frame's first picture slice):
      `cea608`, `cea708`, `lastSeenSec` and `lastTextSec` (seconds since a
      frame carried captions, and caption text rather than padding). It
      stays after captions stop, so a dropout keeps being reported;
    - `windowSec` (up to 60) and `unread`, packets the ring reused before
      the inspector read them (the counts miss those).
- `ring` is `null` while the pipeline has no source ring.
- `decoders` lists the pipeline's FFmpeg stages (absent when it has none):
  `stage` (the stage kind), `lagSlots` and `lagSec` (how far the stage's
  reader is behind the source ring), and `inputBlocked`, the share of the
  interval the engine waited to write FFmpeg's input because its pipe was
  full (`null` on a stage's first sample or after FFmpeg restarts).
- `outputs` counts wanted outputs, plus the counters summed over the running
  egresses (`retryAttempts` over the retrying ones).
  - `resyncs` and `overflows` are cumulative counters.

## Picture and sound checks

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/pipelines/:pipelineId/stream-qa` | Whether the checks are on, and what they last found |
| `PUT` | `/api/v1/pipelines/:pipelineId/stream-qa` | `{ "enabled": bool }`: persist and start or stop now (operator) |

Both return the state:

```json
{
  "enabled": true,
  "active": true,
  "findings": {
    "pictureCheckedMs": 1792742537000, "blackSinceMs": null,
    "stillSinceMs": null, "stillIsSlate": false,
    "soundCheckedMs": 1792742537400, "silentSinceMs": null,
    "loudnessLufs": -18.4,
    "tracks": [
      { "silentSinceMs": null, "loudnessLufs": -18.4 },
      { "silentSinceMs": 1792742525000, "loudnessLufs": -70.0 }
    ]
  }
}
```

- The checks are off unless turned on (`stream_qa_enabled:{id}` in the meta
  store). On, and while the source is live or within the reconnect grace,
  each checked pipeline runs one external FFmpeg (`crates/restream-engine/src/stream_qa/`;
  no codec runs inside the engine). Its source ring is fed to FFmpeg's
  stdin as MPEG-TS on a dedicated thread.
  - FFmpeg decodes only keyframes (H.264 or H.265, over SRT or RTMP,
    enhanced RTMP included) and writes each as a 64×36 grey thumbnail to
    stdout; the engine judges it black (98% of it dark) or unchanged (mean
    difference to the last under 0.5 of 255). An unchanged picture whose
    neighbouring pixels are mostly equal (60% or more) looks drawn — bars,
    a card — and is reported as a slate (`stillIsSlate`); camera pictures
    measure 10–44%.
  - FFmpeg writes every audio track as 48 kHz stereo f32, up to 32 tracks
    merged side by side per Unix socket the engine listens on (one more socket
    per further 32); the engine
    measures each track's short-term loudness (3 s, ITU-R BS.1770
    K-weighting), floored at -70 LUFS; -60 LUFS or less is silence.
    `silentSinceMs` and `loudnessLufs` are the first track's; `tracks` has
    them all in source order.
  - It never holds the ring's writer: a feed that falls behind is moved to
    the next keyframe like any reader. A child that exits or crashes is
    started again after 2 s, doubling to 30 s.
  - Cost, measured on a 1080p30 H.264 source with a 2 s GOP: about 2% of
    a CPU core per checked pipeline with one audio track (about 0.9% in the
    child, 1.3% in the engine's feed and reader threads). Each further
    track adds about 0.45% in the child for its AAC decode: the checked-in
    16-track clip costs the child 9.5% against 2.6% for its first track
    alone.
- `findings` is `null` while no sampler runs. A `*SinceMs` is set while its
  condition holds. A finding whose check is over 10 s old is ignored by the
  alert rules.
- Alerts (`GET /api/v1/alerts`), and with them incidents and notifications:
  - `pipeline:{id}:input:picture_black`: critical, after 10 s;
  - `pipeline:{id}:input:picture_frozen`: warning, after 30 s unchanged, and
    not while black; `picture_slate` instead when the still picture looks
    drawn;
  - `pipeline:{id}:input:audio_silent`: warning, after 10 s of a silent
    first track; `audio_silent:track{n}` for a later track `n` (1-based).

## Health and status

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/healthz` | Liveness, no session: `{ "status": "ok" }` |
| `GET` | `/api/v1/engine/health` | Engine health snapshot |
| `GET` | `/api/v1/dashboard/runtime` | Health plus host metrics in one call |
| `GET` | `/metrics/system` | Host and engine metrics (JSON, not Prometheus) |
| `GET` | `/api/v1/capacity/scenario` | Observe-only capacity projection of a change |
| `GET` | `/api/v1/engine` | Build, toolchain, native library and OS info |
| `GET` | `/api/v1/engine/sbom` | CycloneDX 1.5 SBOM |

`/metrics/system` is listed in `PUBLIC_ROUTE_PATHS`, but its handler requires
a session. Field derivation for all of these is in
[Observability](observability.md).

### `GET /api/v1/engine/health`

Returns `EngineHealthView`:

| Field | Content |
|---|---|
| `generatedAt`, `status` | Snapshot time and engine status |
| `pipelines.<id>.input` | Ingest status, bytes, bitrate, media, `publisher` (protocol, remote address, `quality`), `disconnectGraceActive`/`disconnectGraceRemainingMs`, `recentDisconnectCount`, `flapping` |
| `pipelines.<id>.outputs.<id>` | Egress status, phase, errors, retry state |
| `pipelines.<id>.recording` | `{ enabled, active }` |
| `stages` | Stage runtime by key |
| `runtimeLimits.nofile` | `configured`, `soft`, `hard`, `satisfied` |
| `hostSettings` | Rows `{ key, label, current, required, unit, status, detail }` (nofile, socket buffer ceilings, Tokio threads, CPU parallelism, cpuset, cgroup quota) |
| `rtmpListener` | `acceptErrors`, `fdExhaustionErrors`, `restarts`, `ingressOwners` |
| `srtListener` | `bondingAvailable`, `restarts`, `ingressOwner` counters |
| `egressFabricShards` | One row per live shard: `protocol`, `feedId`, `shardIndex`, `state`, loop and command-queue counters |
| `containedPanics` | Panics contained at a per-entity boundary since start |
| `tuning.outputMaxRetries` | `RESTREAM_OUTPUT_MAX_RETRIES` |

- `egressFabricShards` covers all six fabric registries: `srt`, `rtmp`,
  `sink`, `pipeline`, `hls-put` and `moq`
  (`crates/restream-engine/src/engine_egress_fabric_diagnostics.rs`). It is empty when no
  output runs. `state` is `healthy`, `stalled`, `stopped` or `panicked`.
- `rxBufferExhaustions` (SRT ingress and egress Owners) counts transient
  managed-buffer pressure. It is not a loss count; `rxRingDropped` and
  `rxTruncated` are.

### `GET /api/v1/dashboard/runtime`

Returns `{ health, metrics }` so a dashboard refresh costs one round trip.

| Param | Default | Notes |
|---|---|---|
| `metrics_view` | full | `summary` trims host metrics to aggregate percentages and rates |
| `pipeline_id` | — | Health for only this pipeline |

### `GET /metrics/system`

`view=summary` trims system-disk and network detail. Top-level keys
(`build_system_metrics_snapshot`): `generatedAt`, `cpu`, `memory`, `engine`,
`capacity`, `hostCapacity`, `mediaExecutor`, `egressShards`, `ioUring`,
`rtmps`, `hlsPutTls`, `disk`, `network`, and `mediaDisk` (both views: the
Media library's storage card reads the summary).

- `engine.cpuPercent` and `engine.totalMemoryBytes` include restream plus its
  FFmpeg children; `restream*` and `externalFfmpeg*` fields split them.
- `capacity` is observe-only: measured `ingressPps`, `mediaBps`, fanout
  `egressPps`, service-center utilizations, `hottestCenter`,
  `projectedUtilization`, and `flow` (measured when `flowMeasured` is true,
  else priced from calibration priors). It admits nothing.
- `ioUring` is a capability probe cached at startup.

### `GET /api/v1/capacity/scenario`

Query: `add_rtmp`, `add_rtmps`, `add_srt`, `add_stages` (non-negative
integers, default 0) and optional `pipeline_id`. A malformed query is `400`
after authentication. Returns `{ addOutputs: { rtmp, rtmps, srt },
addStages, ratedBy, ingest, calibration, retryPolicy, baseline, projected }`.

- Both sides price one sample of the current workload on the calibration
  priors (`flowMeasured: false`), so they compare like with like.
  `projected.flow.status` is `healthy`, `saturated` or `overloaded`.
- The model (`calibration.modelVersion` 5, `CAPACITY_MODEL_VERSION`) prices
  each output with its own pipeline's ingest. RTMP/RTMPS cost the media
  packet rate (TLS not priced); SRT costs twice that. MoQ costs 6.95 times
  that from measured Restream CPU per added output (20 → 120 MoQ publisher
  sessions, each with a continuously subscribed receiver through the whole
  measured window; the relay's and subscribers' own CPU is excluded), HLS
  PUT 0.50, sink 0.15 and pipeline 0.80 times that rate, from measured
  per-output CPU on one host. Sink and pipeline outputs add no NIC bytes;
  MoQ outputs do. `hls://` outputs share the HLS price despite uploading
  nothing; the pipeline target also contributes to ingress cost. These are
  conservative observe-only priors, not admission rules.
- Added outputs carry `pipeline_id`'s ingest, which must be live (else
  `422`), or else the highest-bitrate live ingest. `ratedBy` and `ingest`
  (`{ bps, pps }`) say which; both are `null` with no live ingest.
- `calibration` holds `limits` (six service-center ceilings), `sources`
  (each `configured` via `RESTREAM_CAPACITY_*` or a `prior` scaled to
  `priorCpus`) and `safeUtilization`.
- `retryPolicy` is the output reconnect policy: `{ maxRetries, baseMs,
  maxMs, jitter: "equal" }`. The delay before attempt n is `baseMs · 2^n`,
  capped at `maxMs`; half of it is fixed and half is spread per output.
  It comes from `RESTREAM_OUTPUT_MAX_RETRIES`, `RESTREAM_OUTPUT_RETRY_BASE_MS`
  and `RESTREAM_OUTPUT_RETRY_MAX_MS`. The Stability Lab simulates with it.

### `GET /api/v1/engine`

```json
{
  "restream": { "version": "0.1.0", "commit": "abc558b", "buildTimestamp": "..." },
  "toolchain": { "rustc": "...", "target": "x86_64-unknown-linux-gnu", "llvm": "...", "gccRuntime": "..." },
  "nativeLibraries": { "srt": {}, "sqlite": {} },
  "externalTools": {
    "ffmpeg": { "path": "/usr/local/bin/ffmpeg", "version": "n9.0.2-17-g2a571b6068-20260930", "error": null },
    "ffprobe": { "path": "/usr/local/bin/ffprobe", "version": "n9.0.2-17-g2a571b6068-20260930", "error": null },
    "features": [
      { "feature": "transcoding", "available": false, "missing": ["encoder libx265"] },
      { "feature": "recordingRemux", "available": true, "missing": [] }
    ]
  },
  "sbom": { "format": "CycloneDX", "specVersion": "1.5", "endpoint": "/api/v1/engine/sbom" },
  "os": { "platform": "linux", "arch": "x86_64", "cpu": { "modelName": "...", "logicalCpus": 20, "flags": [] } }
}
```

- `nativeLibraries` lists the libraries linked into the engine. `srt`
  reports `version` (runtime), `buildVersion` (build script), `license` and
  `bondingAvailable`; a version mismatch is a packaging problem. `sqlite`
  reports `version`, `sourceId` and `license`.
- `externalTools` is the startup check of the FFmpeg executables (`null`
  until it finishes, about a second after start). Each feature
  (`transcoding`, `fileIngest`, `recordingRemux`, `posters`,
  `pictureAndSoundChecks`, `mediaAnalysis`) is `available` or names what the
  build lacks; an executable that did not run has `version: null` and an
  `error`.
- `os.cpu` is a debugging subset: model, core/thread topology,
  virtualization and the acceleration flags that explain codec throughput.

### `GET /api/v1/engine/sbom`

Content type `application/vnd.cyclonedx+json; version=1.5`. Contains the
Restream component and build identity; every resolved normal Rust crate from
the locked dependency graph (version, package URL, source, license); the
native components srt-rs, SQLite, libgcc, the Rust standard library and
glibc where it applies; and SPDX license expressions or `NOASSERTION`. The
external FFmpeg executable is not part of the binary and is not listed. Development and benchmark dependencies are not
included.

## Operator views

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/overview` | Pipeline counts, alert counts, SRT listener |
| `GET` | `/api/v1/alerts` | Alerts across all pipelines, optional resolved history |
| `GET` | `/api/v1/events` | Lifecycle event log |
| `GET` | `/api/v1/pipelines/:pipelineId/summary` | One pipeline's operator summary |
| `GET` | `/api/v1/pipelines/:pipelineId/alerts` | One pipeline's alerts |
| `GET` | `/api/v1/engine/resource-map` | Resource attribution |

### `GET /api/v1/overview`

```json
{
  "generatedAt": "...",
  "totalPipelines": 3,
  "activePipelines": 2,
  "degradedPipelines": 0,
  "failedOutputs": 0,
  "alertCount": { "critical": 0, "warning": 1 },
  "srtListener": {}
}
```

`degradedPipelines` counts pipelines with any alert; `failedOutputs` counts
outputs not `running` on live pipelines.

### `GET /api/v1/alerts`

```json
{
  "generatedAt": "...",
  "alerts": [ { "id": "...", "severity": "Warning", "evidence": [ { "signal": "lagSlots = 900", "confidence": "measured" } ] } ],
  "resolved": [ { "alert": { "id": "..." }, "resolvedAt": "..." } ],
  "resolvedCompleteFrom": "..."
}
```

- Alerts carry `id`, `severity`, `scope`, `evidence`, `recommendedAction`,
  `firstSeen` and `lastSeen`, sorted critical first. Evidence confidence is
  `measured` (a reading) or `derived` (a rule's conclusion).
- History is opt-in: `resolved` and `resolvedCompleteFrom` appear only with
  `?since=` (RFC 3339; anything else is `400` after authentication). An alert
  enters `resolved` on the first read that no longer lists it, so
  `resolvedAt` is only as fine as the reads.
- The last 256 resolved alerts are kept in memory (`RESOLVED_CAP`), one entry
  per condition at its latest closing.
- The alert catalogue (ids, thresholds, which causes raise no output alert)
  is in [Observability](observability.md#alerts).

`GET /api/v1/pipelines/:pipelineId/alerts` returns `{ generatedAt, alerts }`
with the same alert shape. A pipeline summary read only stamps alerts that
are already open: it lacks stage, reader and fabric inputs, so it neither
opens nor resolves any.

### `GET /api/v1/events`

`pipeline_id` (optional) and `limit` (default 100, max 1000; the log keeps
the last 1000 events, `MAX_EVENTS`). Returns
`{ generatedAt, count, events: [{ seq, kind, pipelineId, timestamp, … }] }`.

### `GET /api/v1/pipelines/:pipelineId/summary`

Operator summary: `input`, `source` (`status`, `bitrateKbps`, `protocol`,
`readers`), output rollup, graph node and edge counts, recording, HLS
preview and alerts. `404` for an unknown pipeline.

### `GET /api/v1/engine/resource-map`

Without `pipeline_id`, `scope.kind` is `runtime`; with it, `pipeline`, and
stage and output nodes are filtered to that pipeline.

| Param | Default | Notes |
|---|---|---|
| `view` | `grouped` | `summary`: counters only. `grouped`: outputs collapsed by kind, protocol and execution. `detail`: top nodes plus, at runtime scope, `memoryAccounting`. Unknown values fall back to `grouped`. |
| `top_n` | 25 | Clamped to 1–200 |

- The summary holds measured process and child fields (CPU, RSS, threads,
  fds, FFmpeg children, SRT sender permits) and fabric shard counters
  (`fabricShardThreadCount`, `fabricShardStalledCount`,
  `fabricShardPanickedCount`). Process fields, `retainedPayloadBytes` and
  `activeTranscoderBuffers` are runtime-wide at either scope.
- Node `execution` is `tokio_task`, `os_thread`, `shard_thread`,
  `child_process`, `shared` or `process`. A fabric egress node is
  `shard_thread` with `threads.appOwned: 0`; an egress off the fabric is
  `tokio_task`. Output nodes report `memory: null`.
- `egress_shard` nodes (`feedId`, `shardIndex`, `state`) are every live shard
  at runtime scope, and the shards this pipeline's outputs are placed on at
  pipeline scope. They hold no attributed memory (`confidence: unmeasured`)
  and are cut first when nodes exceed `top_n`.
- Memory confidence: `measured` (process RSS, child RSS, thread/fd counts),
  `derived` (ring payload stats, stage counters), `estimated` (unassigned
  overheads), `unmeasured` (egress shards).
- Some nodes carry `judgement` `{ verdict, center, utilization, measured,
  reason }`. A shard is `limiting` when not `healthy`, at 80% or more of its
  command queue, or without progress for 1 s; else `clear`. A fabric egress
  node takes its shard's judgement. FFmpeg stages and the source ring take
  their shared pool's utilization: `limiting` at 100%, `pressured` at the
  safe utilization (80%), else `clear`.
- `limits` has `totalNodeCount`, `returnedNodeCount`, `truncatedNodeCount`
  and `maxTopN`.

## Notifications

The engine evaluates alerts every 5 s (`run_alert_notifier`), whether or
not a dashboard is open, and tells each recipient that wants a confirmed
change (`src/application/notify.rs`):

- An alert must be present for 2 evaluations in a row before it is
  announced as opened, and absent for 2 before it is announced as resolved.
  A condition that flickers for one sample pages nobody. Alerts already
  open when the engine starts are learned, not announced.
- A recipient wants an event when its severity is at least the recipient's
  `minSeverity` (`warning` < `critical`). For channels, resolutions also
  need `onResolved`.
- Each delivery gets up to 3 tries, about 2 s and 15 s apart (plus up to
  25% jitter). A 4xx other than 408 and 429 is final. A push service's 404
  or 410 removes the subscription. Each outcome is logged (newest 2,000).
- Deliveries run off the evaluation loop: a slow receiver never delays the
  next check. The HTTP client has a 10 s timeout and follows no redirects.

| Method | Route | Role | Purpose |
|---|---|---|---|
| `GET`, `POST` | `/api/v1/notifications/channels` | admin | List channels; add one |
| `PUT`, `DELETE` | `/api/v1/notifications/channels/:id` | admin | Edit (the kind is fixed); remove |
| `POST` | `/api/v1/notifications/channels/:id/test` | admin | Send a test now; returns the delivery |
| `GET` | `/api/v1/notifications/channels/:id/secret` | admin | A webhook's signing secret |
| `GET` | `/api/v1/notifications/deliveries?limit` | admin | Delivery outcomes, newest first (1–500, default 100) |
| `GET` | `/api/v1/notifications/push/key` | any | `{ "publicKey" }`: the VAPID key browsers subscribe with |
| `GET`, `POST` | `/api/v1/notifications/push/subscriptions` | any | The caller's own browsers; register one |
| `DELETE` | `/api/v1/notifications/push/subscriptions/:id` | any | Remove one of the caller's own |
| `POST` | `/api/v1/notifications/push/test` | any | A test push to the caller's browsers |

A channel is
`{ "kind": "webhook" | "slack" | "discord", "name", "target", "minSeverity", "onResolved", "enabled" }`.
`target` must be an `http(s)` URL; the listing never includes a secret.
Adding a webhook returns `{ "channel", "secret" }` once.

What each kind is sent:

- **webhook**: `{ "schema": "restream.notify.v1", "event": "opened" | "resolved" | "test", "atMs", "alert": { "id", "severity", "title", "cause", "recommendedAction", "pipelineId"?, "outputId"? }, "link" }`.
  It carries the header `X-Restream-Event` and
  `X-Restream-Signature: t=<unix s>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`.
  A receiver recomputes the signature and rejects stale `t` values.
- **slack**: `{ "text" }` with the headline, cause, next step and link.
- **discord**: `{ "content", "embeds": [{ "title", "description", "color", "url" }] }`.

`link` points at `RESTREAM_PUBLIC_URL` when that is set.

**Browser push.** A subscription is the browser's `PushSubscription.toJSON()`
(`{ "endpoint", "keys": { "p256dh", "auth" } }`) plus `minSeverity`, and it
belongs to the member who registers it. The endpoint must be `https` on a
named public host: no address literals, `localhost` or `.local` names, so a
member cannot point the engine at internal services. Messages are encrypted
per RFC 8291 (`aes128gcm`) and signed with a VAPID key (RFC 8292) that the
engine generates on first use and keeps in `meta`. Its `sub` claim is
`RESTREAM_PUSH_SUBJECT`. `TTL` is one hour, and `Urgency` is `high` for a
critical opening. The service worker (`public/sw.js`) shows the payload
`{ "title", "body", "tag", "severity", "url" }`. One notification is kept per
alert, and a click opens the incident view.

## Schedules

Scheduled events start or stop a pipeline's destinations or its file
source at set times (`src/application/schedules.rs`). The scheduler task
(`run_scheduler`, every 5 s) runs each due occurrence, whether or not a
dashboard is open.

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/schedules` | `{ "schedules": [...] }`, each with `nextAtMs` |
| `POST` | `/api/v1/schedules` | Add one (`201`) |
| `PUT`, `DELETE` | `/api/v1/schedules/:id` | Replace its settings; remove it |
| `POST` | `/api/v1/schedules/:id/run` | Run the action now: `{ "ok": true, "result" }`, or `409 { "ok": false, "error" }` |

Reads need a viewer; changes need an operator.

A schedule is
`{ "pipelineId", "name", "action", "outputIds", "atMs", "repeat", "enabled" }`.
Responses add `id`, `lastRunMs`, `lastResult`, `createdAtMs` and `nextAtMs`.

- `action` is one of:
  - `startOutputs` / `stopOutputs`: set the listed destinations' desired
    state (`outputIds` empty: all of the pipeline's) and wake the
    reconciler, as the per-output start and stop routes do;
  - `startFile` / `stopFile`: play or stop the pipeline's configured file
    or playlist (no `outputIds`, else `409`).
- `atMs` is the first occurrence in UTC milliseconds. `repeat` is `once`,
  `daily` (every 24 h of UTC) or `weekly`. A daily event keeps its UTC time
  across a daylight-saving change.
- An occurrence runs if the scheduler sees it within 60 s
  (`RUN_GRACE_MS`). One missed by more (the engine was down) is recorded as
  `skipped: the engine was not running at …` and never fired late.
- Occurrences already past when a schedule is added or edited are not run.
- `lastResult` is `running`, `ok: …` (what it did), `failed: …` or
  `skipped: …`. Each run is also in the audit log as member `scheduler`,
  method `RUN`.

## Share links

A share link lets anyone watch one pipeline's live preview without signing
in, until it expires or an admin revokes it (`src/application/share.rs`).

| Method | Route | Role | Purpose |
|---|---|---|---|
| `GET` | `/api/v1/pipelines/:pipelineId/share-links` | admin | `{ "links": [...] }` |
| `POST` | `/api/v1/pipelines/:pipelineId/share-links` | admin | `{ "label", "lifetimeHours" }` (`null`: until revoked) → `201` link |
| `DELETE` | `/api/v1/share-links/:id` | admin | Revoke: `{ "deleted": true }` |
| `GET` | `/watch/:token` | public | The player page; `404` once expired or revoked |

A link is
`{ "id", "pipelineId", "label", "token", "createdAtMs", "expiresAtMs", "views" }`.

- `token` is 48 hex characters. Admins read it, so a link can be copied
  again.
- A lifetime is 1 hour to 1 year, or until revoked. `views` counts
  watch-page loads.
- The token admits HLS reads (`/hls/:pipelineId/…?t=<token>`) of its own
  pipeline only, never `/hls/inputs/…`, the API or another pipeline.
  `require_hls_access` accepts a session or a live token.
- Links are held in memory and reloaded on every change. Revoking stops
  every player at once, including one already watching. Deleting a pipeline
  deletes its links.
- The player is a bare page using hls.js; every playlist and segment
  request carries the token. It sends `Content-Security-Policy:
  frame-ancestors *`, so any site can embed it in an iframe. That header
  takes precedence over the server-wide `X-Frame-Options: SAMEORIGIN`. Its
  asset and stream URLs are relative (`../`), so it works under a proxy
  prefix.

## Telemetry

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/api/v1/measure` | Fixed-size counters for capacity measurement |
| `GET` | `/api/v1/engine/telemetry` | Every ingest, input, stage and egress |
| `GET` | `/api/v1/pipelines/:pipelineId/telemetry` | One pipeline's ingest, ring, stages, egresses, delivery |
| `GET` | `/api/v1/stages/:stageKey/telemetry` | One stage by key (`404` if inactive) |

### `GET /api/v1/measure`

The response size does not grow with output count. Byte fields sum every ring
or queue of their kind; `delivery` folds every pipeline's per-feed rows, with
`ratioMin` and `jainMin` `null` until an output has a delivery sample.

```json
{
  "pipelines": 1, "ingests": 1, "egresses": 500, "stages": 0,
  "activeTranscoderBuffers": 0, "ingestBytesReceived": 123456789,
  "retainedPayloadBytes": 4194304, "sourceRingPayloadBytes": 4194304,
  "transcoderRingPayloadBytes": 0, "tsMuxerRingPayloadBytes": 0,
  "delivery": { "destinations": 500, "rated": 500, "delivered": 500, "ratioMin": 0.97, "jainMin": 0.9999 }
}
```

### `GET /api/v1/engine/telemetry`

Returns `{ generatedAt, ingests, inputs, stages, egresses,
activeTranscoderBuffers, memoryAccounting }`.

- `ingests[]`: `pipelineId`, `protocol`, `uptimeSecs`, `bytesReceived`,
  `metrics`.
- `inputs[]`: per session `inputId`, `uptimeSecs`, `bytesReceived`,
  `quality`.
- `stages[]`: `stageKey`, `pipelineId`, `kind`, `metrics` (`packetsIn`,
  `packetsOut`, `bytesIn`, `bytesOut`, `processingUs`), `pipeMetrics`.
- `egresses[]`: `outputId`, `pipelineId`, `protocol`, `targetUrl`,
  `targetAddr`, `status`, `phase`, `uptimeSecs`, `bytesOut`,
  `lastProgressAt`, `lastProgressAgeMs`, `lastError`, `lastErrorAt`,
  `failurePhase`, `fabric`, `fabricFeed`, `shardId`, `quality` (TCP or SRT
  sender stats), `metrics`.

### `GET /api/v1/pipelines/:pipelineId/telemetry`

Returns `{ generatedAt, pipelineId, ingest, sourceRing, stages, egresses,
delivery }`. `sourceRing` has `fill`, `capacity`, `fillPercent`,
`estimatedPktRatePerSec`, `bufferDepthSecs`, `payloadStats` and `readers`
(`name`, `lagSlots`, `overflowCount`, `packetAgeMs`). Fabric outputs are not
ring readers, so a reader overflow sum does not cover them.

`delivery` has one row per feed: outputs that read the same stage over the
same protocol. Each row says how many outputs met the 0.95 delivery floor and
how evenly they were served (Jain's index)
(`{ feed, protocol, destinations, rated, delivered, floor, ratioMin, jain }`);
see [Observability](observability.md#per-destination-delivery).

### `GET /api/v1/stages/:stageKey/telemetry`

One stage by key (for example `pipe1:video:720p`): `generatedAt`,
`stageKey`, `pipelineId`, `kind`, `metrics` and `pipeMetrics` (subprocess
pipe counters, or `null`).

## HLS pull

All routes are `GET` and need the session cookie (`require_hls_access`).

| Route | Purpose |
|---|---|
| `/hls/:pipelineId`, `/hls/:pipelineId/index.m3u8` | Primary media playlist |
| `/hls/:pipelineId/master.m3u8` | Master playlist with alternate audio |
| `/hls/:pipelineId/:segment` | Video segment alias |
| `/hls/:pipelineId/video/index.m3u8` | Video media playlist |
| `/hls/:pipelineId/video/init.mp4` | Video init segment |
| `/hls/:pipelineId/video/:segment` | Video segment |
| `/hls/:pipelineId/audio/:trackIndex/index.m3u8` | Audio media playlist |
| `/hls/:pipelineId/audio/:trackIndex/init.mp4` | Audio init segment |
| `/hls/:pipelineId/audio/:trackIndex/:segment` | Audio segment |
| `/hls/:pipelineId.hevc/…`, `/hls/inputs/:inputId.hevc/…` | The same routes for an H.265 source passed through as `hvc1` (no transcode), for a browser that decodes H.265; an H.264 source plays as it is under either id |
| `/hls/inputs/:inputId/master.m3u8` | Input-scoped master playlist |
| `/hls/inputs/:inputId/video/index.m3u8` | Input-scoped video playlist |
| `/hls/inputs/:inputId/video/init.mp4` | Input-scoped video init |
| `/hls/inputs/:inputId/video/:segment` | Input-scoped video segment |
| `/hls/inputs/:inputId/audio/:trackIndex/index.m3u8` | Input-scoped audio playlist |
| `/hls/inputs/:inputId/audio/:trackIndex/init.mp4` | Input-scoped audio init |
| `/hls/inputs/:inputId/audio/:trackIndex/:segment` | Input-scoped audio segment |

- Content types: playlists `application/vnd.apple.mpegurl`, video
  `video/mp4`, audio `audio/mp4`.
- Errors are JSON `{ error, status, code }`: `404` with `hlsNoStream`,
  `hlsNoSegments` (plus `blockedBy { stage, phase }` when a video stage
  blocks it), `hlsAudioTrackNotFound`, `hlsSegmentNotFound` or
  `hlsInitSegmentNotFound`; `400` with `hlsInvalidSegmentName`.
- Input-scoped stores exist for connected inputs whether or not they are
  selected, so an operator can watch a warm standby without forwarding it.
- No CORS headers are sent, so a browser player on another origin cannot
  fetch these routes.

## HLS push ingest

| Method | Route | Purpose |
|---|---|---|
| `PUT`, `POST` | `/ingest/hls/:streamKey/*path` | Upload a `.m3u8` playlist, an MPEG-TS `.ts` segment or an fMP4 `.mp4`/`.m4s` object |
| `DELETE` | `/ingest/hls/:streamKey/*path` | Accepted and ignored (FFmpeg `delete_segments`) |

The stream key is the credential. It is checked before any body byte is read,
under the `hls-publish` rate-limit scope; no session cookie is used. These
routes sit in `create_hls_ingest_router`, outside the 4 MiB API limit and the
reconciler wake; their own limits apply (`HlsIngestLimits` in
`crates/restream-engine/src/hls_ingest.rs`).

| Status | Meaning |
|---|---|
| `200` | Playlist applied, segment completed a listed sequence, or a duplicate or other-variant object |
| `202` | Segment held until a playlist lists it |
| `400` | Invalid object path, unparseable playlist, or unparseable fMP4 (no `moof` + `mdat`, no `tfdt` in a `traf`, an explicit `tfhd` base offset, truncated) |
| `403` | Wrong key, disabled input, banned client, or the key no longer authorizes the input |
| `405` | Any other method (`Allow: PUT, POST, DELETE`) |
| `408` | No body bytes for 10 s, body slower than max(4 × target duration, 40 s), or both upload slots of the key busy for 10 s |
| `409` | Another publisher (RTMP, SRT, file) holds the input |
| `413` | Over the limit: playlist 1 MiB, segment 64 MiB, fMP4 init 4 MiB |
| `415` | Not `.m3u8`, `.ts`, `.mp4` or `.m4s`; a stream mixing MPEG-TS and fMP4; an fMP4 segment under a playlist without `EXT-X-MAP`; or an init whose codec is not H.264, HEVC or AAC |
| `503` | The 512 MiB ingest buffer is full or the session ended with the object queued (both `Retry-After: 1`), or the key lookup or session is unavailable |

- fMP4 ingest carries H.264 or HEVC video and AAC-LC audio. The init segment
  is named by the playlist's `EXT-X-MAP` URI, not by extension.
- The init is remembered per stream key, so an encoder that uploads it once
  keeps publishing after its session ends. A refused init upload voids that
  memory. A segment that fails to demux is dropped and the session continues.
- Connections whose first request line targets `/ingest/hls/` are served with
  HTTP half-close, so encoders that close right after the body still deliver
  it (`src/api/http_server.rs`).

Ordering, sessions and per-key bounds:
[HLS push ingest](media-pipeline.md#hls-push-ingest).

## Static pages

| Method | Route | Behavior |
|---|---|---|
| `GET` | `/login` | Login page; a valid session is redirected to `./` |
| `GET` | `/login.html` | Redirect to `login` |
| `GET` | `/settings.html` | Redirect to `./?w=settings` |
| `GET` | `/status.html` | Redirect to `./?w=system` |
| `GET` | `/logo.png`, `/output.css` | Embedded assets |
| `GET` | any other path | SPA fallback: an asset path serves the asset (an `.html` asset needs a session); any other path needs a session (else redirect to `login`) and serves `index.html` |

- Assets are embedded from `public/` (debug builds read `public/` from disk
  first). Responses carry a content-hash `ETag`; `If-None-Match` gets `304`.
- `.html`, `.js` and `.css` are `Cache-Control: no-cache`; other assets
  `public, max-age=3600`.
- A nested shell path (`a/b`) is redirected by relative `../` steps, query
  kept, so the shell works under a proxy path prefix.
