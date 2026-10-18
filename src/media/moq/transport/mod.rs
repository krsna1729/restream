//! The Compio QUIC transport moq-net's lite driver runs over.
//!
//! [`MoqSession`] implements `web_transport_trait::poll` (0.4) over one
//! compio-quic connection, either native QUIC (the ALPN names the moq-lite
//! version) or one WebTransport session over HTTP/3 (ALPN `h3`; the CONNECT
//! subprotocol names the version). Every type here is `!Send` and lives on the
//! Compio thread that owns the connection: [`accept`] / [`connect`] build a
//! session, moq-net's `Server::accept_request_lite` / `Client::connect_lite`
//! run the MoQ handshake on it, and [`run_driver`] drives the resulting moq-net
//! driver with Compio timers.

mod driver;
mod session;
#[cfg(test)]
pub(crate) mod test_support;
mod webtransport;

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use compio::quic::{
    ClientConfig, Connection, ConnectionError, Endpoint, Incoming, SendDatagramError, ServerConfig,
    VarInt,
};

pub(crate) use driver::run_driver;
pub(crate) use session::MoqSession;

use crate::media::srt::AddressFamily;

/// The moq-lite versions Restream speaks, most preferred first: a native
/// client offers them as ALPNs, a WebTransport client as subprotocols. They
/// are moq-net's `ALPNS` minus the legacy `moql` SETUP negotiation, which
/// moq-net's `!Send` (lite-only) entry points refuse.
pub(crate) const CLIENT_ALPNS: &[&str] =
    &["moq-lite-06", "moq-lite-05", "moq-lite-04", "moq-lite-03"];

/// HTTP/3, under which a browser opens a WebTransport session.
const H3_ALPN: &str = "h3";

/// Every ALPN a MoQ listener offers: the native moq-lite versions and `h3`.
/// Build the listener's TLS configuration with these (`tls::server_config`).
pub(crate) const SERVER_ALPNS: &[&str] = &[
    "moq-lite-06",
    "moq-lite-05",
    "moq-lite-04",
    "moq-lite-03",
    H3_ALPN,
];

/// Upper bound on [`accept`] and [`connect`]: the QUIC handshake plus, for
/// WebTransport, the HTTP/3 SETTINGS and CONNECT exchange. A peer that stalls
/// inside it cannot hold a listener task longer than this.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How an accepted session reached us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportMode {
    /// Native QUIC; the ALPN named the moq-lite version.
    Native,
    /// A WebTransport session over HTTP/3 (browsers).
    WebTransport,
}

/// A session [`accept`] established, before the MoQ handshake.
pub(crate) struct Accepted {
    pub session: MoqSession,
    pub mode: TransportMode,
    /// The WebTransport CONNECT `:path`; empty for native QUIC, where
    /// moq-lite carries the path in its own SETUP.
    pub path: String,
    /// The CONNECT query string (without `?`), if any; `None` for native QUIC.
    pub query: Option<String>,
}

/// Why a MoQ transport operation failed. `Display` is a reason fit for logs
/// and the API.
#[derive(Debug, Clone)]
pub(crate) enum TransportError {
    /// The QUIC connection failed, or ended without an application close
    /// (including [`ConnectionError::LocallyClosed`] after our own close).
    Connection(ConnectionError),
    /// The peer ended the session with an application code: a QUIC
    /// CONNECTION_CLOSE (native) or a CLOSE_WEBTRANSPORT_SESSION capsule.
    Closed { code: u32, reason: String },
    /// The peer reset the stream we read (RESET_STREAM).
    StreamReset(u32),
    /// The peer stopped the stream we write (STOP_SENDING).
    StreamStopped(u32),
    /// The stream was already finished, reset or stopped locally.
    StreamClosed,
    /// `finish` found a WebTransport stream header that flow control has not
    /// yet accepted, so the FIN would precede it.
    HeaderBlocked,
    /// The peer does not take datagrams, or they are disabled locally.
    Datagram(SendDatagramError),
    /// The session could not be established: no MoQ ALPN, a rejected or
    /// malformed WebTransport CONNECT, or the handshake timed out.
    Handshake(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(error) => write!(f, "MoQ QUIC connection: {error}"),
            Self::Closed { code, reason } if reason.is_empty() => {
                write!(f, "MoQ session closed by peer (code {code})")
            }
            Self::Closed { code, reason } => {
                write!(f, "MoQ session closed by peer (code {code}): {reason}")
            }
            Self::StreamReset(code) => write!(f, "MoQ stream reset by peer (code {code})"),
            Self::StreamStopped(code) => write!(f, "MoQ stream stopped by peer (code {code})"),
            Self::StreamClosed => f.write_str("MoQ stream already closed"),
            Self::HeaderBlocked => {
                f.write_str("MoQ WebTransport stream header blocked by flow control")
            }
            Self::Datagram(error) => write!(f, "MoQ datagram: {error}"),
            Self::Handshake(reason) => write!(f, "MoQ handshake: {reason}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl web_transport_trait::Error for TransportError {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            Self::Closed { code, reason } => Some((*code, reason.clone())),
            _ => None,
        }
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            Self::StreamReset(code) | Self::StreamStopped(code) => Some(*code),
            _ => None,
        }
    }
}

