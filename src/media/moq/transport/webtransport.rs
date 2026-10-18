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

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use compio::buf::IntoInner;
use compio::quic::{Connection, ReadError, RecvStream, SendStream, VarInt};
use web_transport_proto::http::StatusCode;
use web_transport_proto::{Capsule, CapsuleError, ConnectRequest, ConnectResponse, Settings};

use super::session::{
    CloseCell, MoqSession, connection_error, publish_close, read_error, write_error,
};
use super::{Accepted, MOQ_ALPNS, TransportError, TransportMode};

/// HTTP/3 uni stream types (RFC 9114 §6.2, RFC 9204 §4.2) and the
/// WebTransport stream signals.
const STREAM_CONTROL: u64 = 0x00;
const STREAM_QPACK_ENCODER: u64 = 0x02;
const STREAM_QPACK_DECODER: u64 = 0x03;
const STREAM_WEBTRANSPORT_UNI: u64 = 0x54;
const FRAME_WEBTRANSPORT_BI: u64 = 0x41;
const FRAME_DATA: u64 = 0x00;

/// HTTP/3 error codes (RFC 9114 §8.1) and WebTransport's.
pub(super) const H3_NO_ERROR: u32 = 0x100;
pub(super) const H3_STREAM_CREATION_ERROR: u32 = 0x103;
pub(super) const H3_REQUEST_REJECTED: u32 = 0x10b;
pub(super) const H3_MESSAGE_ERROR: u32 = 0x10e;
pub(super) const WT_BUFFERED_STREAM_REJECTED: u32 = 0x3994_bd84;

/// Bound on one HTTP/3 frame (and on buffered capsule bytes) from a peer.
const MAX_FRAME_LEN: usize = 64 * 1024;
/// How long a closing session keeps QUIC open for its close capsule.
const CLOSE_GRACE: Duration = Duration::from_secs(1);
/// CLOSE_WEBTRANSPORT_SESSION reasons are at most 1024 bytes.
const MAX_CLOSE_REASON: usize = 1024;

/// Runs a decoder over peer bytes; `None` if it panicked.
fn guarded<T>(decode: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(decode)).ok()
}

fn varint_len(first: u8) -> usize {
    1 << (first >> 6)
}

fn decode_varint(bytes: &[u8]) -> u64 {
    bytes.iter().skip(1).fold(
        u64::from(bytes.first().map_or(0, |first| first & 0x3f)),
        |value, byte| (value << 8) | u64::from(*byte),
    )
}

/// Writes `value` (< 2^62) as a QUIC varint at the front of `out`; the
/// length written.
fn put_varint(value: u64, out: &mut [u8]) -> usize {
    let (len, tag) = match value {
        0..0x40 => (1, 0x00),
        0x40..0x4000 => (2, 0x40),
        0x4000..0x4000_0000 => (4, 0x80),
        _ => (8, 0xc0),
    };
    out[..len].copy_from_slice(&value.to_be_bytes()[8 - len..]);
    out[0] |= tag;
    len
}

fn push_varint(value: u64, out: &mut Vec<u8>) {
    let mut bytes = [0; 8];
    let len = put_varint(value, &mut bytes);
    out.extend_from_slice(&bytes[..len]);
}

/// The header of a WebTransport stream we open, until flow control has taken
/// all of it.
#[derive(Clone, Copy)]
pub(super) struct Header {
    bytes: [u8; 16],
    start: usize,
    end: usize,
}

impl Header {
    fn new(kind: u64, session_id: u64) -> Self {
        let mut bytes = [0; 16];
        let kind_len = put_varint(kind, &mut bytes);
        let end = kind_len + put_varint(session_id, &mut bytes[kind_len..]);
        Self {
            bytes,
            start: 0,
            end,
        }
    }

    pub(super) fn remaining(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }

    /// The header after `written` more bytes; `None` once all are written.
    pub(super) fn advance(mut self, written: usize) -> Option<Self> {
        self.start = (self.start + written).min(self.end);
        (self.start < self.end).then_some(self)
    }
}

