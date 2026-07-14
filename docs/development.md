# Developer guide

Use this guide to understand the supported contributor workflows and where
their executable definitions live. The top-level [README](../README.md) owns
the shortest clone-to-running path.

## Contents

- [Host setup](#host-setup)
- [Clean-checkout preparation](#clean-checkout-preparation)
- [Running a source build](#running-a-source-build)
- [Daily development](#daily-development)
- [Frontend work](#frontend-work)
- [Testing and benchmarks](#testing-and-benchmarks)
- [Static engineering build](#static-engineering-build)
- [Read next](#read-next)

## Host setup

On Debian or Ubuntu, use the developer bootstrap:

```sh
scripts/dev/bootstrap.sh
```

The bootstrap owns system-package installation, the pinned Rust toolchain,
frontend dependencies, MediaMTX, host readiness checks, Git hooks, and the
repo-managed native prefix. Its current options are documented by
`scripts/dev/bootstrap.sh --help`.

Do not copy its package list into setup documentation. The canonical Debian
package groups and installation behavior live in
[scripts/lib/debian-packages.sh](../scripts/lib/debian-packages.sh). For a host
that only runs the live harness, use
`scripts/dev/bootstrap-runtime.sh --help`; that script deliberately excludes
compiler and frontend setup.

Host-level namespace and SRT buffer changes are opt-in. Use the bootstrap's
`--configure-harness-host` option only when those settings should persist on
that machine.

## Clean-checkout preparation

After host setup, prepare generated build inputs with:

```sh
scripts/dev/prepare.sh
```

This script is the clean-checkout owner for the native prefix and embedded
frontend assets. It fails with a focused instruction when frontend dependencies
have not been installed instead of silently installing a second toolchain.

## Running a source build

Follow the [README daily loop](../README.md#daily-loop) to build and run the
service. A source-built process uses the hidden `.restream/` tree in its
working directory for SQLite state, media, logs, and disposable runtime files.
See [Configuration](configuration.md) for path overrides, listeners,
authentication bootstrap, and deployment settings.

The portable distribution contracts are the released Linux archive and scratch
container. `scripts/build/app-static.sh` is an engineering build path, not a
single-file release contract.

## Daily development

The [README](../README.md#daily-loop) owns the normal backend and frontend
command loop so it does not drift between two newcomer guides.

Use `scripts/build/resource-limit.sh` around Cargo and other heavy commands.
The wrapper owns build serialization and job sizing; its source and usage text
are authoritative. `scripts/build/app-native.sh` additionally verifies that
the development binary uses the expected native linkage.

## Frontend work

Edit authored files under `web/ts/` and `web/styles/input.css`; never edit
generated `public/js/` or `public/output.css` by hand. The current frontend
commands and their composition live in [package.json](../package.json). Run the
scoped frontend test entrypoint for ordinary TypeScript work and the browser-DOM
or Playwright workflow only when behavior depends on real browser facilities.

Layer ownership belongs in [Architecture](architecture.md), API behavior in
[API reference](api-reference.md), and runtime diagnostics in
[Observability](observability.md). This guide intentionally does not duplicate
those maps.

## Testing and benchmarks

[Testing](testing.md) owns gate selection, harness preparation, catalog
inspection, fixtures, and artifact handling. Prefer the narrowest proof that
crosses the changed boundary.

Benchmark target names are declared in `Cargo.toml`, implementations live
under `benches/`, and durable measurements belong in the quality baseline
ledger. The [high-performance data-path guide](high-performance-data-path.md)
explains when a benchmark is required without copying the target inventory.

## Static engineering build

To exercise the fully static engineering path, run:

```sh
scripts/build/resource-limit.sh scripts/build/app-static.sh
```

The build script creates the native prefix when needed, verifies the resulting
binary, and emits its SBOM. Native source pins and build behavior belong to
[FFmpeg version configuration](ffmpeg-versions.md) and the scripts under
`scripts/build/`; do not repeat their internal sequence here.

Release candidates must use the [release runbook](release-runbook.md), which
certifies the packaged bytes rather than treating this engineering build as a
release artifact.

## Read next

1. [Architecture](architecture.md)
2. [Testing](testing.md)
3. [Configuration](configuration.md)
4. Area-specific documents from the [documentation guide](README.md)