/// A MoQ listener endpoint on `addr`. `tls` must offer [`SERVER_ALPNS`].
pub(crate) fn server_endpoint(
    addr: SocketAddr,
    tls: ServerConfig,
) -> impl Future<Output = io::Result<Endpoint>> {
    Endpoint::server(addr, tls)
}

/// A client endpoint on an ephemeral port of `family`'s wildcard address.
pub(crate) fn client_endpoint(family: AddressFamily) -> impl Future<Output = io::Result<Endpoint>> {
    Endpoint::client(match family {
        AddressFamily::V4 => "0.0.0.0:0",
        AddressFamily::V6 => "[::]:0",
    })
}

/// Completes an incoming connection: the QUIC handshake, then, when the ALPN
/// is `h3`, the HTTP/3 SETTINGS exchange and the WebTransport CONNECT (the
/// subprotocol picks the moq-lite version). Bounded by [`HANDSHAKE_TIMEOUT`];
/// a connection without a MoQ ALPN is closed.
pub(crate) async fn accept(incoming: Incoming) -> Result<Accepted, TransportError> {
    compio::time::timeout(HANDSHAKE_TIMEOUT, accept_connection(incoming))
        .await
        .unwrap_or_else(|_| Err(handshake_timeout()))
}

async fn accept_connection(incoming: Incoming) -> Result<Accepted, TransportError> {
    let mut conn = incoming.await.map_err(TransportError::Connection)?;
    let alpn = negotiated_alpn(&mut conn)?;
    if alpn.as_deref() == Some(H3_ALPN.as_bytes()) {
        return webtransport::accept(conn).await;
    }
    let protocol = lite_protocol(&conn, alpn.as_deref())?;
    Ok(Accepted {
        session: MoqSession::new(conn, protocol, None),
        mode: TransportMode::Native,
        path: String::new(),
        query: None,
    })
}

/// Dials `remote` over native QUIC offering [`CLIENT_ALPNS`] (`tls` must
/// carry them). Bounded by [`HANDSHAKE_TIMEOUT`].
pub(crate) async fn connect(
    endpoint: &Endpoint,
    remote: SocketAddr,
    server_name: &str,
    tls: ClientConfig,
) -> Result<MoqSession, TransportError> {
    let connecting = endpoint
        .connect(remote, server_name, Some(tls))
        .map_err(|error| TransportError::Handshake(format!("QUIC connect: {error}")))?;
    let mut conn = compio::time::timeout(HANDSHAKE_TIMEOUT, connecting)
        .await
        .map_err(|_| handshake_timeout())?
        .map_err(TransportError::Connection)?;
    let alpn = negotiated_alpn(&mut conn)?;
    let protocol = lite_protocol(&conn, alpn.as_deref())?;
    Ok(MoqSession::new(conn, protocol, None))
}

fn negotiated_alpn(conn: &mut Connection) -> Result<Option<Vec<u8>>, TransportError> {
    Ok(conn
        .handshake_data()
        .map_err(TransportError::Connection)?
        .protocol)
}

/// The moq-lite version `alpn` names; otherwise the connection is closed.
fn lite_protocol(conn: &Connection, alpn: Option<&[u8]>) -> Result<&'static str, TransportError> {
    alpn.and_then(|alpn| {
        CLIENT_ALPNS
            .iter()
            .copied()
            .find(|known| known.as_bytes() == alpn)
    })
    .ok_or_else(|| {
        conn.close(VarInt::from_u32(0), b"no MoQ ALPN");
        TransportError::Handshake("no MoQ ALPN negotiated".to_owned())
    })
}

fn handshake_timeout() -> TransportError {
    TransportError::Handshake(format!("timed out after {} s", HANDSHAKE_TIMEOUT.as_secs()))
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
#[cfg(test)]
#[path = "webtransport_tests.rs"]
mod webtransport_tests;
