//! The Compio QUIC transport moq-net's drivers run over.
//!
//! [`MoqSession`] implements `web_transport_trait::poll` (0.4) over one
//! compio-quic connection, either native QUIC (the ALPN names the MoQ
//! version: moq-lite or an IETF MOQT draft) or one WebTransport session over
//! HTTP/3 (ALPN `h3`; the CONNECT subprotocol names the version). Every type
//! here is `!Send` and lives on the Compio thread that owns the connection:
//! [`accept`] / [`connect`] build a session, moq-net's
//! `Server::accept_request` / `Client::connect` run the MoQ handshake on it
//! (the vendored `unsend` patch admits this `!Send` transport for the IETF
//! drafts too), and [`run_driver`] drives the resulting moq-net driver with
//! Compio timers.

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

use restream_srt::address_family::AddressFamily;

/// Every MoQ version Restream speaks, most preferred first: moq-net's
/// `ALPNS` (moq-lite, the legacy `moql` SETUP negotiation, IETF MOQT drafts
/// 14-22). A native client offers them as ALPNs, a WebTransport client as
/// subprotocols.
pub(crate) const MOQ_ALPNS: &[&str] = moq_net::ALPNS;

/// HTTP/3, under which a browser opens a WebTransport session.
const H3_ALPN: &str = "h3";

/// Every ALPN a MoQ listener offers: [`MOQ_ALPNS`] and `h3`. Build the
/// listener's TLS configuration with these (`tls::server_config`).
pub(crate) const SERVER_ALPNS: [&str; MOQ_ALPNS.len() + 1] = {
    let mut alpns = [H3_ALPN; MOQ_ALPNS.len() + 1];
    let mut index = 0;
    while index < MOQ_ALPNS.len() {
        alpns[index] = MOQ_ALPNS[index];
        index += 1;
    }
    alpns
};

/// Upper bound on [`accept`] and [`connect`]: the QUIC handshake plus, for
/// WebTransport, the HTTP/3 SETTINGS and CONNECT exchange. A peer that stalls
/// inside it cannot hold a listener task longer than this.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How an accepted session reached us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportMode {
    /// Native QUIC; the ALPN named the MoQ version.
    Native,
    /// A WebTransport session over HTTP/3 (browsers).
    WebTransport,
}

/// A session [`accept`] established, before the MoQ handshake.
pub(crate) struct Accepted {
    pub session: MoqSession,
    pub mode: TransportMode,
    /// The WebTransport CONNECT `:path`; empty for native QUIC, where the
    /// MoQ SETUP carries the path.
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
/// subprotocol picks the MoQ version). Bounded by [`HANDSHAKE_TIMEOUT`];
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
    let protocol = moq_protocol(&conn, alpn.as_deref())?;
    Ok(Accepted {
        session: MoqSession::new(conn, protocol, None),
        mode: TransportMode::Native,
        path: String::new(),
        query: None,
    })
}

/// Dials `remote` over native QUIC offering the MoQ ALPNs `tls` carries
/// (all or some of [`MOQ_ALPNS`]). Bounded by [`HANDSHAKE_TIMEOUT`].
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
    let protocol = moq_protocol(&conn, alpn.as_deref())?;
    Ok(MoqSession::new(conn, protocol, None))
}

fn negotiated_alpn(conn: &mut Connection) -> Result<Option<Vec<u8>>, TransportError> {
    Ok(conn
        .handshake_data()
        .map_err(TransportError::Connection)?
        .protocol)
}

/// The MoQ version `alpn` names; otherwise the connection is closed.
fn moq_protocol(conn: &Connection, alpn: Option<&[u8]>) -> Result<&'static str, TransportError> {
    alpn.and_then(|alpn| {
        MOQ_ALPNS
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
