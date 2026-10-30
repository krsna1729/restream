# Restream

[![CI](https://github.com/krsna1729/restream/actions/workflows/ci.yml/badge.svg?branch=master&event=push)](https://github.com/krsna1729/restream/actions/workflows/ci.yml?query=branch%3Amaster+event%3Apush)
[![Nightly](https://github.com/krsna1729/restream/actions/workflows/nightly-full-certification.yml/badge.svg?branch=master&event=schedule)](https://github.com/krsna1729/restream/actions/workflows/nightly-full-certification.yml?query=branch%3Amaster+event%3Aschedule)
[![Release](https://github.com/krsna1729/restream/actions/workflows/release.yml/badge.svg?event=push)](https://github.com/krsna1729/restream/actions/workflows/release.yml?query=event%3Apush)
[![Security](https://github.com/krsna1729/restream/actions/workflows/sbom-security.yml/badge.svg?branch=master&event=schedule)](https://github.com/krsna1729/restream/actions/workflows/sbom-security.yml?query=branch%3Amaster+event%3Aschedule)
[![Harness Image](https://github.com/krsna1729/restream/actions/workflows/ci-harness-runtime-image.yml/badge.svg?branch=master&event=push)](https://github.com/krsna1729/restream/actions/workflows/ci-harness-runtime-image.yml?query=branch%3Amaster+event%3Apush)

Restream is a live-stream router in one Rust process. It takes a stream in
over RTMP, SRT, HLS (push or pull), MoQ or a media file, and sends it out to
many RTMP/RTMPS, SRT, HLS and MoQ destinations. It can transcode on the way,
switch between backup inputs, record, and preview in the browser. A dashboard,
an HTTP API and SQLite state come in the same binary.

## Contents

- [Quick start](#quick-start)
- [Run the container](#run-the-container)
- [Build from source](#build-from-source)
- [Where to read next](#where-to-read-next)

## Quick start

Download the Linux x86_64 release from GitHub and run it:

```sh
RESTREAM_INITIAL_ADMIN_PASSWORD=change-me ./restream
```

Open `http://localhost:3030` and sign in with that password. Stop with
`Ctrl+C`.

| Port | Use |
|---|---|
| 3030 (TCP) | Dashboard, API, HLS push ingest; binds `127.0.0.1` unless `RESTREAM_HTTP_BIND_ADDR` is set |
| 1935 (TCP) | RTMP ingest and play |
| 10080 (UDP) | SRT ingest and read |

- Use a strong password outside local tests. Without
  `RESTREAM_INITIAL_ADMIN_PASSWORD`, Restream generates one and writes it to
  `.restream/data/restream-initial-admin-password.txt` (owner-only).
- State lives under `.restream/` in the working directory: `data/` (SQLite
  and the password file), `media/`, `logs/`, and `runtime/ffmpeg/` (FFmpeg
  fetched by `restream ffmpeg-fetch`).
- MoQ ingest is off until `RESTREAM_MOQ_INGEST_LISTEN` is set. See
  [Configuration](docs/configuration.md).

The release also carries the license, third-party notices and an SBOM. The
[release runbook](docs/release-runbook.md) owns how releases are built.

## Run the container

Restream's RTMP and SRT transports and its egress fabric need `io_uring`.
Docker's default seccomp profile (Docker 25 and later) blocks it, and the
process does not start. The shipped profile is part of the launch command:

```sh
docker build \
  --build-arg RESTREAM_BUILD_GIT_COMMIT="$(git rev-parse HEAD)" \
  --build-arg RESTREAM_BUILD_TIMESTAMP="$(git show -s --format=%cI HEAD)" \
  -t restream:container .
docker run --rm \
  --security-opt seccomp=distribution/docker/restream-seccomp.json \
  -e RESTREAM_INITIAL_ADMIN_PASSWORD=change-me \
  -p 3030:3030 -p 1935:1935 -p 10080:10080/udp \
  restream:container
```

- The image is distroless and runs as UID 1000. It binds the HTTP port on
  `0.0.0.0`.
- Mount one volume at `/.restream` to keep state across runs.
- The two build arguments are required: `.git/` is not in the build context,
  and the build fails without provenance.

[`distribution/docker/README.md`](distribution/docker/README.md) explains the
seccomp profile. The [developer guide](docs/development.md#containers)
covers the live-harness image.

## Build from source

On Debian or Ubuntu:

```sh
scripts/dev/bootstrap.sh
npm run build:frontend
cargo xtask release app-native
RESTREAM_INITIAL_ADMIN_PASSWORD=change-me target/debug/restream
```

The [developer guide](docs/development.md) explains each step, the daily loop
and shared-host rules.

## Where to read next

| Page | Read it for |
|---|---|
| [Documentation index](docs/README.md) | Every page, grouped by task |
| [Architecture](docs/architecture.md) | What runs where: data plane, control plane, threads, layers |
| [Developer guide](docs/development.md) | Setup, daily loop, shared-host rules |
| [Media pipeline](docs/media-pipeline.md) | Protocols, codecs and stages |
| [Configuration](docs/configuration.md) | Environment variables, ports, paths |
| [API reference](docs/api-reference.md) | HTTP routes |
| [Testing](docs/testing.md) | Which gate proves which change |
| [Observability](docs/observability.md) | Health, telemetry, alerts, logs |
| [Backlog](docs/backlog.md) | Open engineering work |
| [AGENTS.md](AGENTS.md) | Rules for coding agents |
