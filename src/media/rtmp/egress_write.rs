use std::collections::VecDeque;
use std::{cmp, io};

use bytes::Bytes;
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RtmpWriteAdvanceError {
    BeyondPending { written: usize, pending: usize },
}

#[derive(Debug, Default)]
pub(crate) struct RtmpWriteQueue {
    chunks: VecDeque<Bytes>,
    front_offset: usize,
    pending_bytes: usize,
}

impl RtmpWriteQueue {
    pub(crate) fn push(&mut self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }
        self.pending_bytes = self.pending_bytes.saturating_add(bytes.len());
        self.chunks.push_back(bytes);
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

pub(crate) async fn write_rtmp_pending_bytes<S>(
    socket: &mut S,
    queue: &mut RtmpWriteQueue,
    bytes: Bytes,
) -> io::Result<usize>
where
    S: AsyncWrite + Unpin,
{
    let pending_before = queue.pending_bytes();
    queue.push(bytes);
    let total = queue.pending_bytes().saturating_sub(pending_before);

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

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

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

    #[test]
    fn rtmp_write_queue_advances_across_packet_boundaries() {
        let mut queue = RtmpWriteQueue::default();
        queue.push(Bytes::from_static(b"abc"));
        queue.push(Bytes::from_static(b"de"));

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

        queue.push(Bytes::new());

        assert!(queue.is_empty());
        assert_eq!(queue.front_chunk(), None);
    }

    #[test]
    fn rtmp_write_queue_rejects_advancing_past_pending_bytes() {
        let mut queue = RtmpWriteQueue::default();
        queue.push(Bytes::from_static(b"abc"));

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

    #[tokio::test]
    async fn rtmp_pending_write_preserves_bytes_across_partial_socket_writes() {
        let mut writer = PartialWriter::with_max_write(2);
        let mut queue = RtmpWriteQueue::default();

        let written =
            write_rtmp_pending_bytes(&mut writer, &mut queue, Bytes::from_static(b"abcde"))
                .await
                .unwrap();

        assert_eq!(written, 5);
        assert_eq!(writer.bytes, b"abcde");
    }

    #[tokio::test]
    async fn rtmp_pending_write_rejects_zero_byte_socket_writes() {
        let mut writer = PartialWriter::with_max_write(0);
        let mut queue = RtmpWriteQueue::default();

        let error = write_rtmp_pending_bytes(&mut writer, &mut queue, Bytes::from_static(b"abc"))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        assert!(writer.bytes.is_empty());
        assert_eq!(queue.pending_bytes(), 3);
    }
}
