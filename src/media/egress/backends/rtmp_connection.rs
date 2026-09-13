//! Plain-or-TLS transport for the RTMP fabric engine.
//!
//! Wraps a non-blocking `std::net::TcpStream` directly with
//! `rustls::StreamOwned` for RTMPS (no async runtime involved — the same
//! `rustls::ClientConnection` state machine the legacy Tokio adapter drives
//! via `tokio_rustls` in `src/media/rtmp/egress_transport.rs`, here driven
//! synchronously against a non-blocking socket instead).
//!
//! `rustls::Stream`/`StreamOwned`'s `Read`/`Write` impls already interleave
//! TLS handshake I/O with application data transparently, surfacing
//! `WouldBlock` exactly like a raw non-blocking socket — so the RTMP-level
//! drivers (`NonBlockingRtmpHandshake`, `SessionNegotiation`,
//! `MediaPublisher`) do not need TLS-specific logic. The one thing they
//! *do* need is [`RtmpConnection::interest_hint`]: a blocked read or write
//! call on a TLS connection does not necessarily mean the *same* direction
//! is what unblocks it (e.g. `write()` may internally need to `read_tls()`
//! a ServerHello before it can finish flushing a handshake flight), so
//! guessing "needs read" from a blocked `read()` call (correct for plain
//! TCP) can under-request interest for TLS and stall the connection
//! forever. Asking `rustls::ClientConnection::wants_read()`/`wants_write()`
//! directly after any blocked operation is the source of truth.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;

use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::media::egress::backend::Interest;
#[cfg(test)]
use crate::media::rtmp::rustls_client_config;

#[path = "rtmp_ktls.rs"]
mod rtmp_ktls;

pub(crate) enum RtmpConnection {
    Plain(TcpStream),
    Ktls(TcpStream),
    Tls(Option<Box<StreamOwned<ClientConnection, TcpStream>>>),
}

impl RtmpConnection {
    pub(crate) fn plain(stream: TcpStream) -> Self {
        Self::Plain(stream)
    }

    // Production always calls `tls_with_config` directly with an explicit
    // client config (see rtmp_shard.rs); this default-config convenience
    // wrapper is only exercised by tests.
    #[cfg(test)]
    pub(crate) fn tls(stream: TcpStream, host: &str) -> Result<Self, String> {
        Self::tls_with_config(stream, host, rustls_client_config())
    }

