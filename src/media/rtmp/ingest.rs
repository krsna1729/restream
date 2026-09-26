//! RTMP connection ownership and Tokio-side ingest control plane.

use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, RawFd};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ingest_auth::{
    PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode,
};
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
use crate::media::security::IngestSecurityService;
use crate::media::snapshots::PublisherQuality;
use crate::media::stage_metrics::StageMetrics;
use crate::media::standby_gop::StandbyGopCache;
use crate::media::tcp_stats::collect_tcp_stats_by_fd;
use crate::secret_display::redact_secret;

use super::flv::{
    FlvVideoPacketKind, classify_flv_video_packet, flv_avcc_config_annexb_parameter_sets,
    flv_video_composition_time_ms, parse_flv_audio_meta, parse_flv_video_meta,
};
use super::handshake::perform_server_handshake;
use super::ingest_packets::try_promote_cached_rtmp;
use super::play::{PlayAuthorization, RtmpPlayRequest, handle_play_request};

/// Spare capacity reserved for each ingest socket read: the size of the
/// separate read buffer this replaced.
const INGEST_READ_BYTES: usize = 4096;

#[path = "ingest/parser_budget.rs"]
pub(super) mod parser_budget;
#[path = "ingest/session.rs"]
mod session;
use session::{ProbeState, handle_session_results, process_audio_data, process_video_data};
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

    pub(super) async fn read(&mut self, buffer: Vec<u8>) -> io::Result<(usize, Vec<u8>)> {
        use compio::io::AsyncRead;
        let compio::BufResult(result, buffer) = self.stream.read(buffer).await;
        result.map(|count| (count, buffer))
    }

    /// Read into the spare capacity after `buffer`'s current bytes, keeping
    /// them (`read` overwrites from the start).
    async fn append(&mut self, buffer: BytesMut) -> io::Result<(usize, BytesMut)> {
        use compio::io::AsyncReadExt;
        let compio::BufResult(result, buffer) = self.stream.append(buffer).await;
        result.map(|count| (count, buffer))
    }

    pub(super) async fn write_all(&mut self, buffer: Vec<u8>) -> io::Result<()> {
        use compio::io::AsyncWriteExt;
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "RTMP listener shutting down",
            )),
            compio::BufResult(result, _buffer) = self.stream.write_all(buffer) => result,
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
    VideoData {
        data: Bytes,
        timestamp: u32,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
    AudioData {
        data: Bytes,
        timestamp: u32,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
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

pub(super) async fn handoff_media_data(
    commands: &mpsc::Sender<RtmpControlCommand>,
    media_handoff: &Arc<tokio::sync::Semaphore>,
    shutdown: &CancellationToken,
    media_type: MediaType,
    data: Bytes,
    timestamp: u32,
) -> Result<(), &'static str> {
    let permits = u32::try_from(data.len()).map_err(|_| "RTMP media message too large")?;
    let permit = tokio::select! {
        _ = shutdown.cancelled() => return Err("RTMP listener shutting down"),
        result = media_handoff.clone().acquire_many_owned(permits) => {
            result.map_err(|_| "RTMP media handoff closed")?
        }
    };
    let command = match media_type {
        MediaType::Video => RtmpControlCommand::VideoData {
            data,
            timestamp,
            permit,
        },
        MediaType::Audio => RtmpControlCommand::AudioData {
            data,
            timestamp,
            permit,
        },
    };
    tokio::select! {
        _ = shutdown.cancelled() => Err("RTMP listener shutting down"),
        result = commands.send(command) => {
            result.map_err(|_| "RTMP control session closed")
        }
    }
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
    Accepted,
    Cancelled,
    Rejected {
        code: &'static str,
        description: &'static str,
        error: &'static str,
    },
}

pub(super) async fn run_rtmp_control_session(
    mut commands: mpsc::Receiver<RtmpControlCommand>,
    shutdown: CancellationToken,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) {
    let mut active_ingest: Option<RtmpIngestHandle> = None;
    let mut probe = ProbeState {
        video_done: false,
        audio_done: false,
    };
    let mut playback: Option<(String, Reader, Vec<Arc<MediaPacket>>)> = None;
    let mut disconnect = None;
    let mut finish_reply = None;
    let mut deferred_command = None;

    'actor: loop {
        let command = if let Some(command) = deferred_command.take() {
            Some(command)
        } else {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => break 'actor,
                command = commands.recv() => command,
            }
        };
        let Some(command) = command else {
            break;
        };
        match command {
            RtmpControlCommand::PublishRequested {
                stream_key,
                client_ip,
                client_addr,
                reply,
            } => {
                let decision = authorize_publish(
                    pipeline_access.as_ref(),
                    &security,
                    &engine,
                    &shutdown,
                    PublishContext {
                        client_ip: &client_ip,
                        client_addr: &client_addr,
                        stream_key: &stream_key,
                    },
                    &mut active_ingest,
                )
                .await;
                let cancelled = matches!(&decision, PublishAuthorization::Cancelled);
                let _ = reply.send(decision);
                if cancelled {
                    break 'actor;
                }
            }
            RtmpControlCommand::PublishAccepted { client_ip } => {
                security.record_success(&client_ip);
            }
            RtmpControlCommand::VideoData {
                data,
                timestamp,
                permit: _permit,
            } => {
                if let Some(active) = active_ingest.as_mut() {
                    tokio::select! {
                        _ = shutdown.cancelled() => break 'actor,
                        _ = process_video_data(&engine, active, &mut probe, data, timestamp) => {}
                    }
                }
            }
            RtmpControlCommand::AudioData {
                data,
                timestamp,
                permit: _permit,
            } => {
                if let Some(active) = active_ingest.as_mut() {
                    tokio::select! {
                        _ = shutdown.cancelled() => break 'actor,
                        _ = process_audio_data(&engine, active, &mut probe, data, timestamp) => {}
                    }
                }
            }
            RtmpControlCommand::PlayRequested {
                stream_key,
                client_ip,
                reply,
            } => {
                tokio::select! {
                    _ = shutdown.cancelled() => break 'actor,
                    authorization = authorize_play(
                        pipeline_access.as_ref(),
                        &engine,
                        &client_ip,
                        &stream_key,
                    ) => {
                        let _ = reply.send(authorization);
                    }
                }
            }
            RtmpControlCommand::PlayStart { pipeline_id, reply } => {
                let state = tokio::select! {
                    _ = shutdown.cancelled() => break 'actor,
                    state = async {
                        let headers = engine.get_sequence_headers(&pipeline_id).await;
                        let ring = engine.get_or_create_pipeline(&pipeline_id).await;
                        (headers, ring)
                    } => state,
                };
                playback = Some((
                    pipeline_id,
                    Reader::new("rtmp_play".to_string(), state.1),
                    Vec::with_capacity(MEDIA_PULL_BURST_PACKETS),
                ));
                let _ = reply.send(state.0);
            }
            RtmpControlCommand::PlayNext { reply } => {
                let Some((pipeline_id, reader, burst)) = playback.as_mut() else {
                    let _ = reply.send(None);
                    continue;
                };
                burst.clear();
                let mut cancelled = false;
                'pull: loop {
                    match reader.pull_burst(burst, MEDIA_PULL_BURST_PACKETS) {
                        Ok(0) => {
                            tokio::select! {
                                _ = shutdown.cancelled() => {
                                    cancelled = true;
                                    break 'pull;
                                }
                                _ = reader.wait_for_data() => {}
                                command = commands.recv() => {
                                    match command {
                                        Some(RtmpControlCommand::Cancel) | None => {
                                            cancelled = true;
                                            break 'pull;
                                        }
                                        Some(command) => {
                                            deferred_command = Some(command);
                                            cancelled = true;
                                            break 'pull;
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => continue,
                        Ok(_) => break,
                    }
                }
                let packets = if cancelled || commands.is_closed() {
                    None
                } else {
                    info!(pipeline = %pipeline_id, "RTMP play media burst pulled");
                    Some(std::mem::take(burst))
                };
                let finished = packets.is_none();
                let _ = reply.send(packets);
                if finished {
                    playback = None;
                }
            }
            RtmpControlCommand::Cancel => {
                playback = None;
            }
            RtmpControlCommand::Quality(quality) => {
                if let Some(active) = active_ingest.as_ref() {
                    tokio::select! {
                        _ = shutdown.cancelled() => break 'actor,
                        _ = engine.update_ingest_session_quality(&active.registration, *quality) => {}
                    }
                }
            }
            RtmpControlCommand::Finish {
                phase,
                reason,
                had_error,
                reply,
            } => {
                disconnect = Some((phase, reason, had_error));
                finish_reply = Some(reply);
                break;
            }
        }
    }

    if let Some(active) = &active_ingest {
        let (phase, reason, had_error) = disconnect.unwrap_or_else(|| {
            if shutdown.is_cancelled() {
                (
                    "shutdown".to_string(),
                    "RTMP listener shutting down".to_string(),
                    false,
                )
            } else {
                (
                    "disconnect".to_string(),
                    "publisher disconnected".to_string(),
                    false,
                )
            }
        });
        info!(pipeline = %active.pipeline_id, "[rtmp] Publisher disconnected");
        engine
            .record_ingest_disconnect_if_current(
                &active.pipeline_id,
                &active.registration,
                Some(&phase),
                Some(reason),
                had_error,
            )
            .await;
        engine
            .unregister_ingest_if_current(&active.pipeline_id, &active.registration)
            .await;
    }
    if let Some(reply) = finish_reply {
        let _ = reply.send(());
    }
}

struct PublishContext<'a> {
    client_ip: &'a str,
    client_addr: &'a str,
    stream_key: &'a str,
}

async fn authorize_publish(
    pipeline_access: &dyn PipelineAccessAuthenticator,
    security: &IngestSecurityService,
    engine: &MediaEngine,
    shutdown: &CancellationToken,
    context: PublishContext<'_>,
    active_ingest: &mut Option<RtmpIngestHandle>,
) -> PublishAuthorization {
    if security.is_ip_banned(context.client_ip).is_some() {
        return PublishAuthorization::Rejected {
            code: "NetStream.Publish.BadName",
            description: "IP temporarily banned due to too many login/publish failures",
            error: "IP is banned",
        };
    }
    let auth_result = tokio::select! {
        biased;
        result = pipeline_access.authenticate(PipelineAccessMode::RtmpPublish, context.stream_key, context.client_ip) => result,
        _ = shutdown.cancelled() => return PublishAuthorization::Cancelled,
    };
    let pipeline = match auth_result {
        Ok(pipeline) => pipeline,
        Err(PipelineAccessError::InvalidStreamKey) => {
            warn!(stream_key = %redact_secret(context.stream_key), "publish stream key not found");
            return PublishAuthorization::Rejected {
                code: "NetStream.Publish.BadName",
                description: "Invalid stream key",
                error: "Invalid stream key",
            };
        }
        Err(PipelineAccessError::LookupFailed(err)) => {
            error!("publish stream key lookup failed: {}", err);
            return PublishAuthorization::Rejected {
                code: "NetStream.Publish.BadName",
                description: "Invalid stream key",
                error: "Invalid stream key",
            };
        }
    };

    let registration = tokio::select! {
        biased;
        registration = engine.try_register_pipeline_input_attempt(
            &pipeline.id,
            &pipeline.input_id,
            context.stream_key,
            "rtmp",
            pipeline.selected,
        ) => registration,
        _ = shutdown.cancelled() => return PublishAuthorization::Cancelled,
    };
    let Some(registration) = registration else {
        return PublishAuthorization::Rejected {
            code: "NetStream.Publish.BadName",
            description: "Input already has an active publisher",
            error: "Input already has an active publisher",
        };
    };
    if shutdown.is_cancelled() {
        engine
            .unregister_ingest_if_current(&pipeline.id, &registration)
            .await;
        return PublishAuthorization::Cancelled;
    }

    let ring = tokio::select! {
        ring = engine.get_or_create_pipeline(&pipeline.id) => ring,
        _ = shutdown.cancelled() => {
            engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            return PublishAuthorization::Cancelled;
        }
    };
    let ingest = tokio::select! {
        ingest = engine.with_ingest_session(&registration, |ingest| {
            (
                ingest.bytes_received.clone(),
                ingest.metrics.clone(),
                ingest.last_progress_ms.clone(),
            )
        }) => ingest,
        _ = shutdown.cancelled() => {
            engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            return PublishAuthorization::Cancelled;
        }
    };
    let Some((bytes_received, ingest_metrics, last_progress_ms)) = ingest else {
        engine
            .unregister_ingest_if_current(&pipeline.id, &registration)
            .await;
        return PublishAuthorization::Rejected {
            code: "NetStream.Publish.BadName",
            description: "Input already has an active publisher",
            error: "Active ingest disappeared during registration",
        };
    };
    tokio::select! {
        _ = engine.update_ingest_session_meta(
            &pipeline.id,
            &registration,
            None,
            None,
            Some(context.client_addr.to_string()),
        ) => {}
        _ = shutdown.cancelled() => {
            engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            return PublishAuthorization::Cancelled;
        }
    };
    *active_ingest = Some(RtmpIngestHandle {
        pipeline_id: pipeline.id.clone(),
        registration,
        ring,
        bytes_received,
        ingest_metrics,
        last_progress_ms,
        timestamp_mapper: InputTimestampMapper::default(),
        standby_gop: StandbyGopCache::default(),
    });
    info!(pipeline = %pipeline.id, "[rtmp] Ingest registered");
    PublishAuthorization::Accepted
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
        Err(PipelineAccessError::LookupFailed(err)) => {
            error!("play stream key lookup failed: {}", err);
            PlayAuthorization::Rejected {
                code: "NetStream.Play.StreamNotFound",
                description: "Invalid stream key",
                error: "Invalid stream key for play",
            }
        }
    }
}

pub(super) struct RtmpIngestHandle {
    pub(super) pipeline_id: String,
    pub(super) registration: IngestRegistration,
    pub(super) ring: Arc<RingBuffer>,
    pub(super) bytes_received: Arc<AtomicU64>,
    pub(super) ingest_metrics: Arc<StageMetrics>,
    pub(super) last_progress_ms: Arc<AtomicU64>,
    pub(super) timestamp_mapper: InputTimestampMapper,
    pub(super) standby_gop: StandbyGopCache,
}
#[cfg(target_os = "linux")]
fn set_tcp_socket_buffers(fd: RawFd, size: usize) {
    let Ok(size) = libc::c_int::try_from(size) else {
        warn!("RTMP socket buffer size does not fit c_int");
        return;
    };
    // SAFETY: `fd` is the live TCP socket still owned by this Compio session.
    // `setsockopt` reads the stack-allocated `size` value of the stated length.
    // The socket option values are valid for both calls below.
    unsafe {
        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        ) != 0
        {
            warn!("failed to set RTMP receive socket buffer");
        }
        if libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        ) != 0
        {
            warn!("failed to set RTMP send socket buffer");
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn set_tcp_socket_buffers(_fd: RawFd, _size: usize) {}
pub(super) async fn handle_rtmp_client(
    stream: compio::net::TcpStream,
    client_addr: SocketAddr,
    commands: mpsc::Sender<RtmpControlCommand>,
    shutdown: CancellationToken,
    engine: Arc<MediaEngine>,
    media_handoff: Arc<tokio::sync::Semaphore>,
    parser_budget: parser_budget::ParserBudget,
) -> Result<(), &'static str> {
    let mut socket = RtmpClientSocket::new(stream, shutdown.clone());
    let client_ip = client_addr.ip().to_string();
    let client_addr_text = client_addr.to_string();
    set_tcp_socket_buffers(socket.raw_fd(), engine.config.rtmp_preauth_buffer_bytes);

    let handshake = tokio::select! {
        result = compio::time::timeout(
            Duration::from_millis(engine.config.rtmp_handshake_timeout_ms),
            perform_server_handshake(&mut socket, vec![0; 4096]),
        ) => Some(result),
        _ = shutdown.cancelled() => None,
    };
    let Some(handshake) = handshake else {
        return Ok(());
    };
    let (remaining, _handshake_buffer) = handshake.map_err(|_| "RTMP handshake timed out")??;

    let mut session_config = ServerSessionConfig::new();
    session_config.max_message_length =
        u32::try_from(engine.config.rtmp_max_message_bytes).unwrap_or(u32::MAX);
    let (mut session, initial_results) =
        ServerSession::new(session_config).map_err(|_| "Failed to initialize server session")?;
    let mut parser_charge = parser_budget.charge();
    for result in initial_results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            socket
                .write_all(packet.bytes)
                .await
                .map_err(|_| "Failed to write initial response")?;
        }
    }

    let mut publishing = false;
    let mut disconnect = None;
    if !remaining.is_empty() {
        match session.handle_input(&remaining) {
            Ok(results)
                if parser_charge
                    .update(session.inbound_buffered_bytes() + awaiting_handoff_bytes(&results))
                    .is_err() =>
            {
                disconnect = Some(("session", PARSER_BUDGET_EXHAUSTED, true));
            }
            Ok(results) => {
                match handle_session_results(
                    &mut session,
                    results,
                    &mut socket,
                    &commands,
                    &client_ip,
                    &client_addr_text,
                    &engine,
                    &shutdown,
                    &media_handoff,
                )
                .await
                {
                    Ok(accepted_publish) => publishing |= accepted_publish,
                    Err(error) => {
                        disconnect = Some(if shutdown.is_cancelled() {
                            ("shutdown", "RTMP listener shutting down", false)
                        } else {
                            ("session", error, true)
                        });
                    }
                }
                // Handed-off media is now bounded by the handoff semaphore.
                parser_charge.release_to(session.inbound_buffered_bytes());
            }
            Err(error) => {
                disconnect = Some(("session", session_error_reason(&error), true));
            }
        }
    }

    let mut previous_tcp_bytes: Option<(u64, Instant)> = None;
    let mut stats_tick: Pin<Box<dyn Future<Output = ()>>> =
        Box::pin(compio::time::sleep(Duration::from_secs(2)));
    let fd = socket.raw_fd();
    while disconnect.is_none() {
        // Read straight into the session's own input buffer: the only copy of
        // a received byte before chunk reassembly is the kernel's.
        let mut input = session.take_input_buffer();
        input.reserve(INGEST_READ_BYTES);
        let Some(read_result) = read_rtmp_input_or_quality(
            &mut socket,
            input,
            &shutdown,
            &mut stats_tick,
            publishing,
            fd,
            &mut previous_tcp_bytes,
            &commands,
        )
        .await
        else {
            disconnect = Some(("shutdown", "RTMP listener shutting down", false));
            break;
        };
        let (count, input) = match read_result {
            Ok(result) => result,
            Err(_) => {
                warn!("read error in main loop for {}", client_addr_text);
                disconnect = Some(("io", "Read error in main loop", true));
                break;
            }
        };
        if count == 0 {
            disconnect = Some(("disconnect", "publisher disconnected", false));
            break;
        }
        let results = match session.handle_buffered_input(input, count) {
            Ok(results) => results,
            Err(error) => {
                warn!(%error, "session parse error for {}", client_addr_text);
                disconnect = Some(("session", session_error_reason(&error), true));
                break;
            }
        };
        // Completed media still awaiting a handoff permit counts too, so the
        // budget bounds everything held before the 64 MiB handoff takes over.
        if parser_charge
            .update(session.inbound_buffered_bytes() + awaiting_handoff_bytes(&results))
            .is_err()
        {
            warn!(
                "RTMP ingest parser budget exhausted; rejecting {}",
                client_addr_text
            );
            disconnect = Some(("session", PARSER_BUDGET_EXHAUSTED, true));
            break;
        }
        match handle_session_results(
            &mut session,
            results,
            &mut socket,
            &commands,
            &client_ip,
            &client_addr_text,
            &engine,
            &shutdown,
            &media_handoff,
        )
        .await
        {
            Ok(accepted_publish) => publishing |= accepted_publish,
            Err(error) => {
                disconnect = Some(if shutdown.is_cancelled() {
                    ("shutdown", "RTMP listener shutting down", false)
                } else {
                    ("session", error, true)
                });
            }
        }
        // Handed-off media is now bounded by the handoff semaphore.
        parser_charge.release_to(session.inbound_buffered_bytes());
    }

    let (phase, reason, had_error) =
        disconnect.unwrap_or(("disconnect", "publisher disconnected", false));
    if !shutdown.is_cancelled() {
        let (finish_tx, finish_rx) = oneshot::channel();
        if send_control_command(
            &commands,
            &shutdown,
            RtmpControlCommand::Finish {
                phase: phase.to_string(),
                reason: reason.to_string(),
                had_error,
                reply: finish_tx,
            },
        )
        .await
        .is_ok()
        {
            tokio::select! {
                _ = shutdown.cancelled() => {}
                _ = finish_rx => {}
            }
        }
    }
    if had_error { Err(reason) } else { Ok(()) }
}

const PARSER_BUDGET_EXHAUSTED: &str = "RTMP ingest parser budget exhausted";

/// Media payload bytes in parsed results that still await a handoff permit.
fn awaiting_handoff_bytes(results: &[ServerSessionResult]) -> usize {
    results
        .iter()
        .map(|result| match result {
            ServerSessionResult::RaisedEvent(
                ServerSessionEvent::VideoDataReceived { data, .. }
                | ServerSessionEvent::AudioDataReceived { data, .. },
            ) => data.len(),
            _ => 0,
        })
        .sum()
}

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
                    crate::media::tcp_stats::bytes_delta_rate_mbps(
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