/// Reads one QUIC varint off a stream, never past its last byte.
#[derive(Default)]
struct VarIntRead {
    bytes: [u8; 8],
    have: usize,
}

impl VarIntRead {
    /// `Ok(None)`: the stream ended first.
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        stream: &mut RecvStream,
    ) -> Poll<Result<Option<u64>, ReadError>> {
        loop {
            let need = if self.have == 0 {
                1
            } else {
                varint_len(self.bytes[0])
            };
            if self.have == need {
                self.have = 0;
                return Poll::Ready(Ok(Some(decode_varint(&self.bytes[..need]))));
            }
            let Some(chunk) =
                std::task::ready!(pin!(stream.read_chunk(need - self.have, true)).poll(cx))?
            else {
                return Poll::Ready(Ok(None));
            };
            let end = self.have + chunk.bytes.len();
            self.bytes[self.have..end].copy_from_slice(&chunk.bytes);
            self.have = end;
        }
    }
}

/// What a peer stream's header turned out to be.
enum Arrival {
    /// A WebTransport stream of this session.
    Data,
    /// An HTTP/3 control or QPACK stream (its type) to hold open.
    Hold(u64),
    /// Refused with this HTTP/3 code.
    Reject(u32),
    /// Ended or failed before its header did.
    Gone,
}

/// A peer stream whose header is still arriving.
struct Pending<S> {
    send: S,
    recv: RecvStream,
    read: VarIntRead,
    /// The first varint (uni stream type / bi frame type) once read.
    kind: Option<u64>,
}

impl<S> Pending<S> {
    fn new(send: S, recv: RecvStream) -> Self {
        Self {
            send,
            recv,
            read: VarIntRead::default(),
            kind: None,
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>, session_id: u64, uni: bool) -> Poll<Arrival> {
        loop {
            let value = match std::task::ready!(self.read.poll(cx, &mut self.recv)) {
                Ok(Some(value)) => value,
                Ok(None) | Err(_) => return Poll::Ready(Arrival::Gone),
            };
            if self.kind.is_some() {
                return Poll::Ready(if value == session_id {
                    Arrival::Data
                } else {
                    Arrival::Reject(WT_BUFFERED_STREAM_REJECTED)
                });
            }
            match (uni, value) {
                (true, STREAM_WEBTRANSPORT_UNI) | (false, FRAME_WEBTRANSPORT_BI) => {
                    self.kind = Some(value)
                }
                (true, STREAM_CONTROL | STREAM_QPACK_ENCODER | STREAM_QPACK_DECODER) => {
                    return Poll::Ready(Arrival::Hold(value));
                }
                (true, _) => return Poll::Ready(Arrival::Reject(H3_STREAM_CREATION_ERROR)),
                (false, _) => return Poll::Ready(Arrival::Reject(H3_REQUEST_REJECTED)),
            }
        }
    }
}

/// The WebTransport half of a session (see the module docs).
pub(super) struct WtSession {
    session_id: u64,
    /// Our HTTP/3 control stream: closing it is a connection error, so it
    /// stays open for the session's life.
    _control: SendStream,
    /// The CONNECT stream's send half, taken by the close.
    connect: RefCell<Option<SendStream>>,
    /// Peer HTTP/3 control and QPACK streams, held open unread.
    held: RefCell<Vec<RecvStream>>,
    /// Bit `1 << type` per held stream type: one of each is allowed.
    held_types: Cell<u8>,
    /// Peer streams whose header is still arriving. Shared by every session
    /// clone, so a stream one acceptor started reading is finished by the
    /// next (moq-net hands accepting from its handshake clone to its driver).
    pending_uni: RefCell<Vec<Pending<()>>>,
    pending_bi: RefCell<Vec<Pending<SendStream>>>,
    /// The datagram prefix: the quarter stream id as a varint.
    datagram_prefix: [u8; 8],
    datagram_prefix_len: usize,
}

impl WtSession {
    fn new(
        session_id: u64,
        control: SendStream,
        connect: SendStream,
        held: Vec<RecvStream>,
        held_types: u8,
    ) -> Self {
        let mut datagram_prefix = [0; 8];
        let datagram_prefix_len = put_varint(session_id / 4, &mut datagram_prefix);
        Self {
            session_id,
            _control: control,
            connect: RefCell::new(Some(connect)),
            held: RefCell::new(held),
            held_types: Cell::new(held_types),
            pending_uni: RefCell::default(),
            pending_bi: RefCell::default(),
            datagram_prefix,
            datagram_prefix_len,
        }
    }

