//! WebTransport over HTTP/3 (draft-ietf-webtrans-http3) for one session per
//! connection: the server handshake, the stream and datagram headers, and
//! the session close.
//!
//! - Handshake (server): open our control stream with SETTINGS (WebTransport
//!   enabled), accept the CONNECT stream, read its HEADERS frame byte-exactly
//!   (anything after it on that stream is capsules) and answer 200 with the
//!   most preferred MoQ version (moq-lite or IETF MOQT) the client offers as
//!   a subprotocol.
//! - Streams: a WebTransport uni stream starts `0x54, session id`, a bi stream
//!   `0x41, session id` (both QUIC varints). Peer HTTP/3 control and QPACK
//!   streams are held open unread; a wrong session id is stopped with
//!   `WT_BUFFERED_STREAM_REJECTED`, an unknown uni stream with
//!   `H3_STREAM_CREATION_ERROR`, a non-WebTransport bi stream with
//!   `H3_REQUEST_REJECTED`.
//! - Datagrams carry the quarter stream id (session id / 4) as a prefix.
//! - Close: a CLOSE_WEBTRANSPORT_SESSION capsule (in an HTTP/3 DATA frame)
//!   and FIN on the CONNECT stream, then the QUIC close about a second later
//!   so the capsule is not discarded. A detached task reads the peer's
//!   capsules to see its close.
//!
//! `web-transport-proto`'s QPACK decoding can panic on hostile input (integer
//! overflow under debug assertions), so every decode of peer bytes runs
//! inside [`guarded`]. Release builds have no overflow checks: there the
//! prefix integers wrap instead, within bounds (string reads check the bytes
//! left, a frame is at most [`MAX_FRAME_LEN`]), so a hostile CONNECT is at
//! worst misread and rejected.

use bytes::Bytes;
use compio::quic::{Connection, SendStream, VarInt};
use web_transport_proto::http::StatusCode;
use web_transport_proto::{ConnectRequest, ConnectResponse, Settings};

use super::session::RawSession;
use super::wire::{
    H3_MESSAGE_ERROR, H3_REQUEST_REJECTED, WtSession, connection_error, guarded, read_frame,
    write_error, write_now,
};
#[cfg(any(test, feature = "test-support"))]
use super::wire::{
    H3_STREAM_CREATION_ERROR, STREAM_CONTROL, STREAM_QPACK_DECODER, STREAM_QPACK_ENCODER,
    read_varint,
};
use super::{Accepted, MOQ_ALPNS, MoqSession, TransportError, TransportMode};

/// Writes all bytes, mapping a stopped or lost stream to the transport error.
async fn write_all(stream: &mut SendStream, bytes: Vec<u8>) -> Result<(), TransportError> {
    stream
        .write_all_chunks(&mut [Bytes::from(bytes)])
        .await
        .map_err(|error| write_error(error, true))
}

/// Opens our HTTP/3 control stream and sends SETTINGS (WebTransport on).
async fn send_settings(conn: &Connection) -> Result<SendStream, TransportError> {
    let mut control = conn
        .open_uni_wait()
        .await
        .map_err(|error| connection_error(error, true))?;
    let mut settings = Settings::default();
    settings.enable_webtransport(1);
    let mut bytes = Vec::new();
    settings.encode(&mut bytes);
    write_all(&mut control, bytes).await?;
    Ok(control)
}

/// Answers the CONNECT with `status` (best effort), closes the connection
/// with `code`, and reports `reason`.
fn reject(
    conn: &Connection,
    send: SendStream,
    status: StatusCode,
    code: u32,
    reason: String,
) -> TransportError {
    let mut response = Vec::new();
    if ConnectResponse::new(status).encode(&mut response).is_ok() {
        write_now(send, &response);
    }
    conn.close(VarInt::from_u32(code), reason.as_bytes());
    TransportError::Handshake(reason)
}

