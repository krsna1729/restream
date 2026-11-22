//! RTMP connection ownership and the Tokio-side ingest control plane. Publish
//! media runs to completion on the RTMP ingress owner (`ingest_media`); Tokio
//! sees authorization, registration, stream probes, quality and finish.

use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, RawFd};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::engine::MediaEngine;
use crate::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode};
use crate::ingest_lease::{IngestEnd, IngestLease};
use crate::input_gate::InputTimestampMapper;
use crate::secret_display::redact_secret;
use crate::security::IngestSecurityService;
use crate::snapshots::PublisherQuality;
use crate::tcp_stats::collect_tcp_stats_by_fd;
use restream_core::MediaPacket;
use restream_sync::standby_gop::StandbyGopCache;
use restream_sync::{MEDIA_PULL_BURST_PACKETS, Reader};

use super::handshake::perform_server_handshake;
use super::ingest_media::{RtmpMediaEvent, RtmpPublisherMedia};
use super::play::{PlayAuthorization, RtmpPlayRequest, handle_play_request};

/// Spare capacity reserved for each ingest socket read: the size of the
/// separate read buffer this replaced.
const INGEST_READ_BYTES: usize = 4096;

use restream_rtmp::parser_budget;
#[path = "ingest/client.rs"]
mod client;
pub(super) use client::handle_rtmp_client;
#[path = "ingest/control.rs"]
mod control;
pub(super) use control::run_rtmp_control_session;
#[path = "ingest/session.rs"]
mod session;
use session::handle_session_results;
pub(super) struct RtmpClientSocket {
    stream: compio::net::TcpStream,
    shutdown: CancellationToken,
}

impl RtmpClientSocket {
    pub(super) fn new(stream: compio::net::TcpStream, shutdown: CancellationToken) -> Self {
        Self { stream, shutdown }
    }

    fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
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

async fn send_control_command(
    commands: &mpsc::Sender<RtmpControlCommand>,
    shutdown: &CancellationToken,
    command: RtmpControlCommand,
) -> Result<(), &'static str> {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err("RTMP listener shutting down"),
        result = commands.send(command) => {
            result.map_err(|_| "RTMP control session closed")
        }
    }
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

/// Record a publisher's one-time stream probe as ingest metadata; the audio
/// tracks land before the metadata that opens an output's start gate.
async fn record_media_probe(engine: &MediaEngine, active: &IngestLease, event: RtmpMediaEvent) {
    match event {
        RtmpMediaEvent::Video(meta) => {
            info!(
                "[rtmp] Probed video: {} {}x{} profile={:?} level={:?}",
                meta.codec, meta.width, meta.height, meta.profile, meta.level
            );
            engine
                .update_ingest_session_meta(
                    active.pipeline_id(),
                    active.registration(),
                    Some(meta),
                    None,
                    None,
                )
                .await;
        }
        RtmpMediaEvent::Audio(meta) => {
            info!(
                "[rtmp] Probed audio: {} {}Hz {}ch",
                meta.codec, meta.sample_rate, meta.channels
            );
            engine
                .update_ingest_session_audio_tracks(
                    active.pipeline_id(),
                    active.registration(),
                    vec![meta.clone()],
                )
                .await;
            engine
                .update_ingest_session_meta(
                    active.pipeline_id(),
                    active.registration(),
                    None,
                    Some(meta),
                    None,
                )
                .await;
        }
    }
    engine
        .note_ingest_session_probed(active.registration())
        .await;
}

async fn authorize_play(
    pipeline_access: &dyn PipelineAccessAuthenticator,
    engine: &MediaEngine,
    client_ip: &str,
    stream_key: &str,
) -> PlayAuthorization {
    match pipeline_access
        .authenticate(PipelineAccessMode::RtmpPlay, stream_key, client_ip)
        .await
    {
        Ok(pipeline) => {
            if engine
                .ingests
                .active
                .read()
                .await
                .contains_key(&pipeline.id)
            {
                PlayAuthorization::Accepted {
                    pipeline_id: pipeline.id,
                }
            } else {
                PlayAuthorization::Rejected {
                    code: "NetStream.Play.StreamNotFound",
                    description: "No active ingest",
                    error: "No active ingest for play",
                }
            }
        }
        Err(PipelineAccessError::InvalidStreamKey) => PlayAuthorization::Rejected {
            code: "NetStream.Play.StreamNotFound",
            description: "Invalid stream key",
            error: "Invalid stream key for play",
        },
        Err(PipelineAccessError::InputUnreadable) => PlayAuthorization::Rejected {
            code: "NetStream.Play.StreamNotFound",
            description: "Input unavailable",
            error: "Input configuration unreadable for play",
        },
    }
}

fn set_tcp_socket_buffers(socket: std::os::fd::BorrowedFd<'_>, size: usize) {
    use rustix::net::sockopt;
    if sockopt::set_socket_recv_buffer_size(socket, size).is_err() {
        warn!("failed to set RTMP receive socket buffer");
    }
    if sockopt::set_socket_send_buffer_size(socket, size).is_err() {
        warn!("failed to set RTMP send socket buffer");
    }
}

const PARSER_BUDGET_EXHAUSTED: &str = "RTMP ingest parser budget exhausted";
const PREAUTH_DEADLINE_PASSED: &str = "RTMP client did not publish before the admission deadline";

