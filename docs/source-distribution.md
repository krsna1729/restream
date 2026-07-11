# Source Distribution Manifest

This repository is the source distribution. It intentionally does not commit
large generated native build artifacts.

## Committed Inputs

- Rust sources, tests, benchmarks, `Cargo.toml`, and `Cargo.lock`.
- Frontend TypeScript sources under `public/ts/`, static HTML/CSS inputs, and
  generated `public/js/`/`public/output.css` artifacts for runtime embedding.
- Node toolchain metadata: `package.json`, `package-lock.json`, and
  `tsconfig.json`.
- Native build scripts and metadata under `scripts/`, including the pinned
  static-build workflow.

## Generated Artifacts

The following paths are generated locally and are not part of the committed
source bundle:

- `.build/static/prefix/`: static SRT, Mbed TLS, FFmpeg, x264, and x265 prefix.
- `.build/static/env.sh`: generated native build environment.
- `target/` and `test/artifacts/`: Rust build and harness outputs.
- `node_modules/`: installed frontend dependencies.

Regenerate the native prefix with:

```sh
scripts/resource-limit ./scripts/setup-static-build.sh
```

Regenerate frontend runtime assets with:

```sh
npm ci
npm run build:frontend
```

The checked-in frontend bundle must remain reproducible from `public/ts/`,
`package-lock.json`, and the vendored dependency versions. The HLS bundle sync
step deliberately strips the `hls.min.js.map` source-map directive because the
map is not shipped.
