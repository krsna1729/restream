//! RTMP publisher session handling.

use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
#[cfg(test)]
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ingest_auth::{
    PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode,
};
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
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
use super::play::{RtmpPlayRequest, handle_play_request};

enum RtmpClientIo {
    Duplex(DuplexStream),
    #[cfg(test)]
    Tcp(TcpStream),
}

pub(super) struct RtmpClientSocket {
    io: RtmpClientIo,
    socket_fd: Option<OwnedFd>,
    closed: Option<CancellationToken>,
}

impl RtmpClientSocket {
    pub(super) fn from_duplex(
        stream: DuplexStream,
        socket_fd: Option<OwnedFd>,
        closed: CancellationToken,
    ) -> Self {
        Self {
            io: RtmpClientIo::Duplex(stream),
            socket_fd,
            closed: Some(closed),
        }
    }

    #[cfg(test)]
    pub(super) fn from_tcp(stream: TcpStream) -> Self {
        Self {
            io: RtmpClientIo::Tcp(stream),
            socket_fd: None,
            closed: None,
        }
    }

    fn raw_fd(&self) -> Option<RawFd> {
        match &self.io {
            RtmpClientIo::Duplex(_) => self.socket_fd.as_ref().map(AsRawFd::as_raw_fd),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => Some(stream.as_raw_fd()),
        }
    }
}

pub(super) fn duplicate_socket_fd(fd: RawFd) -> Option<OwnedFd> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        warn!(
            error = %io::Error::last_os_error(),
            "failed to retain RTMP socket descriptor for TCP statistics"
        );
        return None;
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returns a new descriptor owned by this caller.
    Some(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

impl Drop for RtmpClientSocket {
    fn drop(&mut self) {
        if let Some(closed) = &self.closed {
            closed.cancel();
        }
    }
}

impl AsyncRead for RtmpClientSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().io {
            RtmpClientIo::Duplex(stream) => Pin::new(stream).poll_read(cx, buffer),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => Pin::new(stream).poll_read(cx, buffer),
        }
    }
}

