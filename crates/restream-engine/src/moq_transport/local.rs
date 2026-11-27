//! A checked adapter from the Compio transport's `!Send` session to
//! moq-net's native `Send + Sync` transport bounds.
//!
//! The underlying [`RawSession`] is thread-affine: its `Rc` state must only
//! be touched on the Compio thread that created it. `SendWrapper` supplies
//! the `Send + Sync` implementations moq-net requires, and [`ThreadBound`]
//! checks the owner thread before every access. A wrong-thread call returns
//! [`TransportError::WrongThread`] instead of unwinding through a driver
//! poll. Dropping a wrapper on the wrong thread leaks the raw value rather
//! than panicking; that is an internal invariant failure and is never
//! expected in normal operation.

use std::mem;
use std::task::{Context, Poll};

use bytes::{Buf, BufMut, Bytes};
use send_wrapper::SendWrapper;
use web_transport_trait::Stats;
use web_transport_trait::poll::BiStreams;

use super::TransportError;
use super::session::{MoqStats, RawRecvStream, RawSendStream, RawSession};

/// A value that may move between threads but is only accessed on the thread
/// that created it.
struct ThreadBound<T> {
    inner: Option<SendWrapper<T>>,
}

impl<T> ThreadBound<T> {
    fn new(inner: T) -> Self {
        Self {
            inner: Some(SendWrapper::new(inner)),
        }
    }

    fn get(&self) -> Result<&T, TransportError> {
        self.inner
            .as_ref()
            .filter(|inner| inner.valid())
            .map(|inner| &**inner)
            .ok_or(TransportError::WrongThread)
    }

    fn get_mut(&mut self) -> Result<&mut T, TransportError> {
        self.inner
            .as_mut()
            .filter(|inner| inner.valid())
            .map(|inner| &mut **inner)
            .ok_or(TransportError::WrongThread)
    }
}

impl<T: Clone> Clone for ThreadBound<T> {
    fn clone(&self) -> Self {
        match self.get() {
            Ok(inner) => Self::new(inner.clone()),
            // An off-thread clone cannot name a new owner. Keep the empty
            // state so every later access reports WrongThread.
            Err(_) => Self { inner: None },
        }
    }
}

impl<T> Drop for ThreadBound<T> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            if inner.valid() {
                drop(inner);
            } else {
                // `SendWrapper` would panic here. A leak is safer for the
                // engine than unwinding during transport teardown.
                mem::forget(inner);
            }
        }
    }
}

/// moq-net's view of one Compio MoQ session.
#[derive(Clone)]
pub struct LocalSession {
    inner: ThreadBound<RawSession>,
}

impl LocalSession {
    pub(super) fn new(inner: RawSession) -> Self {
        Self {
            inner: ThreadBound::new(inner),
        }
    }

    fn raw(&self) -> Result<&RawSession, TransportError> {
        self.inner.get()
    }

    fn raw_mut(&mut self) -> Result<&mut RawSession, TransportError> {
        self.inner.get_mut()
    }

    /// The underlying connection, for tests that speak raw QUIC beside the
    /// session.
    #[cfg(test)]
    pub(super) fn connection(&self) -> &compio::quic::Connection {
        self.raw()
            .expect("test access to a MoQ session on its owner thread")
            .connection()
    }
}

impl web_transport_trait::poll::Session for LocalSession {
    type SendStream = LocalSendStream;
    type RecvStream = LocalRecvStream;
    type Error = TransportError;

    fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_accept_uni(cx).map_ok(LocalRecvStream::new),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<BiStreams<Self>, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw
                .poll_accept_bi(cx)
                .map_ok(|(send, recv)| (LocalSendStream::new(send), LocalRecvStream::new(recv))),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_open_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_open_uni(cx).map_ok(LocalSendStream::new),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<BiStreams<Self>, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw
                .poll_open_bi(cx)
                .map_ok(|(send, recv)| (LocalSendStream::new(send), LocalRecvStream::new(recv))),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_send_datagram(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_send_datagram(cx, payload),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_recv_datagram(cx),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn max_datagram_size(&self) -> usize {
        self.raw().map_or(0, RawSession::max_datagram_size)
    }

    fn protocol(&self) -> Option<&str> {
        self.raw().ok().and_then(RawSession::protocol)
    }

    fn close(&mut self, code: u32, reason: &str) {
        if let Ok(raw) = self.raw_mut() {
            raw.close(code, reason);
        }
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<TransportError> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_closed(cx),
            Err(error) => Poll::Ready(error),
        }
    }

