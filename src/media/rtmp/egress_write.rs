use std::collections::VecDeque;
use std::{cmp, io};

use bytes::Bytes;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::media::egress::policy::WorkBudget;

use super::egress_transport::RtmpEgressStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RtmpWriteAdvanceError {
    BeyondPending { written: usize, pending: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RtmpWriteQueueError {
    PendingCapacityExceeded {
        pending: usize,
        additional: usize,
        max_pending: usize,
    },
}

#[derive(Debug)]
pub(crate) struct RtmpWriteQueue {
    chunks: VecDeque<Bytes>,
    front_offset: usize,
    pending_bytes: usize,
    max_pending_bytes: usize,
}

impl RtmpWriteQueue {
    pub(crate) fn new(max_pending_bytes: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            front_offset: 0,
            pending_bytes: 0,
            max_pending_bytes,
        }
    }

    pub(crate) fn try_push(&mut self, bytes: Bytes) -> Result<(), RtmpWriteQueueError> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > self.max_pending_bytes.saturating_sub(self.pending_bytes) {
            return Err(RtmpWriteQueueError::PendingCapacityExceeded {
                pending: self.pending_bytes,
                additional: bytes.len(),
                max_pending: self.max_pending_bytes,
            });
        }
        self.pending_bytes += bytes.len();
        self.chunks.push_back(bytes);
        Ok(())
    }

    pub(crate) fn front_chunk(&self) -> Option<&[u8]> {
        self.chunks.front().map(|chunk| &chunk[self.front_offset..])
    }

    pub(crate) fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pending_bytes == 0
    }

    pub(crate) fn advance(&mut self, written: usize) -> Result<(), RtmpWriteAdvanceError> {
        if written > self.pending_bytes {
            return Err(RtmpWriteAdvanceError::BeyondPending {
                written,
                pending: self.pending_bytes,
            });
        }

        let mut remaining = written;
        self.pending_bytes -= written;
        while remaining > 0 {
            let Some(front) = self.chunks.front() else {
                break;
            };
            let front_remaining = front.len() - self.front_offset;
            if remaining < front_remaining {
                self.front_offset += remaining;
                return Ok(());
            }

            remaining -= front_remaining;
            self.chunks.pop_front();
            self.front_offset = 0;
        }

        Ok(())
    }
}

impl Default for RtmpWriteQueue {
    fn default() -> Self {
        Self::new(usize::MAX)
    }
}

pub(crate) trait RtmpWriteTransport {
    fn try_write(&mut self, bytes: &[u8]) -> io::Result<usize>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RtmpPendingDrain {
    Drained { bytes: usize, units: usize },
    Blocked { bytes: usize, units: usize },
    Yield { bytes: usize, units: usize },
}

pub(crate) fn drain_rtmp_pending_bytes<T>(
    transport: &mut T,
    queue: &mut RtmpWriteQueue,
    budget: WorkBudget,
) -> io::Result<RtmpPendingDrain>
where
    T: RtmpWriteTransport,
{
    let mut bytes = 0;
    let mut units = 0;

    while !queue.is_empty() {
        if budget.is_exhausted(units, bytes) {
            return Ok(RtmpPendingDrain::Yield { bytes, units });
        }

        let Some(chunk) = queue.front_chunk() else {
            break;
        };
        let allowed = chunk.len().min(budget.remaining_bytes(bytes));
        if allowed == 0 {
            return Ok(RtmpPendingDrain::Yield { bytes, units });
        }

        match transport.try_write(&chunk[..allowed]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "RTMP transport accepted zero bytes",
                ));
            }
            Ok(written) => {
                if written > allowed {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "RTMP transport wrote more bytes than it was offered",
                    ));
                }
                queue
                    .advance(written)
                    .map_err(|error| io::Error::other(format!("{error:?}")))?;
                bytes += written;
                units += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(RtmpPendingDrain::Blocked { bytes, units });
            }
            Err(error) => return Err(error),
        }
    }

    Ok(RtmpPendingDrain::Drained { bytes, units })
}

