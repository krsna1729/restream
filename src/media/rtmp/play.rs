//! RTMP play-subscriber admission and ring-buffer delivery.

use rml_rtmp::sessions::{ServerSession, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tracing::{error, info};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{
    PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode,
};
use crate::media::packet::MediaType;
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader};

use super::timestamps::RtmpTimestampGuard;

pub(super) struct RtmpPlayRequest<'a> {
    pub(super) session: &'a mut ServerSession,
    pub(super) socket: &'a mut TcpStream,
    pub(super) pipeline_access: &'a dyn PipelineAccessAuthenticator,
    pub(super) engine: &'a MediaEngine,
    pub(super) client_ip: &'a str,
    pub(super) request_id: u32,
    pub(super) stream_key: &'a str,
    pub(super) stream_id: u32,
}

pub(super) async fn handle_play_request(request: RtmpPlayRequest<'_>) -> Result<(), &'static str> {
    let pipeline = match request
        .pipeline_access
        .authenticate(
            PipelineAccessMode::RtmpPlay,
            request.stream_key,
            request.client_ip,
        )
        .await
    {
        Ok(pipeline) => pipeline,
        Err(PipelineAccessError::InvalidStreamKey) => {
            let _ = request.session.reject_request(
                request.request_id,
                "NetStream.Play.StreamNotFound",
                "Invalid stream key",
            );
            return Err("Invalid stream key for play");
        }
        Err(PipelineAccessError::LookupFailed(err)) => {
            error!("play stream key lookup failed: {}", err);
            let _ = request.session.reject_request(
                request.request_id,
                "NetStream.Play.StreamNotFound",
                "Invalid stream key",
            );
            return Err("Invalid stream key for play");
        }
    };

    if !request
        .engine
        .ingests
        .active
        .read()
        .await
        .contains_key(&pipeline.id)
    {
        let _ = request.session.reject_request(
            request.request_id,
            "NetStream.Play.StreamNotFound",
            "No active ingest",
        );
        return Err("No active ingest for play");
    }

    let responses = request
        .session
        .accept_request(request.request_id)
        .map_err(|_| "Failed to accept play request")?;
    // rml_rtmp 0.8 appends two optional AMF data messages after the required
    // reset, stream-begin, and play-start responses. Omitting them keeps the
    // read endpoint media-only and avoids synthetic FFmpeg data streams.
    for response in responses.into_iter().take(3) {
        if let ServerSessionResult::OutboundResponse(packet) = response {
            request
                .socket
                .write_all(&packet.bytes)
                .await
                .map_err(|_| "Write error")?;
        }
    }

    info!(
        "[rtmp] Play subscriber connected for pipeline: {} (stream_id={})",
        pipeline.id, request.stream_id
    );

    let (video_sequence_header, audio_sequence_header) =
        request.engine.get_sequence_headers(&pipeline.id).await;
    if let Some(sequence_header) = video_sequence_header
        && let Ok(packet) = request.session.send_video_data(
            request.stream_id,
            sequence_header,
            RtmpTimestamp::new(0),
            false,
        )
    {
        let _ = request.socket.write_all(&packet.bytes).await;
    }
    if let Some(sequence_header) = audio_sequence_header
        && let Ok(packet) = request.session.send_audio_data(
            request.stream_id,
            sequence_header,
            RtmpTimestamp::new(0),
            false,
        )
    {
        let _ = request.socket.write_all(&packet.bytes).await;
    }

    let ring = request.engine.get_or_create_pipeline(&pipeline.id).await;
    let mut reader = Reader::new(format!("rtmp_play:{}", pipeline.id), ring);
    let mut burst = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut timestamp_guard = RtmpTimestampGuard::new();

    'play: loop {
        burst.clear();
        match reader.pull_burst(&mut burst, MEDIA_PULL_BURST_PACKETS) {
            Ok(0) => {
                reader.wait_for_data().await;
                continue;
            }
            Err(_) => continue,
            Ok(_) => {}
        }

        for media_packet in &burst {
            let timestamp = timestamp_guard.packet_timestamp(media_packet);
            let result = match media_packet.media_type {
                MediaType::Video => request.session.send_video_data(
                    request.stream_id,
                    media_packet.payload.clone(),
                    timestamp,
                    !media_packet.is_keyframe,
                ),
                MediaType::Audio => request.session.send_audio_data(
                    request.stream_id,
                    media_packet.payload.clone(),
                    timestamp,
                    false,
                ),
            };
            match result {
                Ok(packet) => {
                    if request.socket.write_all(&packet.bytes).await.is_err() {
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

    Err("Play finished")
}
