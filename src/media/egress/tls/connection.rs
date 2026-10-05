//! Plain-or-TLS transport for fabric protocol engines.
//!
//! Compio-owned completion workers drive socket reads and writes on the egress
//! shard runtime. The synchronous protocol and rustls state machines exchange
//! bytes with those workers through bounded connection-local buffers.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::Arc;

use crate::media::egress::backend::Interest;
use crate::media::egress::backends::compio_tcp::CompioTcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ClientConnection, ProtocolVersion, StreamOwned};

use super::ktls;
use super::telemetry::TlsCounters;

enum ConnectionState {
    Plain(CompioTcpStream),
    Ktls(KtlsConnection),
    Tls(Option<Box<StreamOwned<ClientConnection, CompioTcpStream>>>),
    Failed(CompioTcpStream),
}

struct KtlsConnection {
    stream: CompioTcpStream,
    version: ProtocolVersion,
    /// Application data rustls decrypted before the hand-off (a server that
    /// writes first, or half-RTT data): served before any kernel record.
    early_plaintext: Vec<u8>,
    handshake_buffer: Vec<u8>,
    pending_alert_level: Option<u8>,
    peer_closed: bool,
}

impl KtlsConnection {
    const MAX_CONTROL_RECORDS_PER_READ: usize = 8;

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.early_plaintext.is_empty() {
            let count = buffer.len().min(self.early_plaintext.len());
            buffer[..count].copy_from_slice(&self.early_plaintext[..count]);
            self.early_plaintext.drain(..count);
            return Ok(count);
        }
        let stream = &mut self.stream;
        Self::read_records(
            buffer,
            self.version,
            &mut self.handshake_buffer,
            &mut self.pending_alert_level,
            &mut self.peer_closed,
            |buffer| stream.read_record(buffer),
        )
    }

    #[cfg(test)]
    fn read_with(
        &mut self,
        buffer: &mut [u8],
        recv_record: impl FnMut(&mut [u8]) -> io::Result<(usize, u8)>,
    ) -> io::Result<usize> {
        Self::read_records(
            buffer,
            self.version,
            &mut self.handshake_buffer,
            &mut self.pending_alert_level,
            &mut self.peer_closed,
            recv_record,
        )
    }

    fn read_records(
        buffer: &mut [u8],
        version: ProtocolVersion,
        handshake_buffer: &mut Vec<u8>,
        pending_alert_level: &mut Option<u8>,
        peer_closed: &mut bool,
        mut recv_record: impl FnMut(&mut [u8]) -> io::Result<(usize, u8)>,
    ) -> io::Result<usize> {
        if buffer.is_empty() || *peer_closed {
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
                *peer_closed = true;
                return Ok(0);
            }
            if record_type != ktls::RECORD_TYPE_DATA {
                control_records += 1;
            }
            match record_type {
                ktls::RECORD_TYPE_DATA => return Ok(count),
                ktls::RECORD_TYPE_HANDSHAKE if version == ProtocolVersion::TLSv1_3 => {
                    if handshake_buffer.len().saturating_add(count) > (1 << 20) + 4 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "oversized TLS 1.3 post-handshake data",
                        ));
                    }
                    handshake_buffer.extend_from_slice(&buffer[..count]);
                    Self::process_handshake_buffer(handshake_buffer)?;
                }
                ktls::RECORD_TYPE_ALERT => {
                    let (level, description) = match (pending_alert_level.take(), count) {
                        (None, 1) => {
                            *pending_alert_level = Some(buffer[0]);
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
                        *peer_closed = true;
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

    fn process_handshake_buffer(buffer: &mut Vec<u8>) -> io::Result<()> {
        loop {
            if buffer.len() < 4 {
                return Ok(());
            }
            let message_type = buffer[0];
            let message_len =
                ((buffer[1] as usize) << 16) | ((buffer[2] as usize) << 8) | buffer[3] as usize;
            if message_len > (1 << 20) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "oversized TLS 1.3 handshake message",
                ));
            }
            let frame_len = 4 + message_len;
            if buffer.len() < frame_len {
                return Ok(());
            }
            match message_type {
                4 => {
                    // ponytail: buffered Rustls extraction cannot retain session state; ignore
                    // tickets and fail closed on KeyUpdate until the unbuffered API is used.
                    drop(buffer.drain(..frame_len));
                }
                24 if message_len == 1 && buffer[4] <= 1 => {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KtlsState {
    NotRequested,
    Requested,
    Enabled,
    Unsupported,
    SetupFailed,
}

pub(crate) struct TlsTcpConnection {
    state: ConnectionState,
    tls_version_recorded: bool,
    ktls_state: KtlsState,
    /// The owning protocol's counters; `None` for plain TCP.
    counters: Option<&'static TlsCounters>,
}

impl TlsTcpConnection {
    pub(crate) fn plain(stream: impl Into<CompioTcpStream>) -> Self {
        Self {
            state: ConnectionState::Plain(stream.into()),
            tls_version_recorded: false,
            ktls_state: KtlsState::NotRequested,
            counters: None,
        }
    }

    // Production always calls `tls_with_config` directly with an explicit
    // client config (see rtmp_shard.rs); this default-config convenience
    // wrapper is only exercised by tests.
    #[cfg(test)]
    pub(crate) fn tls(stream: impl Into<CompioTcpStream>, host: &str) -> Result<Self, String> {
        static TEST_COUNTERS: TlsCounters = TlsCounters::new();
        Self::tls_with_config(stream, host, super::rustls_client_config(), &TEST_COUNTERS)
    }

    /// Same as [`Self::tls`] but with an explicit `ClientConfig` and the
    /// owning protocol's counters. Production passes the destination's
    /// resolved config; tests substitute a locally generated certificate.
    pub(crate) fn tls_with_config(
        stream: impl Into<CompioTcpStream>,
        host: &str,
        config: Arc<ClientConfig>,
        counters: &'static TlsCounters,
    ) -> Result<Self, String> {
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|_| format!("invalid TLS server name: {host}"))?;
        let mut config = (*config).clone();
        config.enable_secret_extraction = true;
        let connection = ClientConnection::new(Arc::new(config), server_name)
            .map_err(|error| format!("rustls client connection init failed: {error}"))?;
        TlsCounters::add(&counters.connections);
        TlsCounters::add(&counters.ktls_requested);
        let stream: CompioTcpStream = stream.into();
        stream.set_ancillary_mode();
        Ok(Self {
            state: ConnectionState::Tls(Some(Box::new(StreamOwned::new(connection, stream)))),
            tls_version_recorded: false,
            ktls_state: KtlsState::Requested,
            counters: Some(counters),
        })
    }

    pub(crate) fn completion_stream(&self) -> &CompioTcpStream {
        match &self.state {
            ConnectionState::Plain(stream) | ConnectionState::Failed(stream) => stream,
            ConnectionState::Ktls(connection) => &connection.stream,
            ConnectionState::Tls(Some(stream)) => &stream.sock,
            ConnectionState::Tls(None) => {
                unreachable!("TLS stream is only temporarily taken during handoff")
            }
        }
    }

    fn tcp_stream(&self) -> &CompioTcpStream {
        self.completion_stream()
    }

    /// The interest that will actually unblock a `WouldBlock`. While Rustls
    /// owns the socket (handshake, then until the kTLS handoff), the RTMP
    /// state machine's requested direction is not what gates progress: a
    /// handshake write blocks on reading the server's flight. Reporting the
    /// request there would make the completion scheduler revisit a leaf that
    /// has nothing in flight and no staged input, over and over, instead of
    /// waiting for the receive completion.
    pub(crate) fn interest_hint(&self, requested: Interest) -> Interest {
        let ConnectionState::Tls(Some(stream)) = &self.state else {
            return requested;
        };
        let hint = Interest {
            readable: stream.conn.wants_read() || stream.sock.pending_receive_bytes() > 0,
            writable: stream.conn.wants_write() || stream.sock.pending_write_bytes() > 0,
        };
        if hint.is_empty() { requested } else { hint }
    }

    pub(crate) fn raw_fd(&self) -> RawFd {
        self.tcp_stream().as_raw_fd()
    }

    pub(crate) fn pending_transport_write_bytes(&self) -> usize {
        self.tcp_stream().pending_write_bytes()
    }
    pub(crate) fn resume_receive(&self) {
        self.tcp_stream().resume_receive();
    }
    pub(crate) fn has_buffered_receive(&self) -> bool {
        matches!(&self.state, ConnectionState::Ktls(connection) if !connection.early_plaintext.is_empty())
            || self.tcp_stream().has_buffered_receive()
    }

    /// Conservative estimate of rustls-internal buffered bytes not visible
    /// to `MediaPublisher::pending_bytes()`. rustls exposes no occupancy
    /// getter for its internal plaintext/TLS-record buffers —
    /// `ConnectionCommon::set_buffer_limit` is the only related API, a cap
    /// *setter* with no matching getter (checked against rustls 0.23.41's
    /// actual public API; see the egress migration record (git history) Phase 5
    /// status). Returns rustls's own default 64KB `sendable_plaintext`/
    /// `sendable_tls` cap whenever the connection still wants to write
    /// (`wants_write()` — i.e. it is holding data this leaf hasn't
    /// finished flushing), `0` otherwise. This is a worst-case upper bound
    /// on the hidden buffer, not an exact occupancy count — the point is
    /// keeping `LeafLimits::max_pending_bytes` enforcement from
    /// under-counting a backpressured TLS leaf by an unbounded amount,
    /// not precise accounting.
    pub(crate) fn rustls_pending_bytes_estimate(&self) -> usize {
        const RUSTLS_DEFAULT_BUFFER_LIMIT: usize = 64 * 1024;
        match &self.state {
            ConnectionState::Plain(_) | ConnectionState::Ktls(_) | ConnectionState::Failed(_) => 0,
            ConnectionState::Tls(Some(stream)) => {
                if stream.conn.wants_write() {
                    RUSTLS_DEFAULT_BUFFER_LIMIT
                } else {
                    0
                }
            }
            ConnectionState::Tls(None) => 0,
        }
    }
    pub(crate) fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.tcp_stream().shutdown(how)
    }

    fn advance_tls_handshake(&mut self) -> io::Result<()> {
        if let ConnectionState::Tls(Some(stream)) = &mut self.state {
            pump_rustls(stream)?;
            if stream.conn.is_handshaking() || stream.conn.wants_write() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        self.maybe_handoff_ktls()?;
        if matches!(self.state, ConnectionState::Tls(_)) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(())
    }

    fn maybe_handoff_ktls(&mut self) -> io::Result<()> {
        if self.ktls_state != KtlsState::Requested {
            return Ok(());
        }
        let Some(counters) = self.counters else {
            return Ok(()); // plain TCP never requests kTLS
        };
        let (version, suite, ready_to_handoff) = {
            let ConnectionState::Tls(Some(stream)) = &mut self.state else {
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
            // Every byte received so far went through rustls as whole
            // records (`pump_rustls`), so none is cut at the hand-off.
            let ready_to_handoff = !stream.conn.wants_write()
                && stream.sock.pending_write_bytes() == 0
                && stream.sock.pending_receive_bytes() == 0;
            (version, suite, ready_to_handoff)
        };
        if !self.tls_version_recorded {
            match version {
                ProtocolVersion::TLSv1_2 => TlsCounters::add(&counters.tls12),
                ProtocolVersion::TLSv1_3 => TlsCounters::add(&counters.tls13),
                _ => {}
            }
            self.tls_version_recorded = true;
        }
        let supported = super::supports_cipher_suite(version, suite.suite())
            && ktls::supports(version, suite.suite());
        if !supported {
            TlsCounters::add(&counters.ktls_attempts);
            let tls_stream = match &mut self.state {
                ConnectionState::Tls(stream) => {
                    stream.take().expect("TLS stream was checked above")
                }
                ConnectionState::Plain(_)
                | ConnectionState::Ktls(_)
                | ConnectionState::Failed(_) => {
                    return Err(io::Error::other("TLS state changed during kTLS check"));
                }
            };
            let (_, socket) = tls_stream.into_parts();
            self.state = ConnectionState::Failed(socket);
            self.ktls_state = KtlsState::Unsupported;
            TlsCounters::add(&counters.ktls_unsupported);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "TLS egress requires kTLS, unsupported negotiated TLS {:?} suite {:?}",
                    version,
                    suite.suite()
                ),
            ));
        }
        if !ready_to_handoff {
            return Ok(());
        }
        TlsCounters::add(&counters.ktls_attempts);
        let mut stream = match &mut self.state {
            ConnectionState::Tls(stream) => stream.take().expect("TLS stream was checked above"),
            ConnectionState::Plain(_) | ConnectionState::Ktls(_) | ConnectionState::Failed(_) => {
                return Ok(());
            }
        };
        let early_plaintext = take_plaintext(&mut stream.conn);
        let (connection, socket) = stream.into_parts();
        #[allow(deprecated)]
        let secrets = match connection.dangerous_extract_secrets() {
            Ok(secrets) => secrets,
            Err(error) => {
                self.state = ConnectionState::Failed(socket);
                self.ktls_state = KtlsState::SetupFailed;
                TlsCounters::add(&counters.ktls_error);
                return Err(io::Error::other(format!("rustls kTLS handoff: {error}")));
            }
        };
        if let Err(error) = ktls::install(socket.as_raw_fd(), version, suite.suite(), &secrets) {
            self.state = ConnectionState::Failed(socket);
            self.ktls_state = KtlsState::SetupFailed;
            TlsCounters::add(&counters.ktls_error);
            return Err(error);
        }
        socket.set_ktls_mode();
        self.state = ConnectionState::Ktls(KtlsConnection {
            stream: socket,
            version,
            early_plaintext,
            handshake_buffer: Vec::new(),
            pending_alert_level: None,
            peer_closed: false,
        });
        self.ktls_state = KtlsState::Enabled;
        TlsCounters::add(&counters.ktls_success);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_ktls(&self) -> bool {
        matches!(self.state, ConnectionState::Ktls(_))
    }
}

/// Drive rustls over the Compio stream: send what it wants to write, then
/// give it only the whole TLS records at the front of the receive buffer,
/// until no whole record is left. `complete_io` would read raw bytes (up to
/// 4 KiB at a time), which can end inside a record; at the kTLS hand-off
/// that record's start would stay in rustls and its end go to the kernel
/// (rustls' secret extraction does not check for buffered input). Whole
/// records also cover what a TLS 1.3 server sends after the handshake
/// (session tickets), which nothing else reads before the hand-off: it
/// waits for an empty receive buffer. A partial record waits for its next
/// receive event. Limit: a handshake message split across records with only
/// its first record arrived would still reach the kernel without its start;
/// rustls has no public check for it, and tickets fit in one record.
fn pump_rustls(stream: &mut StreamOwned<ClientConnection, CompioTcpStream>) -> io::Result<()> {
    loop {
        while stream.conn.wants_write() {
            match stream.conn.write_tls(&mut stream.sock) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        let whole = stream.sock.complete_tls_records_len();
        if whole == 0 {
            if stream.sock.receive_closed() {
                // The peer closed or the socket failed: a record cut short
                // can never complete, and a handshake cannot finish.
                if stream.sock.pending_receive_bytes() > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "peer closed inside a TLS record before the kTLS hand-off",
                    ));
                }
                if stream.conn.is_handshaking() && stream.conn.read_tls(&mut stream.sock)? == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
            }
            return Ok(());
        }
        let mut records = Read::take(&mut stream.sock, whole as u64);
        while records.limit() > 0 {
            if stream.conn.read_tls(&mut records)? == 0 {
                // rustls reads nothing after close_notify: the records left
                // can never be consumed, and the hand-off waits for them.
                let kind = if stream.conn.is_handshaking() {
                    io::ErrorKind::UnexpectedEof
                } else {
                    io::ErrorKind::ConnectionAborted
                };
                return Err(io::Error::new(
                    kind,
                    "peer sent TLS records after close_notify before the kTLS hand-off",
                ));
            }
            if let Err(error) = stream.conn.process_new_packets() {
                // Send the alert rustls queued for the failure, as
                // `complete_io` does, then fail the connection.
                let _ = stream.conn.write_tls(&mut stream.sock);
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
        }
    }
}

/// Application data rustls already decrypted; it would be lost with the
/// rustls connection at the hand-off.
fn take_plaintext(connection: &mut ClientConnection) -> Vec<u8> {
    let mut plaintext = Vec::new();
    let mut chunk = [0_u8; 4096];
    while let Ok(count @ 1..) = connection.reader().read(&mut chunk) {
        plaintext.extend_from_slice(&chunk[..count]);
    }
    plaintext
}

impl Drop for TlsTcpConnection {
    fn drop(&mut self) {
        if self.tls_version_recorded
            && matches!(self.state, ConnectionState::Tls(_))
            && let Some(counters) = self.counters
        {
            TlsCounters::add(&counters.userspace_tls_connections);
        }
    }
}

impl Read for TlsTcpConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            ConnectionState::Plain(stream) => stream.read(buf),
            ConnectionState::Ktls(connection) => connection.read(buf),
            ConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            ConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }
}

