//! [`MoqSession`] and its streams: `web_transport_trait::poll` over one
//! compio-quic connection.
//!
//! Every compio-quic operation used here (`accept_*`, `open_*_wait`,
//! `recv_datagram`, `read_chunk`, `stopped`, `received_reset`, compat
//! `write`) is a stateless `poll_fn` wrapper, so each `poll_*` re-creates the
//! future and polls it once; nothing is boxed or retained between calls.
//!
//! `Connection::closed()` is never called from a droppable future: it takes
//! the connection worker's `JoinHandle`, and dropping that cancels the worker
//! (and a second caller panics). One detached task per connection awaits it
//! and publishes the reason into a [`CloseCell`] that `poll_closed` reads.
//! Because that task holds a `Connection` clone, the session closes the
//! connection itself when its last clone drops.

use std::cell::{Cell, RefCell};
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker, ready};

use bytes::Bytes;
use compio::buf::IntoInner;
use compio::quic::{
    Connection, ConnectionError, ConnectionStats, ReadError, RecvStream, ResetError,
    SendDatagramError, SendStream, StoppedError, VarInt, WriteError,
};
use web_transport_proto::{error_from_http3, error_to_http3};

use super::TransportError;
use super::webtransport::{Header, WtSession};

/// How the session ended, shared with the tasks that observe it.
#[derive(Default)]
pub(super) struct CloseState {
    reason: Option<TransportError>,
    wakers: Vec<Waker>,
}

pub(super) type CloseCell = Rc<RefCell<CloseState>>;

/// Records the first reason the session ended and wakes every `poll_closed`
/// caller; later reasons are ignored.
pub(super) fn publish_close(cell: &CloseCell, reason: TransportError) {
    let wakers = {
        let mut state = cell.borrow_mut();
        if state.reason.is_some() {
            return;
        }
        state.reason = Some(reason);
        std::mem::take(&mut state.wakers)
    };
    wakers.into_iter().for_each(Waker::wake);
}

fn close_reason(cell: &CloseCell) -> Option<TransportError> {
    cell.borrow().reason.clone()
}

fn poll_close(cell: &CloseCell, cx: &mut Context<'_>) -> Poll<TransportError> {
    let mut state = cell.borrow_mut();
    if let Some(reason) = &state.reason {
        return Poll::Ready(reason.clone());
    }
    if !state.wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
        state.wakers.push(cx.waker().clone());
    }
    Poll::Pending
}

/// An application code from the wire, in moq-net's `u32` space: WebTransport
/// codes are mapped out of the HTTP/3 range (anything else reads as 0).
pub(super) fn wire_code(code: VarInt, wt: bool) -> u32 {
    let code = code.into_inner();
    if wt {
        error_from_http3(code).unwrap_or(0)
    } else {
        u32::try_from(code).unwrap_or(u32::MAX)
    }
}

/// The inverse of [`wire_code`] for codes we send.
pub(super) fn stream_code(code: u32, wt: bool) -> VarInt {
    if wt {
        // `error_to_http3` stays below 2^62, so this never falls back.
        VarInt::from_u64(error_to_http3(code)).unwrap_or(VarInt::from_u32(0))
    } else {
        VarInt::from_u32(code)
    }
}

pub(super) fn connection_error(error: ConnectionError, wt: bool) -> TransportError {
    match error {
        ConnectionError::ApplicationClosed(close) => TransportError::Closed {
            code: wire_code(close.error_code, wt),
            reason: String::from_utf8_lossy(&close.reason).into_owned(),
        },
        other => TransportError::Connection(other),
    }
}

pub(super) fn write_error(error: WriteError, wt: bool) -> TransportError {
    match error {
        WriteError::Stopped(code) => TransportError::StreamStopped(wire_code(code, wt)),
        WriteError::ConnectionLost(error) => connection_error(error, wt),
        _ => TransportError::StreamClosed,
    }
}

pub(super) fn read_error(error: ReadError, wt: bool) -> TransportError {
    match error {
        ReadError::Reset(code) => TransportError::StreamReset(wire_code(code, wt)),
        ReadError::ConnectionLost(error) => connection_error(error, wt),
        _ => TransportError::StreamClosed,
    }
}

struct Shared {
    conn: Connection,
    protocol: &'static str,
    wt: Option<WtSession>,
    close: CloseCell,
    closing: Cell<bool>,
}

