# Restream

Restream is a Rust live-stream routing service. One process owns the dashboard,
API, SQLite state, RTMP/SRT ingest, RTMP/SRT egress, HLS preview, recording,
and the media-stage orchestration around transcoding.

This README is intentionally short. It should get a new developer from clone to
useful context without making them read the whole system on day one.

## Start Here

On Debian/Ubuntu, the fastest setup path is:

```sh
./scripts/dev/prepare.sh
cargo run
```

`prepare.sh` is the clean-checkout contract: it verifies the committed Node
toolchain, builds the pinned native prefix, and generates `public/` from the
authored files in `web/`. Use `bootstrap.sh` only when a Debian/Ubuntu host
still needs its system packages, Rust toolchain, Node, or Mediamtx installed.

Then open `http://localhost:3030`.

Default ports:

- `3030` for the dashboard and API
- `1935` for RTMP ingest/play
- `10080` for SRT ingest/read

The dashboard/API binds to `127.0.0.1` by default. Override that with
`RESTREAM_HTTP_BIND_ADDR` when you intentionally want to expose it on another
interface.

On first startup, Restream uses `RESTREAM_INITIAL_ADMIN_PASSWORD` when it is
set. Otherwise it generates a high-entropy initial password and writes it next
to the SQLite database as `restream-initial-admin-password.txt` with
owner-only permissions.

## Running Restream

The supported portable launch paths are the scratch container below and the
checksummed Linux x86_64 bundle attached to each GitHub release. The bundle
contains the native runtime closure verified by the container, so after
unpacking it, run `./run restream` without host FFmpeg, SRT, or C/C++ runtime
packages. It also contains the feature-enabled MCP and diagnostic harness
executables; use live harness tooling from a source checkout when it needs
fixtures, MediaMTX, or host network setup.

`scripts/build/app-static.sh` remains an engineering build path. It is not a
single-file release contract until its static-runtime proof is restored.

For a host source build, install the dependencies described in
[docs/development.md](docs/development.md), then use the daily loop below.

## Daily Loop

Most backend work stays in this loop:

```sh
scripts/build/resource-limit.sh ./scripts/build/app-native.sh
scripts/build/resource-limit.sh cargo test
scripts/build/resource-limit.sh cargo clippy
cargo fmt --all
```

If you edit frontend assets:

```sh
npm run build:frontend
npm run test:frontend
npm run test:frontend:browser-dom
```

Use `npm run test:frontend:coverage` for the scoped Node-side TypeScript
coverage gate and `npm run test:frontend:coverage:all` for the broader
diagnostic all-files report.

## Codebase Map

- `src/api.rs` and `src/lib.rs`: app startup, routes, runtime wiring
- `src/media/`: ingest, egress, mux/demux, ring buffers, HLS, transcoding
- `src/domain/`: persisted models and business logic
- `src/planner/`: pipeline planning/orchestration helpers
- `web/`: authored dashboard pages, assets, styles, and TypeScript
- `public/`: generated browser output; never edit it directly
- `test/fixtures/`, `test/native/`, `test/frontend/`, `test/harness/`: committed media, native probes, frontend tests, and live-harness support
- `tests/`: Rust integration tests
- `scripts/build/`, `scripts/check/`, `scripts/dev/`, `scripts/harness/`: builds, gates, setup, and live validation

## Scratch Runtime and Live-Harness Containers

The Dockerfile rebuilds native dependencies and frontend output from the same
committed scripts used locally in a clean build container, then produces a
pure `scratch` runtime. It copies the generated binary's small glibc/C++ loader
closure, certificates, timezone/NSS files, and the writable runtime paths; the
media stack and embedded FFmpeg remain static.

```sh
docker build -t restream:container .
docker run --rm \
  -e RESTREAM_INITIAL_ADMIN_PASSWORD=change-me \
  -p 3030:3030 restream:container
```

Bare Restream and the container use the same owned layout under `.restream/`:
`data/restream.db` (including WAL/SHM sidecars and the initial-password file),
`media/`, `logs/`, and `runtime/ffmpeg/`. A container can run with no mounts
for an ephemeral session; mount one volume at `/.restream` to persist all
state. `RESTREAM_LOG_DIR` overrides the default `.restream/logs` directory;
stdout/stderr and SQLite-backed log history remain enabled as well.

For the complete live protocol harness, build the explicit `harness` target.
It contains the bench-profile `restream` and `test_harness` binaries, pinned
MediaMTX, FFmpeg/ffprobe, and committed fixtures without carrying a compiler or
source checkout:

```sh
docker build --target harness -t restream:harness .
docker run --rm --network host restream:harness mixed.live.srt.h264.a1.bf0 -- --no-netns
```

Use `--network host` for harness modes that open loopback publishers and sinks;
the normal production image needs only its documented TCP/UDP ports. The
`runtime-ubuntu` target remains available as a compatibility fallback, but the
default image is `runtime`/scratch.

## Read Next

- [Developer Guide](docs/development.md): setup, inner loop, tests, benchmarks, static build
- [Architecture](docs/architecture.md): runtime shape and major moving parts
- [Configuration](docs/configuration.md): env vars, ports, paths, persisted settings
- [API Reference](docs/api-reference.md): route-level behavior
- [Testing](docs/testing.md): verification strategy and live test entry points
- [Observability](docs/observability.md): health, diagnostics, telemetry
- [Current Priorities](docs/current-priorities.md): current platform priorities and still-relevant follow-up work

## Expectations

The repository includes deep reference docs because the runtime is doing real
media work, but you do not need all of them to start contributing. Begin with
the developer guide and architecture doc, then pull in the more specific docs
only when your change touches those areas.
