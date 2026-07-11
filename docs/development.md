# Developer Guide

This guide is the longer companion to the top-level README. Use it when you
need setup details, the normal edit/test loop, or release-build notes.

## Quick Start

For a fresh Debian/Ubuntu machine:

```sh
./scripts/dev/bootstrap.sh
scripts/build/resource-limit.sh ./scripts/build/app-native.sh
cargo run
```

`scripts/dev/bootstrap.sh` installs host packages, the pinned Rust toolchain, frontend
dependencies, a pinned `mediamtx` binary for the live harness, and the
repo-managed native dependency prefix used by the build.

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

There are two different stories here:

- Building from source: requires the host toolchain and native dependencies
- Running a static release binary: does not require those runtime dependencies

If you already have a binary built with:

```sh
scripts/build/resource-limit.sh ./scripts/build/app-static.sh
```

you can run that artifact directly with:

```sh
./restream
```

`scripts/build/app-static.sh` verifies that the produced binary is statically linked, so
the release artifact does not depend on the host having FFmpeg, libsrt, or
other shared runtime libraries installed.

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
  ninja-build perl pkg-config
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

Edit `web/ts/`, not generated files in `public/js/`. The build now re-syncs
the browser HLS runtime from the `hls.js` npm dependency automatically.
Frontend orchestration entrypoints live in `web/ts/app/`, shared transport
and state helpers in `web/ts/core/`, bounded UI modules in
`web/ts/features/`, and history-specific UI in `web/ts/history/`.
The Node-based frontend suite now uses a temporary sourcemapped test build so
coverage reports point at `web/ts/**`, while `npm run test:frontend:js-smoke`
keeps a smaller direct check against the shipped `public/js/**` bundle.
Use `npm run test:frontend:coverage` for the Node-scope TypeScript coverage
gate. That covered surface now includes the dashboard/history/status transport
modules that own the polling-vs-SSE split, plus the small reactive helpers for
output control intent and Rust-process lifecycle indication. Use
`npm run test:frontend:coverage:all` when you want the broader all-files
report as a diagnostic view.

The dashboard runtime surface now prefers a single `/api/v1/dashboard/runtime`
snapshot whenever a refresh needs both engine health and host metrics; only
metrics-only modes still hit `/metrics/system` directly. In selected-pipeline
detail modes, summary health requests now include the selected `pipeline_id` so
the backend can keep summary liveness for every pipeline while upgrading the
active pipeline entry to the full runtime shape in the same response.
Output start/stop now reuse the mutation response to patch local desired state
immediately, then let the already-open lifecycle SSE drive the runtime re-sync
with a short `/api/v1/dashboard/runtime` fallback if no wakeup arrives. The
button busy state now stays pinned until the selected output actually reaches
the requested runtime state, so unrelated lifecycle wakeups do not clear
operator feedback early.
File-ingest start/stop now follow the same pattern when a lifecycle stream is
already open, while cold/no-stream file-ingest controls still fall back
directly to a runtime refresh. The file-ingest button now also shows its own
`Starting...` / `Stopping...` in-flight state immediately so operators do not
have to infer whether the backend accepted the click, and it clears as soon as
the mutation response confirms the new `running` flag while the runtime refresh
continues in the background. Recording start/stop is different in transport shape: the mutation
response already contains the operator-facing `enabled` / `active` state, so
the dashboard patches local recording state immediately instead of forcing a
follow-up runtime fetch, while the button itself still shows immediate
`Starting...` / `Stopping...` feedback during the request. Status mode now
reuses its own restream log SSE
instead of opening a second lifecycle-only dashboard stream on top. Settings
and media modes also use their existing metrics refresh to mark the Rust
process indicator as running immediately, rather than waiting for a later
lifecycle event to clear the initial "Connecting" state. The same reachability
hint now also clears stale `Stopped` / `Faulted` badges once the API is back,
without overriding an in-progress `Stopping` state. Output create/update
flows, output deletes, pipeline create/update flows, and pipeline deletes now
reuse returned mutation payloads or apply targeted local removals to patch
dashboard state immediately instead of following each mutation with another
`/api/v1/settings?view=dashboard` fetch. Pipeline edit/create now also reuses
the dashboard's inline `fileIngest` state when opening the modal and sends file
ingest changes inside the same pipeline mutation, removing the extra
`GET/PUT/DELETE /api/v1/pipelines/:id/file-ingest` round-trips from the common
editor flow. Status mode now also opts out of the dashboard's background
`/metrics/system` poll entirely because that screen already has its own engine
snapshot plus restream-log SSE; this removes a redundant heartbeat without
changing the operator-visible status feed.

Recommended transport split for this dashboard:

| UI surface | Transport | Why |
| --- | --- | --- |
| Restream lifecycle, output/pipeline history live tails, global process indicator | SSE (`/api/v1/logs/stream`) | These are edge-triggered event streams where waiting for the next poll feels laggy and wasteful. SSE stays on plain HTTP, survives ordinary load balancers / tunnels more easily than WebSockets, and gives us `Last-Event-ID` resume without inventing a custom session layer. |
| Dashboard runtime cards, pipeline detail runtime, inspect graph refreshes, host metrics | Polling snapshot (`/api/v1/dashboard/runtime`, `/metrics/system`) | These are durable state snapshots, not append-only events. Polling keeps the backend contract simple, lets the client recover from missed lifecycle events, and avoids pushing high-frequency metric streams through proxies. |
| Mutations that already return the user-visible state (`recording`, config edits, create/update/delete flows) | Mutation response + local patch | If the response already contains what the operator needs to see, refetching immediately is redundant. Patch local state and rerender. |
| Mutations that need runtime confirmation (`output` and `file-ingest` start/stop) | Mutation response + immediate local intent + lifecycle SSE + bounded poll fallback | The click should feel instant, but the runtime still has to converge. Show the in-flight intent immediately, let lifecycle SSE wake the confirming runtime refresh when available, and keep one short fallback refresh for cold/no-stream cases. |
| Background / hidden tabs | Slower polling, close SSE | Hidden tabs should not hold open hot event streams or 5 s runtime polls. Resume from the last event id or take a fresh snapshot when visible again. |

The practical rule is: use SSE for sparse, operator-visible edges; use polling for compact snapshots that must stay self-healing; use mutation responses whenever they already carry the exact UI state.

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

Available suites include:

- `ring_buffer`
- `avio_throughput`
- `high_performance_data_path`
- `hls_cost`
- `matrix_throughput`
- `srt_ingest_latency`
- `transcoder_throughput`
- `codec_conversions`
- `stage_metrics`
- `alert_tracker`
- `stage_feeder`
- `simd_alternatives`

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

For the optimization roadmap behind those benches, see
[High-Performance Data Path](high-performance-data-path.md).

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
