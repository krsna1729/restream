//! MoQ (Media over QUIC): egress to a relay, ingest from a publisher, and
//! an origin for direct subscribers.
//!
//! Phase 1 speaks moq-lite + hang through the pinned `moq-net` lite driver
//! over [`transport`] (Compio QUIC and WebTransport), with [`media`] mapping
//! frames and the hang catalog. The phase-2 IETF/MediaMTX profile is not in
//! the tree yet.
//!
//! [`origin`] serves the pipelines' broadcasts to direct subscribers on the
//! ingest listener's thread. [`tls`] builds the rustls configurations.

#[doc(hidden)]
pub mod bench_entry;
pub(crate) use restream_moq::budget;
pub(crate) mod egress;
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz_entry;
pub(crate) mod ingest;
pub(crate) use restream_moq::media;
pub(crate) mod origin;
#[doc(hidden)]
pub mod subscribe_entry;
pub(crate) use restream_moq::tls;
pub(crate) mod transport;

pub(crate) use restream_moq::WireError;
#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tls_tests;
