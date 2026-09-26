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

#[test]
fn published_bytes_counts_every_payload_on_both_publish_paths() {
    let ring = RingBuffer::new(4);
    let single = video_packet(0, 0, true);
    let single_len = single.payload.len() as u64;
    ring.push(single);
    assert_eq!(ring.published_bytes(), single_len);

    let batch: Vec<_> = (1..4).map(|pts| video_packet(pts, pts, false)).collect();
    let batch_len: u64 = batch.iter().map(|packet| packet.payload.len() as u64).sum();
    assert_eq!(ring.push_batch(batch), 3);
    assert_eq!(ring.published_bytes(), single_len + batch_len);
}
