# FFmpeg version configuration

Restream does not link FFmpeg. All codec work runs in external `ffmpeg` and
`ffprobe` processes chosen at runtime. This page names the files that own
each choice and how to change the version.

## Contents

- [Current version](#current-version)
- [Runtime executable](#runtime-executable)
- [Update the version](#update-the-version)
- [Troubleshooting](#troubleshooting)

## Current version

| Input | Value | Owner |
|---|---|---|
| `restream ffmpeg-fetch` | BtbN release `autobuild-2026-09-30-13-08`: `ffmpeg-n8.1.3-9-g29e619e767-linux64-gpl-8.1` (or `linuxarm64`), SHA-256 pinned per architecture | [crates/restream-engine/src/ffmpeg_binary.rs](../crates/restream-engine/src/ffmpeg_binary.rs) (`PINNED_RELEASE`, `pinned_asset`) |
| Container images | The same release and digests, in the `ffmpeg-fetch` stage | [Dockerfile](../Dockerfile) |

Pin a month's last BtbN build: BtbN keeps those for two years and daily
builds for 14 days.

The engine binary has no FFmpeg build-time input. Rebuilding Restream does
not change which FFmpeg runs.

## Runtime executable

External transforms, subprocess file ingest, recording remux, posters, media
analysis and the picture and sound checks run `ffmpeg` or `ffprobe`.
`crates/restream-engine/src/ffmpeg_binary.rs` resolves `ffmpeg` in this order: `FFMPEG_BIN_PATH`
(config, then environment), the copy installed by `restream ffmpeg-fetch`
under `.restream/runtime/ffmpeg/`, then `ffmpeg` on `PATH`. `ffprobe`
resolves as `FFPROBE_BIN_PATH`, then the `ffprobe` beside the resolved
`ffmpeg`, then `ffprobe` on `PATH`. See
[Configuration](configuration.md#ffmpeg) for the user contract and the
features each FFmpeg component backs.

## Update the version

Make one reviewed change:

1. Set `PINNED_RELEASE` and the per-architecture names and SHA-256 in
   `crates/restream-engine/src/ffmpeg_binary.rs`, and the same values in the `Dockerfile`'s
   `ffmpeg-fetch` stage (`tests/docker_build_recipe.rs` fails if they
   differ).
2. Replace the saved listings in `test/fixtures/ffmpeg/` with the new
   build's `-version`, `-encoders`, `-decoders`, `-muxers`, `-demuxers`,
   `-filters`, `-protocols` and `-bsfs` output; the capability tests check
   that the pinned build provides every feature.
3. Run the media and release gates, then update
   [Current version](#current-version) and the FFmpeg row in
   [THIRD_PARTY_COMPONENTS.md](../distribution/THIRD_PARTY_COMPONENTS.md).

## Troubleshooting

- No `ffmpeg` executable at startup: install the distribution's `ffmpeg`
  package or run `restream ffmpeg-fetch`. The server still starts; only
  FFmpeg-backed features fail.
- A feature fails: `externalTools` on `GET /api/v1/engine` (and the startup
  log) names the resolved executables and what each feature lacks; then use
  [Observability](observability.md) for stage and subprocess diagnostics.
