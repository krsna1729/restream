//! TLS policy for egress transports: the rustls client configuration, the
//! kernel-TLS record helpers, and handshake telemetry. The connection that
//! drives a rustls handshake and hands off to kernel TLS over the shard's
//! Compio TCP stream lives with that stream
//! (`backends::compio_tcp::TlsTcpConnection`); RTMPS and HLS PUT over HTTPS
//! use it.
pub(crate) mod client_config;
pub mod ktls;
mod telemetry;

pub(crate) use client_config::rustls_client_config;
pub(crate) use client_config::{resolve_client_config, supports_cipher_suite};
pub(crate) use telemetry::TlsEvent;
pub use telemetry::{TlsCounters, TlsTelemetrySnapshot};