    pub(super) fn uni_header(&self) -> Header {
        Header::new(STREAM_WEBTRANSPORT_UNI, self.session_id)
    }

    pub(super) fn bi_header(&self) -> Header {
        Header::new(FRAME_WEBTRANSPORT_BI, self.session_id)
    }

    fn hold(&self, kind: u64, mut recv: RecvStream) {
        let bit = 1 << kind;
        if self.held_types.get() & bit != 0 {
            let _ = recv.stop(VarInt::from_u32(H3_STREAM_CREATION_ERROR));
            return;
        }
        self.held_types.set(self.held_types.get() | bit);
        self.held.borrow_mut().push(recv);
    }

    pub(super) fn poll_accept_uni(
        &self,
        conn: &Connection,
        cx: &mut Context<'_>,
    ) -> Poll<Result<RecvStream, TransportError>> {
        let mut pending = self.pending_uni.borrow_mut();
        loop {
            match pin!(conn.accept_uni()).poll(cx) {
                Poll::Ready(Ok(recv)) => pending.push(Pending::new((), recv)),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(connection_error(error, true))),
                Poll::Pending => break,
            }
        }
        let mut index = 0;
        while index < pending.len() {
            let Poll::Ready(arrival) = pending[index].poll(cx, self.session_id, true) else {
                index += 1;
                continue;
            };
            let mut recv = pending.swap_remove(index).recv;
            match arrival {
                Arrival::Data => return Poll::Ready(Ok(recv)),
                Arrival::Hold(kind) => self.hold(kind, recv),
                Arrival::Reject(code) => {
                    let _ = recv.stop(VarInt::from_u32(code));
                }
                Arrival::Gone => {}
            }
        }
        Poll::Pending
    }

    pub(super) fn poll_accept_bi(
        &self,
        conn: &Connection,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(SendStream, RecvStream), TransportError>> {
        let mut pending = self.pending_bi.borrow_mut();
        loop {
            match pin!(conn.accept_bi()).poll(cx) {
                Poll::Ready(Ok((send, recv))) => pending.push(Pending::new(send, recv)),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(connection_error(error, true))),
                Poll::Pending => break,
            }
        }
        let mut index = 0;
        while index < pending.len() {
            let Poll::Ready(arrival) = pending[index].poll(cx, self.session_id, false) else {
                index += 1;
                continue;
            };
            let Pending {
                mut send, mut recv, ..
            } = pending.swap_remove(index);
            match arrival {
                Arrival::Data => return Poll::Ready(Ok((send, recv))),
                Arrival::Reject(code) => {
                    let _ = recv.stop(VarInt::from_u32(code));
                    let _ = send.reset(VarInt::from_u32(code));
                }
                Arrival::Hold(_) | Arrival::Gone => {}
            }
        }
        Poll::Pending
    }

    pub(super) fn datagram(&self, payload: &[u8]) -> Bytes {
        let mut datagram = BytesMut::with_capacity(self.datagram_prefix_len + payload.len());
        datagram.extend_from_slice(&self.datagram_prefix[..self.datagram_prefix_len]);
        datagram.extend_from_slice(payload);
        datagram.freeze()
    }

