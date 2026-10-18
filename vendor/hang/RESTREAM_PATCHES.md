# Restream patches to hang 0.21.16

Vendored from crates.io `hang` 0.21.16 (MIT OR Apache-2.0, `LICENSE-MIT` and
`LICENSE-APACHE` from <https://github.com/moq-dev/moq>) and wired in through
`[patch.crates-io]` in the root and `fuzz/` `Cargo.toml`. Upstream `main`
still has the bug.

Patch scope, marked `restream vendor patch` in the source:

- `H264::from_str` (`src/catalog/video/h264.rs`) rejects a profile field that
  is not ASCII. It checked `part.len() != 6` (bytes) and then sliced
  `&part[0..2]`, `[2..4]`, `[4..6]`, which panics when a multi-byte character
  straddles a slice end: `"codec":"avc1.4Ψ01e"` in a publisher's
  `catalog.json` panicked the MoQ ingest session that parsed it ("end byte
  index 2 is not a char boundary"). Found by the `moq_ingest` fuzz target.

  ```diff
  -		if part.len() != 6 {
  +		if part.len() != 6 || !part.is_ascii() {
  ```

- Tests: `h264::tests::non_ascii_is_invalid_not_a_panic`, and a sweep over
  every codec string parser (`video::codec` and `audio::codec`
  `non_ascii_codec_strings_never_panic`) with a two-byte character in place
  of zero, one or two characters at each position. The other parsers were
  audited and need no change: H.265 slices `[1..]` only after an ASCII first
  byte, AV1 indexes regex captures, VP9 and AAC parse whole fields.
- Packaging: `CHANGELOG.md`, `Cargo.lock`, `Cargo.toml.orig` and `examples/`
  are not vendored, and the manifest's `[[example]]` entries are removed (the
  examples need dev-dependencies the published manifest does not carry).

Keep this patch minimal; drop it once an upstream release carries the fix.
