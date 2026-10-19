# Source distribution manifest

This repository is the source distribution. It commits reproducible inputs
only. Native builds, Rust builds and the generated frontend (`public/`) are
local outputs.

## Contents

- [Committed inputs](#committed-inputs)
- [Generated local state](#generated-local-state)
- [Preparation owners](#preparation-owners)
- [Binary and container distributions](#binary-and-container-distributions)

## Committed inputs

- Rust sources, tests, benchmarks, `Cargo.toml` and `Cargo.lock`.
- Frontend sources under `web/`, `package.json`, `package-lock.json`,
  `tsconfig.json` and `vite.config.ts` (one Vite build; `tsc` only
  type-checks). See [Frontend](frontend.md#build).
- Native build scripts, source pins, patches and configuration under
  `scripts/build/` and `scripts/native/`.
- License and distribution inputs under `distribution/`.

## Generated local state

These paths are gitignored outputs:

| Path | Contents |
|---|---|
| `public/` | Frontend output from `npm run build:frontend`, embedded into the binary at compile time |
| `.local/build/static/` | Native build environment; `prefix/` holds installed libraries and tools |
| `target/` | Rust binaries, incremental state, reports, Criterion data |
| `.local/artifacts/` | Harness and measurement evidence |
| `node_modules/` | Installed frontend dependencies |
| `.restream/` | Runtime state and local recordings |

## Preparation owners

| Script | Purpose |
|---|---|
| [scripts/dev/prepare.sh](../scripts/dev/prepare.sh) | Clean-checkout preparation for development |
| [scripts/release/prepare-build-tree.sh](../scripts/release/prepare-build-tree.sh) | Release-tree preparation for packaging and CI: native prefix, then `public/`, then output checks |
| [scripts/build/native-deps.sh](../scripts/build/native-deps.sh) | Native prefix |
| `npm run build:frontend` | Embedded browser output |

Do not copy their steps here. Update this page only when the distribution
boundary changes.

```sh
npm ci
npm run build:frontend
```

## Binary and container distributions

Release artifacts carry the checked-in `distribution/` material: the
Restream license, the native-component index and license texts. Container
images and host bundles copy the same source; packaging scripts must not
keep a second notice set.

The GPL-enabled native build links x264 and x265 and may make FFmpeg GPL.
Binary and container releases therefore need source availability for the
exact commit and all notices. Native pins live in
`scripts/build/native/native-inputs.lock`; bundle contents and evidence are
described in [Release compliance](release-compliance.md).
