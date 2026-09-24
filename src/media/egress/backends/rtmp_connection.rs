//! Plain-or-TLS transport for the RTMP fabric engine.
//!
//! Compio owns the nonblocking TCP stream and readiness registration. The
//! synchronous RTMP and rustls state machines perform bounded, nonblocking
//! reads and writes during a shard visit; `WouldBlock` returns control to the
//! shard. TLS interest is derived from rustls' `wants_read()` and
//! `wants_write()` state because a blocked read or write can need the opposite
//! readiness direction to make progress.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::media::egress::backend::Interest;
use crate::media::egress::backends::compio_tcp::CompioTcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ClientConnection, ProtocolVersion, StreamOwned};

#[cfg(test)]
use crate::media::rtmp::rustls_client_config;

#[path = "rtmp_ktls.rs"]
mod rtmp_ktls;

enum RtmpConnectionState {
    Plain(CompioTcpStream),
    Ktls(KtlsConnection),
    Tls(Option<Box<StreamOwned<ClientConnection, CompioTcpStream>>>),
    Failed(CompioTcpStream),
}

struct KtlsConnection {
    stream: CompioTcpStream,
    version: ProtocolVersion,
    handshake_buffer: Vec<u8>,
    pending_alert_level: Option<u8>,
    peer_closed: bool,
}

impl KtlsConnection {
    const MAX_CONTROL_RECORDS_PER_READ: usize = 8;

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let fd = self.stream.as_raw_fd();
        self.read_with(buffer, |buffer| rtmp_ktls::recv_record(fd, buffer))
    }

    fn read_with(
        &mut self,
        buffer: &mut [u8],
        mut recv_record: impl FnMut(&mut [u8]) -> io::Result<(usize, u8)>,
    ) -> io::Result<usize> {
        if buffer.is_empty() || self.peer_closed {
            return Ok(0);
        }
        let mut control_records = 0;
        loop {
            if control_records == Self::MAX_CONTROL_RECORDS_PER_READ {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "kTLS control-record work budget exhausted",
                ));
            }
            let (count, record_type) = recv_record(buffer)?;
            if count == 0 {
                self.peer_closed = true;
                return Ok(0);
            }
            if record_type != rtmp_ktls::RECORD_TYPE_DATA {
                control_records += 1;
            }
            match record_type {
                rtmp_ktls::RECORD_TYPE_DATA => return Ok(count),
                rtmp_ktls::RECORD_TYPE_HANDSHAKE if self.version == ProtocolVersion::TLSv1_3 => {
                    if self.handshake_buffer.len().saturating_add(count) > (1 << 20) + 4 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "oversized TLS 1.3 post-handshake data",
                        ));
                    }
                    self.handshake_buffer.extend_from_slice(&buffer[..count]);
                    self.process_handshake_buffer()?;
                }
                rtmp_ktls::RECORD_TYPE_ALERT => {
                    let (level, description) = match (self.pending_alert_level.take(), count) {
                        (None, 1) => {
                            self.pending_alert_level = Some(buffer[0]);
                            continue;
                        }
                        (Some(level), 1) => (level, buffer[0]),
                        (None, 2) => (buffer[0], buffer[1]),
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid TLS alert record",
                            ));
                        }
                    };
                    if description == 0 {
                        self.peer_closed = true;
                        return Ok(0);
                    }
                    if level == 2 {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            format!("peer sent TLS fatal alert {description}"),
                        ));
                    }
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unsupported post-handshake TLS record type",
                    ));
                }
            }
        }
    }

    fn process_handshake_buffer(&mut self) -> io::Result<()> {
        loop {
            if self.handshake_buffer.len() < 4 {
                return Ok(());
            }
            let message_type = self.handshake_buffer[0];
            let message_len = ((self.handshake_buffer[1] as usize) << 16)
                | ((self.handshake_buffer[2] as usize) << 8)
                | self.handshake_buffer[3] as usize;
            if message_len > (1 << 20) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "oversized TLS 1.3 handshake message",
                ));
            }
            let frame_len = 4 + message_len;
            if self.handshake_buffer.len() < frame_len {
                return Ok(());
            }
            match message_type {
                4 => {
                    // ponytail: buffered Rustls extraction cannot retain session state; ignore
                    // tickets and fail closed on KeyUpdate until the unbuffered API is used.
                    drop(self.handshake_buffer.drain(..frame_len));
                }
                24 if message_len == 1 && self.handshake_buffer[4] <= 1 => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "TLS 1.3 key updates are unsupported after the kTLS handoff",
                    ));
                }
                24 => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "malformed TLS 1.3 key update",
                    ));
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unsupported TLS 1.3 handshake message {message_type}"),
                    ));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RtmpsTelemetrySnapshot {
    pub(crate) connections: u64,
    pub(crate) tls12: u64,
    pub(crate) tls13: u64,
    pub(crate) ktls_requested: u64,
    pub(crate) ktls_attempts: u64,
    pub(crate) ktls_success: u64,
    pub(crate) ktls_unsupported: u64,
    pub(crate) ktls_error: u64,
    pub(crate) userspace_tls_connections: u64,
    pub(crate) ktls_tls12_aes128_gcm: bool,
    pub(crate) ktls_tls12_aes256_gcm: bool,
    pub(crate) ktls_tls13_aes128_gcm: bool,
    pub(crate) ktls_tls13_aes256_gcm: bool,
}

