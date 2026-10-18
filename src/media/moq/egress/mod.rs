//! MoQ egress (Q-047): `moqt://` outputs published to a relay over
//! moq-lite + hang, and the ring → broadcast path the origin reuses.
//!
//! - [`url`]: the output URL (relay address, session path, broadcast name,
//!   TLS trust keys).
//! - [`media`]: ring packets → hang frames, with the catalog configurations.
//! - [`publish`]: a hang broadcast fed from a ring (groups, catalog
//!   republish, retention).
//! - [`startup`]: the per-broadcast snapshot (tracks, decoder
//!   configuration, relay trust) built on Tokio before `Add`.
//! - [`leaf`] / [`backend`]: the connection task and the fabric shard
//!   backend that drives it.

mod backend;
#[cfg(test)]
pub(crate) mod fake_session;
pub(crate) mod leaf;
pub(crate) mod media;
pub(crate) mod publish;
pub(crate) mod startup;
pub(crate) mod url;

pub(crate) use backend::moq_shard_backend;
pub(crate) use startup::{MoqPublishStartup, SharedMoqStartupSource, publish_startup};
pub(crate) use url::parse_moq_url;