impl AsyncWrite for RtmpClientSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().io {
            RtmpClientIo::Duplex(stream) => Pin::new(stream).poll_write(cx, buffer),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => Pin::new(stream).poll_write(cx, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().io {
            RtmpClientIo::Duplex(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().io {
            RtmpClientIo::Duplex(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match &self.io {
            RtmpClientIo::Duplex(stream) => stream.is_write_vectored(),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => stream.is_write_vectored(),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().io {
            RtmpClientIo::Duplex(stream) => Pin::new(stream).poll_write_vectored(cx, buffers),
            #[cfg(test)]
            RtmpClientIo::Tcp(stream) => Pin::new(stream).poll_write_vectored(cx, buffers),
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
    // SAFETY: `fd` is the live TCP socket retained by the Compio bridge.
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
    mut socket: RtmpClientSocket,
    client_addr: SocketAddr,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) -> Result<(), &'static str> {
    let client_ip = client_addr.ip().to_string();
    let client_addr_text = client_addr.to_string();
    if let Some(fd) = socket.raw_fd() {
        set_tcp_socket_buffers(fd, engine.config.rtmp_preauth_buffer_bytes);
    }
    let mut buffer = vec![0u8; 4096];

    // 1. Handshake Loop
    let remaining = tokio::time::timeout(
        Duration::from_millis(engine.config.rtmp_handshake_timeout_ms),
        perform_server_handshake(&mut socket, &mut buffer),
    )
    .await
    .map_err(|_| "RTMP handshake timed out")??;

    // 2. Initialize ServerSession
    let config = ServerSessionConfig::new();
    let (mut session, initial_results) =
        ServerSession::new(config).map_err(|_| "Failed to initialize server session")?;

    for res in initial_results {
        if let ServerSessionResult::OutboundResponse(pkt) = res {
            socket
                .write_all(&pkt.bytes)
                .await
                .map_err(|_| "Failed to write initial response")?;
        }
    }

    let mut active_ingest: Option<RtmpIngestHandle> = None;
    let mut probe = ProbeState {
        video_done: false,
        audio_done: false,
    };

    // Process left over bytes from handshake
    if !remaining.is_empty() {
        let results = session
            .handle_input(&remaining)
            .map_err(|_| "Session parse error on remaining bytes")?;
        if let Err(error) = handle_session_results(
            &mut session,
            results,
            &mut socket,
            pipeline_access.as_ref(),
            &security,
            &engine,
            &client_ip,
            &client_addr_text,
            &mut probe,
            &mut active_ingest,
        )
        .await
        {
            if let Some(active) = &active_ingest {
                engine
                    .record_ingest_disconnect_if_current(
                        &active.pipeline_id,
                        &active.registration,
                        Some("session"),
                        Some(error.to_string()),
                        true,
                    )
                    .await;
                engine
                    .unregister_ingest_if_current(&active.pipeline_id, &active.registration)
                    .await;
            }
            return Err(error);
        }
    }

    // 3. Main Protocol Loop
    let mut tcp_stats_interval = tokio::time::interval(std::time::Duration::from_secs(2));
    tcp_stats_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous_tcp_bytes: Option<(u64, Instant)> = None;
    let disconnect_outcome = loop {
        tokio::select! {
            read_result = socket.read(&mut buffer) => {
                let n = match read_result {
                    Ok(n) => n,
                    Err(_) => {
                        warn!("read error in main loop for {}", client_addr_text);
                        break Some((
                            "io".to_string(),
                            "Read error in main loop".to_string(),
                            true,
                        ));
                    }
                };
                if n == 0 {
                    break Some((
                        "disconnect".to_string(),
                        "publisher disconnected".to_string(),
                        false,
                    ));
                }

                let results = match session.handle_input(&buffer[..n]) {
                    Ok(results) => results,
                    Err(_) => {
                        warn!("session parse error for {}", client_addr_text);
                        break Some((
                            "session".to_string(),
                            "Session parse error".to_string(),
                            true,
                        ));
                    }
                };
                if let Err(e) = handle_session_results(
                    &mut session,
                    results,
                    &mut socket,
                    pipeline_access.as_ref(),
                    &security,
                    &engine,
                    &client_ip,
                    &client_addr_text,
                    &mut probe,
                    &mut active_ingest,
                )
                .await
                {
                    warn!("session result error: {}", e);
                    break Some(("session".to_string(), e.to_string(), true));
                }
            }
            _ = tcp_stats_interval.tick(), if active_ingest.is_some() => {
                let now = Instant::now();
                let quality = match socket.raw_fd() {
                    Some(fd) => match collect_tcp_stats_by_fd(fd) {
                        Ok(stats) => {
                            let receive_rate = stats.tcp_bytes_received.and_then(|bytes| {
                                let rate = previous_tcp_bytes.and_then(|(previous, sampled_at)| {
                                    crate::media::tcp_stats::bytes_delta_rate_mbps(
                                        bytes,
                                        previous,
                                        now.duration_since(sampled_at).as_secs_f64(),
                                    )
                                });
                                previous_tcp_bytes = Some((bytes, now));
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
                                    std::io::ErrorKind::Unsupported => "not_linux",
                                    _ => "collection_failed",
                                }
                                .to_string(),
                            ),
                            ..PublisherQuality::default()
                        },
                    },
                    None => PublisherQuality {
                        tcp_stats_unavailable_reason: Some("descriptor_unavailable".to_string()),
                        ..PublisherQuality::default()
                    },
                };
                if let Some(active) = active_ingest.as_ref() {
                    engine
                        .update_ingest_session_quality(&active.registration, quality)
                        .await;
                }
            }
        }
    };

    // Clean up active ingest on disconnect
    if let Some(active) = &active_ingest {
        info!(
            "[rtmp] Publisher disconnected for pipeline: {}",
            active.pipeline_id
        );
        let (phase, reason, had_error) = disconnect_outcome.unwrap_or((
            "disconnect".to_string(),
            "publisher disconnected".to_string(),
            false,
        ));
        engine
            .record_ingest_disconnect_if_current(
                &active.pipeline_id,
                &active.registration,
                Some(phase.as_str()),
                Some(reason),
                had_error,
            )
            .await;
        engine
            .unregister_ingest_if_current(&active.pipeline_id, &active.registration)
            .await;
    }

    Ok(())
}

struct ProbeState {
    video_done: bool,
    audio_done: bool,
}

// CLIPPY-ALLOW: single event boundary owns live protocol state and authenticated runtime owners.
#[allow(clippy::too_many_arguments)]
async fn handle_session_results(
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
    socket: &mut RtmpClientSocket,
    pipeline_access: &dyn PipelineAccessAuthenticator,
    security: &IngestSecurityService,
    engine: &MediaEngine,
    client_ip: &str,
    client_addr: &str,
    probe: &mut ProbeState,
    active_ingest: &mut Option<RtmpIngestHandle>,
) -> Result<(), &'static str> {
    for res in results {
        match res {
            ServerSessionResult::OutboundResponse(pkt) => {
                socket
                    .write_all(&pkt.bytes)
                    .await
                    .map_err(|_| "Failed to write outbound response")?;
            }
            ServerSessionResult::RaisedEvent(event) => {
                match event {
                    ServerSessionEvent::ConnectionRequested {
                        request_id,
                        app_name: _,
                    } => {
                        // Accept connection
                        if let Ok(resp) = session.accept_request(request_id) {
                            for r in resp {
                                if let ServerSessionResult::OutboundResponse(pkt) = r {
                                    socket
                                        .write_all(&pkt.bytes)
                                        .await
                                        .map_err(|_| "Write error")?;
                                }
                            }
                        }
                    }
                    ServerSessionEvent::PublishStreamRequested {
                        request_id,
                        app_name: _,
                        stream_key,
                        mode: _,
                    } => {
                        // Rate limit security check
                        if security.is_ip_banned(client_ip).is_some() {
                            let _ = session.reject_request(
                                request_id,
                                "NetStream.Publish.BadName",
                                "IP temporarily banned due to too many login/publish failures",
                            );
                            return Err("IP is banned");
                        }

                        // Validate stream key against database pipelines
                        let pipeline = match pipeline_access
                            .authenticate(PipelineAccessMode::RtmpPublish, &stream_key, client_ip)
                            .await
                        {
                            Ok(pipeline) => pipeline,
                            Err(PipelineAccessError::InvalidStreamKey) => {
                                warn!(
                                    stream_key = %redact_secret(&stream_key),
                                    "publish stream key not found"
                                );
                                let _ = session.reject_request(
                                    request_id,
                                    "NetStream.Publish.BadName",
                                    "Invalid stream key",
                                );
                                return Err("Invalid stream key");
                            }
                            Err(PipelineAccessError::LookupFailed(err)) => {
                                error!("publish stream key lookup failed: {}", err);
                                let _ = session.reject_request(
                                    request_id,
                                    "NetStream.Publish.BadName",
                                    "Invalid stream key",
                                );
                                return Err("Invalid stream key");
                            }
                        };

                        let Some(registration) = engine
                            .try_register_pipeline_input_attempt(
                                &pipeline.id,
                                &pipeline.input_id,
                                &stream_key,
                                "rtmp",
                                pipeline.selected,
                            )
                            .await
                        else {
                            let _ = session.reject_request(
                                request_id,
                                "NetStream.Publish.BadName",
                                "Input already has an active publisher",
                            );
                            return Err("Input already has an active publisher");
                        };
                        let ring = engine.get_or_create_pipeline(&pipeline.id).await;
                        let Some((bytes_received, ingest_metrics, last_progress_ms)) = engine
                            .with_ingest_session(&registration, |ingest| {
                                (
                                    ingest.bytes_received.clone(),
                                    ingest.metrics.clone(),
                                    ingest.last_progress_ms.clone(),
                                )
                            })
                            .await
                        else {
                            engine
                                .unregister_ingest_if_current(&pipeline.id, &registration)
                                .await;
                            return Err("Active ingest disappeared during registration");
                        };
                        engine
                            .update_ingest_session_meta(
                                &pipeline.id,
                                &registration,
                                None,
                                None,
                                Some(client_addr.to_string()),
                            )
                            .await;
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
                        if let Some(fd) = socket.raw_fd() {
                            set_tcp_socket_buffers(fd, engine.config.rtmp_stream_buffer_bytes);
                        }

                        // Success! Accept publish request
                        let resp = session
                            .accept_request(request_id)
                            .map_err(|_| "Failed to accept publish request")?;
                        for r in resp {
                            if let ServerSessionResult::OutboundResponse(pkt) = r {
                                socket
                                    .write_all(&pkt.bytes)
                                    .await
                                    .map_err(|_| "Write error")?;
                            }
                        }

                        security.record_success(client_ip);
                        info!(
                            "[rtmp] Ingest successfully started on pipeline: {}",
                            pipeline.id
                        );
                    }
                    ServerSessionEvent::VideoDataReceived {
                        app_name: _,
                        stream_key: _,
                        data,
                        timestamp,
                    } => {
                        if let Some(active) = active_ingest.as_mut() {
                            let pipeline_id = &active.pipeline_id;
                            active
                                .bytes_received
                                .fetch_add(data.len() as u64, Ordering::Relaxed);
                            active.ingest_metrics.record_in(data.len() as u64);
                            active
                                .last_progress_ms
                                .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);

                            let packet_kind = classify_flv_video_packet(&data);
                            let is_keyframe =
                                matches!(packet_kind, Some(FlvVideoPacketKind::Keyframe));

                            let dts = timestamp.value as i64;
                            let pts = dts + flv_video_composition_time_ms(&data) as i64;

                            // Cache video sequence header for play subscribers
                            let parameter_sets = flv_avcc_config_annexb_parameter_sets(&data);
                            if matches!(packet_kind, Some(FlvVideoPacketKind::SequenceHeader))
                                && (data[0] & 0x0F) == 7
                            {
                                engine
                                    .cache_ingest_session_sequence_header(
                                        &active.registration,
                                        true,
                                        data.clone(),
                                    )
                                    .await;
                            }

                            // Probe video metadata from sequence header (first config packet)
                            if !probe.video_done
                                && let Some(meta) = parse_flv_video_meta(&data)
                            {
                                if meta.width > 0 {
                                    probe.video_done = true;
                                }
                                info!(
                                    "[rtmp] Probed video: {} {}x{} profile={:?} level={:?}",
                                    meta.codec, meta.width, meta.height, meta.profile, meta.level
                                );
                                engine
                                    .update_ingest_session_meta(
                                        pipeline_id,
                                        &active.registration,
                                        Some(meta),
                                        None,
                                        None,
                                    )
                                    .await;
                            }

                            let mut packet = MediaPacket {
                                media_type: MediaType::Video,
                                track_index: 0,
                                pts,
                                dts,
                                is_keyframe,
                                format: PayloadFormat::Flv,
                                payload: data,
                            };
                            let boundary = if is_keyframe {
                                InputPacketBoundary::VideoKeyframe
                            } else {
                                InputPacketBoundary::Other
                            };
                            if let Some(preview_ring) = active.registration.preview_ring.load_full()
                            {
                                if let Some(parameter_sets) = parameter_sets.clone() {
                                    preview_ring.set_video_parameter_sets(parameter_sets);
                                }
                                preview_ring.push(packet.clone());
                            }
                            if active.registration.gate.state() == InputForwardState::Active {
                                let Some(lease) = active.registration.gate.try_enter(boundary)
                                else {
                                    continue;
                                };
                                active.timestamp_mapper.map_packet(
                                    &mut packet,
                                    false,
                                    &active.registration.last_forwarded_dts,
                                );
                                if let Some(parameter_sets) = parameter_sets {
                                    active.ring.set_video_parameter_sets(parameter_sets);
                                }
                                let keyframe_pts = is_keyframe.then_some(packet.pts);
                                InputTimestampMapper::record_forwarded(
                                    &packet,
                                    &active.registration.last_forwarded_dts,
                                );
                                active.ring.push(packet);
                                drop(lease);
                                if let Some(pts) = keyframe_pts {
                                    engine.record_keyframe(pipeline_id, pts).await;
                                }
                            } else {
                                active.standby_gop.push(packet);
                                try_promote_cached_rtmp(engine, active).await;
                            }
                        }
                    }
                    ServerSessionEvent::AudioDataReceived {
                        app_name: _,
                        stream_key: _,
                        data,
                        timestamp,
                    } => {
                        if let Some(active) = active_ingest.as_mut() {
                            let pipeline_id = &active.pipeline_id;
                            active
                                .bytes_received
                                .fetch_add(data.len() as u64, Ordering::Relaxed);
                            active.ingest_metrics.record_in(data.len() as u64);
                            active
                                .last_progress_ms
                                .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);

                            // Cache audio sequence header for play subscribers
                            if data.len() >= 2 && (data[0] >> 4) == 10 && data[1] == 0 {
                                engine
                                    .cache_ingest_session_sequence_header(
                                        &active.registration,
                                        false,
                                        data.clone(),
                                    )
                                    .await;
                            }

                            // AAC's FLV sound-rate/channel bits are only legacy
                            // hints. Wait for AudioSpecificConfig so 48 kHz,
                            // mono, and other real AAC layouts are not reported
                            // as the FLV fallback of 44.1 kHz stereo.
                            if !probe.audio_done {
                                let format_id =
                                    data.first().map(|byte| (byte >> 4) & 0x0f).unwrap_or(0xff);
                                let has_complete_config =
                                    format_id != 10 || (data.len() >= 3 && data[1] == 0);
                                if has_complete_config
                                    && let Some(meta) = parse_flv_audio_meta(&data)
                                {
                                    probe.audio_done = true;
                                    info!(
                                        "[rtmp] Probed audio: {} {}Hz {}ch",
                                        meta.codec, meta.sample_rate, meta.channels
                                    );
                                    engine
                                        .update_ingest_session_meta(
                                            pipeline_id,
                                            &active.registration,
                                            None,
                                            Some(meta.clone()),
                                            None,
                                        )
                                        .await;
                                    engine
                                        .update_ingest_session_audio_tracks(
                                            pipeline_id,
                                            &active.registration,
                                            vec![meta],
                                        )
                                        .await;
                                }
                            }

                            let mut packet = MediaPacket {
                                media_type: MediaType::Audio,
                                track_index: 0,
                                pts: timestamp.value as i64,
                                dts: timestamp.value as i64,
                                is_keyframe: false,
                                format: PayloadFormat::Flv,
                                payload: data,
                            };
                            if let Some(preview_ring) = active.registration.preview_ring.load_full()
                            {
                                preview_ring.push(packet.clone());
                            }
                            if active.registration.gate.state() == InputForwardState::Active {
                                let Some(lease) = active
                                    .registration
                                    .gate
                                    .try_enter(InputPacketBoundary::Other)
                                else {
                                    continue;
                                };
                                active.timestamp_mapper.map_packet(
                                    &mut packet,
                                    false,
                                    &active.registration.last_forwarded_dts,
                                );
                                InputTimestampMapper::record_forwarded(
                                    &packet,
                                    &active.registration.last_forwarded_dts,
                                );
                                active.ring.push(packet);
                                drop(lease);
                            } else {
                                active.standby_gop.push(packet);
                                try_promote_cached_rtmp(engine, active).await;
                            }
                        }
                    }
                    ServerSessionEvent::PlayStreamRequested {
                        request_id,
                        app_name: _,
                        stream_key,
                        start_at: _,
                        duration: _,
                        reset: _,
                        stream_id,
                    } => {
                        return handle_play_request(RtmpPlayRequest {
                            session,
                            socket,
                            pipeline_access,
                            engine,
                            client_ip,
                            request_id,
                            stream_key: &stream_key,
                            stream_id,
                        })
                        .await;
                    }
                    ServerSessionEvent::PublishStreamFinished {
                        app_name: _,
                        stream_key: _,
                    } => {
                        return Err("Publish finished by client");
                    }
                    _ => {}
                }
            }
            ServerSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    Ok(())
}
#[cfg(all(test, target_os = "linux"))]
mod socket_fd_tests {
    use super::RtmpClientSocket;
    use std::net::{TcpListener, TcpStream};
    use std::os::fd::AsRawFd;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn duplicated_socket_descriptor_outlives_the_compio_bridge() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let original_fd = client.as_raw_fd();
        let retained_fd = super::duplicate_socket_fd(original_fd)
            .expect("socket descriptor should be retained before bridge handoff");
        let socket = RtmpClientSocket::from_duplex(
            tokio::io::duplex(32).0,
            Some(retained_fd),
            CancellationToken::new(),
        );
        let retained_fd = socket
            .raw_fd()
            .expect("socket descriptor should remain owned by the session");
        assert_ne!(retained_fd, original_fd);

        drop(client);
        super::collect_tcp_stats_by_fd(retained_fd)
            .expect("TCP_INFO remains readable after the bridge descriptor closes");
        drop(socket);
        drop(server);
    }
}