struct RtmpsCounters {
    connections: AtomicU64,
    tls12: AtomicU64,
    tls13: AtomicU64,
    ktls_requested: AtomicU64,
    ktls_attempts: AtomicU64,
    ktls_success: AtomicU64,
    ktls_unsupported: AtomicU64,
    ktls_error: AtomicU64,
    userspace_tls_connections: AtomicU64,
}

static RTMPS_COUNTERS: RtmpsCounters = RtmpsCounters {
    connections: AtomicU64::new(0),
    tls12: AtomicU64::new(0),
    tls13: AtomicU64::new(0),
    ktls_requested: AtomicU64::new(0),
    ktls_attempts: AtomicU64::new(0),
    ktls_success: AtomicU64::new(0),
    ktls_unsupported: AtomicU64::new(0),
    ktls_error: AtomicU64::new(0),
    userspace_tls_connections: AtomicU64::new(0),
};

pub(crate) fn rtmps_telemetry_snapshot() -> RtmpsTelemetrySnapshot {
    use tokio_rustls::rustls::{CipherSuite, ProtocolVersion};

    RtmpsTelemetrySnapshot {
        connections: RTMPS_COUNTERS.connections.load(Ordering::Relaxed),
        tls12: RTMPS_COUNTERS.tls12.load(Ordering::Relaxed),
        tls13: RTMPS_COUNTERS.tls13.load(Ordering::Relaxed),
        ktls_requested: RTMPS_COUNTERS.ktls_requested.load(Ordering::Relaxed),
        ktls_attempts: RTMPS_COUNTERS.ktls_attempts.load(Ordering::Relaxed),
        ktls_success: RTMPS_COUNTERS.ktls_success.load(Ordering::Relaxed),
        ktls_unsupported: RTMPS_COUNTERS.ktls_unsupported.load(Ordering::Relaxed),
        ktls_error: RTMPS_COUNTERS.ktls_error.load(Ordering::Relaxed),
        userspace_tls_connections: RTMPS_COUNTERS
            .userspace_tls_connections
            .load(Ordering::Relaxed),
        ktls_tls12_aes128_gcm: rtmp_ktls::supports(
            ProtocolVersion::TLSv1_2,
            CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
        ),
        ktls_tls12_aes256_gcm: rtmp_ktls::supports(
            ProtocolVersion::TLSv1_2,
            CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
        ),
        ktls_tls13_aes128_gcm: rtmp_ktls::supports(
            ProtocolVersion::TLSv1_3,
            CipherSuite::TLS13_AES_128_GCM_SHA256,
        ),
        ktls_tls13_aes256_gcm: rtmp_ktls::supports(
            ProtocolVersion::TLSv1_3,
            CipherSuite::TLS13_AES_256_GCM_SHA384,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KtlsState {
    NotRequested,
    Requested,
    Enabled,
    Unsupported,
    SetupFailed,
}

pub(crate) struct RtmpConnection {
    state: RtmpConnectionState,
    tls_version_recorded: bool,
    ktls_state: KtlsState,
}

impl RtmpConnection {
    pub(crate) fn plain(stream: impl Into<CompioTcpStream>) -> Self {
        Self {
            state: RtmpConnectionState::Plain(stream.into()),
            tls_version_recorded: false,
            ktls_state: KtlsState::NotRequested,
        }
    }

    // Production always calls `tls_with_config` directly with an explicit
    // client config (see rtmp_shard.rs); this default-config convenience
    // wrapper is only exercised by tests.
    #[cfg(test)]
    pub(crate) fn tls(stream: impl Into<CompioTcpStream>, host: &str) -> Result<Self, String> {
        Self::tls_with_config(stream, host, rustls_client_config())
    }

    /// Same as [`Self::tls`] but with an explicit `ClientConfig` — the
    /// production path always uses `rustls_client_config()`'s
    /// webpki-roots-trusting config; tests use this to substitute a locally
    /// generated test certificate instead.
    pub(crate) fn tls_with_config(
        stream: impl Into<CompioTcpStream>,
        host: &str,
        config: Arc<ClientConfig>,
    ) -> Result<Self, String> {
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|_| format!("invalid RTMPS host name: {host}"))?;
        let mut config = (*config).clone();
        config.enable_secret_extraction = true;
        let connection = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|error| format!("rustls client connection init failed: {error}"))?;
        RTMPS_COUNTERS.connections.fetch_add(1, Ordering::Relaxed);
        RTMPS_COUNTERS
            .ktls_requested
            .fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            state: RtmpConnectionState::Tls(Some(Box::new(StreamOwned::new(
                connection,
                stream.into(),
            )))),
            tls_version_recorded: false,
            ktls_state: KtlsState::Requested,
        })
    }

    fn tcp_stream(&self) -> &CompioTcpStream {
        match &self.state {
            RtmpConnectionState::Plain(stream) | RtmpConnectionState::Failed(stream) => stream,
            RtmpConnectionState::Ktls(connection) => &connection.stream,
            RtmpConnectionState::Tls(Some(stream)) => &stream.sock,
            RtmpConnectionState::Tls(None) => {
                unreachable!("TLS stream is only temporarily taken during handoff")
            }
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
        match &self.state {
            RtmpConnectionState::Plain(_)
            | RtmpConnectionState::Ktls(_)
            | RtmpConnectionState::Failed(_) => 0,
            RtmpConnectionState::Tls(Some(stream)) => {
                if stream.conn.wants_write() {
                    RUSTLS_DEFAULT_BUFFER_LIMIT
                } else {
                    0
                }
            }
            RtmpConnectionState::Tls(None) => 0,
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
        match &self.state {
            RtmpConnectionState::Plain(_)
            | RtmpConnectionState::Ktls(_)
            | RtmpConnectionState::Failed(_) => fallback,
            RtmpConnectionState::Tls(Some(stream)) => {
                let hint = Interest {
                    readable: stream.conn.wants_read(),
                    writable: stream.conn.wants_write(),
                };
                if hint.is_empty() { fallback } else { hint }
            }
            RtmpConnectionState::Tls(None) => fallback,
        }
    }
    fn advance_tls_handshake(&mut self) -> io::Result<()> {
        if let RtmpConnectionState::Tls(Some(stream)) = &mut self.state
            && (stream.conn.is_handshaking() || stream.conn.wants_write())
        {
            stream.conn.complete_io(&mut stream.sock)?;
            if stream.conn.is_handshaking() || stream.conn.wants_write() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        self.maybe_handoff_ktls()?;
        if matches!(self.state, RtmpConnectionState::Tls(_)) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(())
    }

    fn maybe_handoff_ktls(&mut self) -> io::Result<()> {
        if self.ktls_state != KtlsState::Requested {
            return Ok(());
        }
        let (version, suite, ready_to_handoff) = {
            let RtmpConnectionState::Tls(Some(stream)) = &mut self.state else {
                return Ok(());
            };
            if stream.conn.is_handshaking() {
                return Ok(());
            }
            let Some(version) = stream.conn.protocol_version() else {
                return Ok(());
            };
            let Some(suite) = stream.conn.negotiated_cipher_suite() else {
                return Ok(());
            };
            let ready_to_handoff = !stream.conn.wants_write()
                && !stream
                    .conn
                    .reader()
                    .into_first_chunk()
                    .is_ok_and(|chunk| !chunk.is_empty());
            (version, suite, ready_to_handoff)
        };
        if !self.tls_version_recorded {
            match version {
                tokio_rustls::rustls::ProtocolVersion::TLSv1_2 => {
                    RTMPS_COUNTERS.tls12.fetch_add(1, Ordering::Relaxed);
                }
                tokio_rustls::rustls::ProtocolVersion::TLSv1_3 => {
                    RTMPS_COUNTERS.tls13.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
            self.tls_version_recorded = true;
        }
        let supported = crate::media::rtmp::supports_rtmps_cipher_suite(version, suite.suite())
            && rtmp_ktls::supports(version, suite.suite());
        if !supported {
            RTMPS_COUNTERS.ktls_attempts.fetch_add(1, Ordering::Relaxed);
            let tls_stream = match &mut self.state {
                RtmpConnectionState::Tls(stream) => {
                    stream.take().expect("TLS stream was checked above")
                }
                RtmpConnectionState::Plain(_)
                | RtmpConnectionState::Ktls(_)
                | RtmpConnectionState::Failed(_) => {
                    return Err(io::Error::other("RTMPS state changed during kTLS check"));
                }
            };
            let (_, socket) = tls_stream.into_parts();
            self.state = RtmpConnectionState::Failed(socket);
            self.ktls_state = KtlsState::Unsupported;
            RTMPS_COUNTERS
                .ktls_unsupported
                .fetch_add(1, Ordering::Relaxed);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "RTMPS requires kTLS, unsupported negotiated TLS {:?} suite {:?}",
                    version,
                    suite.suite()
                ),
            ));
        }
        if !ready_to_handoff {
            return Ok(());
        }
        RTMPS_COUNTERS.ktls_attempts.fetch_add(1, Ordering::Relaxed);
        let stream = match &mut self.state {
            RtmpConnectionState::Tls(stream) => {
                stream.take().expect("TLS stream was checked above")
            }
            RtmpConnectionState::Plain(_)
            | RtmpConnectionState::Ktls(_)
            | RtmpConnectionState::Failed(_) => return Ok(()),
        };
        let (connection, socket) = stream.into_parts();
        #[allow(deprecated)]
        let secrets = match connection.dangerous_extract_secrets() {
            Ok(secrets) => secrets,
            Err(error) => {
                self.state = RtmpConnectionState::Failed(socket);
                self.ktls_state = KtlsState::SetupFailed;
                RTMPS_COUNTERS.ktls_error.fetch_add(1, Ordering::Relaxed);
                return Err(io::Error::other(format!("rustls kTLS handoff: {error}")));
            }
        };
        if let Err(error) = rtmp_ktls::install(socket.as_raw_fd(), version, suite.suite(), &secrets)
        {
            self.state = RtmpConnectionState::Failed(socket);
            self.ktls_state = KtlsState::SetupFailed;
            RTMPS_COUNTERS.ktls_error.fetch_add(1, Ordering::Relaxed);
            return Err(error);
        }
        self.state = RtmpConnectionState::Ktls(KtlsConnection {
            stream: socket,
            version,
            handshake_buffer: Vec::new(),
            pending_alert_level: None,
            peer_closed: false,
        });
        self.ktls_state = KtlsState::Enabled;
        RTMPS_COUNTERS.ktls_success.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_ktls(&self) -> bool {
        matches!(self.state, RtmpConnectionState::Ktls(_))
    }
}

impl Drop for RtmpConnection {
    fn drop(&mut self) {
        if self.tls_version_recorded && matches!(self.state, RtmpConnectionState::Tls(_)) {
            RTMPS_COUNTERS
                .userspace_tls_connections
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Read for RtmpConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            RtmpConnectionState::Plain(stream) => stream.read(buf),
            RtmpConnectionState::Ktls(connection) => connection.read(buf),
            RtmpConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            RtmpConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }
}

impl Write for RtmpConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            RtmpConnectionState::Plain(stream) => stream.write(buf),
            RtmpConnectionState::Ktls(connection) => connection.stream.write(buf),
            RtmpConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            RtmpConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            RtmpConnectionState::Plain(stream) => stream.write_vectored(bufs),
            RtmpConnectionState::Ktls(connection) => connection.stream.write_vectored(bufs),
            RtmpConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            RtmpConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            RtmpConnectionState::Plain(stream) => stream.flush(),
            RtmpConnectionState::Ktls(connection) => connection.stream.flush(),
            RtmpConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            RtmpConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }
}

#[cfg(test)]
#[path = "rtmp_connection_tests.rs"]
mod tests;
