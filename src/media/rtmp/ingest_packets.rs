use bytes::Bytes;

use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, RingBuffer};

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
