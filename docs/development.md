# Developer guide

This page takes you from a fresh host to a working change. Scripts own the
exact steps; this page says which script to run and when. Run any script
with `--help` for its options.

## Contents

- [Prerequisites](#prerequisites)
- [First build](#first-build)
- [Daily loop](#daily-loop)
- [Frontend work](#frontend-work)
- [Sharing one host](#sharing-one-host)
- [Containers](#containers)
- [Release builds](#release-builds)

## Prerequisites

Restream builds on Linux x86_64. The scripts support Debian and Ubuntu.

| Host | Run once |
|---|---|
| Development | `scripts/dev/bootstrap.sh` |
| Live harness only (no compiler) | `scripts/dev/bootstrap-runtime.sh` |

`bootstrap.sh` installs the apt packages, the pinned Rust toolchain
(`rust-toolchain.toml`), Node and the frontend dependencies, a pinned
MediaMTX and the Git pre-commit hook. It also checks the host for the live
harness.

- The package list lives in
  [`scripts/data/debian-packages.tsv`](../scripts/data/debian-packages.tsv). Do
  not copy it into docs.
- `--configure-harness-host` makes the user-namespace and SRT UDP-buffer
  settings persistent. Use it only on a machine you want changed.

## First build

```sh
npm run build:frontend
cargo xtask release app-native
RESTREAM_INITIAL_ADMIN_PASSWORD=change-me target/debug/restream
```

1. `npm run build:frontend` builds the embedded frontend assets into `public/`.
   It needs the `node_modules/` that `bootstrap.sh` installs; run `npm ci`
   if it is missing.
2. `cargo xtask release app-native` builds `target/debug/restream`, checks
   that it links no SRT or FFmpeg libraries, and emits its SBOM.
   `RESTREAM_BUILD_PROFILE=release` builds a release binary instead.
3. The binary keeps its state under `.restream/` in the working directory.
   [Configuration](configuration.md) lists the path, listener and auth
   settings.

## Daily loop

```sh
cargo xtask release app-native
cargo test <name>
cargo clippy
cargo fmt --all
```

- Run the test you are writing, not the full suite. GitHub PR CI runs the
  correctness gates. [AGENTS.md](../AGENTS.md#inner-loop) maps each kind of
  change to its CI job.
- The pre-commit hook runs `cargo xtask precommit`: `cargo fmt --all --check`,
  `docs-audit` and `source-audit`. Each CI source gate is one
  `cargo xtask <group>`; `cargo xtask help` lists the groups and their steps.
- Use `--profile bench` instead of `--release` for local builds. Harness
  modes need `cargo xtask build-bench`, which fills `target/bench/`.
- Engine benchmarks are declared in `crates/restream-engine/Cargo.toml` and
  live in `crates/restream-engine/benches/`; the API benchmarks stay in the
  root `Cargo.toml` and `benches/`. A measured
  change puts its numbers in its commit message.
  [High-performance data path](high-performance-data-path.md) says when a
  benchmark is required.

[Testing](testing.md) owns gate choice, harness modes, fixtures and
artifacts.

The [workspace map](architecture.md#workspace-layout) locates implementation
owners and distinguishes package dependencies from runtime threads. Select
the package that owns the relevant test, not just the changed source file:
engine and protocol consumer suites run under `restream-engine`, application
suites under the root `restream` package.
[Testing](testing.md#rust-test-suite) lists focused commands and explains
feature-isolated `-p restream --no-default-features` invocations.

## Frontend work

Edit `web/ts/` and `web/styles/input.css`. Never edit the generated
`public/js/` or `public/output.css`.

```sh
npm run build:frontend
npm run test:frontend
npm run test:e2e
```

- `test:frontend` runs the Node unit tests and a built-module smoke test.
  `test:frontend:coverage` adds coverage.
- `test:e2e` (`cargo xtask frontend e2e`) builds the app, starts it in
  `.local/e2e` and runs Playwright. Use `npx playwright test` only against an
  app you already run on port 3030.
- [package.json](../package.json) owns the command definitions.
  [Frontend](frontend.md) owns the design system and control room structure.

## Sharing one host

Work on a branch in the primary checkout; do not create Git worktrees.
Tasks that share a host also share its checkout, processes, ports and CPU.
Check `git status` before builds and edits.

| Work | Rule |
|---|---|
| Builds | Never build while a live pipeline runs, and check for other builds first ([AGENTS.md](../AGENTS.md#branches-and-build-safety)) |
| Live correctness runs | Run modes through `cargo xtask harness`; it runs `build-bench` first unless `BENCH_BUILD=never`, so Cargo rebuilds what is stale. Overlap runs only when each has its own network, ports and artifacts, as `cargo xtask parallel-fast-breadth` provides |
| Measurements | Benchmarks, sweeps and capacity ramps run serially on an idle host. A competing build or pipeline makes the result invalid |
| Long-lived sessions | Give each dashboard or debug session an owner, its own runtime directory and a cleanup step. Prefer the harness or demo tooling that already owns ports |

Do not kill processes another task owns.

## Containers

The [README](../README.md#run-the-container) shows the production image. The
Dockerfile builds the frontend and binary from the same scripts, then copies
the binary and the BtbN `ffmpeg` and `ffprobe` executables into a distroless
image.

The `harness` target holds the bench-profile `restream` and `test_harness`,
pinned MediaMTX, FFmpeg and the committed fixtures. Its entry point is
`test_harness`, so harness modes and flags go straight after the image name:

```sh
docker build \
  --build-arg RESTREAM_BUILD_GIT_COMMIT="$(git rev-parse HEAD)" \
  --build-arg RESTREAM_BUILD_TIMESTAMP="$(git show -s --format=%cI HEAD)" \
  --target harness -t restream:harness .
docker run --rm --network host \
  --security-opt seccomp=distribution/docker/restream-seccomp.json \
  restream:harness mixed.live.srt.h264.a1.bf0 --no-netns
```

- `--network host` lets harness modes open loopback publishers and sinks.
- The harness image needs the same seccomp profile as production.
- The `runtime-ubuntu` target is an escape hatch for unusual NSS or DNS
  setups. The default target is the distroless `runtime`.

## Release builds

The engine links no FFmpeg; codec work runs in an external `ffmpeg`.
[FFmpeg versions](ffmpeg-versions.md) owns which build runs.

Committed performance evidence uses `cargo xtask build-release`
(`target/qual-release/`). Release candidates follow the
[release runbook](release-runbook.md), which certifies the packaged bytes.
