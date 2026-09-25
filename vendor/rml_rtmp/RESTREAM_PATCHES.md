# Restream patches to rml_rtmp 0.8.0

Vendored from crates.io `rml_rtmp` 0.8.0 (MIT, <https://github.com/KallDrexx/rust-media-libs>)
and wired in through `[patch.crates-io]` in the root `Cargo.toml`.

Patch scope, marked `restream vendor patch` in the source:

- `ChunkDeserializer::set_max_message_length`: a chunk header that declares a
  message longer than the maximum fails with
  `ChunkDeserializationError::MessageTooLarge` when the length is decoded,
  before any payload is buffered. The default is the 24-bit protocol maximum,
  so behavior is unchanged unless configured.
- The deserializer no longer reserves the whole declared message length on a
  message's first chunk; the payload grows with the bytes that arrived. One
  128-byte chunk declaring 16 MiB used to force a 16 MiB allocation.
- `ChunkDeserializer::in_progress_bytes` and
  `ServerSession::inbound_buffered_bytes` expose the bytes a session holds
  before a message completes; `ServerSessionConfig::max_message_length`
  configures the limit.

Tests for each change live next to the patched code. Keep this patch minimal;
drop it if upstream gains equivalent admission control.
