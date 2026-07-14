# Developer Guide

This guide is the longer companion to the top-level README. Use it when you
need setup details, the normal edit/test loop, or release-build notes.

## Contents

- [Quick Start](#quick-start)
- [Running The Binary Directly](#running-the-binary-directly)
- [Manual Prerequisites](#manual-prerequisites)
- [Inner Loop](#inner-loop)
- [Testing](#testing)
- [Benchmarks](#benchmarks)
- [Static Release Build](#static-release-build)
- [Recommended Reading Order](#recommended-reading-order)

## Quick Start

For a fresh Debian/Ubuntu machine:

```sh
./scripts/dev/bootstrap.sh
scripts/build/resource-limit.sh ./scripts/build/app-native.sh
cargo run
```

`scripts/dev/bootstrap.sh` installs host packages, the pinned Rust toolchain,
frontend dependencies, the pinned `mediamtx` live-harness peer, and the
repo-managed native dependency prefix used by the build. It also reports
whether this host can use the private network namespace and SRT buffer policy
expected by live tests. To deliberately persist those host settings, use
`scripts/dev/bootstrap.sh --configure-harness-host`.

`scripts/dev/bootstrap-runtime.sh` is intentionally separate: it installs only
the FFmpeg/MediaMTX and networking tools needed to run the live harness. It
does not install compilers or npm dependencies, but it shares the same explicit
`--configure-harness-host` SRT/sysctl option as the developer bootstrap.
Docker's `harness` target uses this runtime bootstrap with host checks skipped,
because a container must not modify its host kernel.

After `cargo run`, the service is available at `http://localhost:3030`.
The dashboard/API binds to `127.0.0.1` by default; set
`RESTREAM_HTTP_BIND_ADDR=0.0.0.0` or another address when you deliberately want
to expose it beyond loopback.

Default runtime ports:

- `3030`: dashboard and HTTP API
- `1935`: RTMP ingest/play
- `10080`: SRT ingest/read

For the first dashboard login, set `RESTREAM_INITIAL_ADMIN_PASSWORD` in CI or a
local dev environment. If it is unset, Restream generates a high-entropy
initial password and writes it next to the SQLite database as
`restream-initial-admin-password.txt` with owner-only permissions.

## Running The Binary Directly

Source builds require the host toolchain and native dependencies. The supported
portable launch path is the scratch container documented in the README; the
single-file static build is not a release artifact until its startup proof is
restored. A direct source-built binary owns `.restream/data/restream.db`,
`.restream/media/`, `.restream/logs/`, and `.restream/runtime/` in its working
directory by default; explicit database, media, and log-directory environment
variables override their respective locations.

## Manual Prerequisites

If you are not using `scripts/dev/bootstrap.sh`, you will need:

- Rust toolchain pinned in `rust-toolchain.toml`
- FFmpeg development packages available through `pkg-config`
- `clang`, `nasm`, `mold`, `cmake`, `pkg-config`, `perl`
- `ffmpeg` / `ffprobe`, `curl`, `bzip2`, `jq`, `mediamtx`
- Node.js `>= 20` plus `npm` for frontend work

On Debian/Ubuntu, the bootstrap script installs:

```sh
apt-get install -y build-essential bzip2 ca-certificates clang cmake curl ffmpeg \
  git jq libavcodec-dev libavdevice-dev libavfilter-dev libavformat-dev \
  libavutil-dev libswresample-dev libswscale-dev mold nasm \
  ninja-build perl pkg-config iproute2 sqlite3 util-linux
```

Then install a current Node.js toolchain for Tailwind/TypeScript work
(the bootstrap script uses NodeSource `22.x` by default because Tailwind 4's
native tooling requires Node `>= 20`).

Before the first Rust build, make sure the repo-managed native prefix exists:

```sh
scripts/build/resource-limit.sh ./scripts/build/native-deps.sh
```

That native setup builds SRT against a repo-managed Mbed TLS instead of the
host's OpenSSL. [scripts/native/mbedtls-config-srt.h](../scripts/native/mbedtls-config-srt.h)
is intentionally a whole-build replacement config, not a small override: it
keeps only the AES-CTR, PBKDF2-HMAC-SHA1, entropy/CTR-DRBG, and version-report
pieces that SRT's CRYSPR backend actually calls. The goal is a smaller static
artifact, a tighter SBOM, and less unused crypto surface in the shipped binary.

## Inner Loop

The usual backend loop is:

```sh
scripts/build/resource-limit.sh ./scripts/build/app-native.sh
scripts/build/resource-limit.sh cargo test
scripts/build/resource-limit.sh cargo clippy
cargo fmt --all
```

`scripts/build/app-native.sh` verifies that the debug build is using the expected native
linkage, including the repo-managed static `libsrt`.

### Frontend

Only needed when editing `web/ts/` or `web/styles/input.css`:

```sh
npm run build:frontend
npm run test:frontend
npm run test:frontend:browser-dom
```

Edit authored files under `web/`; do not hand-edit generated `public/js/` or
`public/output.css`. The main ownership boundaries are:

- `web/ts/app/` — application composition and workspace orchestration;
- `web/ts/core/` — shared transport, state, and protocol contracts;
- `web/ts/features/` — bounded user-facing capabilities;
- `web/ts/history/` — history-specific state and rendering;
- `web/styles/input.css` — authored styles.

Use `npm run test:frontend:coverage` for the maintained Node-side coverage gate
and `npm run test:frontend:coverage:all` only as a diagnostic all-files view.
Use Playwright when behavior depends on real navigation, focus, media playback,
layout, or browser APIs. Current endpoint and runtime contracts belong in
[the API reference](api-reference.md) and
[observability guide](observability.md), not in this setup guide.

## Testing

For the broader testing story, use [Testing](testing.md). The short version:

```sh
scripts/build/resource-limit.sh cargo test
scripts/build/resource-limit.sh target/bench/test_harness mixed.live.srt.h264.a1
```

Prefer scoped tests first, then broaden when the change crosses module or
protocol boundaries.

## Benchmarks

Run benchmarks before and after hot-path work:

```sh
scripts/build/resource-limit.sh cargo bench --bench <name>
scripts/build/resource-limit.sh cargo bench
```

Benchmark targets are declared in `Cargo.toml` and implemented under
`benches/`; use those as the inventory instead of copying a list into this
guide. The [high-performance data-path guide](high-performance-data-path.md)
maps production concerns to the benchmark workflow.

For the SRT crypto migration specifically, compare plaintext vs encrypted local
socket cost with:

```sh
scripts/build/resource-limit.sh cargo bench --bench srt_ingest_latency -- srt_(ingest|egress)
```

That bench fixes the transport shape at `8 x 1316-byte` live-mode packets per
timed iteration and compares `plain`, `aes128`, `aes192`, and `aes256` via
`SRTO_PBKEYLEN=16/24/32`. That keeps the MPEG-TS-over-SRT packet shape stable
and makes the benchmark answer the narrower question we actually care about:
whether stronger SRT encryption changes hot-path cost.

## Static Release Build

The release path builds pinned native dependencies into
`.local/build/static/prefix/`, then links the Rust binary against them:

```sh
scripts/build/resource-limit.sh ./scripts/build/native-deps.sh
scripts/build/resource-limit.sh ./scripts/build/app-static.sh
```

Use this path when you need the pinned FFmpeg/x264/x265/libsrt toolchain rather
than the faster debug-iteration path.

Helpful variants:

```sh
RESTREAM_REBUILD_NATIVE=1 scripts/build/resource-limit.sh ./scripts/build/native-deps.sh
RESTREAM_BUILD_PROFILE=fast-release scripts/build/resource-limit.sh ./scripts/build/app-static.sh
```

See [FFmpeg Version Configuration](ffmpeg-versions.md) for version-selection
details.

## Recommended Reading Order

For a new contributor, this sequence keeps the context load reasonable:

1. [README](../README.md)
2. [Architecture](architecture.md)
3. [Configuration](configuration.md)
4. [Testing](testing.md)
5. Area-specific docs only when your change needs them
