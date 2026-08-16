# VENDOR.md — shiguredo/srt-rs import

This crate (`shiguredo_srt`) is a vendored import of
[`shiguredo/srt-rs`](https://github.com/shiguredo/srt-rs), imported via
`git subtree` so future upstream commits can be pulled in with a normal
merge rather than a manual re-copy. See
[`../../docs/srt-pure-rust-plan.md`](../../docs/srt-pure-rust-plan.md)
(decision D1) for why this crate specifically, and
[`../../docs/agent-guidance/quality/srt-bonding-wire-spec-2026-08-16.md`](../../docs/agent-guidance/quality/srt-bonding-wire-spec-2026-08-16.md)
for the bonding-specific source verification done against it.

## Contents

- [Provenance](#provenance)
- [What was trimmed from the upstream tree](#what-was-trimmed-from-the-upstream-tree)
- [Local patches](#local-patches)
- [Pulling future upstream commits](#pulling-future-upstream-commits)
- [Known open upstream issues, not yet patched locally](#known-open-upstream-issues-not-yet-patched-locally)
- [License and dependency audit](#license-and-dependency-audit)

## Provenance

- Upstream: <https://github.com/shiguredo/srt-rs>
- Branch: `develop` (this is upstream's actively-developed default branch,
  confirmed via `git remote show origin` — not `main`, which lags behind)
- Commit at import: `6779cdddb7cd3233032e06538243715d50df3d0b`
  (2026-08-16 10:53:50 +0900)
- License: Apache-2.0 (upstream `LICENSE`, matches restream's own MIT
  license with no conflict either direction)
- Import method: `git subtree add --prefix=crates/srt-protocol
  shiguredo-srt 6779cddd... --squash`

**A note on the commit choice, since `develop` moved again within the same
session this was vendored in:** between first reading this repo
(`5a8aa3b`, 00:04) and the actual import (`6779cdd`, 10:53), 20 commits
landed on `develop`, and the import-time HEAD commit's own message reads
*"0049-0069 の polish をやり直すため一度元に戻す"* ("reverting once to
redo the 0049-0069 polish"). This looked concerning until checked directly:
`git diff 5a8aa3b 6779cdd --stat -- src/ crates/` is **empty** — the entire
20-commit batch touched only `issues/*.md` tracking files, never `src/`.
The "revert" was to issue-tracker paperwork, not code. `6779cdd` was chosen
deliberately after confirming this, not blindly as "whatever's newest."

## What was trimmed from the upstream tree

Removed from the vendored copy (not needed by restream, and would otherwise
need separate Cargo workspace-member wiring to avoid breaking `cargo build
--workspace`):

- `crates/c-api/` — C FFI bindings for other languages to call this crate.
  Restream consumes it as a normal Rust dependency; irrelevant here.
- `examples/srt_caller/`, `examples/srt_listener/` — upstream's own demo
  binaries. restream's own interop binaries live in the sibling
  `crates/srt-interop` crate instead (Phase 3 onward).

Also removed entirely (`git rm`, not `--cached` — not present on disk in
this checkout, retrievable from the `shiguredo-srt` remote or the subtree-
add commit's history if needed): upstream's own `README.md`, `CHANGES.md`,
`issues/` (open + closed ticket files), `AGENTS.md`, `CLAUDE.md`,
`.markdownlint.jsonc`, `Makefile`, `canary.py`, `prek.toml`,
`refs/srt/draft-sharabayko-srt.md` — not because they're not useful
(several are cited directly in this file and in
[`srt-bonding-wire-spec-2026-08-16.md`](../../docs/agent-guidance/quality/srt-bonding-wire-spec-2026-08-16.md)),
but because `git ls-files '*.md'` picked up 74 of them at import time, and
`scripts/check/docs.mjs` requires every tracked Markdown file in the whole
repo to be linked from `docs/README.md` with restream's own doc conventions
(a `Contents` H2, no SVG badge links — upstream's `README.md` has crates.io/
docs.rs/license SVG badges, which trip the "no SVG" rule meant for
architecture diagrams). Rewriting vendored upstream content to satisfy a
different project's doc-lint conventions isn't worth doing, and would fight
future `subtree pull`s (upstream will keep writing its own README/CHANGES
its own way). Read them directly from the live upstream repo
(<https://github.com/shiguredo/srt-rs/tree/develop>) or from disk in this
checkout — they're real files, just not tracked or doc-indexed by restream.

**Kept, and wired into restream's root workspace** (`Cargo.toml`
`members`): `pbt/` (property-based tests, one per core module — Phase 3/4
should extend these, not duplicate them). `fuzz/` is kept but stays
`exclude`d from the main workspace (matches upstream's own original
`Cargo.toml`; `cargo fuzz` tooling handles it separately, avoiding
nightly-toolchain requirements leaking into the main build).

The vendored crate's own `[workspace]` block (which listed the four paths
above) was removed from `crates/srt-protocol/Cargo.toml` — a crate cannot
both be a member of restream's root workspace and declare its own separate
workspace. `pbt/Cargo.toml`'s `shiguredo_srt = { path = "../" }` dependency
still resolves correctly regardless of which workspace root is in effect.

## Local patches

Applied directly on top of the squashed import commit, each tagged
`// restream local patch (crates/srt-protocol/VENDOR.md, upstream issue
NNNN)` at the call site so a future `git subtree pull` merge — or a
maintainer just reading the diff — can tell local patches apart from
vendored code, and recognize when an upstream fix has made a local patch
redundant:

| Issue | Severity (upstream's own label) | Fix |
|---|---|---|
| [0049](https://github.com/shiguredo/srt-rs/blob/develop/issues/0049-bug-fix-crypto-context-debug-leaks-secret-keys.md) | Critical | `CryptoContext` had `#[derive(Debug)]`, printing raw `kek`/`sek_even`/`sek_odd` key bytes via `{:?}`/`dbg!()`. Replaced with a manual `Debug` impl that redacts those three fields (`src/crypto.rs`). |
| [0050](https://github.com/shiguredo/srt-rs/blob/develop/issues/0050-bug-fix-crypto-context-drop-not-zeroize-secret-keys.md) | Critical | `Vec<u8>`'s default `Drop` frees `kek`/`sek_even`/`sek_odd` without zeroing — key material could linger in freed heap memory. Added a `Drop` impl that zeroes all three (`src/crypto.rs`). |
| [0052](https://github.com/shiguredo/srt-rs/blob/develop/issues/0052-bug-fix-crypto-salt-default-all-zero.md) | High | `handle_handshake_caller` defaulted an unset `crypto_salt` to `[0u8; 16]`, making PBKDF2 derive the same KEK from the same passphrase every time (defeats rainbow-table resistance). Now returns `Error::crypto_error(...)` instead of defaulting (`src/srt_connection.rs`). The listener side was already safe — it derives salt from the peer's KMREQ, never invents its own. |
| *(not upstream-tracked — found here, via live capture against real libsrt, not from upstream's own issue list)* | Critical for StreamID-dependent features | `add_sid_extension`/`add_congestion_extension` wrote the extension bytes correctly but never set the `CONFIG` bit (`0x0004`) in `extension_field`. Real libsrt gates its own extension-scanning loop on that exact bit (confirmed at `srtcore/core.cpp:2925,12433`) and always sets it itself when adding a SID/congestion extension (`core.cpp:1708`). Without the fix: a Rust caller's StreamID was correctly encoded on the wire (verified via `tcpdump` — packet size delta matched the StreamID length exactly) but a real libsrt listener silently never looked for it. This crate's own `test_sid_extension_basic` didn't catch it because it only round-trips through this crate's own `decode()`, which doesn't gate on the flag either — only a real cross-implementation test surfaces this class of bug. Fixed in `src/srt_handshake.rs`; added `test_sid_extension_sets_config_flag`/`test_congestion_extension_sets_config_flag` regression tests. Live-verified fixed against real libsrt in both directions (Rust caller → libsrt listener and libsrt caller → Rust listener), see `crates/srt-interop/`. |

Fixing the crypto issues required updating 4 existing integration tests
(`tests/test_srt_connection.rs`) that relied on the old implicit-zero-salt
default to now explicitly set `crypto_salt`, matching how a real caller
must use the API post-patch. All 127 tests across the crate (unit +
integration + property-based + doctests) pass after these patches —
verified via `cargo test -p shiguredo_srt` and `cargo test -p pbt`.

**Why patch locally instead of waiting for upstream:** this code will
eventually carry real customer stream encryption (Phase 5). All three are
small, mechanical, exactly-as-upstream-specified fixes (each issue file
already states the precise design direction) — the cost of patching now is
low and the cost of shipping with an open, self-identified Critical crypto
bug is not a tradeoff worth making for the sake of staying byte-identical
to upstream.

## Pulling future upstream commits

```sh
git fetch shiguredo-srt develop
git subtree pull --prefix=crates/srt-protocol shiguredo-srt develop --squash
```

This performs a real merge against the squashed import history, so local
patches (above) will show as ordinary merge conflicts if upstream touches
the same lines — most likely because upstream fixed the same issue
themselves, in which case prefer upstream's version and drop the local
patch. Re-run `cargo test -p shiguredo_srt -p pbt` after any pull, and
re-check the trimmed paths above (`crates/c-api`, `examples/`) in case
upstream reintroduces them — re-remove or reconsider case by case.

If the `shiguredo-srt` remote isn't configured in a fresh clone:

```sh
git remote add shiguredo-srt https://github.com/shiguredo/srt-rs.git
```

## Known open upstream issues, not yet patched locally

At import time, `issues/` (open) vs. `issues/closed/` showed 27 closed
issues and roughly 20 open ones on `develop`, numbered up to `0069`. Only
0049/0050/0052 (above) were patched — they were the ones with direct,
concrete security implications for restream's actual use. Others worth a
look before Phase 5 (crypto) or Phase 4 (data plane) land, not yet
triaged in depth here: 0051 (`should_pre_announce` duplicate key-refresh
event), 0056 (`CONCLUSION` KMREQ silent failure), 0059 (receiver buffer has
no explicit limit — a potential resource-exhaustion vector worth checking
against restream's own `SRTO_RCVBUF`-equivalent tuning), 0066 (retransmit
timer not reset after handling). Re-check `issues/closed/` after any
`subtree pull` — some of these may already be resolved by the time Phase 3
onward actually reads this list again.

## License and dependency audit

`cargo-deny` is not installed in the environment this vendoring was done
in, so this was checked manually — **re-run `cargo deny check` in an
environment that has it before this lands in a release build.** New
transitive dependencies pulled in by `shiguredo_srt` (`aws-lc-rs`,
`aws-lc-sys`, `cmake`, `dunce`, `fs_extra`, `jobserver`, `untrusted`), all
from crates.io (satisfies `deny.toml`'s `allow-registry` restriction — the
whole reason this is vendored in-tree rather than as a `git =` dependency,
see the plan's Workspace section): all license expressions resolve to at
least one term already in `deny.toml`'s `[licenses] allow` list (MIT,
Apache-2.0, BSD-3-Clause, ISC all appear; `aws-lc-sys`'s multi-clause `AND`
expression was checked term-by-term, not just at a glance). No GPL/copyleft
term anywhere in the new dependency tree.

`aws-lc-sys` builds a C library via `cmake` at compile time (confirmed:
`cargo build` pulled and built `cmake`, `cc`, `jobserver` — the standard
Rust `cmake`-crate build chain) — this is a new native-build-tooling
requirement for whichever environment builds restream going forward
(cmake + a C compiler), separate from and in addition to restream's
existing FFmpeg/libsrt static-build toolchain. Confirmed present and
working in the environment this was vendored in; worth an explicit check
in CI/Docker image definitions before this lands there.