    /// Same as [`Self::tls`] but with an explicit `ClientConfig` — the
    /// production path always uses `rustls_client_config()`'s
    /// webpki-roots-trusting config; tests use this to substitute a
    /// verifier that trusts a locally generated test certificate instead.
    pub(crate) fn tls_with_config(
        stream: TcpStream,
        host: &str,
        config: Arc<ClientConfig>,
    ) -> Result<Self, String> {
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|_| format!("invalid RTMPS host name: {host}"))?;
        let mut config = (*config).clone();
        config.enable_secret_extraction = true;
        let connection = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|error| format!("rustls client connection init failed: {error}"))?;
        Ok(Self::Tls(Some(Box::new(StreamOwned::new(
            connection, stream,
        )))))
    }

    fn tcp_stream(&self) -> &TcpStream {
        match self {
            Self::Plain(stream) | Self::Ktls(stream) => stream,
            Self::Tls(Some(stream)) => &stream.sock,
            Self::Tls(None) => unreachable!("TLS stream is only temporarily taken during handoff"),
        }
    }

    pub(crate) fn raw_fd(&self) -> RawFd {
        self.tcp_stream().as_raw_fd()
    }

    /// Conservative estimate of rustls-internal buffered bytes not visible
    /// to `MediaPublisher::pending_bytes()`. rustls exposes no occupancy
    /// getter for its internal plaintext/TLS-record buffers —
    /// `ConnectionCommon::set_buffer_limit` is the only related API, a cap
    /// *setter* with no matching getter (checked against rustls 0.23.41's
    /// actual public API; see `docs/archive/egress/implementation.md` Phase 5
    /// status). Returns rustls's own default 64KB `sendable_plaintext`/
    /// `sendable_tls` cap whenever the connection still wants to write
    /// (`wants_write()` — i.e. it is holding data this leaf hasn't
    /// finished flushing), `0` otherwise. This is a worst-case upper bound
    /// on the hidden buffer, not an exact occupancy count — the point is
    /// keeping `LeafLimits::max_pending_bytes` enforcement from
    /// under-counting a backpressured RTMPS leaf by an unbounded amount,
    /// not precise accounting.
    pub(crate) fn rustls_pending_bytes_estimate(&self) -> usize {
        const RUSTLS_DEFAULT_BUFFER_LIMIT: usize = 64 * 1024;
        match self {
            Self::Plain(_) | Self::Ktls(_) => 0,
            Self::Tls(Some(stream)) => {
                if stream.conn.wants_write() {
                    RUSTLS_DEFAULT_BUFFER_LIMIT
                } else {
                    0
                }
            }
            Self::Tls(None) => 0,
        }
    }

    pub(crate) fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.tcp_stream().shutdown(how)
    }

    /// What interest to register after a blocked read or write. `fallback`
    /// is the naive per-direction guess (correct for plain TCP, where read
    /// and write are independent); TLS connections instead ask the
    /// underlying `rustls::ClientConnection` what it actually needs, since
    /// one direction blocking does not imply that same direction is what
    /// unblocks it (see module docs).
    pub(crate) fn interest_hint(&self, fallback: Interest) -> Interest {
        match self {
            Self::Plain(_) | Self::Ktls(_) => fallback,
            Self::Tls(Some(stream)) => {
                let hint = Interest {
                    readable: stream.conn.wants_read(),
                    writable: stream.conn.wants_write(),
                };
                if hint.is_empty() { fallback } else { hint }
            }
            Self::Tls(None) => fallback,
        }
    }

    fn maybe_handoff_ktls(&mut self) -> io::Result<()> {
        let (version, suite) = {
            let Self::Tls(Some(stream)) = self else {
                return Ok(());
            };
            if stream.conn.is_handshaking()
                || stream.conn.wants_write()
                || stream
                    .conn
                    .reader()
                    .into_first_chunk()
                    .is_ok_and(|chunk| !chunk.is_empty())
            {
                return Ok(());
            }
            let Some(version) = stream.conn.protocol_version() else {
                return Ok(());
            };
            let Some(suite) = stream.conn.negotiated_cipher_suite() else {
                return Ok(());
            };
            (version, suite)
        };
        if version != tokio_rustls::rustls::ProtocolVersion::TLSv1_2
            || !matches!(
                suite.suite(),
                tokio_rustls::rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256
                    | tokio_rustls::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256
                    | tokio_rustls::rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
                    | tokio_rustls::rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384
            )
            || !rtmp_ktls::available()
        {
            return Ok(());
        }

        let stream = match self {
            Self::Tls(stream) => stream.take().expect("TLS stream was checked above"),
            Self::Plain(_) | Self::Ktls(_) => return Ok(()),
        };
        let (connection, socket) = stream.into_parts();
        #[allow(deprecated)]
        let secrets = connection
            .dangerous_extract_secrets()
            .map_err(|error| io::Error::other(format!("rustls kTLS handoff: {error}")))?;
        rtmp_ktls::install(socket.as_raw_fd(), version, suite.suite(), &secrets)?;
        *self = Self::Ktls(socket);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_ktls(&self) -> bool {
        matches!(self, Self::Ktls(_))
    }
}

impl Read for RtmpConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) | Self::Ktls(stream) => stream.read(buf),
            Self::Tls(Some(stream)) => {
                let result = stream.read(buf);
                if result.is_ok() {
                    self.maybe_handoff_ktls()?;
                }
                result
            }
            Self::Tls(None) => Err(io::Error::other("TLS stream unavailable during handoff")),
        }
    }
}

impl Write for RtmpConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(stream) | Self::Ktls(stream) => stream.write(buf),
            Self::Tls(Some(stream)) => {
                let result = stream.write(buf);
                if result.is_ok() {
                    self.maybe_handoff_ktls()?;
                }
                result
            }
            Self::Tls(None) => Err(io::Error::other("TLS stream unavailable during handoff")),
        }
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        let result = match self {
            Self::Plain(stream) | Self::Ktls(stream) => stream.write_vectored(bufs),
            Self::Tls(Some(stream)) => stream.write_vectored(bufs),
            Self::Tls(None) => Err(io::Error::other("TLS stream unavailable during handoff")),
        };
        if result.is_ok() {
            self.maybe_handoff_ktls()?;
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(stream) | Self::Ktls(stream) => stream.flush(),
            Self::Tls(Some(stream)) => {
                let result = stream.flush();
                if result.is_ok() {
                    self.maybe_handoff_ktls()?;
                }
                result
            }
            Self::Tls(None) => Err(io::Error::other("TLS stream unavailable during handoff")),
        }
    }
}

#[cfg(test)]
#[path = "rtmp_connection_tests.rs"]
mod tests;
