use std::sync::Arc;

use bytes::Bytes;

use super::{PacketFeedConfig, TsPacketFeeder};
use crate::media::metadata::VideoMeta;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

fn h264_video_meta() -> VideoMeta {
    VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    }
}

fn h265_video_meta() -> VideoMeta {
    VideoMeta {
        codec: "hevc".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    }
}

#[test]
fn feeder_seeds_raw_h265_parameter_sets_for_late_joining_keyframes() {
    let video = h265_video_meta();
    let parameter_sets = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig {
            raw_video_parameter_sets: Some(parameter_sets.clone()),
            ..PacketFeedConfig::default()
        },
    );
    let packet = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD]),
    };

    let mut output = Vec::new();
    assert!(feeder.extend_ts_for_packet(&packet, &mut output));

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut remuxed_packets = Vec::new();
    for chunk in output.chunks(188) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut remuxed_packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut remuxed_packets);

    let remuxed = remuxed_packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("remuxed output should contain video");
    assert!(remuxed.payload.starts_with(&parameter_sets));
    assert!(remuxed.payload.ends_with(&packet.payload));
}

#[test]
fn needs_raw_video_parameter_sets_is_false_without_a_video_track() {
    let feeder = TsPacketFeeder::new(None, Arc::new(Vec::new()), PacketFeedConfig::default());
    assert!(!feeder.needs_raw_video_parameter_sets());
}

#[test]
fn needs_raw_video_parameter_sets_is_true_when_video_present_and_cache_empty() {
    let video = h264_video_meta();
    let feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig::default(),
    );
    assert!(feeder.needs_raw_video_parameter_sets());
}

#[test]
fn needs_raw_video_parameter_sets_is_false_when_config_already_seeded_cache() {
    let video = h264_video_meta();
    let feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig {
            raw_video_parameter_sets: Some(vec![0x00, 0x00, 0x00, 0x01, 0x67]),
            ..PacketFeedConfig::default()
        },
    );
    assert!(!feeder.needs_raw_video_parameter_sets());
}

#[test]
fn set_raw_video_parameter_sets_if_empty_is_noop_when_already_seeded() {
    let video = h264_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig {
            raw_video_parameter_sets: Some(vec![0x00, 0x00, 0x00, 0x01, 0x67]),
            ..PacketFeedConfig::default()
        },
    );
    assert!(!feeder.set_raw_video_parameter_sets_if_empty(&[0x00, 0x00, 0x00, 0x01, 0x99]));
}

#[test]
fn set_raw_video_parameter_sets_if_empty_is_noop_for_empty_input() {
    let video = h264_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig::default(),
    );
    assert!(!feeder.set_raw_video_parameter_sets_if_empty(&[]));
    assert!(feeder.needs_raw_video_parameter_sets());
}

#[test]
fn set_raw_video_parameter_sets_if_empty_sets_once_then_becomes_idempotent() {
    let video = h264_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig::default(),
    );
    let first = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x11];
    let second = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x22];

    assert!(feeder.set_raw_video_parameter_sets_if_empty(&first));
    assert!(!feeder.needs_raw_video_parameter_sets());
    assert!(
        !feeder.set_raw_video_parameter_sets_if_empty(&second),
        "cache is already primed; a second call must not overwrite it"
    );

    let keyframe = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0xAA]),
    };
    let mut output = Vec::new();
    assert!(feeder.extend_ts_for_packet(&keyframe, &mut output));

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut remuxed_packets = Vec::new();
    for chunk in output.chunks(188) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut remuxed_packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut remuxed_packets);
    let remuxed = remuxed_packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("remuxed output should contain video");
    assert!(
        remuxed.payload.starts_with(&first),
        "the first-set parameter sets must be the ones actually muxed"
    );
}

#[test]
fn set_video_sequence_header_from_avcc_is_noop_when_parameter_sets_already_present() {
    let video = h264_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig {
            raw_video_parameter_sets: Some(vec![0x00, 0x00, 0x00, 0x01, 0x67]),
            ..PacketFeedConfig::default()
        },
    );
    // 5-byte FLV header + a well-formed AVCC config; if this were parsed
    // it would replace the cache, so absence of a change proves the
    // early-return guard fired.
    let flv_header = [
        0, 0, 0, 0, 0, // FLV video-tag prefix consumed by parse_video_sequence_header
        1, 66, 0, 30, 0xFF, 0xE1, 0, 4, 0x67, 0x42, 0x00, 0x1E, 1, 0, 4, 0x68, 0xCE, 0x38, 0x80,
    ];
    feeder.set_video_sequence_header_from_avcc(&flv_header);
    assert!(!feeder.needs_raw_video_parameter_sets());
}

#[test]
fn set_video_sequence_header_from_avcc_ignores_truncated_header() {
    let video = h264_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig::default(),
    );
    // 5-byte FLV prefix + a 3-byte AVCC config body: far short of the
    // 8-byte minimum parse_avcc_config requires, so it must decode to
    // (4, empty) and leave the cache untouched.
    feeder.set_video_sequence_header_from_avcc(&[0, 0, 0, 0, 0, 1, 2, 3]);
    assert!(
        feeder.needs_raw_video_parameter_sets(),
        "a truncated header must not be treated as a successful prime"
    );
}

#[test]
fn set_video_sequence_header_from_avcc_parses_valid_header_and_primes_cache() {
    let video = h264_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig::default(),
    );

    let sps = [0x67, 0x42, 0x00, 0x1E];
    let pps = [0x68, 0xCE, 0x38, 0x80];
    let mut flv_header = vec![0, 0, 0, 0, 0]; // FLV video-tag prefix
    flv_header.extend_from_slice(&[1, 66, 0, 30, 0xFF]); // AVCC version/profile/level, len_size=4
    flv_header.push(0xE1); // num_sps = 1
    flv_header.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    flv_header.extend_from_slice(&sps);
    flv_header.push(1); // num_pps = 1
    flv_header.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    flv_header.extend_from_slice(&pps);

    feeder.set_video_sequence_header_from_avcc(&flv_header);
    assert!(!feeder.needs_raw_video_parameter_sets());

    let mut expected_annexb = Vec::new();
    expected_annexb.extend_from_slice(&[0, 0, 0, 1]);
    expected_annexb.extend_from_slice(&sps);
    expected_annexb.extend_from_slice(&[0, 0, 0, 1]);
    expected_annexb.extend_from_slice(&pps);

    let keyframe = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0xAA]),
    };
    let mut output = Vec::new();
    assert!(feeder.extend_ts_for_packet(&keyframe, &mut output));

    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut remuxed_packets = Vec::new();
    for chunk in output.chunks(188) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut remuxed_packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut remuxed_packets);
    let remuxed = remuxed_packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("remuxed output should contain video");
    assert!(remuxed.payload.starts_with(&expected_annexb));
}
