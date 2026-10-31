//! MoQ (Media over QUIC): egress to a relay, ingest from a publisher, and
//! an origin for direct subscribers.
//!
//! The pinned `moq-net` lite and IETF drivers run over [`transport`] (Compio
//! QUIC and WebTransport). Shared frame/catalog helpers come
//! from `restream-moq`; endpoints, sessions and their lifecycle remain here.
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
pub mod transport;

#[cfg(test)]
mod tests;
