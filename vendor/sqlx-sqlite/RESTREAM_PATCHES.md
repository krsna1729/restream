# Restream patches to sqlx-sqlite 0.9.0

Vendored from crates.io `sqlx-sqlite` 0.9.0 (MIT OR Apache-2.0, `LICENSE-MIT` and
`LICENSE-APACHE` from <https://github.com/launchbadge/sqlx>) and wired in through
`[patch.crates-io]` in the root and `fuzz/` `Cargo.toml`.

Patch scope:

- Relax `libsqlite3-sys` dependency version from `>=0.30.1, <0.38.0` to
  `>=0.30.1, <0.40.0` in `Cargo.toml`. This allows upgrading `libsqlite3-sys`
  to `0.39.1`, which bundles SQLite 3.54.1 and resolves CVE-2026-119375
  (RUSTSEC-2026-0459: uninitialized memory exposure on differing page sizes).
  Upstream sqlx-sqlite 0.9.0 was published prior to `libsqlite3-sys` 0.38/0.39
  and had an overly restrictive `<0.38.0` upper bound.

Keep this patch minimal; drop it once an upstream sqlx-sqlite release carries the updated libsqlite3-sys dependency.
