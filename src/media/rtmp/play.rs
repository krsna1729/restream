//! RTMP play admission and owner-side protocol delivery.

use rml_rtmp::sessions::{ServerSession, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::media::packet::{MediaPacket, MediaType};

use super::ingest::{RtmpClientSocket, RtmpControlCommand};
use super::timestamps::RtmpTimestampGuard;

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

pub(super) struct RtmpPlayRequest<'a> {
    pub(super) session: &'a mut ServerSession,
    pub(super) socket: &'a mut RtmpClientSocket,
    pub(super) commands: &'a mpsc::Sender<RtmpControlCommand>,
    pub(super) shutdown: &'a CancellationToken,
    pub(super) client_ip: &'a str,
    pub(super) request_id: u32,
    pub(super) stream_key: String,
    pub(super) stream_id: u32,
}

pub(super) async fn handle_play_request(request: RtmpPlayRequest<'_>) -> Result<(), &'static str> {
    let (reply, response) = oneshot::channel();
    tokio::select! {
        _ = request.shutdown.cancelled() => return Err("Play finished"),
        result = request.commands.send(RtmpControlCommand::PlayRequested {
            stream_key: request.stream_key,
            client_ip: request.client_ip.to_string(),
            reply,
        }) => result.map_err(|_| "RTMP control session closed")?,
    }
    let authorization = tokio::select! {
        _ = request.shutdown.cancelled() => return Err("Play finished"),
        result = response => result.map_err(|_| "RTMP control session closed")?,
    };
    let pipeline_id = match authorization {
        PlayAuthorization::Accepted { pipeline_id } => pipeline_id,
        PlayAuthorization::Rejected {
            code,
            description,
            error,
        } => {
            let _ = request
                .session
                .reject_request(request.request_id, code, description);
            return Err(error);
        }
    };

    let responses = request
        .session
        .accept_request(request.request_id)
        .map_err(|_| "Failed to accept play request")?;
    // rml_rtmp 0.8 adds optional AMF data messages after the required three
    // play responses. Keep the read endpoint media-only.
    for response in responses.into_iter().take(3) {
        if let ServerSessionResult::OutboundResponse(packet) = response {
            request
                .socket
                .write_all(packet.bytes)
                .await
                .map_err(|_| "Write error")?;
        }
    }
    info!(
        pipeline = %pipeline_id,
        stream_id = request.stream_id,
        "[rtmp] Play subscriber connected"
    );

    let (reply, response) = oneshot::channel();
    tokio::select! {
        _ = request.shutdown.cancelled() => return Err("Play finished"),
        result = request.commands.send(RtmpControlCommand::PlayStart {
            pipeline_id: pipeline_id.clone(),
            reply,
        }) => result.map_err(|_| "RTMP control session closed")?,
    }
    let (video_sequence_header, audio_sequence_header) = tokio::select! {
        _ = request.shutdown.cancelled() => return Err("Play finished"),
        result = response => result.map_err(|_| "RTMP control session closed")?,
    };
    if let Some(sequence_header) = video_sequence_header
        && let Ok(packet) = request.session.send_video_data(
            request.stream_id,
            sequence_header,
            RtmpTimestamp::new(0),
            false,
        )
    {
        let _ = request.socket.write_all(packet.bytes).await;
    }
    if let Some(sequence_header) = audio_sequence_header
        && let Ok(packet) = request.session.send_audio_data(
            request.stream_id,
            sequence_header,
            RtmpTimestamp::new(0),
            false,
        )
    {
        let _ = request.socket.write_all(packet.bytes).await;
    }

    let mut timestamp_guard = RtmpTimestampGuard::new();
    loop {
        let (reply, response) = oneshot::channel();
        tokio::select! {
            _ = request.shutdown.cancelled() => return Err("Play finished"),
            result = request.commands.send(RtmpControlCommand::PlayNext { reply }) => {
                result.map_err(|_| "RTMP control session closed")?;
            }
        }
        let packets = tokio::select! {
            result = response => result.map_err(|_| "RTMP control session closed")?,
            _ = request.shutdown.cancelled() => {
                let _ = request.commands.try_send(RtmpControlCommand::Cancel);
                return Err("Play finished");
            }
        };
        let Some(packets) = packets else {
            return Err("Play finished");
        };
        if let Err(error) = send_media_packets(
            request.session,
            request.socket,
            request.stream_id,
            &mut timestamp_guard,
            packets,
        )
        .await
        {
            info!(pipeline = %pipeline_id, "[rtmp] Play subscriber disconnected");
            return Err(error);
        }
    }
}

async fn send_media_packets(
    session: &mut ServerSession,
    socket: &mut RtmpClientSocket,
    stream_id: u32,
    timestamp_guard: &mut RtmpTimestampGuard,
    packets: Vec<std::sync::Arc<MediaPacket>>,
) -> Result<(), &'static str> {
    for media_packet in packets {
        let timestamp = timestamp_guard.packet_timestamp(&media_packet);
        let payload = media_packet.payload.clone();
        let result = match media_packet.media_type {
            MediaType::Video => {
                session.send_video_data(stream_id, payload, timestamp, !media_packet.is_keyframe)
            }
            MediaType::Audio => session.send_audio_data(stream_id, payload, timestamp, false),
        };
        let packet = result.map_err(|_| "Play finished")?;
        socket
            .write_all(packet.bytes)
            .await
            .map_err(|_| "Play subscriber disconnected")?;
    }
    Ok(())
}
