use std::sync::Arc;

use bytes::Bytes;

use super::{PacketFeedConfig, TsPacketFeeder};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

fn audio_track(index: u32) -> AudioMeta {
    AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        track_index: index,
        pid: None,
        language: None,
        title: None,
        profile: None,
    }
}

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
fn feeder_holds_audio_until_video_startup_packet() {
    let video = h264_video_meta();
    let audio_tracks = Arc::new(vec![audio_track(0)]);
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        audio_tracks,
        PacketFeedConfig {
            raw_video_parameter_sets: Some(vec![
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
                0xCE, 0x38, 0x80,
            ]),
            ..PacketFeedConfig::default()
        },
    );
    let audio = MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x11; 32]),
    };
    let keyframe = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 33,
        dts: 33,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80]),
    };
    let mut output = Vec::new();

    assert!(
        !feeder.extend_ts_for_packet(&audio, &mut output),
        "audio must not lead the TS stream before video startup"
    );
    assert!(output.is_empty());
    assert!(
        feeder.extend_ts_for_packet(&keyframe, &mut output),
        "first keyframe should unlock video startup"
    );
    assert!(!output.is_empty());
}

#[test]
fn feeder_skips_pre_keyframe_raw_video_until_clean_startup_boundary() {
    let video = h265_video_meta();
    let parameter_sets = vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ];
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig {
            raw_video_parameter_sets: Some(parameter_sets),
            ..PacketFeedConfig::default()
        },
    );
    let pre_keyframe = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 33,
        dts: 33,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xDD]),
    };
    let keyframe = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 66,
        dts: 66,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xEE]),
    };

    let mut output = Vec::new();
    assert!(!feeder.extend_ts_for_packet(&pre_keyframe, &mut output));
    assert!(output.is_empty());
    assert!(feeder.extend_ts_for_packet(&keyframe, &mut output));
    assert!(
        !output.is_empty(),
        "once the first keyframe arrives, the feeder should start emitting TS"
    );
}

#[test]
fn feeder_keeps_waiting_after_parameter_sets_only_raw_packet() {
    let video = h265_video_meta();
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        Arc::new(Vec::new()),
        PacketFeedConfig::default(),
    );
    let parameter_sets_only = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ]),
    };
    let delta = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 33,
        dts: 33,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xDD]),
    };
    let keyframe = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 66,
        dts: 66,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xEE]),
    };

    let mut output = Vec::new();
    assert!(
        !feeder.extend_ts_for_packet(&parameter_sets_only, &mut output),
        "parameter sets alone should prime the cache but not unlock startup, even when TS random-access marks the PES as a keyframe"
    );
    assert!(!feeder.extend_ts_for_packet(&delta, &mut output));
    assert!(
        output.is_empty(),
        "delta frames must stay suppressed until a true random-access frame arrives"
    );
    assert!(feeder.extend_ts_for_packet(&keyframe, &mut output));
    assert!(!output.is_empty());
}
