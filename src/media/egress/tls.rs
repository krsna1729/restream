//! TLS for egress transports over the shard's Compio TCP streams: a rustls
//! handshake, then a hand-off to kernel TLS so payload is written by
//! reference with no userspace copy. Protocol-neutral: RTMPS uses it, and
//! any later TLS egress (HLS PUT over HTTPS) uses the same connection.
pub(crate) mod client_config;
mod connection;
pub(crate) mod ktls;
mod telemetry;

#[cfg(test)]
pub(crate) use client_config::rustls_client_config;
pub(crate) use client_config::{resolve_client_config, supports_cipher_suite};
pub(crate) use connection::TlsTcpConnection;
pub(crate) use telemetry::TlsCounters;
