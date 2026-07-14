# FFmpeg version configuration

This reference explains which files own native FFmpeg selection and how to
change the pin without creating a second build recipe.

## Contents

- [Sources of truth](#sources-of-truth)
- [Build the pinned version](#build-the-pinned-version)
- [Update the version](#update-the-version)
- [Runtime executable override](#runtime-executable-override)
- [Troubleshooting](#troubleshooting)

## Sources of truth

Two checked-in inputs must remain compatible:

- [scripts/build/native/native-inputs.lock](../scripts/build/native/native-inputs.lock)
  owns the immutable FFmpeg tag and resolved upstream commit used by the native
  build;
- [Cargo.toml](../Cargo.toml) owns the `ffmpeg-next` Rust binding family.

[scripts/build/native-deps.sh](../scripts/build/native-deps.sh) owns download,
configuration, compilation, capability verification, and placement of the
embedded `public/bin/ffmpeg`. It rejects a version override that disagrees
with the committed lock. Do not copy its flags, dependency list, or output-copy
steps into this document.

## Build the pinned version

Run the canonical native builder through the resource limiter:

```sh
scripts/build/resource-limit.sh scripts/build/native-deps.sh
```

The script reuses a valid native prefix and explains how to request a rebuild
when one is needed. Application build entrypoints consume the generated
environment; callers should not reconstruct it with manual `pkg-config`,
compiler, or copy commands.

## Update the version

A native-version change should be one reviewed change set:

1. Update the FFmpeg tag and resolved commit in
   `scripts/build/native/native-inputs.lock`.
2. Update the default consumed by `scripts/build/native-deps.sh` in the same
   change.
3. Change the `ffmpeg-next` dependency only when the selected FFmpeg release
   crosses binding families or requires an API adjustment.
4. Rebuild through the canonical native builder and run the media and release
   gates appropriate to the changed codecs, filters, protocols, and artifact
   surface.

The lock file, not this page, answers “which version is current.” This avoids a
version table becoming stale whenever the pin moves.

## Runtime executable override

External transforms, subprocess file ingest, and recording remux normally use
the embedded executable prepared by the native build. `FFMPEG_BIN_PATH`
overrides that executable for a deployment; its user-facing contract belongs
in [Configuration](configuration.md).

The linked in-process FFmpeg libraries are selected at build time and cannot be
changed with the runtime executable override.

## Troubleshooting

- If native setup rejects a version, compare the requested value with
  `scripts/build/native/native-inputs.lock`; an unreviewed override is
  intentionally unsupported.
- If the embedded executable is missing, rerun the native builder rather than
  copying a host binary into `public/bin/`.
- If Rust compilation fails after a version update, check the
  `ffmpeg-next` family in `Cargo.toml` and the compiler output before
  changing features.
- If runtime transcoding fails, verify the resolved executable and use
  [Observability](observability.md) for stage and subprocess diagnostics.
