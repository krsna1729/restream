use std::sync::Arc;

use bytes::Bytes;

use super::{PacketFeedConfig, TsPacketFeeder};
use crate::media::codec::{audio_for_ts_into, video_for_ts_into};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::mpegts::TsMuxer;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::DtsEnforcer;

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

fn video_meta() -> VideoMeta {
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

#[test]
fn feeder_skips_unknown_audio_track_to_protect_dts_state() {
    let audio_tracks = Arc::new(vec![audio_track(0)]);
    let mut feeder = TsPacketFeeder::new(None, audio_tracks, PacketFeedConfig::default());
    let packet = MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 7,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00]),
    };
    let mut output = Vec::new();

    assert!(!feeder.extend_ts_for_packet(&packet, &mut output));
    assert!(output.is_empty());
}

#[test]
fn feeder_matches_manual_codec_mux_and_dts_path() {
    let video = video_meta();
    let audio_tracks = Arc::new(vec![audio_track(0), audio_track(1)]);
    let packets = vec![
        MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: 0,
            dts: 0,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x21, 0xA0]),
        },
        MediaPacket {
            media_type: MediaType::Audio,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts: 20,
            dts: 20,
            payload: Bytes::from_static(&[0x11; 32]),
        },
        MediaPacket {
            media_type: MediaType::Audio,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 1,
            pts: 21,
            dts: 21,
            payload: Bytes::from_static(&[0x22; 24]),
        },
    ];

    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        audio_tracks.clone(),
        PacketFeedConfig::default(),
    );
    let mut feeder_output = Vec::new();
    for packet in &packets {
        assert!(feeder.extend_ts_for_packet(packet, &mut feeder_output));
    }

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut dts = DtsEnforcer::new(1 + audio_tracks.len());
    let mut video_conv_buf = Vec::new();
    let mut audio_conv_buf = Vec::new();
    let mut nalu_len_size = 4usize;
    let mut sps_pps_cache = Vec::new();
    let mut manual_output = Vec::new();

    for packet in &packets {
        let payload = match packet.media_type {
            MediaType::Video => video_for_ts_into(
                &packet.payload,
                packet.format,
                &mut nalu_len_size,
                &mut sps_pps_cache,
                &mut video_conv_buf,
            )
            .expect("video packet should convert"),
            MediaType::Audio => {
                let track = audio_tracks
                    .iter()
                    .find(|a| a.track_index == packet.track_index)
                    .expect("test packet track exists");
                audio_for_ts_into(
                    &packet.payload,
                    packet.format,
                    track.sample_rate,
                    track.channels,
                    &mut audio_conv_buf,
                )
                .expect("audio packet should convert")
            }
        };
        let stream_idx = match packet.media_type {
            MediaType::Video => 0,
            MediaType::Audio => {
                1 + audio_tracks
                    .iter()
                    .position(|a| a.track_index == packet.track_index)
                    .expect("test packet track exists")
            }
        };
        let (pts, dts_value) = dts.enforce(stream_idx, packet.pts, packet.dts);
        manual_output.extend_from_slice(muxer.mux_packet(
            packet.media_type,
            packet.track_index,
            pts,
            dts_value,
            packet.is_keyframe,
            payload,
        ));
    }

    assert_eq!(feeder_output, manual_output);
}

#[test]
fn feeder_remuxed_h265_multi_audio_fixture_decodes_cleanly() {
    let fixture = crate::test_fixtures::bench_transport_fixture("h265", "1.5M", true)
        .expect("fixture must exist");
    let input = std::fs::read(&fixture).expect("fixture must be readable");
    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();

    for chunk in input.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer.take_probe().expect("fixture must probe");
    let video = probe.video.expect("fixture must have video");
    let audio_tracks = Arc::new(probe.audio_tracks);
    assert_eq!(
        audio_tracks.len(),
        2,
        "regression fixture must keep both audio tracks"
    );

    let mut feeder = TsPacketFeeder::new(Some(&video), audio_tracks, PacketFeedConfig::default());
    let mut output = Vec::new();
    for packet in &packets {
        feeder.extend_ts_for_packet(packet, &mut output);
    }
    assert!(
        !output.is_empty(),
        "remuxed fixture must produce MPEG-TS bytes"
    );

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock must be valid")
        .as_nanos();
    let output_path = std::env::temp_dir().join(format!(
        "restream-h265-multi-remux-{}-{unique}.ts",
        std::process::id()
    ));
    std::fs::write(&output_path, &output).expect("remuxed fixture must be writable");

    let ffmpeg = std::process::Command::new(crate::ffmpeg_extract::ensure_ffmpeg_extracted())
        .args(["-nostdin", "-hide_banner", "-v", "warning", "-i"])
        .arg(&output_path)
        .args(["-t", "5", "-map", "0", "-f", "null", "-"])
        .output()
        .expect("ffmpeg must run");
    let _ = std::fs::remove_file(&output_path);
    let stderr = String::from_utf8_lossy(&ffmpeg.stderr);
    let stderr_lower = stderr.to_ascii_lowercase();

    assert!(
        ffmpeg.status.success(),
        "ffmpeg should decode remuxed fixture: {stderr}"
    );
    assert!(
        !stderr_lower.contains("non monoton"),
        "remuxed fixture must not contain duplicate DTS: {stderr}"
    );
}
