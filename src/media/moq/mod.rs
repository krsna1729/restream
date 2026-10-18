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

use std::fmt;

pub(crate) mod egress;
pub(crate) mod media;
pub(crate) mod tls;
pub(crate) mod transport;

/// Malformed MoQ wire data. Every parser in this module returns it instead
/// of panicking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WireError {
    /// The bytes do not follow the profile.
    Invalid(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid MoQ data: {reason}"),
        }
    }
}

impl std::error::Error for WireError {}
