//! One RTMP client connection's socket and the vocabulary its Compio owner
//! and the Tokio control session exchange. A leaf: handshake, ingest and
//! play all import it, and it imports none of them.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::snapshots::PublisherQuality;
use restream_core::MediaPacket;

use super::ingest_media::{RtmpMediaEvent, RtmpPublisherMedia};

/// Spare capacity reserved for each ingest socket read: the size of the
/// separate read buffer this replaced.
const INGEST_READ_BYTES: usize = 4096;

pub(super) struct RtmpClientSocket {
    stream: compio::net::TcpStream,
    shutdown: CancellationToken,
}

impl RtmpClientSocket {
    pub(super) fn new(stream: compio::net::TcpStream, shutdown: CancellationToken) -> Self {
        Self { stream, shutdown }
    }

    pub(super) fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    pub(super) fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.stream)
    }

    pub(super) async fn read(&mut self, buffer: Vec<u8>) -> io::Result<(usize, Vec<u8>)> {
        use compio::io::AsyncRead;
        let compio::BufResult(result, buffer) = self.stream.read(buffer).await;
        result.map(|count| (count, buffer))
    }

    /// Read at most `INGEST_READ_BYTES` after `buffer`'s current bytes,
    /// keeping them (`read` overwrites from the start). The cap keeps one read
    /// from landing more than the parser budget checks per step, however far
    /// the buffer's capacity has grown.
    ///
    /// Takes `&self`: a read may stay in flight while this connection writes
    /// (io_uring allows one read and one write on a socket at once), which the
    /// play loop relies on to keep handling client commands during playback.
    pub(super) async fn append(&self, mut buffer: BytesMut) -> io::Result<(usize, BytesMut)> {
        use compio::buf::{IntoInner, IoBuf};
        use compio::io::AsyncRead;
        buffer.reserve(INGEST_READ_BYTES);
        let start = buffer.len();
        let mut stream = &self.stream;
        let compio::BufResult(result, slice) = stream
            .read(buffer.slice(start..start.saturating_add(INGEST_READ_BYTES)))
            .await;
        let buffer = slice.into_inner();
        result.map(|count| (count, buffer))
    }

    pub(super) async fn write_all(&self, buffer: Vec<u8>) -> io::Result<()> {
        use compio::io::AsyncWriteExt;
        let mut stream = &self.stream;
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "RTMP listener shutting down",
            )),
            compio::BufResult(result, _buffer) = stream.write_all(buffer) => result,
        }
    }
}

pub(super) enum RtmpControlCommand {
    PublishRequested {
        stream_key: String,
        client_ip: String,
        client_addr: String,
        reply: oneshot::Sender<PublishAuthorization>,
    },
    PublishAccepted {
        client_ip: String,
    },
    /// The publisher's one-time stream probe, for ingest metadata.
    MediaProbe(RtmpMediaEvent),
    /// What the encoder declared in `onMetaData` (sent again when it
    /// changes).
    EncoderDeclared(restream_core::metadata::DeclaredEncoder),
    PlayRequested {
        stream_key: String,
        client_ip: String,
        reply: oneshot::Sender<PlayAuthorization>,
    },
    PlayStart {
        pipeline_id: String,
        reply: oneshot::Sender<(Option<Bytes>, Option<Bytes>)>,
    },
    PlayNext {
        reply: oneshot::Sender<Option<Vec<Arc<MediaPacket>>>>,
        /// The previous burst's vector, emptied by the Compio side, so the
        /// control session refills it instead of allocating per burst.
        recycled: Vec<Arc<MediaPacket>>,
    },
    Cancel,
    Quality(Box<PublisherQuality>),
    Finish {
        phase: String,
        reason: String,
        had_error: bool,
        reply: oneshot::Sender<()>,
    },
}

pub(super) enum PublishAuthorization {
    /// The publisher's media state, which the owner runs from here on.
    Accepted(Box<RtmpPublisherMedia>),
    Cancelled,
    Rejected {
        code: &'static str,
        description: &'static str,
        error: &'static str,
    },
}

pub(super) enum PlayAuthorization {
    Accepted {
        pipeline_id: String,
    },
    Rejected {
        code: &'static str,
        description: &'static str,
        error: &'static str,
    },
}
