# Source Distribution Manifest

This repository is the source distribution. It intentionally does not commit
large generated native build artifacts.

## Contents

- [Committed Inputs](#committed-inputs)
- [Generated Artifacts](#generated-artifacts)
- [Binary and container distributions](#binary-and-container-distributions)

## Committed Inputs

- Rust sources, tests, benchmarks, `Cargo.toml`, and `Cargo.lock`.
- Frontend TypeScript sources under `web/ts/`, static HTML/CSS inputs, and
  generated `public/js/`/`public/output.css` artifacts for runtime embedding.
- Node toolchain metadata: `package.json`, `package-lock.json`, and
  `tsconfig.json`.
- Native build scripts and metadata under `scripts/`, including the pinned
  static-build workflow.

## Generated Artifacts

The following paths are generated locally and are not part of the committed
source bundle:

- `.local/build/static/prefix/`: static SRT, Mbed TLS, FFmpeg, x264, and x265 prefix.
- `.local/build/static/env.sh`: generated native build environment.
- `target/` and `.local/artifacts/`: Rust build and harness outputs.
- `node_modules/`: installed frontend dependencies.

## Binary and container distributions

Release artifacts include the checked-in `distribution/` directory. It carries
the Restream license, a native-component license index, and the applicable GPL,
MPL, and Apache license texts. The same directory is copied into scratch images
at `/usr/share/doc/restream/distribution/` and is the canonical input for binary
bundles; do not maintain a second set of release notices in a packaging script.

The GPL-enabled FFmpeg build statically links x264 and x265. Consequently, a
binary or container release must be paired with the GitHub source archive for
its exact Git commit. The immutable native source pins and build flags live in
`scripts/build/native/` and `scripts/build/native-deps.sh`.

Regenerate the native prefix with:

```sh
scripts/build/resource-limit.sh ./scripts/build/native-deps.sh
```

Regenerate frontend runtime assets with:

```sh
npm ci
npm run build:frontend
```

The checked-in frontend bundle must remain reproducible from `web/ts/`,
`package-lock.json`, and the vendored dependency versions. The HLS bundle sync
step deliberately strips the `hls.min.js.map` source-map directive because the
map is not shipped.
