//! MoQ (Media over QUIC): ingest from a publisher, an origin for direct
//! subscribers, and the engine query that starts an egress broadcast.
//!
//! The pinned `moq-net` lite and IETF drivers run over
//! [`crate::moq_transport`] (Compio QUIC and WebTransport). The `moqt://`
//! egress backend lives in [`crate::egress::backends::moq`]. Shared
//! frame/catalog helpers come from `restream-moq`; endpoints, sessions and
//! their lifecycle remain here.
//!
//! [`origin`] serves the pipelines' broadcasts to direct subscribers on the
//! ingest listener's thread. `restream_moq::tls` builds the rustls configurations.

#[doc(hidden)]
pub mod bench_entry;
pub mod egress;
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz_entry;
pub mod ingest;
pub mod origin;
#[doc(hidden)]
pub mod subscribe_entry;

#[cfg(test)]
mod tests;