impl Shared {
    /// Idempotent. `linger` lets a WebTransport close capsule reach the peer
    /// before the QUIC connection closes; a drop closes at once.
    fn close(&self, code: u32, reason: &str, linger: bool) {
        if self.closing.replace(true) {
            return;
        }
        match &self.wt {
            None => self.conn.close(VarInt::from_u32(code), reason.as_bytes()),
            Some(wt) => wt.close(&self.conn, code, reason, linger),
        }
        publish_close(
            &self.close,
            TransportError::Connection(ConnectionError::LocallyClosed),
        );
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.close(0, "", false);
    }
}

/// One MoQ transport session: a native QUIC connection or one WebTransport
/// session. Clones share the connection; the last drop closes it.
#[derive(Clone)]
pub struct MoqSession {
    shared: Rc<Shared>,
}

impl MoqSession {
    /// Wraps an established connection speaking `protocol`. `wt` carries the
    /// WebTransport session state and the CONNECT stream's receive half.
    /// Spawns the detached close observers, so it must run on a Compio
    /// runtime.
    pub(super) fn new(
        conn: Connection,
        protocol: &'static str,
        wt: Option<(WtSession, RecvStream)>,
    ) -> Self {
        let close = CloseCell::default();
        let is_wt = wt.is_some();
        let observed = conn.clone();
        let observer = close.clone();
        compio::runtime::spawn(async move {
            let error = observed.closed().await;
            publish_close(&observer, connection_error(error, is_wt));
        })
        .detach();
        let wt = wt.map(|(session, connect_recv)| {
            super::webtransport::watch_connect_stream(connect_recv, conn.clone(), close.clone());
            session
        });
        Self {
            shared: Rc::new(Shared {
                conn,
                protocol,
                wt,
                close,
                closing: Cell::new(false),
            }),
        }
    }

    fn is_wt(&self) -> bool {
        self.shared.wt.is_some()
    }

    fn closed_reason(&self) -> Option<TransportError> {
        close_reason(&self.shared.close)
    }

    /// The underlying connection, for tests that speak raw QUIC beside the
    /// session.
    #[cfg(test)]
    pub(super) fn connection(&self) -> &Connection {
        &self.shared.conn
    }
}

impl web_transport_trait::poll::Session for MoqSession {
    type SendStream = MoqSendStream;
    type RecvStream = MoqRecvStream;
    type Error = TransportError;

    fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<MoqRecvStream, TransportError>> {
        if let Some(reason) = self.closed_reason() {
            return Poll::Ready(Err(reason));
        }
        let shared = &*self.shared;
        match &shared.wt {
            None => pin!(shared.conn.accept_uni())
                .poll(cx)
                .map(|accepted| accepted.map(|stream| MoqRecvStream::new(stream, false)))
                .map_err(|error| connection_error(error, false)),
            Some(wt) => wt
                .poll_accept_uni(&shared.conn, cx)
                .map_ok(|stream| MoqRecvStream::new(stream, true)),
        }
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(MoqSendStream, MoqRecvStream), TransportError>> {
        if let Some(reason) = self.closed_reason() {
            return Poll::Ready(Err(reason));
        }
        let shared = &*self.shared;
        let (send, recv) = match &shared.wt {
            None => ready!(pin!(shared.conn.accept_bi()).poll(cx))
                .map_err(|error| connection_error(error, false))?,
            Some(wt) => ready!(wt.poll_accept_bi(&shared.conn, cx))?,
        };
        let wt = self.is_wt();
        Poll::Ready(Ok((
            MoqSendStream::new(send, None, wt),
            MoqRecvStream::new(recv, wt),
        )))
    }

    fn poll_open_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<MoqSendStream, TransportError>> {
        if let Some(reason) = self.closed_reason() {
            return Poll::Ready(Err(reason));
        }
        let wt = self.is_wt();
        let stream = ready!(pin!(self.shared.conn.open_uni_wait()).poll(cx))
            .map_err(|error| connection_error(error, wt))?;
        let header = self.shared.wt.as_ref().map(WtSession::uni_header);
        Poll::Ready(MoqSendStream::opened(stream, header, wt, cx))
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(MoqSendStream, MoqRecvStream), TransportError>> {
        if let Some(reason) = self.closed_reason() {
            return Poll::Ready(Err(reason));
        }
        let wt = self.is_wt();
        let (send, recv) = ready!(pin!(self.shared.conn.open_bi_wait()).poll(cx))
            .map_err(|error| connection_error(error, wt))?;
        let header = self.shared.wt.as_ref().map(WtSession::bi_header);
        let send = MoqSendStream::opened(send, header, wt, cx)?;
        Poll::Ready(Ok((send, MoqRecvStream::new(recv, wt))))
    }

    /// Never pends: compio-quic drops the oldest queued datagram when full,
    /// and a datagram over `max_datagram_size` is dropped as the trait allows.
    fn poll_send_datagram(
        &mut self,
        _cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), TransportError>> {
        if let Some(reason) = self.closed_reason() {
            return Poll::Ready(Err(reason));
        }
        let shared = &*self.shared;
        let datagram = match &shared.wt {
            None => Bytes::copy_from_slice(payload),
            Some(wt) => wt.datagram(payload),
        };
        Poll::Ready(match shared.conn.send_datagram(datagram) {
            Ok(()) | Err(SendDatagramError::TooLarge) => Ok(()),
            Err(SendDatagramError::ConnectionLost(error)) => {
                Err(connection_error(error, self.is_wt()))
            }
            Err(error) => Err(TransportError::Datagram(error)),
        })
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, TransportError>> {
        if let Some(reason) = self.closed_reason() {
            return Poll::Ready(Err(reason));
        }
        let shared = &*self.shared;
        loop {
            let datagram = ready!(pin!(shared.conn.recv_datagram()).poll(cx))
                .map_err(|error| connection_error(error, self.is_wt()))?;
            match &shared.wt {
                None => return Poll::Ready(Ok(datagram)),
                // Another session's (or a malformed) datagram is dropped.
                Some(wt) => {
                    if let Some(payload) = wt.strip_datagram(datagram) {
                        return Poll::Ready(Ok(payload));
                    }
                }
            }
        }
    }

    fn max_datagram_size(&self) -> usize {
        let max = self.shared.conn.max_datagram_size().unwrap_or(0);
        match &self.shared.wt {
            None => max,
            Some(wt) => max.saturating_sub(wt.datagram_prefix_len()),
        }
    }

    fn protocol(&self) -> Option<&str> {
        Some(self.shared.protocol)
    }

    fn close(&mut self, code: u32, reason: &str) {
        self.shared.close(code, reason, true);
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<TransportError> {
        poll_close(&self.shared.close, cx)
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        MoqStats(self.shared.conn.stats())
    }
}

/// compio-quic's connection counters in the trait's shape.
struct MoqStats(ConnectionStats);

impl web_transport_trait::Stats for MoqStats {
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.0.udp_tx.bytes)
    }

    fn bytes_received(&self) -> Option<u64> {
        Some(self.0.udp_rx.bytes)
    }

    fn bytes_lost(&self) -> Option<u64> {
        Some(self.0.path.lost_bytes)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.0.path.sent_packets)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.0.udp_rx.datagrams)
    }

    fn packets_lost(&self) -> Option<u64> {
        Some(self.0.path.lost_packets)
    }

    fn rtt(&self) -> Option<std::time::Duration> {
        Some(self.0.path.rtt)
    }
}

/// An outgoing stream. A WebTransport stream first sends its header (stream
/// type + session id), ahead of any caller byte; `poll_write` only reports
/// caller bytes. Dropped unfinished, it is reset (code 0).
pub struct MoqSendStream {
    /// `None` only inside [`Self::poll_raw_write`], which borrows the stream
    /// as compio's slice-writing compat type.
    stream: Option<SendStream>,
    header: Option<Header>,
    wt: bool,
    done: bool,
}

impl MoqSendStream {
    pub(super) fn new(stream: SendStream, header: Option<Header>, wt: bool) -> Self {
        Self {
            stream: Some(stream),
            header,
            wt,
            done: false,
        }
    }

    /// A stream we opened: its header is queued at once so the peer sees the
    /// stream even before the first caller write. A header flow control
    /// cannot take yet stays pending (the next write sends it first).
    fn opened(
        stream: SendStream,
        header: Option<Header>,
        wt: bool,
        cx: &mut Context<'_>,
    ) -> Result<Self, TransportError> {
        let mut stream = Self::new(stream, header, wt);
        if let Poll::Ready(Err(error)) = stream.poll_header(cx) {
            return Err(error);
        }
        Ok(stream)
    }

