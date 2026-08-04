//! Non-blocking-after-connect TCP dial for the RTMP/RTMPS fabric.
//!
//! Mirrors the SRT fabric's connect shape
//! (`src/media/srt/egress_connect/single.rs`): a bounded blocking connect on
//! the shard's dedicated OS thread — acceptable there because it blocks only
//! that shard's own leaves for at most the connect timeout, not the process
//! — followed by an explicit switch to non-blocking mode for the steady-state
//! read/write path driven by `TcpEgressPoller`.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TcpFabricConnectConfig {
    pub peer_addr: SocketAddr,
    pub connect_timeout: Duration,
}

#[derive(Debug)]
pub(crate) struct TcpFabricConnectError {
    pub operation: &'static str,
    pub source: io::Error,
}

impl std::fmt::Display for TcpFabricConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: {}", self.operation, self.source)
    }
}

impl std::error::Error for TcpFabricConnectError {}

/// Connect to `config.peer_addr`, then switch the socket to non-blocking
/// mode. Returns the connected, non-blocking `TcpStream`; the caller
/// registers `raw_fd(&stream)` with `TcpEgressPoller` for the steady-state
/// send path.
pub(crate) fn connect_fabric_tcp_egress_socket(
    config: TcpFabricConnectConfig,
) -> Result<TcpStream, TcpFabricConnectError> {
    let stream = TcpStream::connect_timeout(&config.peer_addr, config.connect_timeout).map_err(
        |source| TcpFabricConnectError {
            operation: "connect",
            source,
        },
    )?;
    stream
        .set_nodelay(true)
        .map_err(|source| TcpFabricConnectError {
            operation: "set_nodelay",
            source,
        })?;
    stream
        .set_nonblocking(true)
        .map_err(|source| TcpFabricConnectError {
            operation: "set_nonblocking",
            source,
        })?;
    Ok(stream)
}

#[cfg(test)]
#[path = "tcp_connect_tests.rs"]
mod tests;
