//! Progress budgets: a request body that arrives, or a response the client
//! takes, slower than a minimum rate ends after a grace period.
//!
//! A budget is time the transfer may spend blocked on the peer. It starts at
//! `grace`, drains only while the transfer waits on the peer (the handler
//! waits for the next body frame, or a write returns `Pending`), and refills
//! by bytes moved / `min_bytes_per_sec`, capped at `grace`. So an idle state
//! stream or a slow handler costs nothing, a peer that stops entirely gets
//! `grace`, and one that trickles bytes below the rate runs out; a plain idle
//! timer would be reset by every trickled byte. Credit is capped so a long
//! healthy connection cannot bank hours of it before its peer goes silent.

use std::future::Future as _;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Bytes;
use hyper::body::{Body as HttpBody, Frame, Incoming, SizeHint};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// The grace and refill rate of a progress budget.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub grace: Duration,
    pub min_bytes_per_sec: u32,
}

/// Apache `mod_reqtimeout`'s body defaults: 20 s, extended by 1 s per 500
/// bytes.
pub const PACE: Pace = Pace {
    grace: Duration::from_secs(20),
    min_bytes_per_sec: 500,
};

struct Budget {
    pace: Pace,
    credit: Duration,
    blocked_since: Option<Instant>,
    /// Boxed once, on the first wait, then reset.
    timer: Option<Pin<Box<Sleep>>>,
}

impl Budget {
    fn new(pace: Pace) -> Self {
        Self {
            pace,
            credit: pace.grace,
            blocked_since: None,
            timer: None,
        }
    }

    fn progressed(&mut self, bytes: usize) {
        if let Some(since) = self.blocked_since.take() {
            self.credit = self.credit.saturating_sub(since.elapsed());
        }
        let nanos = u64::try_from(bytes)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000_000)
            / u64::from(self.pace.min_bytes_per_sec.max(1));
        self.credit = self
            .credit
            .saturating_add(Duration::from_nanos(nanos))
            .min(self.pace.grace);
    }

    /// The transfer waits on the peer: `Ready` once the credit is spent.
    fn blocked(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.blocked_since.is_none() {
            let now = Instant::now();
            self.blocked_since = Some(now);
            let deadline = now + self.credit;
            match &mut self.timer {
                Some(timer) => timer.as_mut().reset(deadline),
                None => self.timer = Some(Box::pin(tokio::time::sleep_until(deadline))),
            }
        }
        match &mut self.timer {
            Some(timer) => timer.as_mut().poll(cx),
            None => Poll::Pending,
        }
    }
}

/// A request body under a progress budget.
pub struct PacedBody {
    inner: Incoming,
    budget: Budget,
}

impl PacedBody {
    pub fn new(inner: Incoming, pace: Pace) -> Self {
        Self {
            inner,
            budget: Budget::new(pace),
        }
    }
}

impl HttpBody for PacedBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                this.budget
                    .progressed(frame.data_ref().map_or(0, Bytes::len));
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(end) => {
                this.budget.progressed(0);
                Poll::Ready(end.map(|result| result.map_err(Into::into)))
            }
            Poll::Pending => match this.budget.blocked(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "request body arrived slower than the minimum rate",
                ))))),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// A connection whose writes are under a progress budget; reads pass through
/// (the header deadline and [`PacedBody`] bound them where a request waits).
pub struct PacedIo<S> {
    inner: S,
    writes: Budget,
}

impl<S> PacedIo<S> {
    pub fn new(inner: S, pace: Pace) -> Self {
        Self {
            inner,
            writes: Budget::new(pace),
        }
    }
}

impl<S> PacedIo<S> {
    fn wrote<T>(
        &mut self,
        cx: &mut Context<'_>,
        poll: Poll<io::Result<T>>,
        moved: impl FnOnce(&T) -> usize,
    ) -> Poll<io::Result<T>> {
        match poll {
            Poll::Ready(Ok(value)) => {
                self.writes.progressed(moved(&value));
                Poll::Ready(Ok(value))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => match self.writes.blocked(cx) {
                Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "client took the response slower than the minimum rate",
                ))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PacedIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PacedIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.wrote(cx, poll, |written| *written)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.wrote(cx, poll, |written| *written)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_flush(cx);
        this.wrote(cx, poll, |()| 0)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_shutdown(cx);
        this.wrote(cx, poll, |()| 0)
    }
}
