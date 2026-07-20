use super::*;

fn test_packet(media_type: MediaType, payload_len: usize) -> MediaPacket {
    MediaPacket {
        media_type,
        format: crate::media::packet::PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: bytes::Bytes::from(vec![0u8; payload_len]),
    }
}

#[test]
fn estimate_ts_accum_capacity_floors_at_188_for_empty_burst() {
    assert_eq!(estimate_ts_accum_capacity(&[]), 188);
}

#[test]
fn estimate_ts_accum_capacity_floors_at_188_for_tiny_payloads() {
    // A single zero-length packet still needs at least one TS packet's
    // worth of muxer overhead, not a 0-capacity allocation.
    let packets = vec![Arc::new(test_packet(MediaType::Video, 0))];
    assert_eq!(estimate_ts_accum_capacity(&packets), 188 * 4);
}

#[test]
fn estimate_ts_accum_capacity_sums_payload_plus_ts_packet_overhead() {
    let packets = vec![
        Arc::new(test_packet(MediaType::Video, 100)),
        Arc::new(test_packet(MediaType::Audio, 50)),
    ];
    assert_eq!(
        estimate_ts_accum_capacity(&packets),
        (100 + 188 * 4) + (50 + 188 * 4)
    );
}
