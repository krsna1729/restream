# Restream patch to moq-net 0.3.15

Vendored from crates.io `moq-net` 0.3.15 (MIT OR Apache-2.0,
<https://github.com/moq-dev/moq>, `rs/moq-net` at
`e2c5b50572cc305afcfffb4f40184200e9965204`) and wired in through
`[patch.crates-io]` in the root `Cargo.toml`. Not upstreamed.

## Why

moq-net's IETF MOQT entry points (`Server::accept_request` for a
`moqt-*` ALPN, `Client::connect`) require a
`transport::poll::Boxable` transport: `Session + Send + Sync` on native.
The bound exists only to fill the boxed futures and task sets the IETF
driver is still built from (`util::MaybeSendBox`, `MaybeSendTask`,
`MaybeBoxedExt`), which already have `!Send` forms for wasm. Restream's
Compio QUIC/WebTransport transport (`src/media/moq/transport/`) is `!Send`
by design: every session lives on the Compio thread that owns its
connection. The patch lets that transport reach the IETF driver without
making it `Send`.

## What changed

One new Cargo feature, `unsend` (off by default; Restream enables it):

- `src/util.rs`: every `cfg(target_family = "wasm")` /
  `cfg(not(target_family = "wasm"))` pair selecting the boxed future, the
  boxed task and `MaybeBoxedExt` becomes
  `cfg(any(target_family = "wasm", feature = "unsend"))` /
  `cfg(not(any(...)))`, so `unsend` picks the `!Send` forms on native.
- `src/transport.rs` (`transport::poll`): `MaybeSend` / `MaybeSync` are
  `web_transport_trait`'s (`Send` / `Sync` on native) without the feature;
  with it they are blanket traits every type implements. `Boxable` and
  every bound written with them are therefore satisfied by a `!Send`
  transport.
- `src/server.rs`: imports `MaybeSend` / `MaybeSync` from
  `transport::poll` instead of `web_transport_trait`, so the
  `S::SendStream: MaybeSync` bounds on `accept` / `accept_request` relax
  with the feature.

With `unsend` the IETF `Driver<S>` is `!Send`, as the lite driver over a
`!Send` transport already is; it is driven on the thread that owns the
connection. `Session` handles stay `Send + Sync` (moq-net's own
`assert_send_sync::<Session>()` still compiles). No `tokio::spawn` or
thread is introduced: moq-net 0.3.15 has none outside tests.

Packaging only: `tests/`, `benches/`, `examples/`, `CHANGELOG.md`,
`AGENTS.md`, `Cargo.toml.orig` and `Cargo.lock` are omitted, and the
`[[test]]`, `[[bench]]`, `[[example]]` and `[dev-dependencies]` sections of
the normalized `Cargo.toml` are removed with them. `LICENSE-MIT` and
`LICENSE-APACHE` are the upstream repository's (the crate package ships
none).

## Dropping the patch

Upstream's `transport.rs` says the bound stays "until it too becomes named
machines". Once a moq-net release drives the IETF path without `Send`
boxes, remove `vendor/moq-net`, the `[patch.crates-io]` entry and the
`unsend` feature from the root `Cargo.toml`.

- `Budget::reserve` allows the deprecated `fetch_update` name: newer toolchains rename it to `try_update`, and the nightly fuzz/Kani jobs build with `-D warnings`, while the pinned toolchain has only the old name.
