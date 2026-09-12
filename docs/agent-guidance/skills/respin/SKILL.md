---
name: respin
description: Start or restart the local restream live demo with MediaMTX, a fixture publisher, and three outputs; optionally rebuild or skip seeding.
---

# Respin

`/respin [--build] [--no-seed]`

Restart the local demo. `--build` forces a rebuild; otherwise rebuild a missing
or stale binary. `--no-seed` starts only restream and MediaMTX.

Identify demo processes from saved PIDs, command lines, and workspace before
stopping them. A process name alone does not establish ownership; do not use a
host-wide `pkill`. Follow AGENTS.md's build preflight and preserve unrelated
live workloads. Keep logs, cookies, database, PIDs, and generated sink config
in a fresh run directory under `/tmp/restream-live/`.

## Setup

- Build with `scripts/build/resource-limit.sh cargo build --profile bench --bin restream`.
  Cargo places this binary at `target/release/restream`.
- If frontend assets changed, run `npm run build:frontend` before Cargo and
  touch `src/api/static_assets.rs` to refresh the embedded assets.
- Start with `RESTREAM_INITIAL_ADMIN_PASSWORD=admin`,
  `RESTREAM_HTTP_BIND_ADDR=127.0.0.1`, `RESTREAM_HTTP_PORT=39280`,
  `RESTREAM_RTMP_PORT=30280`, `RESTREAM_SRT_PORT=31280`,
  and run-local `RESTREAM_DB_PATH` / `RESTREAM_LOG_DIR`.
  This password is for the loopback demo only.
- Log in at `/api/v1/auth/login` and create the pipeline through
  `/api/v1/pipelines`, using the current API schemas.
- Publish `test/fixtures/media-library/colorbar-timer-2v16a.mp4` over SRT with
  stream key `live`. Stream IDs are `publish:live` and `read:live`, without
  an RTMP-style `live/` prefix.

Seed `RTMP_720p`, `SRT_source`, and `SRT_720p` outputs into the local sink.
Output payloads contain `name`, `url`, `monitoringUrl`, and
`config: {video: {mode: "source"}, audio: {mode: "all"}}`; presets use
`video: {mode: "preset", preset: "720p"}`. Check the current graph for expected
transcoder workers rather than assuming a fixed count.

## Verification

Verify the dashboard at `http://127.0.0.1:39280/`, login, input `on` in
`/api/v1/engine/health`, and a populated source ring in
`/api/v1/pipelines/<id>/telemetry`. Account for the publisher, server, sink,
and output-driven workers. For `--no-seed`, verify server/sink readiness only.

Report the dashboard URL, demo credentials, ingest StreamID, sink URLs, and
log directory. Name retrying or stalled outputs explicitly.