/// Media payload bytes in parsed results not yet published to the ring.
/// Operator-facing reason: an oversized declared message is distinct from
/// malformed input.
fn session_error_reason(error: &rml_rtmp::sessions::ServerSessionError) -> &'static str {
    match error {
        rml_rtmp::sessions::ServerSessionError::ChunkDeserializationError(
            rml_rtmp::chunk_io::ChunkDeserializationError::MessageTooLarge { .. },
        ) => "RTMP message exceeds the maximum message size",
        _ => "Session parse error",
    }
}

fn sample_publisher_quality(
    fd: RawFd,
    previous_tcp_bytes: &mut Option<(u64, Instant)>,
    now: Instant,
) -> PublisherQuality {
    match collect_tcp_stats_by_fd(fd) {
        Ok(stats) => {
            let receive_rate = stats.tcp_bytes_received.and_then(|bytes| {
                let rate = (*previous_tcp_bytes).and_then(|(previous, sampled_at)| {
                    crate::tcp_stats::bytes_delta_rate_mbps(
                        bytes,
                        previous,
                        now.duration_since(sampled_at).as_secs_f64(),
                    )
                });
                *previous_tcp_bytes = Some((bytes, now));
                rate
            });
            PublisherQuality {
                tcp_congestion_algorithm: stats.tcp_congestion_algorithm,
                tcp_rtt_ms: stats.tcp_rtt_ms,
                tcp_rtt_var_ms: stats.tcp_rtt_var_ms,
                tcp_bytes_received: stats.tcp_bytes_received,
                tcp_last_rcv_ms: stats.tcp_last_rcv_ms,
                tcp_rcv_rtt_ms: stats.tcp_rcv_rtt_ms,
                tcp_rcv_space: stats.tcp_rcv_space,
                tcp_rcv_ooopack: stats.tcp_rcv_ooopack,
                tcp_skmem_rmem_alloc: stats.tcp_skmem_rmem_alloc,
                tcp_skmem_rmem_max: stats.tcp_skmem_rmem_max,
                tcp_receive_rate_mbps: receive_rate,
                ..PublisherQuality::default()
            }
        }
        Err(error) => PublisherQuality {
            tcp_stats_unavailable_reason: Some(
                match error.kind() {
                    io::ErrorKind::Unsupported => "not_linux",
                    _ => "collection_failed",
                }
                .to_string(),
            ),
            ..PublisherQuality::default()
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn read_rtmp_input_or_quality(
    socket: &mut RtmpClientSocket,
    buffer: BytesMut,
    shutdown: &CancellationToken,
    stats_tick: &mut Pin<Box<dyn Future<Output = ()>>>,
    publishing: bool,
    fd: RawFd,
    previous_tcp_bytes: &mut Option<(u64, Instant)>,
    commands: &mpsc::Sender<RtmpControlCommand>,
) -> Option<io::Result<(usize, BytesMut)>> {
    let read_future = socket.append(buffer);
    tokio::pin!(read_future);
    loop {
        tokio::select! {
            result = &mut read_future => return Some(result),
            _ = stats_tick.as_mut(), if publishing => {
                let quality =
                    sample_publisher_quality(fd, previous_tcp_bytes, Instant::now());
                let _ = commands.try_send(RtmpControlCommand::Quality(Box::new(quality)));
                *stats_tick = Box::pin(compio::time::sleep(Duration::from_secs(2)));
            }
            _ = shutdown.cancelled() => return None,
        }
    }
}

/// Test-only fault points. A connection from a marked peer address unwinds
/// right after the handshake, as a parser bug would; an owner-marked one
/// unwinds the owner thread itself when that connection ends, outside the
/// per-connection boundary. Both prove the listener contains them.
/// `resume_unwind` skips the panic hook, so logs stay quiet.
#[cfg(test)]
pub(super) mod injected_panics {
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    static MARKED: Mutex<Option<HashSet<SocketAddr>>> = Mutex::new(None);
    static OWNER_MARKED: Mutex<Option<HashSet<SocketAddr>>> = Mutex::new(None);

    pub(in crate::rtmp) fn mark(peer: SocketAddr) {
        insert(&MARKED, peer);
    }

    pub(in crate::rtmp) fn mark_owner(peer: SocketAddr) {
        insert(&OWNER_MARKED, peer);
    }

    pub(super) fn unwind_if_marked(peer: SocketAddr) {
        if take(&MARKED, peer) {
            std::panic::resume_unwind(Box::new("injected RTMP connection panic"));
        }
    }

    pub(in crate::rtmp) fn owner_unwind_if_marked(peer: SocketAddr) {
        if take(&OWNER_MARKED, peer) {
            std::panic::resume_unwind(Box::new("injected RTMP owner panic"));
        }
    }

    fn insert(set: &Mutex<Option<HashSet<SocketAddr>>>, peer: SocketAddr) {
        restream_sync::lock::lock(set)
            .get_or_insert_with(HashSet::new)
            .insert(peer);
    }

    fn take(set: &Mutex<Option<HashSet<SocketAddr>>>, peer: SocketAddr) -> bool {
        restream_sync::lock::lock(set)
            .as_mut()
            .is_some_and(|set| set.remove(&peer))
    }
}