pub(super) async fn write_rtmp_pending_bytes(
    socket: &mut RtmpEgressStream,
    queue: &mut RtmpWriteQueue,
    bytes: Bytes,
) -> io::Result<usize> {
    let pending_before = queue.pending_bytes();
    queue.try_push(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("RTMP pending write limit reached: {error:?}"),
        )
    })?;
    let total = queue.pending_bytes().saturating_sub(pending_before);

    match socket {
        RtmpEgressStream::Plain(stream) => {
            while !queue.is_empty() {
                match drain_rtmp_pending_bytes(
                    stream,
                    queue,
                    WorkBudget::new(usize::MAX, usize::MAX, std::time::Duration::from_secs(1)),
                )? {
                    RtmpPendingDrain::Drained { .. } => break,
                    RtmpPendingDrain::Blocked { .. } => stream.writable().await?,
                    RtmpPendingDrain::Yield { .. } => tokio::task::yield_now().await,
                }
            }
        }
        RtmpEgressStream::Tls(_) => drain_rtmp_pending_bytes_async(socket, queue).await?,
    }

    Ok(total)
}

impl RtmpWriteTransport for TcpStream {
    fn try_write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        TcpStream::try_write(self, bytes)
    }
}

async fn drain_rtmp_pending_bytes_async<S>(
    socket: &mut S,
    queue: &mut RtmpWriteQueue,
) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    while !queue.is_empty() {
        let Some(chunk) = queue.front_chunk() else {
            break;
        };
        let chunk_len = chunk.len();
        let written = socket.write(chunk).await?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "RTMP socket accepted zero bytes",
            ));
        }
        queue
            .advance(cmp::min(written, chunk_len))
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    #[derive(Default)]
    struct PartialWriter {
        bytes: Vec<u8>,
        max_write: usize,
    }

    impl PartialWriter {
        fn with_max_write(max_write: usize) -> Self {
            Self {
                bytes: Vec::new(),
                max_write,
            }
        }
    }

    impl AsyncWrite for PartialWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            let written = buf.len().min(self.max_write);
            self.bytes.extend_from_slice(&buf[..written]);
            Poll::Ready(Ok(written))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Default)]
    struct ReadyWriter {
        bytes: Vec<u8>,
    }

    impl RtmpWriteTransport for ReadyWriter {
        fn try_write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    #[derive(Default)]
    struct BlockingWriter;

    impl RtmpWriteTransport for BlockingWriter {
        fn try_write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }
    }

    #[test]
    fn rtmp_write_queue_advances_across_packet_boundaries() {
        let mut queue = RtmpWriteQueue::default();
        queue.try_push(Bytes::from_static(b"abc")).unwrap();
        queue.try_push(Bytes::from_static(b"de")).unwrap();

        assert_eq!(queue.pending_bytes(), 5);
        assert_eq!(queue.front_chunk(), Some(&b"abc"[..]));

        queue.advance(2).unwrap();
        assert_eq!(queue.pending_bytes(), 3);
        assert_eq!(queue.front_chunk(), Some(&b"c"[..]));

        queue.advance(1).unwrap();
        assert_eq!(queue.pending_bytes(), 2);
        assert_eq!(queue.front_chunk(), Some(&b"de"[..]));

        queue.advance(2).unwrap();
        assert!(queue.is_empty());
        assert_eq!(queue.front_chunk(), None);
    }

    #[test]
    fn rtmp_write_queue_ignores_empty_chunks() {
        let mut queue = RtmpWriteQueue::default();

        queue.try_push(Bytes::new()).unwrap();

        assert!(queue.is_empty());
        assert_eq!(queue.front_chunk(), None);
    }

    #[test]
    fn rtmp_write_queue_rejects_advancing_past_pending_bytes() {
        let mut queue = RtmpWriteQueue::default();
        queue.try_push(Bytes::from_static(b"abc")).unwrap();

        let error = queue.advance(4).unwrap_err();

        assert_eq!(
            error,
            RtmpWriteAdvanceError::BeyondPending {
                written: 4,
                pending: 3
            }
        );
        assert_eq!(queue.pending_bytes(), 3);
        assert_eq!(queue.front_chunk(), Some(&b"abc"[..]));
    }

    #[test]
    fn rtmp_write_queue_rejects_packet_that_exceeds_pending_capacity() {
        let mut queue = RtmpWriteQueue::new(4);
        queue.try_push(Bytes::from_static(b"abc")).unwrap();

        let error = queue.try_push(Bytes::from_static(b"de")).unwrap_err();

        assert_eq!(
            error,
            RtmpWriteQueueError::PendingCapacityExceeded {
                pending: 3,
                additional: 2,
                max_pending: 4,
            }
        );
        assert_eq!(queue.pending_bytes(), 3);
        assert_eq!(queue.front_chunk(), Some(&b"abc"[..]));
    }

    #[test]
    fn rtmp_pending_drain_yields_when_byte_budget_is_exhausted() {
        let mut writer = ReadyWriter::default();
        let mut queue = RtmpWriteQueue::default();
        queue.try_push(Bytes::from_static(b"abc")).unwrap();
        queue.try_push(Bytes::from_static(b"def")).unwrap();

        let outcome = drain_rtmp_pending_bytes(
            &mut writer,
            &mut queue,
            WorkBudget::new(4, 4, Duration::from_secs(1)),
        )
        .unwrap();

        assert_eq!(outcome, RtmpPendingDrain::Yield { bytes: 4, units: 2 });
        assert_eq!(writer.bytes, b"abcd");
        assert_eq!(queue.front_chunk(), Some(&b"ef"[..]));
    }

    #[test]
    fn rtmp_pending_drain_preserves_queue_when_transport_blocks() {
        let mut writer = BlockingWriter;
        let mut queue = RtmpWriteQueue::default();
        queue.try_push(Bytes::from_static(b"abc")).unwrap();

        let outcome = drain_rtmp_pending_bytes(
            &mut writer,
            &mut queue,
            WorkBudget::new(1, 3, Duration::from_secs(1)),
        )
        .unwrap();

        assert_eq!(outcome, RtmpPendingDrain::Blocked { bytes: 0, units: 0 });
        assert_eq!(queue.front_chunk(), Some(&b"abc"[..]));
    }

    #[tokio::test]
    async fn rtmp_pending_write_preserves_bytes_across_partial_socket_writes() {
        let mut writer = PartialWriter::with_max_write(2);
        let mut queue = RtmpWriteQueue::default();

        queue.try_push(Bytes::from_static(b"abcde")).unwrap();
        drain_rtmp_pending_bytes_async(&mut writer, &mut queue)
            .await
            .unwrap();

        assert_eq!(writer.bytes, b"abcde");
        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn rtmp_pending_write_rejects_zero_byte_socket_writes() {
        let mut writer = PartialWriter::with_max_write(0);
        let mut queue = RtmpWriteQueue::default();
        queue.try_push(Bytes::from_static(b"abc")).unwrap();

        let error = drain_rtmp_pending_bytes_async(&mut writer, &mut queue)
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert!(writer.bytes.is_empty());
        assert_eq!(queue.pending_bytes(), 3);
    }

    #[tokio::test]
    async fn rtmp_pending_write_rejects_new_bytes_when_capacity_is_full() {
        let mut queue = RtmpWriteQueue::new(3);
        queue.try_push(Bytes::from_static(b"abc")).unwrap();

        let error = queue.try_push(Bytes::from_static(b"d")).unwrap_err();

        assert_eq!(
            error,
            RtmpWriteQueueError::PendingCapacityExceeded {
                pending: 3,
                additional: 1,
                max_pending: 3,
            }
        );
        assert_eq!(queue.pending_bytes(), 3);
        assert_eq!(queue.front_chunk(), Some(&b"abc"[..]));
    }
}
