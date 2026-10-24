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
| `restream ffmpeg-fetch` | BtbN `ffmpeg-n8.1-latest-linux64-gpl-8.1` (or `linuxarm64`) from the `latest` release, checked against `checksums.sha256` | [src/ffmpeg_binary.rs](../src/ffmpeg_binary.rs) |
| Container images | The same BtbN build, fetched in the `ffmpeg-fetch` stage | [Dockerfile](../Dockerfile) |

The engine binary has no FFmpeg build-time input. Rebuilding Restream does
not change which FFmpeg runs.

## Runtime executable

External transforms, subprocess file ingest, recording remux, posters, media
analysis and the picture and sound checks run `ffmpeg` or `ffprobe`.
`src/ffmpeg_binary.rs` resolves `ffmpeg` in this order: `FFMPEG_BIN_PATH`
(config, then environment), the copy installed by `restream ffmpeg-fetch`
under `.restream/runtime/ffmpeg/`, then `ffmpeg` on `PATH`. `ffprobe`
resolves as `FFPROBE_BIN_PATH`, then the `ffprobe` beside the resolved
`ffmpeg`, then `ffprobe` on `PATH`. See
[Configuration](configuration.md#ffmpeg) for the user contract and the
features each FFmpeg component backs.

## Update the version

Make one reviewed change:

1. Update the archive names in `src/ffmpeg_binary.rs` and the
   `ffmpeg-fetch` stage of the `Dockerfile` together.
2. Run the media and release gates for the changed codecs, filters,
   protocols and artifacts.
3. Update [Current version](#current-version) and the FFmpeg row in
   [THIRD_PARTY_COMPONENTS.md](../distribution/THIRD_PARTY_COMPONENTS.md).

## Troubleshooting

- No `ffmpeg` executable at startup: install the distribution's `ffmpeg`
  package or run `restream ffmpeg-fetch`. The server still starts; only
  FFmpeg-backed features fail.
- Runtime transcoding fails: check which executable was resolved and that
  it has the encoders, muxers and protocols listed in
  [Configuration](configuration.md#ffmpeg), then use
  [Observability](observability.md) for stage and subprocess diagnostics.