/// The server half of the handshake on an `h3` connection.
pub(super) async fn accept(conn: Connection) -> Result<Accepted, TransportError> {
    let control = send_settings(&conn).await?;
    let (mut send, mut recv) = conn
        .accept_bi()
        .await
        .map_err(|error| connection_error(error, true))?;
    let frame = read_frame(&mut recv).await?;
    let request = match guarded(|| ConnectRequest::decode(&mut frame.as_slice())) {
        Some(Ok(request)) => request,
        Some(Err(error)) => {
            return Err(reject(
                &conn,
                send,
                StatusCode::BAD_REQUEST,
                H3_MESSAGE_ERROR,
                format!("WebTransport CONNECT: {error}"),
            ));
        }
        None => {
            return Err(reject(
                &conn,
                send,
                StatusCode::BAD_REQUEST,
                H3_MESSAGE_ERROR,
                "WebTransport CONNECT: malformed HEADERS".to_owned(),
            ));
        }
    };
    let Some(protocol) = MOQ_ALPNS
        .iter()
        .copied()
        .find(|known| request.protocols.iter().any(|offered| offered == known))
    else {
        return Err(reject(
            &conn,
            send,
            StatusCode::BAD_REQUEST,
            H3_REQUEST_REJECTED,
            "WebTransport CONNECT offers no supported MoQ subprotocol".to_owned(),
        ));
    };
    let mut response = Vec::new();
    ConnectResponse::new(StatusCode::OK)
        .with_protocol(protocol)
        .encode(&mut response)
        .map_err(|error| TransportError::Handshake(format!("WebTransport response: {error}")))?;
    write_all(&mut send, response).await?;
    let session_id = u64::from(send.id());
    let path = request.url.path().to_owned();
    let query = request.url.query().map(str::to_owned);
    let session = WtSession::new(session_id, control, send, Vec::new(), 0);
    Ok(Accepted {
        session: MoqSession::new(RawSession::new(conn, protocol, Some((session, recv)))),
        mode: TransportMode::WebTransport,
        path,
        query,
    })
}

/// The client half: SETTINGS both ways, then CONNECT `url` offering
/// `protocols`. Restream dials native QUIC only; tests use this to play the
/// browser.
#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn connect(
    conn: Connection,
    url: url::Url,
    protocols: &[&str],
) -> Result<MoqSession, TransportError> {
    let control = send_settings(&conn).await?;
    let mut held = Vec::new();
    let mut held_types = 0u8;
    loop {
        let mut recv = conn
            .accept_uni()
            .await
            .map_err(|error| connection_error(error, true))?;
        match read_varint(&mut recv).await? {
            Some(STREAM_CONTROL) => {
                let mut settings = vec![0];
                settings.extend_from_slice(&read_frame(&mut recv).await?);
                let settings = guarded(|| Settings::decode(&mut settings.as_slice()))
                    .and_then(Result::ok)
                    .ok_or_else(|| {
                        TransportError::Handshake("malformed HTTP/3 SETTINGS".to_owned())
                    })?;
                if settings.supports_webtransport() == 0 {
                    return Err(TransportError::Handshake(
                        "server does not support WebTransport".to_owned(),
                    ));
                }
                held.push(recv);
                held_types |= 1 << STREAM_CONTROL;
                break;
            }
            Some(kind @ (STREAM_QPACK_ENCODER | STREAM_QPACK_DECODER)) => {
                held.push(recv);
                held_types |= 1 << kind;
            }
            _ => {
                let _ = recv.stop(VarInt::from_u32(H3_STREAM_CREATION_ERROR));
            }
        }
    }
    let (mut send, mut recv) = conn
        .open_bi_wait()
        .await
        .map_err(|error| connection_error(error, true))?;
    let mut request = Vec::new();
    ConnectRequest::new(url)
        .with_protocols(protocols.iter().map(|protocol| (*protocol).to_owned()))
        .encode(&mut request)
        .map_err(|error| TransportError::Handshake(format!("WebTransport CONNECT: {error}")))?;
    write_all(&mut send, request).await?;
    let frame = read_frame(&mut recv).await?;
    let response = guarded(|| ConnectResponse::decode(&mut frame.as_slice()))
        .ok_or_else(|| TransportError::Handshake("malformed CONNECT response".to_owned()))?
        .map_err(|error| TransportError::Handshake(format!("WebTransport CONNECT: {error}")))?;
    let protocol = response
        .protocol
        .as_deref()
        .and_then(|selected| MOQ_ALPNS.iter().copied().find(|known| *known == selected))
        .ok_or_else(|| {
            TransportError::Handshake("server selected no MoQ subprotocol".to_owned())
        })?;
    let session_id = u64::from(send.id());
    let session = WtSession::new(session_id, control, send, held, held_types);
    Ok(MoqSession::new(RawSession::new(
        conn,
        protocol,
        Some((session, recv)),
    )))
}
