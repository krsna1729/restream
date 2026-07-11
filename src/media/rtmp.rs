//! Native RTMP ingest and egress using `rml_rtmp`.
//!
//! Ingest: accepts RTMP publish connections, authenticates stream keys against
//! the database, and pushes `MediaPacket`s into the pipeline's `RingBuffer`.
//! Keyframe detection uses FLV FrameType (works for both H.264 and H.265).
//!
//! Egress: connects to an RTMP target URL and forwards packets from the
//! `RingBuffer` via a `Reader`. Cancellation via `CancellationToken`.

use crate::application::ingest::{IngestAuthError, authenticate_publish_stream_key};
use crate::application::ports::PipelineStore;
use crate::domain::state::EgressPhase;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType, ServerSession, ServerSessionConfig, ServerSessionEvent,
    ServerSessionResult, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::media::MEDIA_PULL_BURST_PACKETS;
use crate::media::codec;
use crate::media::engine::{
    AudioMeta, EgressRegistration, IngestRegistration, MediaEngine, PublisherQuality, StageMetrics,
    VideoMeta,
};
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, Reader, RingBuffer};
use crate::media::security::IngestSecurityService;
use crate::media::startup_policy;
use crate::media::tcp_stats::collect_rtmp_receiver_stats;
use crate::secret_display::{redact_secret, redact_url};
use bytes::Bytes;

mod egress_transport;
mod flv;
mod timestamps;

#[cfg(test)]
#[path = "rtmp/tests.rs"]
mod tests;

use egress_transport::{connect_rtmp_egress_stream, parse_rtmp_url, rtmp_sender_quality};
use flv::{
    FlvVideoPacketKind, classify_flv_video_packet, flv_avcc_config_annexb_parameter_sets,
    flv_video_composition_time_ms, parse_flv_audio_meta, parse_flv_video_meta,
};
use timestamps::{RtmpTimestampGuard, refreshed_video_sequence_header_timestamp};

const RTMP_EGRESS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

struct RtmpIngestHandle {
    pipeline_id: String,
    registration: IngestRegistration,
    ring: Arc<RingBuffer>,
    bytes_received: Arc<AtomicU64>,
    ingest_metrics: Arc<StageMetrics>,
}

/// RTMP Ingest Server
pub async fn start_rtmp_server(
    pipeline_lookup: Arc<dyn PipelineStore>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) {
    start_rtmp_server_on(pipeline_lookup, security, engine, 1935).await;
}

pub async fn start_rtmp_server_on(
    pipeline_lookup: Arc<dyn PipelineStore>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
    port: u16,
) {
    let addr = format!("0.0.0.0:{port}");
    let backlog = engine.config.rtmp_backlog;
    let listener = match bind_rtmp_listener_with_backlog(port, backlog) {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind TCP listener on {}: {:?}", addr, e);
            return;
        }
    };
    info!("Server listening on {}", addr);
    let connection_permits = Arc::new(Semaphore::new(engine.config.rtmp_max_connections));

    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                let permit = match connection_permits.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!("RTMP connection rejected: max connection limit reached");
                        drop(socket);
                        continue;
                    }
                };
                let pipeline_lookup_clone = pipeline_lookup.clone();
                let security_clone = security.clone();
                let engine_clone = engine.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_rtmp_client(
                        socket,
                        addr,
                        pipeline_lookup_clone,
                        security_clone,
                        engine_clone,
                    )
                    .await
                    {
                        warn!("error handling client {}: {:?}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Accept error: {:?}", e);
            }
        }
    }
}

fn bind_rtmp_listener_with_backlog(port: u16, backlog: u32) -> Result<TcpListener, std::io::Error> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    socket.bind(addr)?;
    socket.listen(backlog)
}

#[cfg(target_os = "linux")]
fn set_tcp_socket_buffers(socket: &TcpStream, size: usize) {
    use std::os::unix::io::AsRawFd;

    let Ok(size) = libc::c_int::try_from(size) else {
        warn!("RTMP socket buffer size does not fit c_int");
        return;
    };
    let fd = socket.as_raw_fd();
    // SAFETY: setsockopt is a POSIX socket API. The file descriptor `fd` is a
    // valid socket from tokio's TcpStream. `size` is a stack-allocated c_int,
    // and c_void is the canonical opaque pointer for setsockopt option values.
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
fn set_tcp_socket_buffers(_socket: &TcpStream, _size: usize) {}

async fn perform_server_handshake(
    socket: &mut TcpStream,
    buffer: &mut [u8],
) -> Result<Vec<u8>, &'static str> {
    let mut handshake = Handshake::new(PeerType::Server);

    loop {
        let n = socket
            .read(buffer)
            .await
            .map_err(|_| "Socket read error during handshake")?;
        if n == 0 {
            return Err("Socket closed during handshake");
        }

        let result = handshake
            .process_bytes(&buffer[..n])
            .map_err(|_| "Handshake parsing error")?;
        match result {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    socket
                        .write_all(&response_bytes)
                        .await
                        .map_err(|_| "Socket write error during handshake")?;
                }
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    socket
                        .write_all(&response_bytes)
                        .await
                        .map_err(|_| "Socket write error during handshake")?;
                }
                return Ok(remaining_bytes);
            }
        }
    }
}

async fn perform_client_handshake<S>(
    socket: &mut S,
    cancel_token: &CancellationToken,
) -> Result<Vec<u8>, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut handshake = Handshake::new(PeerType::Client);
    let c0_c1 = handshake
        .generate_outbound_p0_and_p1()
        .map_err(|e| format!("{e:?}"))?;

    socket
        .write_all(&c0_c1)
        .await
        .map_err(|_| "failed to write handshake".to_string())?;

    let mut buffer = vec![0u8; 4096];
    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => return Err("cancelled during handshake".to_string()),
            res = socket.read(&mut buffer) => {
                let n = match res {
                    Ok(n) if n > 0 => n,
                    _ => return Err("remote closed during handshake".to_string()),
                };
                match handshake.process_bytes(&buffer[..n]) {
                    Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                        if !response_bytes.is_empty() {
                            socket
                                .write_all(&response_bytes)
                                .await
                                .map_err(|_| "failed to write handshake response".to_string())?;
                        }
                    }
                    Ok(HandshakeProcessResult::Completed {
                        response_bytes,
                        remaining_bytes,
                    }) => {
                        if !response_bytes.is_empty() {
                            socket
                                .write_all(&response_bytes)
                                .await
                                .map_err(|_| "failed to write handshake completion".to_string())?;
                        }
                        return Ok(remaining_bytes);
                    }
                    Err(e) => return Err(format!("{e:?}")),
                }
            }
        }
    }
}

