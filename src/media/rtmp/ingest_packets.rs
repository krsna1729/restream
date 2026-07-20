use bytes::Bytes;

use super::ingest::RtmpIngestHandle;
use crate::media::engine::MediaEngine;
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;

pub(super) fn push_promotion_headers(
    ring: &RingBuffer,
    (video, audio): (Option<Bytes>, Option<Bytes>),
    timestamp: i64,
) {
    if let Some(payload) = video {
        ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: timestamp,
            dts: timestamp,
            is_keyframe: false,
            format: PayloadFormat::Flv,
            payload,
        });
    }
    if let Some(payload) = audio {
        ring.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: timestamp,
            dts: timestamp,
            is_keyframe: false,
            format: PayloadFormat::Flv,
            payload,
        });
    }
}

pub(super) async fn try_promote_cached_rtmp(engine: &MediaEngine, active: &mut RtmpIngestHandle) {
    if !active.standby_gop.is_replay_ready()
        || active.registration.gate.state() != InputForwardState::AwaitingKeyframe
    {
        return;
    }

    let promotion_headers = engine
        .get_ingest_session_sequence_headers(&active.registration)
        .await;
    let Some(lease) = active
        .registration
        .gate
        .try_enter(InputPacketBoundary::ReplayReady)
    else {
        return;
    };

    let mut replay = active.standby_gop.take_replay();
    for (index, packet) in replay.iter_mut().enumerate() {
        active.timestamp_mapper.map_packet(
            packet,
            lease.activated() && index == 0,
            &active.registration.last_forwarded_dts,
        );
    }
    let first_dts = replay.first().map(|packet| packet.dts);
    let keyframe_pts = replay
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
        .map(|packet| packet.pts)
        .collect::<Vec<_>>();
    if lease.activated()
        && let Some(first_dts) = first_dts
    {
        push_promotion_headers(&active.ring, promotion_headers, first_dts.saturating_sub(1));
    }
    if let Some(last) = replay.iter().max_by_key(|packet| packet.dts) {
        InputTimestampMapper::record_forwarded(last, &active.registration.last_forwarded_dts);
    }
    active.ring.push_drained_batch_capped(&mut replay);
    drop(lease);

    for pts in keyframe_pts {
        engine.record_keyframe(&active.pipeline_id, pts).await;
    }
}