    /// The payload of a datagram for this session; `None` for another
    /// session's or a malformed one.
    pub(super) fn strip_datagram(&self, datagram: Bytes) -> Option<Bytes> {
        let len = varint_len(*datagram.first()?);
        let prefix = datagram.get(..len)?;
        (prefix == &self.datagram_prefix[..self.datagram_prefix_len]).then(|| datagram.slice(len..))
    }

    pub(super) fn datagram_prefix_len(&self) -> usize {
        self.datagram_prefix_len
    }

    /// Sends the close capsule and FIN on the CONNECT stream (best effort: a
    /// write the window cannot take now is dropped), then closes QUIC, after
    /// [`CLOSE_GRACE`] when `linger` and a Compio runtime is current.
    pub(super) fn close(&self, conn: &Connection, code: u32, reason: &str, linger: bool) {
        if let Some(connect) = self.connect.borrow_mut().take() {
            write_now(connect, &close_capsule(code, reason));
        }
        let no_error = VarInt::from_u32(H3_NO_ERROR);
        if linger {
            let lingering = conn.clone();
            let delayed = async move {
                compio::time::sleep(CLOSE_GRACE).await;
                lingering.close(no_error, b"");
            };
            if compio::runtime::Runtime::try_with_current(|runtime| runtime.spawn(delayed).detach())
                .is_ok()
            {
                return;
            }
        }
        conn.close(no_error, b"");
    }
}

