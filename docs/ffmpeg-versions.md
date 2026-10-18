# FFmpeg version configuration

Restream links FFmpeg n8.1.2 statically through the `ffmpeg-next` 9.0
binding. Some features also run an external `ffmpeg` executable chosen at
runtime. This page names the files that own each choice and how to change
the pin.

## Contents

- [Current pins](#current-pins)
- [Build the pinned version](#build-the-pinned-version)
- [Update the version](#update-the-version)
- [Runtime executable](#runtime-executable)
- [Troubleshooting](#troubleshooting)

## Current pins

| Input | Value | Owner |
|---|---|---|
| FFmpeg tag | `n8.1.2` | `RESTREAM_LOCK_FFMPEG_VERSION` in [native-inputs.lock](../scripts/build/native/native-inputs.lock) |
| FFmpeg commit | `38b88335f99e76ed89ff3c93f877fdefce736c13` | `RESTREAM_LOCK_FFMPEG_COMMIT` in the same lock |
| Builder default | `FFMPEG_VERSION=n8.1.2` | [scripts/build/native-deps.sh](../scripts/build/native-deps.sh) |
| Rust binding | `ffmpeg-next = "9.0"` (`codec`, `filter`, `format`, `software-resampling`, `software-scaling`) | [Cargo.toml](../Cargo.toml) |

The lock also pins x264 and x265 by commit. The lock is the source of truth;
this table must match it.

## Build the pinned version

```sh
scripts/build/native-deps.sh
```

The script clones the locked tag, builds static libraries into
`.local/build/static/prefix/`, and writes the environment that
`scripts/build/app-static.sh` and other build entry points source. It
refuses an `FFMPEG_VERSION` that differs from the lock. Do not rebuild the
environment by hand with `pkg-config` or copy commands.

## Update the version

Make one reviewed change:

1. Update `RESTREAM_LOCK_FFMPEG_VERSION` and `RESTREAM_LOCK_FFMPEG_COMMIT`
   in `scripts/build/native/native-inputs.lock`.
2. Update the `FFMPEG_VERSION` default in `scripts/build/native-deps.sh`.
3. Change `ffmpeg-next` in `Cargo.toml` only when the new FFmpeg crosses a
   binding family or needs an API change.
4. Rebuild with `scripts/build/native-deps.sh` and run the media and release
   gates for the changed codecs, filters, protocols and artifacts.
5. Update [Current pins](#current-pins).

## Runtime executable

External transforms, subprocess file ingest and recording remux run an
`ffmpeg` executable that is not part of the build. `src/ffmpeg_binary.rs`
resolves it in this order: `FFMPEG_BIN_PATH` (config, then environment),
the copy installed by `restream ffmpeg-fetch` (a BtbN FFmpeg 8.1 static GPL
build, checked by SHA-256), then `ffmpeg` on `PATH`. See
[Configuration](configuration.md) for the user contract.

The linked libraries (media-file analysis and the version report) are fixed
at build time. `FFMPEG_BIN_PATH` does not change them.

## Troubleshooting

- Native setup rejects a version: compare it with
  `scripts/build/native/native-inputs.lock`. Unreviewed overrides are
  refused on purpose.
- No `ffmpeg` executable at startup: install the distribution's `ffmpeg`
  package or run `restream ffmpeg-fetch`.
- Rust build fails after an update: check the `ffmpeg-next` version in
  `Cargo.toml` and the compiler output before changing features.
- Runtime transcoding fails: check which executable was resolved, then use
  [Observability](observability.md) for stage and subprocess diagnostics.
