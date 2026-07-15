# Restream

Restream is a Rust live-stream routing service. One process owns the dashboard,
API, SQLite state, RTMP/SRT ingest, RTMP/SRT egress, HLS preview, recording,
and the media-stage orchestration around transcoding.

This README is intentionally short. It should get a new developer from clone to
useful context without making them read the whole system on day one.

## Contents

- [Start here](#start-here)
- [Develop from source](#develop-from-source)
- [Daily loop](#daily-loop)
- [Codebase map](#codebase-map)
- [Runtime and live-harness containers](#runtime-and-live-harness-containers)
- [Read next](#read-next)
- [Expectations](#expectations)

## Start here

With the Linux x86_64 `restream` executable from a GitHub release in the
current directory, start it directly:

```sh
RESTREAM_INITIAL_ADMIN_PASSWORD=change-me ./restream
```

Choose a strong initial password outside local evaluation. Then open
`http://localhost:3030`; stop the process with `Ctrl+C`. Runtime state is
created under `.restream/` in the current directory.

The release also provides the project license, third-party notices and license
texts, and an SBOM. Release automation builds and certifies the downloadable
bytes; `scripts/build/app-static.sh` is a separate engineering tool and is not
the source of the archive.

Default ports:

- `3030` for the dashboard and API
- `1935` for RTMP ingest/play
- `10080` for SRT ingest/read

The dashboard/API binds to `127.0.0.1` by default. Override that with
`RESTREAM_HTTP_BIND_ADDR` when you intentionally want to expose it on another
interface.

When `RESTREAM_INITIAL_ADMIN_PASSWORD` is unset, Restream generates a
high-entropy initial password and writes it next to the SQLite database as
`restream-initial-admin-password.txt` with owner-only permissions.

Release operators should follow the [release runbook](docs/release-runbook.md),
which owns local due diligence, GitHub dry-runs, and gated tag publishing.

## Develop from source

On a fresh Debian or Ubuntu development host, install the repository toolchain
once:

```sh
scripts/dev/bootstrap.sh
```

Then prepare generated inputs, build the development binary, and run that
binary directly:

```sh
scripts/dev/prepare.sh
scripts/build/resource-limit.sh scripts/build/app-native.sh
RESTREAM_INITIAL_ADMIN_PASSWORD=change-me target/debug/restream
```

`bootstrap.sh` owns host packages, Rust, Node, frontend dependencies, MediaMTX,
and the pinned native prefix. `prepare.sh` assumes that host setup already
exists; it refreshes the native prefix and generated frontend assets for the
checkout. See the [developer guide](docs/development.md) for other Linux
distributions and scoped workflows.

## Daily loop

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

## Codebase map

- `src/api/` and `src/lib.rs`: routes, handlers, app startup, and runtime wiring
- `src/media/`: ingest, egress, mux/demux, ring buffers, HLS, transcoding
- `src/domain/`: persisted models and business logic
- `src/planner/`: pipeline planning/orchestration helpers
- `web/`: authored dashboard pages, assets, styles, and TypeScript
- `public/`: generated browser output; never edit it directly
- `test/fixtures/`, `test/native/`, `test/frontend/`, `test/harness/`: committed media, native probes, frontend tests, and live-harness support
- `tests/`: Rust integration tests
- `scripts/build/`, `scripts/check/`, `scripts/dev/`, `scripts/harness/`: builds, gates, setup, and live validation

## Runtime and live-harness containers

The Dockerfile rebuilds native dependencies and frontend output from the same
committed scripts used locally in a clean build container, then produces a
distroless runtime image with the generated binary, release metadata, and the
owned runtime paths. The media stack and embedded FFmpeg remain linked from the
repo-managed static native prefix.

```sh
docker build \
  --build-arg RESTREAM_BUILD_GIT_COMMIT="$(git rev-parse HEAD)" \
  --build-arg RESTREAM_BUILD_TIMESTAMP="$(git show -s --format=%cI HEAD)" \
  -t restream:container .
docker run --rm \
  -e RESTREAM_INITIAL_ADMIN_PASSWORD=change-me \
  -p 3030:3030 \
  -p 1935:1935 \
  -p 10080:10080/udp \
  restream:container
```

The provenance arguments are required because `.git/` is intentionally absent
from the Docker context. They are embedded in both the binary and OCI labels;
the build fails instead of publishing placeholder provenance when either is
missing. License and source information is available inside the runtime image
at `/usr/share/doc/restream/distribution/`.

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
docker build \
  --build-arg RESTREAM_BUILD_GIT_COMMIT="$(git rev-parse HEAD)" \
  --build-arg RESTREAM_BUILD_TIMESTAMP="$(git show -s --format=%cI HEAD)" \
  --target harness -t restream:harness .
docker run --rm --network host \
  restream:harness mixed.live.srt.h264.a1.bf0 --no-netns
```

Use `--network host` for harness modes that open loopback publishers and sinks;
the harness binary is the target's entry point, so modes and harness flags are
passed directly. The normal production image needs only its documented TCP/UDP
ports. The `runtime-ubuntu` target remains available as a compatibility
fallback, but the default image is `runtime`/distroless.

## Read next

- [Documentation Guide](docs/README.md): reading paths and the complete documentation index
- [Developer Guide](docs/development.md): setup, inner loop, tests, benchmarks, static build
- [Architecture](docs/architecture.md): runtime shape and major moving parts
- [Configuration](docs/configuration.md): env vars, ports, paths, persisted settings
- [API Reference](docs/api-reference.md): route-level behavior
- [Testing](docs/testing.md): verification strategy and live test entry points
- [Observability](docs/observability.md): health, diagnostics, telemetry
- [Current Priorities](docs/current-priorities.md): durable platform priority themes and links to actionable work

## Expectations

The repository includes deep reference docs because the runtime is doing real
media work, but you do not need all of them to start contributing. Begin with
the developer guide and architecture doc, then pull in the more specific docs
only when your change touches those areas.