/// CLOSE_WEBTRANSPORT_SESSION in an HTTP/3 DATA frame (RFC 9297 §3.2).
fn close_capsule(code: u32, reason: &str) -> Vec<u8> {
    let mut end = reason.len().min(MAX_CLOSE_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    let mut capsule = Vec::new();
    Capsule::CloseWebTransportSession {
        code,
        reason: reason[..end].to_owned(),
    }
    .encode(&mut capsule);
    let mut frame = Vec::with_capacity(capsule.len() + 4);
    push_varint(FRAME_DATA, &mut frame);
    push_varint(capsule.len() as u64, &mut frame);
    frame.extend_from_slice(&capsule);
    frame
}

/// Writes what the stream takes right now, then finishes it.
fn write_now(stream: SendStream, bytes: &[u8]) {
    let mut compat = stream.into_compat();
    let _ = pin!(compat.write_all(bytes)).poll(&mut Context::from_waker(Waker::noop()));
    let _ = compat.into_inner().finish();
}

/// Publishes the peer's close of the session: its CLOSE_WEBTRANSPORT_SESSION
/// capsule, or the CONNECT stream's end (code 0); then closes QUIC.
pub(super) fn watch_connect_stream(recv: RecvStream, conn: Connection, close: CloseCell) {
    compio::runtime::spawn(async move {
        let reason = read_peer_close(recv).await;
        publish_close(&close, reason);
        conn.close(VarInt::from_u32(H3_NO_ERROR), b"");
    })
    .detach();
}

async fn read_peer_close(mut recv: RecvStream) -> TransportError {
    let clean = || TransportError::Closed {
        code: 0,
        reason: String::new(),
    };
    let mut capsules = Vec::new();
    loop {
        let (kind, len) = match read_frame_head(&mut recv).await {
            Ok(Some(head)) => head,
            Ok(None) => return clean(),
            Err(TransportError::StreamReset(code)) => {
                return TransportError::Closed {
                    code,
                    reason: "WebTransport CONNECT stream reset".to_owned(),
                };
            }
            Err(error) => return error,
        };
        if kind != FRAME_DATA {
            if let Err(error) = skip(&mut recv, len).await {
                return error;
            }
            continue;
        }
        if capsules.len() + len > MAX_FRAME_LEN {
            return protocol_close("capsule data exceeds 64 KiB");
        }
        let start = capsules.len();
        capsules.resize(start + len, 0);
        if let Err(error) = read_exact(&mut recv, &mut capsules[start..]).await {
            return error;
        }
        loop {
            let mut rest = capsules.as_slice();
            match guarded(|| Capsule::decode(&mut rest)) {
                Some(Ok(Capsule::CloseWebTransportSession { code, reason })) => {
                    return TransportError::Closed { code, reason };
                }
                Some(Ok(_)) => {
                    let consumed = capsules.len() - rest.len();
                    capsules.drain(..consumed);
                }
                Some(Err(CapsuleError::UnexpectedEnd | CapsuleError::VarInt(_))) => break,
                Some(Err(_)) | None => return protocol_close("malformed capsule"),
            }
        }
    }
}

fn protocol_close(reason: &str) -> TransportError {
    TransportError::Closed {
        code: 0,
        reason: format!("WebTransport CONNECT stream: {reason}"),
    }
}

async fn read_varint(recv: &mut RecvStream) -> Result<Option<u64>, TransportError> {
    let mut read = VarIntRead::default();
    poll_fn(|cx| read.poll(cx, recv))
        .await
        .map_err(|error| read_error(error, true))
}

/// An HTTP/3 frame's type and payload length; `None` at a clean end.
async fn read_frame_head(recv: &mut RecvStream) -> Result<Option<(u64, usize)>, TransportError> {
    let Some(kind) = read_varint(recv).await? else {
        return Ok(None);
    };
    let len = read_varint(recv)
        .await?
        .ok_or_else(|| ended("an HTTP/3 frame"))?;
    let len = usize::try_from(len)
        .ok()
        .filter(|len| *len <= MAX_FRAME_LEN)
        .ok_or_else(|| {
            TransportError::Handshake(format!(
                "HTTP/3 frame of {len} bytes exceeds {MAX_FRAME_LEN}"
            ))
        })?;
    Ok(Some((kind, len)))
}

/// Reads one HTTP/3 frame, re-encoded whole (type, length, payload), past any
/// grease frames.
async fn read_frame(recv: &mut RecvStream) -> Result<Vec<u8>, TransportError> {
    loop {
        let (kind, len) = read_frame_head(recv)
            .await?
            .ok_or_else(|| ended("the HEADERS frame"))?;
        if kind >= 0x21 && (kind - 0x21) % 0x1f == 0 {
            skip(recv, len).await?;
            continue;
        }
        let mut frame = Vec::with_capacity(len + 16);
        push_varint(kind, &mut frame);
        push_varint(len as u64, &mut frame);
        let start = frame.len();
        frame.resize(start + len, 0);
        read_exact(recv, &mut frame[start..]).await?;
        return Ok(frame);
    }
}

async fn read_exact(recv: &mut RecvStream, dst: &mut [u8]) -> Result<(), TransportError> {
    let mut filled = 0;
    while filled < dst.len() {
        let chunk = recv
            .read_chunk(dst.len() - filled, true)
            .await
            .map_err(|error| read_error(error, true))?
            .ok_or_else(|| ended("an HTTP/3 frame"))?;
        dst[filled..filled + chunk.bytes.len()].copy_from_slice(&chunk.bytes);
        filled += chunk.bytes.len();
    }
    Ok(())
}

async fn skip(recv: &mut RecvStream, mut len: usize) -> Result<(), TransportError> {
    while len > 0 {
        let chunk = recv
            .read_chunk(len, true)
            .await
            .map_err(|error| read_error(error, true))?
            .ok_or_else(|| ended("an HTTP/3 frame"))?;
        len -= chunk.bytes.len();
    }
    Ok(())
}

fn ended(what: &str) -> TransportError {
    TransportError::Handshake(format!("stream ended inside {what}"))
}

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
        session: MoqSession::new(conn, protocol, Some((session, recv))),
        mode: TransportMode::WebTransport,
        path,
        query,
    })
}

/// The client half: SETTINGS both ways, then CONNECT `url` offering
/// `protocols`. Restream dials native QUIC only; tests use this to play the
/// browser.
#[cfg(test)]
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
    Ok(MoqSession::new(conn, protocol, Some((session, recv))))
}
