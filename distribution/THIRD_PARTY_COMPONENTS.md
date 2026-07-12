# Third-party components

Restream source code is licensed under MIT; see `LICENSE.txt`.
Linux binary bundles and container images also include the components listed
below. This file is a release index: it identifies the component, the pinned
source input used by the build, and the license family that applies to that
component. It is intentionally not a replacement for the license texts in
`licenses/` or for release-specific legal review.

| Component | Exact distributed input | License | Source |
|---|---|---|---|
| FFmpeg | tag `n8.1.2`, commit `38b88335f99e76ed89ff3c93f877fdefce736c13` | GPL-2.0-or-later when configured with `--enable-gpl` | <https://github.com/FFmpeg/FFmpeg> |
| x264 | commit `b35605ace3ddf7c1a5d67a2eb553f034aef41d55` | GPL-2.0-or-later | <https://code.videolan.org/videolan/x264> |
| x265 | commit `e444744c03978c1fb4e037168967020cf2648427` | GPL-2.0-or-later | <https://bitbucket.org/multicoreware/x265_git> |
| SRT | tag `v1.5.5`, commit `b6b4ae990daa8193625a4ddeaeaed03023b23125` | MPL-2.0 | <https://github.com/Haivision/srt> |
| Mbed TLS | release `mbedtls-3.6.6` | Apache-2.0 OR GPL-2.0-or-later; this distribution selects Apache-2.0 | <https://github.com/Mbed-TLS/mbedtls> |
| hls.js | npm package `hls.js@1.6.16` | Apache-2.0 | <https://github.com/video-dev/hls.js> |

The native FFmpeg build used for release binaries enables GPL components and
links x264 and x265. Binary and container releases therefore keep the
matching GitHub source archive available beside the binary assets. The native
source pins, checksums, configuration, and build scripts are checked in under
`scripts/build/native/`.

The corresponding license texts are in `licenses/`. Rust and frontend
dependencies are enumerated in the release SBOM; their copyright notices and
license terms remain those of their respective authors.

Exact native inputs and checksums are enforced by
`scripts/build/native/native-inputs.lock`. The scripts under `scripts/build/`
are the build and installation information used to create the binaries.
