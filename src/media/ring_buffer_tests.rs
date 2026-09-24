use super::*;
use bytes::Bytes;

fn video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts,
        dts,
        is_keyframe: keyframe,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 16]),
    }
}

fn audio_packet(pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts,
        dts,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 4]),
    }
}

fn media_packet_with_payload(media_type: MediaType, dts: i64, payload_bytes: usize) -> MediaPacket {
    MediaPacket {
        media_type,
        track_index: 0,
        pts: dts,
        dts,
        is_keyframe: matches!(media_type, MediaType::Video) && dts == 0,
        format: PayloadFormat::Raw,
        payload: Bytes::from(vec![0; payload_bytes]),
    }
}

#[path = "ring_buffer_tests/concurrency.rs"]
mod concurrency;
#[path = "ring_buffer_tests/overflow.rs"]
mod overflow;
#[path = "ring_buffer_tests/reader.rs"]
mod reader;