impl TlsTcpConnection {
    /// Zero-copy counterpart of `write_vectored` for media: shared payload
    /// slices are queued by reference. Only plain and kTLS connections carry
    /// media; Rustls-owned sockets refuse like `write` does.
    pub(crate) fn write_shared(
        &mut self,
        parts: &[crate::media::egress::backends::compio_tcp::TxPart<'_>],
    ) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            ConnectionState::Plain(stream) => stream.write_shared(parts),
            ConnectionState::Ktls(connection) => connection.stream.write_shared(parts),
            ConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            ConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }
}

impl TlsTcpConnection {
    /// `write_shared` for the rest of one message (an HTTP request): sends
    /// carry `MSG_MORE` until its last byte is staged, so kTLS records and
    /// TCP segments fill instead of each record being pushed alone.
    pub(crate) fn write_shared_message(
        &mut self,
        parts: &[crate::media::egress::backends::compio_tcp::TxPart<'_>],
    ) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            ConnectionState::Plain(stream) => stream.write_shared_message(parts),
            ConnectionState::Ktls(connection) => connection.stream.write_shared_message(parts),
            ConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            ConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }
}

impl Write for TlsTcpConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            ConnectionState::Plain(stream) => stream.write(buf),
            ConnectionState::Ktls(connection) => connection.stream.write(buf),
            ConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            ConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            ConnectionState::Plain(stream) => stream.write_vectored(bufs),
            ConnectionState::Ktls(connection) => connection.stream.write_vectored(bufs),
            ConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            ConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.advance_tls_handshake()?;
        match &mut self.state {
            ConnectionState::Plain(stream) => stream.flush(),
            ConnectionState::Ktls(connection) => connection.stream.flush(),
            ConnectionState::Tls(_) => Err(io::ErrorKind::WouldBlock.into()),
            ConnectionState::Failed(_) => Err(io::Error::other("TLS handoff failed")),
        }
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
