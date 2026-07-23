//! RTMP egress connection, startup gating, and packet publication.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{error, info};

use crate::domain::output_spec::RtmpOutputMode;
use crate::domain::state::EgressPhase;
use crate::media::codec;
use crate::media::engine::{EgressRegistration, MediaEngine};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
use crate::media::startup_policy;
use crate::secret_display::redact_url;

use super::egress_metadata::{
    output_ring_video_codec_kind, resolved_output_audio_tracks, rtmp_publish_metadata,
    validate_rtmp_output_audio_tracks,
};
use super::egress_packets::{
    cache_h264_parameter_sets, h264_sps_nalu, resolve_deferred_audio_sequence_header,
    rtmp_video_packet_can_be_dropped, should_defer_audio_until_video_ready,
    should_send_startup_audio_sequence_header, startup_video_sequence_header,
    validate_rtmp_output_audio_packet_track, video_sequence_header_for_keyframe,
};
use super::egress_transport::{connect_rtmp_egress_stream, parse_rtmp_url, rtmp_sender_quality};
use super::egress_write::write_rtmp_pending_bytes;
use super::enhanced::{
    cache_hevc_parameter_sets, enhanced_rtmp_connect_packet,
    raw_packet_starts_with_hevc_parameter_set,
};
use super::flv::{FlvVideoPacketKind, classify_flv_video_packet};
use super::handshake::perform_client_handshake;
use super::timestamps::{RtmpTimestampGuard, refreshed_video_sequence_header_timestamp};

const RTMP_EGRESS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn start_rtmp_egress(
    output_id: String,
    pipeline_id: String,
    target_url: String,
    ring_buffer: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    registration: EgressRegistration,
    rtmp_mode: RtmpOutputMode,
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
    let enhanced_rtmp_connect = rtmp_mode.is_enhanced();
    let enhanced_hevc_video = enhanced_rtmp_connect
        && output_ring_video_codec_kind(&engine, &pipeline_id, &ring_buffer)
            .await
            .is_hevc();

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
    config.chunk_size = engine.config.rtmp_egress_chunk_size;
    let connect_config = config.clone();
    let (mut session, initial_results) = match ClientSession::new(config) {
        Ok(s) => s,
        Err(e) => {
            egress_error!("session", format!("{:?}", e));
            return;
        }
    };

    for res in initial_results {
        if let ClientSessionResult::OutboundResponse(pkt) = res
            && write_rtmp_pending_bytes(&mut socket, Bytes::from(pkt.bytes))
                .await
                .is_err()
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
    let conn_bytes = if enhanced_rtmp_connect {
        match enhanced_rtmp_connect_packet(&connect_config, &parts.app) {
            Ok(bytes) => bytes,
            Err(error) => {
                egress_error!("connect_app", error);
                return;
            }
        }
    } else {
        conn_pkt.bytes
    };
    if write_rtmp_pending_bytes(&mut socket, Bytes::from(conn_bytes))
        .await
        .is_err()
    {
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
    let mut raw_video_parameter_sets = ring_buffer
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
    let mut last_sent_video_config: Option<Vec<u8>> = None;
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
                                        && write_rtmp_pending_bytes(&mut socket, Bytes::from(p.bytes))
                                            .await
                                            .is_err()
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
                                        enhanced_hevc_video,
                                    );
                                    let mut sent_startup_video_sequence_header = false;
                                    if let Some(vsh) = video_sh
                                        && let Ok(ClientSessionResult::OutboundResponse(p)) =
                                            session.publish_video_data(
                                                vsh,
                                                RtmpTimestamp::new(0),
                                                false,
                                            )
                                    {
                                        if write_rtmp_pending_bytes(&mut socket, Bytes::from(p.bytes))
                                        .await
                                        .is_err()
                                        {
                                            egress_error!(
                                                "send",
                                                "failed to write video sequence header"
                                            );
                                            return;
                                        }
                                        sent_startup_video_sequence_header = true;
                                    }
                                    if sent_startup_video_sequence_header {
                                        if enhanced_hevc_video {
                                            last_sent_video_config =
                                                reader.current_ring().video_parameter_sets();
                                        } else if let Some(parameter_sets) =
                                            reader.current_ring().video_parameter_sets()
                                        {
                                            last_sent_video_config = h264_sps_nalu(&parameter_sets);
                                        }
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
                                        if write_rtmp_pending_bytes(&mut socket, Bytes::from(p.bytes))
                                        .await
                                        .is_err()
                                        {
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
                                if write_rtmp_pending_bytes(&mut socket, Bytes::from(p.bytes))
                                    .await
                                    .is_err()
                                {
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
                                    if enhanced_hevc_video {
                                        cache_hevc_parameter_sets(
                                            &packet.payload,
                                            &mut raw_video_parameter_sets,
                                        );
                                    } else {
                                        cache_h264_parameter_sets(
                                            &packet.payload,
                                            &mut raw_video_parameter_sets,
                                        );
                                        if raw_packet_starts_with_hevc_parameter_set(&packet.payload)
                                        {
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
                                    if packet.is_keyframe
                                        && let Some((seq_hdr, new_config)) =
                                            video_sequence_header_for_keyframe(
                                                enhanced_hevc_video,
                                                &packet.payload,
                                                &raw_video_parameter_sets,
                                            )
                                    {
                                            let config_changed = match (
                                                &last_sent_video_config,
                                                &new_config,
                                            ) {
                                                (None, Some(_)) => true,
                                                (Some(old), Some(new)) => old != new,
                                                _ => false,
                                            };
                                            if config_changed {
                                                let sequence_header_ts =
                                                    refreshed_video_sequence_header_timestamp(ts);
                                                if let Ok(ClientSessionResult::OutboundResponse(
                                                    p,
                                                )) = session.publish_video_data(
                                                    seq_hdr,
                                                    sequence_header_ts,
                                                    false,
                                                ) && write_rtmp_pending_bytes(
                                                    &mut socket,
                                                    Bytes::from(p.bytes),
                                                )
                                                .await
                                                .is_err()
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
                                                last_sent_video_config = new_config;
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
                                    let wrote_video = if enhanced_hevc_video {
                                        codec::hevc_video_for_enhanced_rtmp_with_composition_into(
                                            &packet.payload,
                                            packet.is_keyframe,
                                            composition_time_ms,
                                            &mut video_buf,
                                        )
                                    } else {
                                        codec::video_for_rtmp_with_composition_into(
                                            &packet.payload,
                                            packet.is_keyframe,
                                            composition_time_ms,
                                            &mut video_buf,
                                        )
                                    };
                                    if !wrote_video {
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
                                let can_be_dropped =
                                    rtmp_video_packet_can_be_dropped(&payload, packet.is_keyframe);
                                session.publish_video_data(payload, ts, can_be_dropped)
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
                write_rtmp_pending_bytes(socket, Bytes::from(pkt.bytes))
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
                    write_rtmp_pending_bytes(socket, Bytes::from(pub_pkt.bytes))
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