async fn handle_rtmp_client(
    mut socket: TcpStream,
    client_addr: SocketAddr,
    pipeline_lookup: Arc<dyn PipelineStore>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) -> Result<(), &'static str> {
    let client_ip = client_addr.ip().to_string();
    let client_addr_text = client_addr.to_string();
    // Configure socket for low jitter and fast response
    let _ = socket.set_nodelay(true);
    set_tcp_socket_buffers(&socket, engine.config.rtmp_preauth_buffer_bytes);
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
            pipeline_lookup.as_ref(),
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
                let n = read_result.map_err(|_| "Read error in main loop")?;
                if n == 0 {
                    break Some((
                        "disconnect".to_string(),
                        "publisher disconnected".to_string(),
                        false,
                    ));
                }

                let results = session
                    .handle_input(&buffer[..n])
                    .map_err(|_| "Session parse error")?;
                if let Err(e) = handle_session_results(
                    &mut session,
                    results,
                    &mut socket,
                    pipeline_lookup.as_ref(),
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
                let pipeline_id = active_ingest
                    .as_ref()
                    .map(|active| active.pipeline_id.as_str())
                    .unwrap_or_default();
                let now = Instant::now();
                let quality = match collect_rtmp_receiver_stats(&socket) {
                    Ok(stats) => {
                        let receive_rate = stats.tcp_bytes_received.and_then(|bytes| {
                            let rate = previous_tcp_bytes.and_then(|(previous, sampled_at)| {
                                let elapsed = now.duration_since(sampled_at).as_secs_f64();
                                let delta = bytes.checked_sub(previous)?;
                                (elapsed > 0.0).then_some(
                                    (delta as f64 * 8.0) / (elapsed * 1_000_000.0),
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
                        tcp_stats_unavailable_reason: Some(match error.kind() {
                            std::io::ErrorKind::Unsupported => "not_linux",
                            _ => "collection_failed",
                        }.to_string()),
                        ..PublisherQuality::default()
                    },
                };
                engine.update_publisher_quality(pipeline_id, quality).await;
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

#[allow(clippy::too_many_arguments)]
async fn handle_session_results(
    session: &mut ServerSession,
    results: Vec<ServerSessionResult>,
    socket: &mut TcpStream,
    pipeline_lookup: &dyn PipelineStore,
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
                        let pipeline = match authenticate_publish_stream_key(
                            pipeline_lookup,
                            security,
                            &stream_key,
                            client_ip,
                        )
                        .await
                        {
                            Ok(pipeline) => pipeline,
                            Err(IngestAuthError::InvalidStreamKey) => {
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
                            Err(IngestAuthError::LookupFailed(err)) => {
                                error!("publish stream key lookup failed: {:?}", err);
                                let _ = session.reject_request(
                                    request_id,
                                    "NetStream.Publish.BadName",
                                    "Invalid stream key",
                                );
                                return Err("Invalid stream key");
                            }
                        };

                        // Reserve the pipeline before accepting the publish request.
                        // A bonded SRT group is one logical publisher, but a second
                        // independent RTMP/SRT publisher must not create another
                        // writer for the same RingBuffer.
                        let Some(registration) = engine
                            .try_register_ingest_attempt(&pipeline.id, &stream_key, "rtmp")
                            .await
                        else {
                            let _ = session.reject_request(
                                request_id,
                                "NetStream.Publish.BadName",
                                "Pipeline already has an active publisher",
                            );
                            return Err("Pipeline already has an active publisher");
                        };
                        let ring = engine.get_or_create_pipeline(&pipeline.id).await;
                        let Some((bytes_received, ingest_metrics)) = engine
                            .with_active_ingest(&pipeline.id, |ingest| {
                                (ingest.bytes_received.clone(), ingest.metrics.clone())
                            })
                            .await
                        else {
                            engine
                                .unregister_ingest_if_current(&pipeline.id, &registration)
                                .await;
                            return Err("Active ingest disappeared during registration");
                        };
                        *active_ingest = Some(RtmpIngestHandle {
                            pipeline_id: pipeline.id.clone(),
                            registration,
                            ring,
                            bytes_received,
                            ingest_metrics,
                        });
                        set_tcp_socket_buffers(socket, engine.config.rtmp_stream_buffer_bytes);

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

                        engine
                            .update_ingest_meta(
                                &pipeline.id,
                                None,
                                None,
                                Some(client_addr.to_string()),
                            )
                            .await;
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
                        if let Some(active) = active_ingest.as_ref() {
                            let pipeline_id = &active.pipeline_id;
                            active
                                .bytes_received
                                .fetch_add(data.len() as u64, Ordering::Relaxed);
                            active.ingest_metrics.record_in(data.len() as u64);

                            let packet_kind = classify_flv_video_packet(&data);
                            let is_keyframe =
                                matches!(packet_kind, Some(FlvVideoPacketKind::Keyframe));

                            let dts = timestamp.value as i64;
                            let pts = dts + flv_video_composition_time_ms(&data) as i64;

                            if is_keyframe {
                                engine.record_keyframe(pipeline_id, pts).await;
                            }

                            // Cache video sequence header for play subscribers
                            if matches!(packet_kind, Some(FlvVideoPacketKind::SequenceHeader))
                                && (data[0] & 0x0F) == 7
                            {
                                engine
                                    .cache_sequence_header(pipeline_id, true, data.clone())
                                    .await;
                                // Raw SPS/PPS on the ingest ring let stages that need
                                // eager parameter sets (e.g. VideoPreset stages —
                                // see wait_for_stage_metadata) become ready without
                                // waiting on a decoded frame; file/SRT ingest already
                                // populate this, RTMP previously never did.
                                if let Some(parameter_sets) =
                                    flv_avcc_config_annexb_parameter_sets(&data)
                                {
                                    active.ring.set_video_parameter_sets(parameter_sets);
                                }
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
                                    .update_ingest_meta(pipeline_id, Some(meta), None, None)
                                    .await;
                            }

                            let packet = MediaPacket {
                                media_type: MediaType::Video,
                                track_index: 0,
                                pts,
                                dts,
                                is_keyframe,
                                format: PayloadFormat::Flv,
                                payload: data,
                            };
                            active.ring.push(packet);
                        }
                    }
                    ServerSessionEvent::AudioDataReceived {
                        app_name: _,
                        stream_key: _,
                        data,
                        timestamp,
                    } => {
                        if let Some(active) = active_ingest.as_ref() {
                            let pipeline_id = &active.pipeline_id;
                            active
                                .bytes_received
                                .fetch_add(data.len() as u64, Ordering::Relaxed);
                            active.ingest_metrics.record_in(data.len() as u64);

                            // Cache audio sequence header for play subscribers
                            if data.len() >= 2 && (data[0] >> 4) == 10 && data[1] == 0 {
                                engine
                                    .cache_sequence_header(pipeline_id, false, data.clone())
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
                                        .update_ingest_meta(
                                            pipeline_id,
                                            None,
                                            Some(meta.clone()),
                                            None,
                                        )
                                        .await;
                                    engine
                                        .update_ingest_audio_tracks(pipeline_id, vec![meta])
                                        .await;
                                }
                            }

                            let packet = MediaPacket {
                                media_type: MediaType::Audio,
                                track_index: 0,
                                pts: timestamp.value as i64,
                                dts: timestamp.value as i64,
                                is_keyframe: false,
                                format: PayloadFormat::Flv,
                                payload: data,
                            };
                            active.ring.push(packet);
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
                        // Look up pipeline by stream key
                        let pipeline = match pipeline_lookup
                            .get_pipeline_by_stream_key(&stream_key)
                            .await
                        {
                            Ok(Some(pipeline)) => pipeline,
                            Ok(None) => {
                                let _ = session.reject_request(
                                    request_id,
                                    "NetStream.Play.StreamNotFound",
                                    "Invalid stream key",
                                );
                                return Err("Invalid stream key for play");
                            }
                            Err(err) => {
                                error!("play stream key lookup failed: {:?}", err);
                                let _ = session.reject_request(
                                    request_id,
                                    "NetStream.Play.StreamNotFound",
                                    "Invalid stream key",
                                );
                                return Err("Invalid stream key for play");
                            }
                        };

                        // Check if there's an active ingest
                        if !engine
                            .ingests
                            .active
                            .read()
                            .await
                            .contains_key(&pipeline.id)
                        {
                            let _ = session.reject_request(
                                request_id,
                                "NetStream.Play.StreamNotFound",
                                "No active ingest",
                            );
                            return Err("No active ingest for play");
                        }

                        let resp = session
                            .accept_request(request_id)
                            .map_err(|_| "Failed to accept play request")?;
                        // rml_rtmp 0.8 appends two optional AMF data messages
                        // after the required reset, stream-begin, and play-start
                        // responses: |RtmpSampleAccess and NetStream.Data.Start.
                        // FFmpeg exposes those notifications as synthetic
                        // subtitle/data streams. We do not send metadata on
                        // their chunk stream, so omitting these two optional
                        // messages is safe and keeps the read endpoint media-only.
                        for r in resp.into_iter().take(3) {
                            if let ServerSessionResult::OutboundResponse(pkt) = r {
                                socket
                                    .write_all(&pkt.bytes)
                                    .await
                                    .map_err(|_| "Write error")?;
                            }
                        }

                        info!(
                            "[rtmp] Play subscriber connected for pipeline: {} (stream_id={})",
                            pipeline.id, stream_id
                        );

                        // Send cached sequence headers so the player can initialize decoders
                        let (video_sh, audio_sh) = engine.get_sequence_headers(&pipeline.id).await;
                        if let Some(vsh) = video_sh
                            && let Ok(pkt) = session.send_video_data(
                                stream_id,
                                vsh,
                                RtmpTimestamp::new(0),
                                false,
                            )
                        {
                            let _ = socket.write_all(&pkt.bytes).await;
                        }
                        if let Some(ash) = audio_sh
                            && let Ok(pkt) = session.send_audio_data(
                                stream_id,
                                ash,
                                RtmpTimestamp::new(0),
                                false,
                            )
                        {
                            let _ = socket.write_all(&pkt.bytes).await;
                        }

                        // Feed loop: read from RingBuffer and send RTMP data.
                        // Use pull_burst() to batch packets per iteration
                        // instead of pull() which acquires the write_idx atomic once
                        // per packet (~170 acquisitions/sec at 170 pkts/sec vs ~5/sec).
                        let ring_buf = engine.get_or_create_pipeline(&pipeline.id).await;
                        let mut reader =
                            Reader::new(format!("rtmp_play:{}", pipeline.id), ring_buf);
                        let mut burst = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
                        let mut timestamp_guard = RtmpTimestampGuard::new();

                        'play: loop {
                            burst.clear();
                            match reader.pull_burst(&mut burst, MEDIA_PULL_BURST_PACKETS) {
                                Ok(0) => {
                                    reader.wait_for_data().await;
                                    continue;
                                }
                                Err(_) => {
                                    // Overflow — reader was fast-forwarded; continue from new pos
                                    continue;
                                }
                                Ok(_) => {}
                            }

                            for pkt in &burst {
                                let ts = timestamp_guard.packet_timestamp(pkt);
                                let result = match pkt.media_type {
                                    MediaType::Video => session.send_video_data(
                                        stream_id,
                                        pkt.payload.clone(),
                                        ts,
                                        !pkt.is_keyframe,
                                    ),
                                    MediaType::Audio => session.send_audio_data(
                                        stream_id,
                                        pkt.payload.clone(),
                                        ts,
                                        false,
                                    ),
                                };
                                match result {
                                    Ok(packet) => {
                                        if socket.write_all(&packet.bytes).await.is_err() {
                                            info!(
                                                "[rtmp] Play subscriber disconnected for pipeline: {}",
                                                pipeline.id
                                            );
                                            return Err("Play subscriber disconnected");
                                        }
                                    }
                                    Err(_) => break 'play,
                                }
                            }
                        }
                        return Err("Play finished");
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

/// H.265 → H.264 transcoder for RTMP egress.
///
/// Reads H.265 MPEG-TS from `in_queue`, decodes with FFmpeg's HEVC decoder,
/// re-encodes to H.264 Annex B, and sends `(annexb, is_keyframe, pts_ms)` via
/// `out_tx`. Runs on a dedicated OS thread (FFmpeg codec calls block).
/// RTMP Egress Client
pub async fn start_rtmp_egress(
    output_id: String,
    pipeline_id: String,
    target_url: String,
    ring_buffer: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    registration: EgressRegistration,
) {
    let cancel_token = registration.cancel_token.clone();
    macro_rules! egress_error {
        ($phase:expr, $message:expr) => {{
            engine
                .record_egress_error_if_current(&output_id, &registration, $phase, $message)
                .await;
        }};
    }
    macro_rules! egress_phase {
        ($phase:expr) => {{
            engine
                .update_egress_phase_if_current(&output_id, &registration, $phase)
                .await;
        }};
    }
    macro_rules! egress_target_addr {
        ($addr:expr) => {{
            engine
                .update_egress_target_addr_if_current(&output_id, &registration, $addr)
                .await;
        }};
    }
    macro_rules! egress_quality {
        ($quality:expr) => {{
            engine
                .update_egress_quality_if_current(&output_id, &registration, $quality)
                .await;
        }};
    }
    let mut reader = Reader::new_with_keyframe_preroll(
        format!("rtmp_egress:{}", output_id),
        ring_buffer.clone(),
        startup_policy::rtmp_egress_keyframe_preroll_packets(),
    );
    let parts = match parse_rtmp_url(&target_url) {
        Some(p) => p,
        None => {
            error!("Invalid RTMP URL: {}", redact_url(&target_url));
            egress_error!("parse_url", "invalid RTMP URL");
            return;
        }
    };

    // Pre-connect warmup: wait for the upstream ring to have data before
    // connecting to MediaMTX. Transcoded/routed rings (codec_hint set) go
    // through a multi-stage chain that takes seconds to warm up. Connecting
    // before any data is ready results in an idle publisher that MediaMTX
    // closes for inactivity before the first packet ever arrives — under
    // high output fanout this manifests as a repeating connect/drop/retry
    // storm. Mirrors the same gate in start_srt_egress.
    engine
        .wait_for_upstream_warmup(
            &output_id,
            &registration,
            ring_buffer.clone(),
            cancel_token.clone(),
        )
        .await;
    if cancel_token.is_cancelled() {
        return;
    }

    // Resolve audio tracks AFTER warmup so that audio-router stages (which
    // run as separate tasks) have had a chance to process the first burst and
    // set audio_tracks on the output ring.  Resolving before warmup races
    // with the audio_router startup and falls back to the raw ingest tracks
    // (all N tracks), causing validate_rtmp_output_audio_tracks to reject
    // the routed single-track output as having "too many tracks".
    //
    // Even after the warmup there is a brief race: the warmup exits on the
    // first packet, but the ingest probe (which triggers set_audio_tracks on
    // the source ring) may complete slightly later.  We wait up to 500 ms for
    // the output ring's audio_tracks to be populated before falling back to
    // the ingest registry.  The audio_router sets them on the next burst after
    // the source ring has been probed, so this delay is usually < 1 burst (~21ms).
    {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if ring_buffer.audio_tracks().is_some() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::select! {
                _ = cancel_token.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
        }
    }
    let output_audio_tracks =
        resolved_output_audio_tracks(&engine, &pipeline_id, &ring_buffer).await;
    if let Err(message) = validate_rtmp_output_audio_tracks(&output_audio_tracks) {
        error!(
            "[rtmp-egress] refusing to start output {} for pipeline {}: {}",
            output_id, pipeline_id, message
        );
        egress_error!("prepare", message);
        return;
    }
    let mut output_audio_track = output_audio_tracks.first().cloned();

    egress_phase!(EgressPhase::Connecting);
    egress_target_addr!(format!("{}:{}", parts.host, parts.port));
    info!(
        "[rtmp-egress] Connecting to {}:{} via {} (app: {}, key: {})",
        parts.host,
        parts.port,
        if parts.tls { "rtmps" } else { "rtmp" },
        parts.app,
        parts.stream_key
    );

    let mut socket =
        match connect_rtmp_egress_stream(&parts, engine.config.rtmp_stream_buffer_bytes).await {
            Ok(s) => s,
            Err(e) => {
                error!(
                    "[rtmp-egress] Connection failed to {}:{}: {:?}",
                    parts.host, parts.port, e
                );
                egress_error!("connect", e.to_string());
                return;
            }
        };

    // Perform handshake
    egress_phase!(EgressPhase::Handshaking);
    let remaining = match tokio::time::timeout(
        RTMP_EGRESS_HANDSHAKE_TIMEOUT,
        perform_client_handshake(&mut socket, &cancel_token),
    )
    .await
    {
        Ok(Ok(remaining)) => remaining,
        Ok(Err(error)) => {
            egress_error!("handshake", error);
            return;
        }
        Err(_) => {
            egress_error!("handshake", "RTMP egress handshake timed out");
            return;
        }
    };

    // Initialize ClientSession with tcUrl for MediaMTX compatibility
    let mut config = ClientSessionConfig::new();
    let scheme = if parts.tls { "rtmps" } else { "rtmp" };
    config.tc_url = Some(format!(
        "{}://{}:{}/{}",
        scheme, parts.host, parts.port, parts.app
    ));
    let (mut session, initial_results) = match ClientSession::new(config) {
        Ok(s) => s,
        Err(e) => {
            egress_error!("session", format!("{:?}", e));
            return;
        }
    };

    for res in initial_results {
        if let ClientSessionResult::OutboundResponse(pkt) = res
            && socket.write_all(&pkt.bytes).await.is_err()
        {
            egress_error!("session", "failed to write session init");
            return;
        }
    }

    // Request connection
    egress_phase!(EgressPhase::ConnectingApp);
    let conn_pkt = match session.request_connection(parts.app.clone()) {
        Ok(ClientSessionResult::OutboundResponse(p)) => p,
        _ => {
            egress_error!("connect_app", "failed to build connect request");
            return;
        }
    };
    if socket.write_all(&conn_pkt.bytes).await.is_err() {
        egress_error!("connect_app", "failed to write connect request");
        return;
    }

    let mut buffer = vec![0u8; 4096];
    if !remaining.is_empty() {
        let results = match session.handle_input(&remaining) {
            Ok(r) => r,
            Err(_) => {
                egress_error!("connect_app", "failed to parse connect response");
                return;
            }
        };
        if handle_client_results(results, &mut socket, &mut session, &parts.stream_key)
            .await
            .is_err()
        {
            egress_error!("connect_app", "failed to handle connect response");
            return;
        }
    }

    let (egress_bytes_sent, egress_metrics, egress_last_progress_ms) = {
        engine
            .with_active_egress(&output_id, |egress| {
                (
                    Some(egress.bytes_sent.clone()),
                    Some(egress.metrics.clone()),
                    Some(egress.last_progress_ms.clone()),
                )
            })
            .await
            .unwrap_or((None, None, None))
    };

    let mut is_publishing = false;
    let mut raw_h264_parameter_sets = ring_buffer
        .video_parameter_sets()
        .map(|parameter_sets| parameter_sets.to_vec())
        .unwrap_or_default();
    let progress_sample_interval = Duration::from_millis(250);
    let mut last_progress_sample = Instant::now() - progress_sample_interval;
    let mut tcp_stats_interval = tokio::time::interval(Duration::from_secs(2));
    tcp_stats_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous_tcp_bytes: Option<(u64, Instant)> = None;
    // Track the last SPS bytes we sent so we can re-send the AVCC decoder
    // config record when the encoder changes resolution or bitrate mid-stream.
    // None = no sequence header sent yet.
    let mut last_sent_sps: Option<Vec<u8>> = None;
    let mut video_ready = false;
    let mut audio_sequence_header_sent = false;
    let mut deferred_audio_sequence_header: Option<Bytes> = None;
    let mut timestamp_guard = RtmpTimestampGuard::new();
    // Per-egress reusable conversion buffers — avoids per-frame Vec allocation.
    // Each task owns its own buffer; no sharing, no contention with transcoder.
    let mut video_buf = Vec::<u8>::new();
    let mut audio_buf = Vec::<u8>::new();

    // Pre-allocated burst buffer — declared outside the loop so capacity
    // is retained across bursts instead of re-allocating per burst.
    let mut packets: Vec<Arc<MediaPacket>> = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = session.stop_publishing();
                break;
            }
            _ = tcp_stats_interval.tick() => {
                let quality = rtmp_sender_quality(&socket, &mut previous_tcp_bytes);
                egress_quality!(quality);
            }
            // Read from server to handle acknowledgements, status codes, pings
            res = socket.read(&mut buffer) => {
                let n = match res {
                    Ok(n) if n > 0 => n,
                    _ => {
                        egress_error!("send", "remote closed connection");
                        break;
                    }
                };
                let results = match session.handle_input(&buffer[..n]) {
                    Ok(r) => r,
                    Err(e) => {
                        egress_error!("send", format!("{:?}", e));
                        break;
                    }
                };
                for r in results {
                    match r {
                        ClientSessionResult::OutboundResponse(pkt) => {
                            if socket.write_all(&pkt.bytes).await.is_err() {
                                egress_error!("session", "failed to write RTMP control response");
                                return;
                            }
                        }
                        ClientSessionResult::RaisedEvent(event) => {
                            match event {
                                ClientSessionEvent::ConnectionRequestAccepted => {
                                    egress_phase!(EgressPhase::Publishing);
                                    let pub_pkt = match session.request_publishing(parts.stream_key.clone(), PublishRequestType::Live) {
                                        Ok(ClientSessionResult::OutboundResponse(p)) => p,
                                        _ => {
                                            egress_error!("publishing", "failed to build publish request");
                                            return;
                                        }
                                    };
                                    if socket.write_all(&pub_pkt.bytes).await.is_err() {
                                        egress_error!("publishing", "failed to write publish request");
                                        return;
                                    }
                                }
                                ClientSessionEvent::PublishRequestAccepted => {
                                    info!("Stream publishing accepted on target");
                                    egress_phase!(EgressPhase::Sending);
                                    if let Some(metadata) = rtmp_publish_metadata(
                                        &engine,
                                        &pipeline_id,
                                        reader.current_ring(),
                                        output_audio_track.as_ref(),
                                    )
                                    .await
                                        && let Ok(ClientSessionResult::OutboundResponse(p)) =
                                            session.publish_metadata(&metadata)
                                        && socket.write_all(&p.bytes).await.is_err()
                                    {
                                        egress_error!("send", "failed to write RTMP metadata");
                                        return;
                                    }
                                    // Send cached sequence headers before media data.
                                    // For H.265 ingests, video_sh is None (only RTMP ingest
                                    // caches FLV seq headers), so this is a no-op for H.265.
                                    let (ingest_video_sh, mut audio_sh) =
                                        engine.get_sequence_headers(&pipeline_id).await;
                                    if audio_sh.is_none() && output_audio_track.is_none() {
                                        let refreshed_tracks = resolved_output_audio_tracks(
                                            &engine,
                                            &pipeline_id,
                                            reader.current_ring(),
                                        )
                                        .await;
                                        if let Err(message) =
                                            validate_rtmp_output_audio_tracks(&refreshed_tracks)
                                        {
                                            error!(
                                                "[rtmp-egress] refusing to start output {} for pipeline {}: {}",
                                                output_id, pipeline_id, message
                                            );
                                            egress_error!("prepare", message);
                                            return;
                                        }
                                        output_audio_track = refreshed_tracks.first().cloned();
                                    }
                                    // Synthesize AAC sequence header from audio meta if not cached
                                    if audio_sh.is_none()
                                        && let Some(track) = output_audio_track.as_ref()
                                    {
                                        audio_sh = Some(codec::build_aac_sequence_header(
                                            track.sample_rate,
                                            track.channels,
                                        ));
                                    }
                                    let video_sh = startup_video_sequence_header(
                                        reader.current_ring(),
                                        ingest_video_sh,
                                    );
                                    if let Some(vsh) = video_sh
                                        && let Ok(ClientSessionResult::OutboundResponse(p)) =
                                            session.publish_video_data(
                                                vsh,
                                                RtmpTimestamp::new(0),
                                                true,
                                            )
                                        && socket.write_all(&p.bytes).await.is_err()
                                    {
                                        egress_error!(
                                            "send",
                                            "failed to write video sequence header"
                                        );
                                        return;
                                    }
                                    if let Some(ref ash) = audio_sh
                                        && should_send_startup_audio_sequence_header(
                                            video_ready,
                                            reader.current_ring(),
                                        )
                                        && let Ok(ClientSessionResult::OutboundResponse(p)) =
                                            session.publish_audio_data(
                                                ash.clone(),
                                                RtmpTimestamp::new(0),
                                                false,
                                            )
                                    {
                                        if socket.write_all(&p.bytes).await.is_err() {
                                            egress_error!(
                                                "send",
                                                "failed to write audio sequence header"
                                            );
                                            return;
                                        }
                                        audio_sequence_header_sent = true;
                                    }
                                    deferred_audio_sequence_header = if audio_sequence_header_sent {
                                        None
                                    } else {
                                        audio_sh
                                    };
                                    is_publishing = true;
                                }
                                ClientSessionEvent::ConnectionRequestRejected { description } => {
                                    error!("Connection rejected: {}", description);
                                    egress_error!("connect_app", description);
                                    return;
                                }
                                _ => {}
                            }
                        }
                        ClientSessionResult::UnhandleableMessageReceived(_) => {}
                    }
                }
            }
            // Write packets from ring buffer when publishing is active
            _ = reader.wait_for_data(), if is_publishing => {
                if reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS).is_ok() {
                    let mut burst_made_progress = false;
                    for packet in packets.drain(..) {
                        if packet.media_type == MediaType::Audio {
                            if should_defer_audio_until_video_ready(
                                video_ready,
                                reader.current_ring(),
                            ) {
                                continue;
                            }
                            if output_audio_track.is_none() {
                                let refreshed_tracks = resolved_output_audio_tracks(
                                    &engine,
                                    &pipeline_id,
                                    reader.current_ring(),
                                )
                                .await;
                                if let Err(message) =
                                    validate_rtmp_output_audio_tracks(&refreshed_tracks)
                                {
                                    error!(
                                        "[rtmp-egress] refusing output {} for pipeline {} after audio track probe: {}",
                                        output_id, pipeline_id, message
                                    );
                                    egress_error!("send", message);
                                    return;
                                }
                                output_audio_track = refreshed_tracks.first().cloned();
                            }
                            if let Err(message) =
                                validate_rtmp_output_audio_packet_track(packet.track_index)
                            {
                                error!(
                                    "[rtmp-egress] refusing output {} for pipeline {}: {}",
                                    output_id, pipeline_id, message
                                );
                                egress_error!("send", message);
                                return;
                            }
                            if !audio_sequence_header_sent
                                && let Some(sequence_header) =
                                    resolve_deferred_audio_sequence_header(
                                        deferred_audio_sequence_header.as_ref(),
                                        output_audio_track.as_ref(),
                                    )
                                && let Ok(ClientSessionResult::OutboundResponse(p)) =
                                    session.publish_audio_data(
                                        sequence_header,
                                        RtmpTimestamp::new(0),
                                        false,
                                    )
                            {
                                if socket.write_all(&p.bytes).await.is_err() {
                                    egress_error!(
                                        "send",
                                        "failed to write deferred audio sequence header"
                                    );
                                    return;
                                }
                                audio_sequence_header_sent = true;
                                deferred_audio_sequence_header = None;
                            }
                            if packet.format == PayloadFormat::Raw && !audio_sequence_header_sent {
                                // Raw AAC packets are not self-describing on the RTMP wire.
                                // Wait until we can announce the AAC track instead of sending
                                // a packet that makes downstream RTMP receivers reject the
                                // entire publish as video-only.
                                continue;
                            }
                        }
                        let mut ts = timestamp_guard.packet_timestamp(&packet);
                        let payload = if packet.format == PayloadFormat::Raw {
                            match packet.media_type {
                                MediaType::Video => {
                                    cache_h264_parameter_sets(
                                        &packet.payload,
                                        &mut raw_h264_parameter_sets,
                                    );
                                    // Guard: Raw path is H.264-only.  H.265 packets
                                    // must be converted by hevc_to_h264 before reaching
                                    // RTMP egress.  If they arrive here the stage graph
                                    // was set up before the codec probe completed; drop
                                    // and warn until a keyframe with a proper H.264 SPS
                                    // arrives.
                                    if packet.payload.len() >= 2 {
                                        // H.265 two-byte NAL header: bits[9:15] = nal_unit_type.
                                        // H.264 one-byte NAL header: bits[0:4] = nal_unit_type.
                                        // Detect HEVC by checking for VPS (type 32) or
                                        // SPS (type 33) in the first NALU — types that cannot
                                        // appear in H.264 streams.
                                        let first_nalu_type_h265 =
                                            (packet.payload[0] >> 1) & 0x3F;
                                        if matches!(first_nalu_type_h265, 32..=34) {
                                            error!(
                                                "[rtmp-egress] H.265 packet on Raw RTMP path \
                                                 for output {} — dropping until hevc_to_h264 \
                                                 stage is ready",
                                                output_id
                                            );
                                            continue;
                                        }
                                    }
                                    if !video_ready && !packet.is_keyframe {
                                        continue;
                                    }
                                    // On each keyframe, check whether the SPS has changed
                                    // (encoder resolution/bitrate switch) and (re-)send the
                                    // AVCC decoder configuration record before the IDR.
                                    if packet.is_keyframe
                                        && let Some((seq_hdr, new_sps)) =
                                            h264_sequence_header_for_keyframe(
                                                &packet.payload,
                                                &raw_h264_parameter_sets,
                                            )
                                    {
                                            let sps_changed = match (&last_sent_sps, &new_sps) {
                                                (None, Some(_)) => true,
                                                (Some(old), Some(new)) => old != new,
                                                _ => false,
                                            };
                                            if sps_changed {
                                                let sequence_header_ts =
                                                    refreshed_video_sequence_header_timestamp(ts);
                                                if let Ok(ClientSessionResult::OutboundResponse(
                                                    p,
                                                )) = session.publish_video_data(
                                                    seq_hdr,
                                                    sequence_header_ts,
                                                    true,
                                                ) && socket.write_all(&p.bytes).await.is_err()
                                                {
                                                    egress_error!(
                                                        "send",
                                                        "failed to write refreshed video sequence header"
                                                    );
                                                    return;
                                                }
                                                if sequence_header_ts.value == ts.value {
                                                    ts = RtmpTimestamp::new(
                                                        timestamp_guard
                                                            .enforce_ms(
                                                                MediaType::Video,
                                                                sequence_header_ts.value as i64,
                                                            )
                                                            as u32,
                                                    );
                                                }
                                                last_sent_sps = new_sps;
                                            }
                                            video_ready = true;
                                    }
                                    if !video_ready {
                                        continue;
                                    }
                                    let composition_time_ms =
                                        (packet.pts - packet.dts).clamp(
                                            -8_388_608,
                                            8_388_607,
                                        ) as i32;
                                    if !codec::video_for_rtmp_with_composition_into(
                                        &packet.payload,
                                        packet.is_keyframe,
                                        composition_time_ms,
                                        &mut video_buf,
                                    ) {
                                        continue;
                                    }
                                    Bytes::copy_from_slice(&video_buf)
                                }
                                MediaType::Audio => {
                                    codec::audio_for_rtmp_into(&packet.payload, &mut audio_buf);
                                    Bytes::copy_from_slice(&audio_buf)
                                }
                            }
                        } else {
                            if packet.media_type == MediaType::Video
                                && !video_ready
                                && let Some(kind) = classify_flv_video_packet(&packet.payload)
                            {
                                match kind {
                                    FlvVideoPacketKind::SequenceHeader => {}
                                    FlvVideoPacketKind::Keyframe => {}
                                    FlvVideoPacketKind::Interframe => continue,
                                }
                            } else if packet.media_type == MediaType::Video
                                && !video_ready
                                && !packet.is_keyframe
                            {
                                continue;
                            }
                            packet.payload.clone()
                        };
                        let pkt = match packet.media_type {
                            MediaType::Video => {
                                if !video_ready
                                    && !matches!(
                                        classify_flv_video_packet(&packet.payload),
                                        Some(FlvVideoPacketKind::SequenceHeader)
                                    )
                                {
                                    video_ready = true;
                                }
                                session.publish_video_data(payload, ts, packet.is_keyframe)
                            }
                            MediaType::Audio => {
                                session.publish_audio_data(payload, ts, false)
                            }
                        };
                        match pkt {
                            Ok(ClientSessionResult::OutboundResponse(p)) => {
                                if socket.write_all(&p.bytes).await.is_err() {
                                    egress_error!("send", "failed to write media packet");
                                    return;
                                }
                                if let Some(ref counter) = egress_bytes_sent {
                                    counter.fetch_add(p.bytes.len() as u64, Ordering::Relaxed);
                                }
                                if let Some(ref m) = egress_metrics {
                                    m.record_out(p.bytes.len() as u64);
                                }
                                burst_made_progress = true;
                            }
                            _ => {
                                error!("Failed to build publish data packet or get OutboundResponse");
                                egress_error!("send", "failed to build RTMP publish packet");
                            }
                        }
                    }
                    if burst_made_progress
                        && last_progress_sample.elapsed() >= progress_sample_interval
                    {
                        if let Some(ref progress) = egress_last_progress_ms {
                            progress.store(
                                chrono::Utc::now().timestamp_millis().max(0) as u64,
                                Ordering::Relaxed,
                            );
                        }
                        last_progress_sample = Instant::now();
                    }
                }
            }
        }
    }
}

async fn resolved_output_audio_tracks(
    engine: &MediaEngine,
    pipeline_id: &str,
    ring_buffer: &Arc<RingBuffer>,
) -> Vec<AudioMeta> {
    if let Some(tracks) = ring_buffer.audio_tracks()
        && !tracks.is_empty()
    {
        return tracks.to_vec();
    }

    engine
        .with_active_ingest(pipeline_id, |ingest| {
            let tracks = ingest
                .audio_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !tracks.is_empty() {
                tracks.as_ref().clone()
            } else {
                ingest.audio.clone().into_iter().collect()
            }
        })
        .await
        .unwrap_or_default()
}

fn validate_rtmp_output_audio_tracks(audio_tracks: &[AudioMeta]) -> Result<(), String> {
    if audio_tracks.len() > 1 {
        return Err(format!(
            "RTMP output supports exactly one audio track, but this output resolved to {} tracks. Choose subset, downmix, or remap audio routing.",
            audio_tracks.len()
        ));
    }
    Ok(())
}

async fn rtmp_publish_metadata(
    engine: &MediaEngine,
    pipeline_id: &str,
    output_ring: &Arc<RingBuffer>,
    output_audio_track: Option<&AudioMeta>,
) -> Option<StreamMetadata> {
    let video = engine
        .with_active_ingest(pipeline_id, |ingest| ingest.video.clone())
        .await
        .flatten();
    let mut metadata = StreamMetadata::new();

    if let Some(video) = video
        && rtmp_output_video_codec(&video, output_ring).eq_ignore_ascii_case("h264")
    {
        metadata.video_codec_id = Some(7);
        metadata.video_width = (video.width > 0).then_some(video.width);
        metadata.video_height = (video.height > 0).then_some(video.height);
        metadata.video_frame_rate = (video.fps > 0.0).then_some(video.fps as f32);
    }

    if let Some(track) = output_audio_track
        && track.codec.eq_ignore_ascii_case("aac")
    {
        metadata.audio_codec_id = Some(10);
        metadata.audio_sample_rate = Some(track.sample_rate);
        metadata.audio_channels = Some(track.channels);
        metadata.audio_is_stereo = Some(track.channels >= 2);
    }

    (metadata.video_codec_id.is_some() || metadata.audio_codec_id.is_some()).then_some(metadata)
}

fn rtmp_output_video_codec<'a>(
    ingest_video: &'a VideoMeta,
    output_ring: &'a RingBuffer,
) -> &'a str {
    let output_codec = output_ring.codec_hint_str();
    if output_codec.is_empty() {
        ingest_video.codec.as_str()
    } else {
        output_codec
    }
}

fn cache_h264_parameter_sets(payload: &[u8], cache: &mut Vec<u8>) {
    let Some(parameter_sets) = codec::annexb_parameter_sets(payload) else {
        return;
    };
    if h264_sps_nalu(&parameter_sets).is_some() {
        *cache = parameter_sets;
    }
}

fn startup_video_sequence_header(
    ring_buffer: &RingBuffer,
    ingest_sequence_header: Option<Bytes>,
) -> Option<Bytes> {
    if let Some(parameter_sets) = ring_buffer.video_parameter_sets()
        && let Some(sequence_header) = codec::build_avcc_sequence_header(&parameter_sets)
    {
        return Some(sequence_header);
    }

    // A brand-new raw H.264 output ring can be empty when the RTMP publisher
    // connects before the stage has emitted its first keyframe. In that case
    // the ingest-cached sequence header may describe the source stream rather
    // than the transcode output (for example 1080p source vs 720p stage), so
    // wait for the output ring's own keyframe/config instead of advertising
    // the wrong decoder config.
    if ring_buffer.get_write_idx() == 0 && !ring_buffer.codec_hint_str().is_empty() {
        return None;
    }

    ingest_sequence_header
}

fn rtmp_output_waits_for_video(ring_buffer: &RingBuffer) -> bool {
    !ring_buffer.codec_hint_str().is_empty() || ring_buffer.video_parameter_sets().is_some()
}
#[cfg(test)]
fn rtmp_warmup_ready(
    ring_buffer: &RingBuffer,
    packets: &[Arc<crate::media::ring_buffer::MediaPacket>],
) -> bool {
    !rtmp_output_waits_for_video(ring_buffer)
        || ring_buffer.video_parameter_sets().is_some()
        || packets
            .iter()
            .any(|packet| packet.media_type == crate::media::ring_buffer::MediaType::Video)
}

fn should_send_startup_audio_sequence_header(video_ready: bool, ring_buffer: &RingBuffer) -> bool {
    video_ready
        || !rtmp_output_waits_for_video(ring_buffer)
        || ring_buffer.video_parameter_sets().is_some()
}

fn should_defer_audio_until_video_ready(video_ready: bool, ring_buffer: &RingBuffer) -> bool {
    !video_ready && rtmp_output_waits_for_video(ring_buffer)
}

fn resolve_deferred_audio_sequence_header(
    cached_sequence_header: Option<&Bytes>,
    output_audio_track: Option<&AudioMeta>,
) -> Option<Bytes> {
    cached_sequence_header.cloned().or_else(|| {
        output_audio_track.and_then(|track| {
            track
                .codec
                .eq_ignore_ascii_case("aac")
                .then(|| codec::build_aac_sequence_header(track.sample_rate, track.channels))
        })
    })
}

fn h264_sps_nalu(payload: &[u8]) -> Option<Vec<u8>> {
    codec::split_annexb_nalus(payload)
        .iter()
        .find(|nalu| !nalu.is_empty() && (nalu[0] & 0x1F) == 7)
        .map(|nalu| nalu.to_vec())
}

fn h264_sequence_header_for_keyframe(
    payload: &[u8],
    parameter_sets_cache: &[u8],
) -> Option<(Bytes, Option<Vec<u8>>)> {
    let sequence_header = codec::build_avcc_sequence_header(payload)
        .or_else(|| codec::build_avcc_sequence_header(parameter_sets_cache))?;
    let sps = h264_sps_nalu(payload).or_else(|| h264_sps_nalu(parameter_sets_cache));
    Some((sequence_header, sps))
}

fn validate_rtmp_output_audio_packet_track(track_index: u32) -> Result<(), String> {
    if track_index != 0 {
        return Err(format!(
            "RTMP output requires a single routed audio track, but observed track index {} on the output ring. Choose subset, downmix, or remap audio routing.",
            track_index
        ));
    }
    Ok(())
}

async fn handle_client_results<S>(
    results: Vec<ClientSessionResult>,
    socket: &mut S,
    session: &mut ClientSession,
    stream_key: &str,
) -> Result<(), &'static str>
where
    S: AsyncWrite + Unpin,
{
    for res in results {
        match res {
            ClientSessionResult::OutboundResponse(pkt) => {
                socket
                    .write_all(&pkt.bytes)
                    .await
                    .map_err(|_| "Socket write error")?;
            }
            ClientSessionResult::RaisedEvent(event) => match event {
                ClientSessionEvent::ConnectionRequestAccepted => {
                    let pub_pkt = match session
                        .request_publishing(stream_key.to_string(), PublishRequestType::Live)
                    {
                        Ok(ClientSessionResult::OutboundResponse(p)) => p,
                        _ => return Err("Failed to build publish request"),
                    };
                    socket
                        .write_all(&pub_pkt.bytes)
                        .await
                        .map_err(|_| "Socket write error")?;
                }
                ClientSessionEvent::ConnectionRequestRejected { description } => {
                    error!("Connection request rejected: {}", description);
                    return Err("Connection request rejected");
                }
                _ => {}
            },
            ClientSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    Ok(())
}
