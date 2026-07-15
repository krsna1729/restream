# Source distribution manifest

This repository is the source distribution. It commits reproducible inputs and
selected runtime frontend outputs, but not large native or Rust build trees.

## Contents

- [Committed inputs](#committed-inputs)
- [Generated local state](#generated-local-state)
- [Preparation owners](#preparation-owners)
- [Binary and container distributions](#binary-and-container-distributions)

## Committed inputs

- Rust sources, tests, benchmarks, `Cargo.toml`, and `Cargo.lock`.
- Authored frontend sources under `web/`, `package-lock.json`, `tsconfig.json`,
  and the generated browser assets required for runtime embedding. Vendored
  HLS assets include `hls.min.js` and its distributable `hls.min.js.map`.
- Native build scripts, immutable source pins, patches, and configuration under
  `scripts/build/` and `scripts/native/`.
- License and distribution inputs under `distribution/`.

The presence of committed generated browser assets is a runtime-embedding
contract. Their generation recipe remains owned by the frontend scripts and
`package.json`, not by prose in this manifest.

## Generated local state

These paths are local outputs and are not part of the committed source bundle:

- `.local/build/static/prefix/`: installed native libraries and tools;
- `.local/build/static/`: the surrounding generated build environment;
- `target/`: Rust binaries, incremental state, reports, and Criterion data;
- `.local/artifacts/`: harness and measurement evidence;
- `node_modules/`: installed frontend dependencies.

Runtime state under `.restream/` and local recordings are also outside the
source distribution.

## Preparation owners

`scripts/dev/prepare.sh` owns clean-checkout preparation for development.
`scripts/release/prepare-build-tree.sh` owns the stricter release-tree
preparation used by packaging and CI.

Do not duplicate their native-build, dependency-install, or frontend-generation
steps here. A change to generated inputs or required outputs belongs in the
owning script and its verification, with this manifest updated only when the
distribution boundary itself changes.

The stable preparation entry points used by that verification are
`scripts/build/resource-limit.sh ./scripts/build/native-deps.sh` for the native
prefix and `npm run build:frontend` for embedded browser output. The scripts
own their internal steps and flags.

## Binary and container distributions

Release artifacts include the checked-in `distribution/` material. It carries
the Restream license, native-component index, and applicable license texts. The
same source is copied into container images and host bundles; packaging scripts
must not maintain a second notice set.

The GPL-enabled native build links x264 and x265 and may make FFmpeg GPL.
Binary and container releases therefore need source availability for the exact
commit and all applicable notices. Immutable native pins live in
`scripts/build/native/native-inputs.lock`; bundle contents and evidence are
owned by the release scripts described in [Release compliance](release-compliance.md).
