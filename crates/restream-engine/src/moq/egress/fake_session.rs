//! A transport session for leaf lifecycle tests: implements the same poll
//! traits as `transport::MoqSession`, negotiates `moq-lite-03` (no SETUP
//! stream, so the moq-lite handshake completes without a peer), never
//! yields a stream, and records how it was closed. The peer side can close
//! it to model a relay going away.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use web_transport_trait::poll::{BiStreams, RecvStream, SendStream, Session};

#[derive(Debug, Clone)]
pub(crate) struct FakeError(Option<(u32, String)>);

impl fmt::Display for FakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fake session closed: {:?}", self.0)
    }
}

impl std::error::Error for FakeError {}

impl web_transport_trait::Error for FakeError {
    fn session_error(&self) -> Option<(u32, String)> {
        self.0.clone()
    }
}

/// A stream the fake never produces.
pub(crate) enum NoStream {}

impl SendStream for NoStream {
    type Error = FakeError;

    fn poll_write(&mut self, _: &mut Context<'_>, _: &[u8]) -> Poll<Result<usize, FakeError>> {
        match *self {}
    }

    fn set_priority(&mut self, _: u8) {
        match *self {}
    }

    fn finish(&mut self) -> Result<(), FakeError> {
        match *self {}
    }

    fn reset(&mut self, _: u32) {
        match *self {}
    }

    fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), FakeError>> {
        match *self {}
    }
}

impl RecvStream for NoStream {
    type Error = FakeError;

    fn poll_read(
        &mut self,
        _: &mut Context<'_>,
        _: &mut [u8],
    ) -> Poll<Result<Option<usize>, FakeError>> {
        match *self {}
    }

    fn stop(&mut self, _: u32) {
        match *self {}
    }

    fn poll_closed(&mut self, _: &mut Context<'_>) -> Poll<Result<(), FakeError>> {
        match *self {}
    }
}

struct FakeStats;

impl web_transport_trait::Stats for FakeStats {}

#[derive(Default)]
struct State {
    /// The first close: (code, reason, closed by the peer).
    closed: Option<(u32, String, bool)>,
    wakers: Vec<Waker>,
}

#[derive(Clone, Default)]
pub(crate) struct FakeSession {
    state: Rc<RefCell<State>>,
}

impl FakeSession {
    /// The relay closes the connection.
    pub(crate) fn close_from_peer(&self, code: u32) {
        self.close_as(code, "peer closed", true);
    }

    /// How the session was closed: (code, closed by the peer).
    pub(crate) fn closed(&self) -> Option<(u32, bool)> {
        self.state
            .borrow()
            .closed
            .as_ref()
            .map(|(code, _, peer)| (*code, *peer))
    }

    fn close_as(&self, code: u32, reason: &str, peer: bool) {
        let mut state = self.state.borrow_mut();
        if state.closed.is_none() {
            state.closed = Some((code, reason.to_string(), peer));
        }
        for waker in state.wakers.drain(..) {
            waker.wake();
        }
    }

    fn wait<T>(&self, cx: &mut Context<'_>) -> Poll<Result<T, FakeError>> {
        let mut state = self.state.borrow_mut();
        match &state.closed {
            Some((code, reason, _)) => Poll::Ready(Err(FakeError(Some((*code, reason.clone()))))),
            None => {
                state.wakers.push(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl Session for FakeSession {
    type SendStream = NoStream;
    type RecvStream = NoStream;
    type Error = FakeError;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<NoStream, FakeError>> {
        self.wait(cx)
    }

    fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<Self>, FakeError>> {
        self.wait(cx)
    }

    fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<NoStream, FakeError>> {
        self.wait(cx)
    }

    fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<BiStreams<Self>, FakeError>> {
        self.wait(cx)
    }

    fn poll_send_datagram(&mut self, _: &mut Context<'_>, _: &[u8]) -> Poll<Result<(), FakeError>> {
        Poll::Ready(Ok(()))
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, FakeError>> {
        self.wait(cx)
    }

    fn max_datagram_size(&self) -> usize {
        1200
    }

    fn protocol(&self) -> Option<&str> {
        Some("moq-lite-03")
    }

    fn close(&mut self, code: u32, reason: &str) {
        self.close_as(code, reason, false);
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<FakeError> {
        match self.wait::<()>(cx) {
            Poll::Ready(Err(error)) => Poll::Ready(error),
            Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
        }
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        FakeStats
    }
}
