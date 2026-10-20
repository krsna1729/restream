# Restream patches to moq-msf 0.5.1

Official crates.io `moq-msf` 0.5.1 source, from upstream commit
[`7de2a7b2b39884f3a5473ee1aca91bcbc23105e3`](https://github.com/moq-dev/moq/tree/7de2a7b2b39884f3a5473ee1aca91bcbc23105e3/rs/moq-msf).
`LICENSE-MIT` and `LICENSE-APACHE` are copied from that upstream commit.
The root and `fuzz/` manifests select this package with `[patch.crates-io]`.

The local patch bounds catalog deserialization before draft-01 `initRef`
expansion. Upstream first deserializes unbounded track/init-entry vectors,
then clones each referenced init string; a small shared payload can therefore
expand into an arbitrarily large catalog before a downstream track guard.
The upstream catalog types, version handling, serialization, reference
resolution, and conversions remain in use.

Guards, marked `Restream vendor patch` in `src/lib.rs`:

- Export `MAX_TRACKS = 64` and `MAX_CATALOG_BYTES = 32 * 1024 * 1024`.
- `Catalog::from_str` rejects raw serialized input above `MAX_CATALOG_BYTES`
  before JSON parsing, returning a `serde_json::Error` of category `Data`.
- Both `tracks` and optional `initDataList` use incremental sequence visitors.
  They ignore untrusted capacity hints, grow only through 64 entries, and use
  a rejecting deserialize seed to reject entry 65 before parsing its fields.
- `Catalog::deserialize` preflights aggregate resolved init string bytes
  before **any** reference payload clone or upstream reference validation.
  Every track's inline or referenced payload counts once, including repeated
  shared references. Checked subtraction rejects totals above the byte cap.
  Matching inline/reference data still counts once; ordinary missing,
  unsupported, and conflicting references retain upstream errors within bounds.
- Structural and aggregate limits apply to all serde entry points, including
  `serde_json::from_value`. The raw serialized-input limit is specific to
  `Catalog::from_str`, where the original byte length is available.

Focused inline regression tests:

- `tracks_limit_rejects_before_deserializing_65th_entry`
- `init_data_limit_rejects_before_deserializing_65th_entry`
- `track_and_init_entry_exact_limits_are_accepted`
- `aggregate_reference_limit_precedes_resolution_errors`
- `aggregate_limit_counts_inline_and_referenced_data`
- `shared_references_accept_exact_resolved_byte_limit`
- `serialized_catalog_byte_limit_is_exact_and_precedes_parsing`

The invalid 65th elements distinguish early rejection from an after-parse
length check. A missing first reference in the over-budget fixture distinguishes
complete preflight from per-reference or post-expansion validation. Exact-limit
tests cover accepted 64-entry collections, 32 MiB resolved data, matching inline
plus shared references, and 32 MiB raw serialized input.

Packaging is limited to the build manifest, source (including upstream and
local inline unit tests), these patch notes, and the upstream licenses.
Registry metadata, lockfile, changelog, README, and non-library targets are not
vendored. Keep the patch local and minimal; replace it when an official release
provides equivalent pre-expansion bounds.
