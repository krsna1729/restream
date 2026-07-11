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

## Running A Built Binary

If you already have a release binary produced by
`scripts/build/resource-limit.sh ./scripts/build/app-static.sh`, you can run it directly:

```sh
./restream
```

That static release artifact does not require FFmpeg, SRT, or other shared
runtime dependencies to be installed on the host. The source-build and `cargo`
paths are different: they do require the build dependencies described in
[docs/development.md](docs/development.md).

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

## Scratch Container Proof

The Dockerfile rebuilds native dependencies and frontend output from the same
committed scripts used locally in a clean build container, then produces a
minimal Ubuntu runtime image. The fully static binary is kept in that runtime
layer because the current static SRT build exits in a pure `scratch` filesystem.

```sh
docker build -t restream:container .
docker run --rm --tmpfs /tmp:exec,mode=1777 \
  -e RESTREAM_INITIAL_ADMIN_PASSWORD=change-me \
  -p 3030:3030 restream:container
```

Mount persistent volumes at `/data` and `/media` for a non-ephemeral service.

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
