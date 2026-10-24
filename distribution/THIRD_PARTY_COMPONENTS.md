# Third-party components

Restream source code is licensed under MIT; see `LICENSE.txt`.
Linux binary bundles and container images also include the components listed
below. This file is a release index: it identifies the component, the pinned
source input used by the build, and the license family that applies to that
component. It is intentionally not a replacement for the license texts in
`licenses/` or for release-specific legal review.

| Component | Exact distributed input | License | Source |
|---|---|---|---|
| srt-rs | git commit `9b20d515805aa2d920ffa2d84fd8061310779910` ([krsna1729/srt-rs](https://github.com/krsna1729/srt-rs), pinned in `Cargo.toml`) | Apache-2.0 | <https://github.com/krsna1729/srt-rs> |
| hls.js | npm package `hls.js@1.7.3` | Apache-2.0 | <https://github.com/video-dev/hls.js> |
| FFmpeg executable (container images only) | BtbN `ffmpeg-n8.1-latest-linux64-gpl-8.1` (or `linuxarm64`) from the `latest` release, verified against that release's `checksums.sha256` at image build | GPL; the archive's `LICENSE.txt` names the exact version | <https://github.com/BtbN/FFmpeg-Builds> |

The Restream binary links no FFmpeg, x264 or x265 code. Its native
components are srt-rs, SQLite, libgcc, the Rust standard library and glibc;
the runtime SBOM lists them. Host binary bundles do not include FFmpeg.
Container images add the GPL BtbN FFmpeg build listed above as separate
`ffmpeg` and `ffprobe` executables, and `restream ffmpeg-fetch` installs the
same build on request. Binary and container releases keep the matching
GitHub source archive available beside the binary assets.

The corresponding license texts are in `licenses/`. Rust and frontend
dependencies are enumerated in the release SBOM; their copyright notices and
license terms remain those of their respective authors.

The scripts under `scripts/build/` and the `Dockerfile` are the build and
installation information used to create the binaries and images.