    fn poll_raw_write(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, TransportError>> {
        let Some(stream) = self.stream.take() else {
            return Poll::Ready(Err(TransportError::StreamClosed));
        };
        let mut compat = stream.into_compat();
        let polled = pin!(compat.write(buf)).poll(cx);
        self.stream = Some(compat.into_inner());
        polled.map_err(|error| write_error(error, self.wt))
    }

    fn poll_header(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), TransportError>> {
        while let Some(header) = self.header {
            let written = ready!(self.poll_raw_write(cx, header.remaining()))?;
            if written == 0 {
                return Poll::Ready(Err(TransportError::StreamClosed));
            }
            self.header = header.advance(written);
        }
        Poll::Ready(Ok(()))
    }
}

impl web_transport_trait::poll::SendStream for MoqSendStream {
    type Error = TransportError;

    fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, TransportError>> {
        if self.done {
            return Poll::Ready(Err(TransportError::StreamClosed));
        }
        ready!(self.poll_header(cx))?;
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        self.poll_raw_write(cx, buf)
    }

    fn set_priority(&mut self, order: u8) {
        if let Some(stream) = &self.stream {
            let _ = stream.set_priority(i32::from(order));
        }
    }

    fn finish(&mut self) -> Result<(), TransportError> {
        if self.done {
            return Err(TransportError::StreamClosed);
        }
        if self.header.is_some() {
            match self.poll_header(&mut Context::from_waker(Waker::noop())) {
                Poll::Ready(result) => result?,
                Poll::Pending => return Err(TransportError::HeaderBlocked),
            }
        }
        let stream = self.stream.as_mut().ok_or(TransportError::StreamClosed)?;
        stream.finish().map_err(|_| TransportError::StreamClosed)?;
        self.done = true;
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        if let Some(stream) = &mut self.stream {
            let _ = stream.reset(stream_code(code, self.wt));
        }
        self.header = None;
        self.done = true;
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), TransportError>> {
        let Some(stream) = &mut self.stream else {
            return Poll::Ready(Ok(()));
        };
        Poll::Ready(match ready!(pin!(stream.stopped()).poll(cx)) {
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(TransportError::StreamStopped(wire_code(code, self.wt))),
            Err(StoppedError::ConnectionLost(error)) => Err(connection_error(error, self.wt)),
            Err(_) => Err(TransportError::StreamClosed),
        })
    }
}

impl Drop for MoqSendStream {
    fn drop(&mut self) {
        if !self.done {
            web_transport_trait::poll::SendStream::reset(self, 0);
        }
    }
}

/// An incoming stream (a WebTransport stream's header already consumed).
/// Dropped before its end, it is stopped (code 0).
pub struct MoqRecvStream {
    stream: RecvStream,
    wt: bool,
}

impl MoqRecvStream {
    pub(super) fn new(stream: RecvStream, wt: bool) -> Self {
        Self { stream, wt }
    }
}

impl web_transport_trait::poll::RecvStream for MoqRecvStream {
    type Error = TransportError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, TransportError>> {
        if dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }
        let chunk = ready!(pin!(self.stream.read_chunk(dst.len(), true)).poll(cx))
            .map_err(|error| read_error(error, self.wt))?;
        Poll::Ready(Ok(chunk.map(|chunk| {
            let len = chunk.bytes.len();
            dst[..len].copy_from_slice(&chunk.bytes);
            len
        })))
    }

    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, TransportError>> {
        if max == 0 {
            return Poll::Ready(Ok(Some(Bytes::new())));
        }
        let chunk = ready!(pin!(self.stream.read_chunk(max, true)).poll(cx))
            .map_err(|error| read_error(error, self.wt))?;
        Poll::Ready(Ok(chunk.map(|chunk| chunk.bytes)))
    }

    fn stop(&mut self, code: u32) {
        let _ = self.stream.stop(stream_code(code, self.wt));
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), TransportError>> {
        Poll::Ready(match ready!(pin!(self.stream.received_reset()).poll(cx)) {
            Ok(None) => Ok(()),
            Ok(Some(code)) => Err(TransportError::StreamReset(wire_code(code, self.wt))),
            Err(ResetError::ConnectionLost(error)) => Err(connection_error(error, self.wt)),
            Err(_) => Err(TransportError::StreamClosed),
        })
    }
}

impl Drop for MoqRecvStream {
    fn drop(&mut self) {
        // compio-quic stops an unread stream with raw code 0 on drop; a
        // WebTransport peer needs it in the session's HTTP/3 code range.
        if self.wt {
            let _ = self.stream.stop(stream_code(0, true));
        }
    }
}
