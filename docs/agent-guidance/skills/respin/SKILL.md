---
name: respin
description: Tear down any running restream/mediamtx/ffmpeg setup, optionally rebuild the bench binary, then spin up a fresh live demo (mediamtx sink + restream server + SRT publisher with the 2v16a multi-track file) and seed an accounted 3-output pipeline. Use when the user says "respin", "restart the setup", "spin up a live stream", "start the demo", or wants the dashboard running with a live feed.
---

# Skill: respin

Tear down any running restream/mediamtx/ffmpeg setup, optionally rebuild the bench binary, then spin up a fresh live demo: mediamtx sink + restream server + SRT publisher sending the 2v16a multi-track test file. Seeds a pipeline with RTMP 720p, SRT source, and SRT 720p outputs. This keeps the default HEVC demo to two output-driven FFmpeg workers: one HEVC-preserving SRT 720p stage and one RTMP H.264 compatibility edge from that 720p stage.

Use this skill when the user says "respin", "restart the setup", "spin up a live stream", "start the demo", "rebuild and respin", or asks to get the dashboard running with a live feed.

## Usage

`/respin [--build] [--no-seed]`

- `--build`: force rebuild the bench binary before starting (default: rebuild only if source is newer than binary)
- `--no-seed`: start restream and mediamtx but skip pipeline creation and publisher

## Current Contract

Use this as a checklist, not a frozen script. Prefer inspecting the current API
or existing helper code over copying snippets blindly.

- Stop only the demo processes by exact process name: `restream`, `mediamtx`,
  and `ffmpeg`. Never use `pkill -f`.
- Do not build while a live pipeline is running. Kill the demo first, then
  rebuild if needed with `scripts/build/resource-limit.sh cargo build --profile
  bench`.
- If frontend assets changed, run `npm run build:frontend` first and touch
  `src/api/static_assets.rs` before the Cargo build so embedded assets refresh.
- Use a fresh `/tmp/restream-live` workspace for logs, cookies, DB, pids, and
  generated MediaMTX config.
- Current API routes are under `/api/v1`: login at `/api/v1/auth/login`,
  engine health at `/api/v1/engine/health`, pipeline creation at
  `/api/v1/pipelines`, and per-pipeline telemetry at
  `/api/v1/pipelines/<id>/telemetry`.
- Start restream with `RESTREAM_INITIAL_ADMIN_PASSWORD=admin`,
  `RESTREAM_HTTP_BIND_ADDR=127.0.0.1`, `RESTREAM_HTTP_PORT=39280`,
  `RESTREAM_RTMP_PORT=30280`, `RESTREAM_SRT_PORT=31280`, a fresh
  `RESTREAM_DB_PATH`, and `RESTREAM_LOG_DIR=/tmp/restream-live/logs`.
- Use `test/fixtures/media-library/colorbar-timer-2v16a.mp4` for the demo
  publisher unless the fixture has moved.
- For SRT, use `streamid=publish:<key>` or `streamid=read:<key>`. Do not add
  the old RTMP-style `live/` path prefix. If the demo stream key is `live`, the
  ingest StreamID is exactly `publish:live`.

## Default Demo Shape

Default to the smallest demo that still exercises the interesting graph:

- One SRT publisher into restream using stream key `live`.
- Three outputs: `RTMP_720p`, `SRT_source`, and `SRT_720p`.
- This should account for the expected output-driven FFmpeg workers without
  adding an extra source-copy RTMP output.

Output creation currently expects a config-shaped payload:

```json
{
  "name": "SRT_source",
  "url": "srt://127.0.0.1:34080?streamid=publish:srt-src&pkt_size=1316",
  "monitoringUrl": null,
  "config": {
    "video": { "mode": "source" },
    "audio": { "mode": "all" }
  }
}
```

For presets, use `"video": { "mode": "preset", "preset": "720p" }`.

## Verification

Before reporting success, verify:

- Dashboard/API is reachable at `http://127.0.0.1:39280/`.
- Login with password `admin` works and cookies are saved.
- `/api/v1/engine/health` shows the demo pipeline input `on`.
- Telemetry shows a populated source ring for the created pipeline.
- Process list is explainable: one fixture publisher, one restream process, one
  MediaMTX process, and only the expected output-driven FFmpeg workers.

Report the dashboard URL, login password, ingest StreamID, sink URLs, and log
paths. If an output is retrying or stalled, say so explicitly instead of
calling the respin clean.