    fn stats(&self) -> impl Stats {
        LocalStats(self.raw().ok().map(RawSession::stats_snapshot))
    }
}

/// `Session::stats` has no error channel. An off-thread call reports no
/// metrics rather than touching the connection.
struct LocalStats(Option<MoqStats>);

impl Stats for LocalStats {
    fn bytes_sent(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::bytes_sent)
    }

    fn bytes_received(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::bytes_received)
    }

    fn bytes_lost(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::bytes_lost)
    }

    fn packets_sent(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::packets_sent)
    }

    fn packets_received(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::packets_received)
    }

    fn packets_lost(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::packets_lost)
    }

    fn rtt(&self) -> Option<std::time::Duration> {
        self.0.as_ref().and_then(MoqStats::rtt)
    }

    fn estimated_send_rate(&self) -> Option<u64> {
        self.0.as_ref().and_then(MoqStats::estimated_send_rate)
    }
}

/// moq-net's view of one outgoing Compio MoQ stream.
pub struct LocalSendStream {
    inner: ThreadBound<RawSendStream>,
}

impl LocalSendStream {
    fn new(inner: RawSendStream) -> Self {
        Self {
            inner: ThreadBound::new(inner),
        }
    }

    fn raw_mut(&mut self) -> Result<&mut RawSendStream, TransportError> {
        self.inner.get_mut()
    }
}

impl web_transport_trait::poll::SendStream for LocalSendStream {
    type Error = TransportError;

    fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_write(cx, buf),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_write_buf(cx, buf),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn set_priority(&mut self, order: u8) {
        if let Ok(raw) = self.raw_mut() {
            raw.set_priority(order);
        }
    }

    fn finish(&mut self) -> Result<(), TransportError> {
        self.raw_mut()?.finish()
    }

    fn reset(&mut self, code: u32) {
        if let Ok(raw) = self.raw_mut() {
            raw.reset(code);
        }
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_closed(cx),
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

/// moq-net's view of one incoming Compio MoQ stream.
pub struct LocalRecvStream {
    inner: ThreadBound<RawRecvStream>,
}

impl LocalRecvStream {
    fn new(inner: RawRecvStream) -> Self {
        Self {
            inner: ThreadBound::new(inner),
        }
    }

    fn raw_mut(&mut self) -> Result<&mut RawRecvStream, TransportError> {
        self.inner.get_mut()
    }
}

impl web_transport_trait::poll::RecvStream for LocalRecvStream {
    type Error = TransportError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_read(cx, dst),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_read_buf<B: BufMut>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<Option<usize>, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_read_buf(cx, buf),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_read_chunk(cx, max),
            Err(error) => Poll::Ready(Err(error)),
        }
    }

    fn stop(&mut self, code: u32) {
        if let Ok(raw) = self.raw_mut() {
            raw.stop(code);
        }
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), TransportError>> {
        match self.raw_mut() {
            Ok(raw) => raw.poll_closed(cx),
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn local_transport_meets_moq_net_bounds() {
        fn assert_boxable<S: moq_net::transport::poll::Boxable>() {}
        assert_boxable::<LocalSession>();
        assert_send_sync::<LocalSession>();
        assert_send_sync::<LocalSendStream>();
        assert_send_sync::<LocalRecvStream>();
    }

    #[test]
    fn owner_thread_can_access_the_value() {
        let bound = ThreadBound::new(Rc::new(7));
        assert_eq!(**bound.get().expect("owner access"), 7);
    }

    #[test]
    fn off_thread_access_returns_an_error_and_does_not_panic() {
        let bound = ThreadBound::new(Rc::new(7));
        std::thread::spawn(move || {
            assert!(matches!(bound.get(), Err(TransportError::WrongThread)));
            // Dropping the wrapper here must leak, not unwind.
        })
        .join()
        .expect("off-thread access thread");
    }

    #[test]
    fn off_thread_clone_is_empty() {
        let bound = ThreadBound::new(Rc::new(7));
        let cloned = std::thread::spawn(move || {
            let clone = bound.clone();
            assert!(matches!(clone.get(), Err(TransportError::WrongThread)));
        })
        .join();
        assert!(cloned.is_ok());
    }
}
