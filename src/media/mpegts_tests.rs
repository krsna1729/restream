use std::process::Command;

use super::mpegts_probe::*;
use super::*;

#[test]
fn parse_timestamp_round_trip() {
    let ts: i64 = 132000; // 90kHz timestamp
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts, 0x02);
    let parsed = parse_timestamp(&buf);
    assert_eq!(parsed, ts);
}

fn fixture_h264_multiaudio_ts() -> Vec<u8> {
    let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
    let fixture = crate::test_fixtures::checked_in_fixture("media/colorbar-timer-2v16a.mp4")
        .expect("2v16a fixture should exist");
    let output = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-i",
            fixture.to_str().expect("utf-8 fixture path"),
            "-map",
            "0:v:1",
            "-map",
            "0:a",
            "-c",
            "copy",
            "-t",
            "1",
            "-f",
            "mpegts",
            "pipe:1",
        ])
        .output()
        .expect("spawn bundled ffmpeg for multiaudio fixture");
    assert!(
        output.status.success(),
        "ffmpeg multiaudio fixture extraction failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "fixture TS segment should not be empty"
    );
    output.stdout
}

fn h264_stream_info(pid: u16) -> StreamInfo {
    StreamInfo {
        pid,
        kind: StreamKind::H264,
        track_index: 0,
        language: None,
        title: None,
        continuity: CC_UNSET,
        pes: PesAccumulator::new(),
    }
}

fn aac_adts_stream_info(pid: u16, track_index: u32) -> StreamInfo {
    StreamInfo {
        pid,
        kind: StreamKind::AacAdts,
        track_index,
        language: None,
        title: None,
        continuity: CC_UNSET,
        pes: PesAccumulator::new(),
    }
}

fn first_probe_ready_payloads() -> (Vec<u8>, Vec<u8>) {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let video = packets
        .iter()
        .find(|packet| {
            packet.media_type == MediaType::Video
                && video_meta_complete(
                    StreamKind::H264,
                    &probe_video(StreamKind::H264, 0x100, None, None, packet.payload.as_ref()),
                )
        })
        .map(|packet| packet.payload.to_vec())
        .expect("fixture should contain a probe-ready H.264 access unit");
    let audio = packets
        .iter()
        .find(|packet| {
            packet.media_type == MediaType::Audio
                && audio_meta_complete(
                    StreamKind::AacAdts,
                    &probe_audio(
                        StreamKind::AacAdts,
                        0,
                        0x101,
                        None,
                        None,
                        packet.payload.as_ref(),
                    ),
                )
        })
        .map(|packet| packet.payload.to_vec())
        .expect("fixture should contain a probe-ready AAC access unit");
    (video, audio)
}

#[test]
fn parse_timestamp_large_value() {
    let ts: i64 = 8_589_934_591; // max 33-bit value
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts, 0x03);
    let parsed = parse_timestamp(&buf);
    assert_eq!(parsed, ts);
}

#[test]
fn ts_ms_conversion() {
    assert_eq!(ts_to_ms(90000), 1000);
    assert_eq!(ts_to_ms(0), 0);
    assert_eq!(ms_to_ts(1000), 90000);
    assert_eq!(ms_to_ts(0), 0);
}

#[test]
fn ts_to_ms_no_float_drift() {
    // Verify no floating-point drift at 24-hour scale.
    // At 90 kHz, 24 hours = 24*3600*90000 = 7_776_000_000 ticks.
    // f64 has 53-bit mantissa; at this scale each ULP is ~1024 ticks = ~11 ms.
    // Integer division: ts / 90 must give exact ms with no drift.
    let day_90k: i64 = 24 * 3600 * 90_000;
    let day_ms: i64 = 24 * 3600 * 1000;
    assert_eq!(
        ts_to_ms(day_90k),
        day_ms,
        "ts_to_ms must be exact for 24-hour timestamps (no f64 drift)"
    );
    // Also verify round-trip for a one-hour mark
    let hour_90k: i64 = 3600 * 90_000;
    let hour_ms: i64 = 3600 * 1000;
    assert_eq!(ts_to_ms(hour_90k), hour_ms);
}

#[test]
fn crc32_known_value() {
    // PAT with known CRC
    let data = [
        0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00,
    ];
    let crc = crc32_mpeg2(&data);
    assert_ne!(crc, 0); // Just verify it produces a non-trivial value
    // The expected CRC32/MPEG-2 of this PAT payload is 0xE8F95E7D
    assert_eq!(crc, 0xE8F95E7D);
}

#[test]
fn crc32_bit_at_a_time_equivalence() {
    // Local reference implementation of the bit-at-a-time algorithm
    let reference_crc = |data: &[u8]| {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= (byte as u32) << 24;
            for _ in 0..8 {
                if crc & 0x8000_0000 != 0 {
                    crc = (crc << 1) ^ 0x04C1_1DB7;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    };

    // Test with different sizes and randomized inputs
    let mut rng = 12345u32;
    let mut next_random_byte = || {
        // simple LCG generator
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        (rng >> 24) as u8
    };

    for size in [0, 1, 4, 12, 188, 1024, 4096] {
        for _ in 0..10 {
            let data: Vec<u8> = (0..size).map(|_| next_random_byte()).collect();
            let ref_val = reference_crc(&data);
            let table_val = crc32_mpeg2(&data);
            assert_eq!(
                table_val, ref_val,
                "Failed equivalence test at size {}",
                size
            );
        }
    }
}

#[test]
fn demux_fixture_file() {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts_data);
    demuxer.flush();
    let packets = demuxer.drain();

    assert!(!packets.is_empty(), "should produce packets");

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();
    assert!(video_count > 0, "should have video packets");
    assert!(audio_count > 0, "should have audio packets");

    // Check that at least one keyframe exists
    let keyframes = packets.iter().filter(|p| p.is_keyframe).count();
    assert!(keyframes > 0, "should have keyframes");

    // PTS should be monotonically non-decreasing per stream
    let mut last_video_pts = i64::MIN;
    let mut last_audio_pts = i64::MIN;
    for pkt in &packets {
        match pkt.media_type {
            MediaType::Video => {
                // DTS must be non-decreasing (PTS can jump with B-frames)
                // Just verify PTS is reasonable (positive)
                assert!(
                    pkt.pts >= 0,
                    "video PTS should be non-negative: {}",
                    pkt.pts
                );
                last_video_pts = pkt.pts;
            }
            MediaType::Audio => {
                assert!(
                    pkt.pts >= last_audio_pts,
                    "audio PTS should be non-decreasing: {} < {}",
                    pkt.pts,
                    last_audio_pts
                );
                last_audio_pts = pkt.pts;
            }
        }
    }
    let _ = last_video_pts;

    // Check probe
    let mut demuxer2 = TsDemuxer::new();
    demuxer2.feed(&ts_data);
    demuxer2.flush();
    let probe = demuxer2.take_probe();
    assert!(probe.is_some(), "should produce probe result");
    let probe = probe.unwrap();

    if let Some(ref video) = probe.video {
        assert_eq!(video.codec, "h264");
        assert_eq!(video.width, 1920);
        assert_eq!(video.height, 1080);
        assert!(
            (video.fps - 30.0).abs() < 1.0,
            "fps should be ~30: {}",
            video.fps
        );
        assert_eq!(video.profile.as_deref(), Some("High"));
    } else {
        panic!("should probe video metadata");
    }

    assert!(!probe.audio_tracks.is_empty());
    assert_eq!(probe.audio_tracks[0].codec, "aac");
    assert_eq!(probe.audio_tracks[0].sample_rate, 48000);
}

#[test]
fn drain_into_reuses_output_batches() {
    let mut demuxer = TsDemuxer::new();
    let mut output = Vec::with_capacity(4);

    assert_eq!(demuxer.drain_into(&mut output), 0);
    assert!(output.is_empty());
    assert!(output.capacity() >= 4);
}

#[test]
fn muxer_writes_service_metadata_sdt_packet() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 1280,
        height: 720,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let metadata = TsServiceMetadata {
        provider_name: "Restream pipeline_id=pipe-1".to_string(),
        service_name: "pipeline=Main; source=publisher; recorded_at=2026-06-27T00:00:00Z"
            .to_string(),
    };
    let mut muxer = TsMuxer::new_with_metadata(Some(&video), &[], metadata);
    let payload = [0x00, 0x00, 0x01, 0x09, 0x10];
    let ts = muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &payload);

    assert!(
        ts.chunks(TS_PACKET_SIZE).any(|pkt| {
            pkt.len() == TS_PACKET_SIZE
                && pkt[0] == TS_SYNC_BYTE
                && ((((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16) == SDT_PID
        }),
        "first table burst should contain an SDT packet"
    );
    let text = String::from_utf8_lossy(ts);
    assert!(text.contains("Restream pipeline_id=pipe-1"));
    assert!(text.contains("pipeline=Main"));
    assert!(text.contains("source=publisher"));
    assert!(text.contains("recorded_at=2026-06-27T00:00:00Z"));
}

#[test]
fn demux_chunked_feed() {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

    // Feed in 1316-byte chunks (SRT packet size)
    let mut demuxer = TsDemuxer::new();
    for chunk in ts_data.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    let packets = demuxer.drain();

    assert!(!packets.is_empty(), "chunked feed should produce packets");

    // Feed all at once for comparison
    let mut demuxer2 = TsDemuxer::new();
    demuxer2.feed(&ts_data);
    demuxer2.flush();
    let packets2 = demuxer2.drain();

    assert_eq!(
        packets.len(),
        packets2.len(),
        "chunked and full feed should produce same packet count"
    );
}

#[test]
fn demux_h265_fixture_file() {
    let fixture =
        crate::test_fixtures::canonical_h265_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

    let mut demuxer = TsDemuxer::new();
    for chunk in ts_data.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    let packets = demuxer.drain();

    assert!(!packets.is_empty(), "should produce packets");

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();
    assert!(video_count > 0, "should have video packets");
    assert!(audio_count > 0, "should have audio packets");

    let keyframes = packets.iter().filter(|p| p.is_keyframe).count();
    assert!(keyframes > 0, "should have keyframes");

    let mut demuxer2 = TsDemuxer::new();
    demuxer2.feed(&ts_data);
    demuxer2.flush();
    let probe = demuxer2.take_probe();
    assert!(probe.is_some(), "should produce probe result");
    let probe = probe.unwrap();

    if let Some(ref video) = probe.video {
        assert_eq!(video.codec, "hevc");
        assert_eq!(video.width, 1920);
        assert_eq!(video.height, 1080);
        assert!(
            (video.fps - 30.0).abs() < 1.0,
            "fps should be ~30: {}",
            video.fps
        );
        assert_eq!(video.profile.as_deref(), Some("Main"));
    } else {
        panic!("should probe video metadata");
    }

    assert!(!probe.audio_tracks.is_empty());
    assert_eq!(probe.audio_tracks[0].codec, "aac");
    assert_eq!(probe.audio_tracks[0].sample_rate, 48000);
}

#[test]
fn demux_corrupt_input_no_panic() {
    let mut demuxer = TsDemuxer::new();

    // Empty input
    demuxer.feed(&[]);
    assert!(demuxer.drain().is_empty());

    // Random garbage
    demuxer.feed(&[0xDE, 0xAD, 0xBE, 0xEF, 0x47, 0x00]);
    assert!(demuxer.drain().is_empty());

    // Truncated TS packet
    let short = vec![0x47u8; 100];
    demuxer.feed(&short);
    assert!(demuxer.drain().is_empty());

    // All zeros
    demuxer.feed(&[0u8; 188]);
    assert!(demuxer.drain().is_empty());
}

#[test]
fn mux_round_trip() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: Some("High".to_string()),
        level: Some("3.0".to_string()),
        pixel_format: None,
    };

    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio]);

    // Create test packets
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC]; // IDR NAL
    let audio_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0xDE, 0x02]; // ADTS

    let ts_out1 = muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &video_payload);
    assert!(!ts_out1.is_empty());
    assert_eq!(ts_out1.len() % TS_PACKET_SIZE, 0);

    let ts_out2 = muxer.mux_packet(MediaType::Audio, 0, 0, 0, false, &audio_payload);
    assert!(!ts_out2.is_empty());
    assert_eq!(ts_out2.len() % TS_PACKET_SIZE, 0);
}

#[test]
fn muxer_enforces_strict_dts_when_ms_timestamps_repeat() {
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(None, &[audio]);
    let audio_payload = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0xDE, 0x02];

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert_eq!(muxer.last_dts_90k[0], 90_000);

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert_eq!(
        muxer.last_dts_90k[0], 90_001,
        "equal millisecond DTS must not become equal 90 kHz DTS"
    );
}

#[test]
fn muxer_tracks_strict_dts_per_audio_track() {
    let audio0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: Some("eng".to_string()),
        title: None,
        profile: None,
    };
    let audio1 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 1,
        pid: None,
        language: Some("spa".to_string()),
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(None, &[audio0, audio1]);
    let audio_payload = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0xDE, 0x02];

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 1, 1000, 1000, false, &audio_payload)
            .is_empty()
    );
    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &audio_payload)
            .is_empty()
    );

    assert_eq!(muxer.last_dts_90k[0], 90_001);
    assert_eq!(
        muxer.last_dts_90k[1], 90_000,
        "DTS repair must be isolated per elementary stream"
    );
}

#[test]
fn muxer_reserves_internal_adts_frame_timestamps() {
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    let mut muxer = TsMuxer::new(None, &[audio]);
    let mut two_frame_payload = Vec::new();
    let frame_a = crate::media::codec::build_adts_header(2, 48000, 2);
    two_frame_payload.extend_from_slice(&frame_a);
    two_frame_payload.extend_from_slice(&[0x11, 0x22]);
    let frame_b = crate::media::codec::build_adts_header(2, 48000, 2);
    two_frame_payload.extend_from_slice(&frame_b);
    two_frame_payload.extend_from_slice(&[0x33, 0x44]);

    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1000, 1000, false, &two_frame_payload)
            .is_empty()
    );
    assert_eq!(
        muxer.last_dts_90k[0], 91_920,
        "two 48 kHz AAC frames occupy the PES start plus one 1024-sample step"
    );

    let mut one_frame_payload = Vec::new();
    let frame = crate::media::codec::build_adts_header(2, 48000, 2);
    one_frame_payload.extend_from_slice(&frame);
    one_frame_payload.extend_from_slice(&[0x55, 0x66]);
    assert!(
        !muxer
            .mux_packet(MediaType::Audio, 0, 1021, 1021, false, &one_frame_payload)
            .is_empty()
    );
    assert_eq!(
        muxer.last_dts_90k[0], 91_921,
        "next PES must start after the previous payload's final internal ADTS frame"
    );
}

#[test]
fn mux_demux_round_trip() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio]);
    let mut all_ts = Vec::new();

    // Mux a few packets
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84];
    let audio_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC];

    for i in 0..5 {
        let pts = i * 33; // ~30fps
        let ts = muxer.mux_packet(MediaType::Video, 0, pts, pts, i == 0, &video_payload);
        all_ts.extend_from_slice(ts);

        let ts = muxer.mux_packet(MediaType::Audio, 0, pts, pts, false, &audio_payload);
        all_ts.extend_from_slice(ts);
    }

    // Demux it back
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();

    assert_eq!(video_count, 5, "round-trip should preserve video count");
    assert_eq!(audio_count, 5, "round-trip should preserve audio count");

    // First video should be keyframe
    let first_video = packets
        .iter()
        .find(|p| p.media_type == MediaType::Video)
        .unwrap();
    assert!(first_video.is_keyframe, "first video should be keyframe");
}

#[test]
fn h265_demux_requires_irap_for_keyframe_even_with_random_access_bit() {
    let video = VideoMeta {
        codec: "hevc".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[]);
    let trail_r_payload = vec![0x00, 0x00, 0x00, 0x01, 0x02u8, 0x01, 0xDD];
    let ts = muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &trail_r_payload);

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let video_packet = packets
        .iter()
        .find(|packet| packet.media_type == MediaType::Video)
        .expect("demuxed HEVC video packet");

    assert!(
        !video_packet.is_keyframe,
        "HEVC random_access alone must not mark non-IRAP payloads keyframe"
    );
}

#[test]
fn mux_demux_two_audio_tracks_round_trip() {
    // TsMuxer assigns separate PIDs to each audio track.
    // TsDemuxer must recover both with distinct track_index values
    // and correct packet counts.
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 320,
        height: 240,
        fps: 30.0,
        bw: None,
        pid: Some(0x100),
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: Some("eng".to_string()),
        title: None,
        profile: None,
    };
    let audio1 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 1,
        channel_layout: None,
        track_index: 1,
        pid: None,
        language: Some("spa".to_string()),
        title: None,
        profile: None,
    };

    let mut muxer = TsMuxer::new(Some(&video), &[audio0, audio1]);
    let mut all_ts = Vec::new();

    // Probe-ready H.264 access unit (contains SPS/PPS) so the demuxer's
    // metadata-completeness gate can build the probe.
    let (video_payload, _) = first_probe_ready_payloads();
    // ADTS frame for AAC-LC 48 kHz stereo (7-byte header, no CRC)
    let audio0_payload = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];
    // ADTS frame for AAC-LC 44.1 kHz mono
    let audio1_payload = vec![0xFF, 0xF1, 0x58, 0x40, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    for i in 0..3u32 {
        let pts = (i as i64) * 33;
        let ts = muxer.mux_packet(MediaType::Video, 0, pts, pts, i == 0, &video_payload);
        all_ts.extend_from_slice(ts);
        let ts = muxer.mux_packet(MediaType::Audio, 0, pts, pts, false, &audio0_payload);
        all_ts.extend_from_slice(ts);
        let ts = muxer.mux_packet(MediaType::Audio, 1, pts, pts, false, &audio1_payload);
        all_ts.extend_from_slice(ts);
    }

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();

    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    let audio0_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio && p.track_index == 0)
        .count();
    let audio1_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio && p.track_index == 1)
        .count();

    assert_eq!(video_count, 3, "should demux 3 video packets");
    assert_eq!(audio0_count, 3, "should demux 3 packets for audio track 0");
    assert_eq!(audio1_count, 3, "should demux 3 packets for audio track 1");

    // Verify both audio track indices appear in the demuxed stream
    let audio_track_indices: std::collections::HashSet<u32> = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .map(|p| p.track_index)
        .collect();
    assert!(
        audio_track_indices.contains(&0),
        "track_index 0 must be present"
    );
    assert!(
        audio_track_indices.contains(&1),
        "track_index 1 must be present"
    );

    let probe = demuxer
        .take_probe()
        .expect("round-trip should produce a probe");
    assert_eq!(probe.video.as_ref().and_then(|v| v.pid), Some(0x100));
    assert_eq!(probe.audio_tracks.len(), 2);
    assert_eq!(probe.audio_tracks[0].pid, Some(0x101));
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(probe.audio_tracks[1].pid, Some(0x102));
    assert_eq!(probe.audio_tracks[1].language.as_deref(), Some("spa"));
}

#[test]
fn mux_demux_32_audio_tracks_spans_pmt_packets() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let languages = [
        "eng", "spa", "fra", "deu", "ita", "por", "nld", "swe", "nor", "dan", "fin", "pol", "ces",
        "slk", "hun", "ron", "bul", "ell", "tur", "rus", "ukr", "ara", "heb", "hin", "tam", "tel",
        "jpn", "kor", "zho", "vie", "tha", "ind",
    ];
    let audio_tracks = languages
        .iter()
        .enumerate()
        .map(|(index, language)| AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 1,
            channel_layout: None,
            track_index: index as u32,
            pid: None,
            language: Some((*language).to_string()),
            title: None,
            profile: None,
        })
        .collect::<Vec<_>>();

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut all_ts = Vec::new();
    // Probe-ready H.264 access unit (contains SPS/PPS) so the demuxer's
    // metadata-completeness gate can build the probe.
    let (video_payload, _) = first_probe_ready_payloads();
    let audio_payload = vec![0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    all_ts.extend_from_slice(muxer.mux_packet(MediaType::Video, 0, 0, 0, true, &video_payload));
    for index in 0..audio_tracks.len() {
        all_ts.extend_from_slice(muxer.mux_packet(
            MediaType::Audio,
            index as u32,
            0,
            0,
            false,
            &audio_payload,
        ));
    }

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&all_ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("32-track round-trip should produce a probe");

    assert_eq!(probe.video.as_ref().and_then(|v| v.pid), Some(0x100));
    assert_eq!(probe.audio_tracks.len(), 32);
    assert_eq!(probe.audio_tracks[0].pid, Some(0x101));
    assert_eq!(probe.audio_tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(probe.audio_tracks[31].pid, Some(0x120));
    assert_eq!(probe.audio_tracks[31].language.as_deref(), Some("ind"));
    assert_eq!(
        packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        32
    );
}

#[test]
fn real_fixture_probe_recovers_all_16_audio_tracks() {
    let ts = fixture_h264_multiaudio_ts();
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let probe = demuxer
        .take_probe()
        .expect("fixture probe should discover stream metadata");

    assert!(
        probe.video.is_some(),
        "fixture should contain a video stream"
    );
    assert_eq!(probe.video_track_count, 1);
    assert_eq!(
        probe.audio_tracks.len(),
        16,
        "fixture should expose 16 audio tracks"
    );
    assert_eq!(
        probe
            .audio_tracks
            .iter()
            .map(|track| track.track_index)
            .collect::<Vec<_>>(),
        (0..16).collect::<Vec<_>>()
    );
    assert_eq!(
        packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        16,
        "fixture packets should cover all 16 logical audio tracks"
    );
}

#[test]
fn try_build_probe_waits_for_complete_h264_and_aac_metadata() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80]);
    demuxer.try_build_probe(1, &complete_audio);
    assert!(
        demuxer.take_probe().is_none(),
        "probe must wait for complete video dimensions instead of locking in 0x0 metadata"
    );

    demuxer.try_build_probe(0, &complete_video);
    let probe = demuxer
        .take_probe()
        .expect("probe should finalize once both tracks have complete metadata");
    let video = probe.video.expect("probe should include video metadata");
    assert!(video.width > 0);
    assert!(video.height > 0);
    assert_eq!(probe.audio_tracks.len(), 1);
    assert!(probe.audio_tracks[0].sample_rate > 0);
    assert!(probe.audio_tracks[0].channels > 0);
}

#[test]
fn try_build_probe_keeps_complete_payload_when_later_frames_lack_sps() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &complete_video);
    demuxer.try_build_probe(0, &[0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x00]);
    assert!(demuxer.take_probe().is_none());

    demuxer.try_build_probe(1, &complete_audio);
    let probe = demuxer
        .take_probe()
        .expect("probe must survive non-SPS frames after complete video metadata");
    let video = probe.video.expect("probe should include video metadata");
    assert!(video.width > 0);
    assert!(video.height > 0);
}

#[test]
fn try_build_probe_caches_h264_sequence_header() {
    let (complete_video, complete_audio) = first_probe_ready_payloads();
    let mut demuxer = TsDemuxer::new();
    demuxer.streams = vec![h264_stream_info(0x100), aac_adts_stream_info(0x101, 0)];

    demuxer.try_build_probe(0, &complete_video);
    demuxer.try_build_probe(1, &complete_audio);

    let probe = demuxer
        .take_probe()
        .expect("probe should finalize once both tracks are complete");
    let sequence_header = probe
        .video_sequence_header
        .expect("H.264 probe should synthesize an RTMP startup header");
    assert_eq!(sequence_header[0], 0x17);
    assert_eq!(sequence_header[1], 0x00);
}

#[test]
fn remux_segment_view_splits_video_and_selected_audio() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let audio_tracks = (0..16)
        .map(|index| AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        })
        .collect::<Vec<_>>();

    let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
    let mut source = Vec::new();
    let video_payload = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88];
    let audio_payload = vec![0xFF, 0xF1, 0x4C, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];

    for frame in 0..3 {
        let pts = frame * 33;
        source.extend_from_slice(muxer.mux_packet(
            MediaType::Video,
            0,
            pts,
            pts,
            frame == 0,
            &video_payload,
        ));
        for track_index in 0..audio_tracks.len() {
            source.extend_from_slice(muxer.mux_packet(
                MediaType::Audio,
                track_index as u32,
                pts,
                pts,
                false,
                &audio_payload,
            ));
        }
    }

    let video_only = remux_segment_view(&source, Some(&video), &audio_tracks, TsSegmentView::Video)
        .expect("video rendition should contain media");
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&video_only);
    demuxer.flush();
    let packets = demuxer.drain();
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Video)
    );

    let audio_only = remux_segment_view(
        &source,
        Some(&video),
        &audio_tracks,
        TsSegmentView::Audio(15),
    )
    .expect("audio rendition should contain media");
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&audio_only);
    demuxer.flush();
    let packets = demuxer.drain();
    assert!(
        packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio)
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Audio)
    );
    assert_eq!(
        packets
            .iter()
            .map(|packet| packet.track_index)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1,
        "audio rendition should expose exactly one logical track"
    );
}

#[test]
fn nal_scanner_h264_idr() {
    // Start code + IDR NAL
    let data = [0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB];
    assert!(h264_is_keyframe(&data));

    // Start code + non-IDR slice
    let data2 = [0x00, 0x00, 0x00, 0x01, 0x41, 0xAA, 0xBB];
    assert!(!h264_is_keyframe(&data2));
}

#[test]
fn h265_irap_detection() {
    // H.265 NAL header: byte0 = forbidden(1b) | nal_unit_type(6b) >> ... encoded as (type << 1)
    // IDR_W_RADL = type 19 → byte0 = (19 << 1) = 0x26, byte1 = 0x01 (layer=0, tid=1)
    // for_each_nal_h265 extracts: (byte0 >> 1) & 0x3F = (0x26 >> 1) & 0x3F = 19 ✓
    let idr_nal = vec![0x00, 0x00, 0x00, 0x01, 0x26u8, 0x01, 0xAA, 0xBB];
    assert!(
        h265_is_keyframe(&idr_nal),
        "IDR_W_RADL (type 19) should be a keyframe"
    );

    // IDR_N_LP = type 20 → byte0 = (20 << 1) = 0x28
    let idr_nlp = vec![0x00, 0x00, 0x00, 0x01, 0x28u8, 0x01, 0xCC];
    assert!(
        h265_is_keyframe(&idr_nlp),
        "IDR_N_LP (type 20) should be a keyframe"
    );

    // Non-IRAP: TRAIL_R = type 1 → byte0 = (1 << 1) = 0x02
    let trail_r = vec![0x00, 0x00, 0x00, 0x01, 0x02u8, 0x01, 0xDD];
    assert!(
        !h265_is_keyframe(&trail_r),
        "TRAIL_R (type 1) should not be a keyframe"
    );

    // CRA_NUT = type 21 → byte0 = (21 << 1) = 0x2A
    // CRA is commonly produced by software encoders (ffmpeg, x265) and hardware
    // encoders. Must be treated as a keyframe for ring-buffer overflow recovery.
    let cra = vec![0x00, 0x00, 0x00, 0x01, 0x2Au8, 0x01, 0xEE];
    assert!(
        h265_is_keyframe(&cra),
        "CRA_NUT (type 21) should be a keyframe"
    );

    // BLA_W_LP = type 16 → byte0 = (16 << 1) = 0x20 (low boundary of IRAP range)
    let bla = vec![0x00, 0x00, 0x00, 0x01, 0x20u8, 0x01, 0xFF];
    assert!(
        h265_is_keyframe(&bla),
        "BLA_W_LP (type 16) should be a keyframe"
    );

    // Type 15 (non-IRAP, just below boundary) → byte0 = (15 << 1) = 0x1E
    let non_irap_below = vec![0x00, 0x00, 0x00, 0x01, 0x1Eu8, 0x01, 0x00];
    assert!(
        !h265_is_keyframe(&non_irap_below),
        "Type 15 is non-IRAP, should not be a keyframe"
    );

    // Type 24 (just above IRAP range) → byte0 = (24 << 1) = 0x30
    let non_irap_above = vec![0x00, 0x00, 0x00, 0x01, 0x30u8, 0x01, 0x00];
    assert!(
        !h265_is_keyframe(&non_irap_above),
        "Type 24 is non-IRAP, should not be a keyframe"
    );
}

// --- NAL scanner edge cases ---

#[test]
fn h264_is_keyframe_empty_payload_returns_false() {
    assert!(!h264_is_keyframe(&[]));
}

#[test]
fn h264_is_keyframe_no_start_codes_returns_false() {
    // Non-Annex B data, no 0x000001 or 0x00000001 start code
    assert!(!h264_is_keyframe(&[0x00, 0x01, 0x65, 0x88]));
}

#[test]
fn h265_is_keyframe_empty_payload_returns_false() {
    assert!(!h265_is_keyframe(&[]));
}

#[test]
fn h265_is_keyframe_no_start_codes_returns_false() {
    assert!(!h265_is_keyframe(&[0x00, 0x01, 0x26, 0x01]));
}

#[test]
fn find_h264_sps_no_sps_nal_returns_none() {
    // IDR slice (nal_type=5), no SPS (nal_type=7) present
    let data = [0x00, 0x00, 0x00, 0x01, 0x65u8, 0xAA, 0xBB];
    assert!(find_h264_sps(&data).is_none());
}

#[test]
fn find_h264_sps_empty_returns_none() {
    assert!(find_h264_sps(&[]).is_none());
}

#[test]
fn find_h264_sps_extracts_sps_nal() {
    // SPS NAL: nal_type=7 (byte & 0x1F == 7)
    // find_h264_sps returns NAL data after the first byte (the header byte)
    let data = [0x00, 0x00, 0x00, 0x01, 0x67u8, 0x64, 0x00, 0x1F];
    let sps = find_h264_sps(&data);
    assert!(sps.is_some(), "SPS NAL type 7 must be found");
    // Returns data after the NAL header byte (0x67)
    assert_eq!(sps.unwrap(), vec![0x64, 0x00, 0x1F]);
}

#[test]
fn find_h265_sps_no_sps_returns_none() {
    // H.265 IDR (nal_type 19, byte0=(19<<1)=0x26), not SPS (nal_type 33)
    let data = [0x00, 0x00, 0x00, 0x01, 0x26u8, 0x01, 0xAA];
    assert!(find_h265_sps(&data).is_none());
}

#[test]
fn find_h265_sps_empty_returns_none() {
    assert!(find_h265_sps(&[]).is_none());
}

#[test]
fn find_h265_sps_extracts_sps_payload() {
    // H.265 SPS: nal_unit_type=33 → byte0=(33<<1)=0x42, byte1=nuh_layer/temporal
    // find_h265_sps returns sps[2..] (skips the 2-byte NAL header)
    let data = [0x00, 0x00, 0x00, 0x01, 0x42u8, 0x01, 0xAA, 0xBB, 0xCC];
    let sps = find_h265_sps(&data);
    assert!(sps.is_some(), "H.265 SPS (type 33) must be found");
    assert_eq!(sps.unwrap(), vec![0xAA, 0xBB, 0xCC]);
}

#[test]
fn parse_h265_sps_too_short_does_not_panic() {
    // < 2 bytes → early return without panic
    let mut meta = VideoMeta::default();
    parse_h265_sps(&[], &mut meta);
    assert_eq!(meta.width, 0);
    parse_h265_sps(&[0x42], &mut meta);
    assert_eq!(meta.width, 0);
}

#[test]
fn mux_audio_only_does_not_panic() {
    // TsMuxer with no video track, one audio track.
    // This path is used when a pipeline receives audio-only ingest.
    let audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    let mut muxer = TsMuxer::new(None, &[audio]);
    let adts = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0x21, 0x10];
    let ts = muxer.mux_packet(MediaType::Audio, 0, 1000, 1000, false, &adts);
    assert!(!ts.is_empty(), "audio-only muxer must produce TS packets");
    assert_eq!(
        ts.len() % TS_PACKET_SIZE,
        0,
        "output must be aligned to TS packet size"
    );

    // Demux round-trip: audio packets must survive
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let audio_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Audio)
        .count();
    assert!(
        audio_count > 0,
        "audio-only round-trip must produce audio packets"
    );
    let video_count = packets
        .iter()
        .filter(|p| p.media_type == MediaType::Video)
        .count();
    assert_eq!(
        video_count, 0,
        "audio-only stream must contain no video packets"
    );
}

#[test]
fn pat_pmt_parsing() {
    // Build a minimal PAT + PMT
    let mut ts_data = Vec::new();

    // PAT packet
    let mut pat_pkt = [0xFFu8; 188];
    pat_pkt[0] = 0x47;
    pat_pkt[1] = 0x40; // PUSI, PID=0
    pat_pkt[2] = 0x00;
    pat_pkt[3] = 0x10; // payload only, CC=0
    pat_pkt[4] = 0x00; // pointer
    pat_pkt[5] = 0x00; // table_id = PAT
    pat_pkt[6] = 0xB0;
    pat_pkt[7] = 13; // section_length
    pat_pkt[8] = 0x00;
    pat_pkt[9] = 0x01; // TSID
    pat_pkt[10] = 0xC1; // version
    pat_pkt[11] = 0x00;
    pat_pkt[12] = 0x00;
    // Program 1 → PMT PID 0x1000
    pat_pkt[13] = 0x00;
    pat_pkt[14] = 0x01;
    pat_pkt[15] = 0xF0;
    pat_pkt[16] = 0x00;
    let crc = crc32_mpeg2(&pat_pkt[5..17]);
    pat_pkt[17] = (crc >> 24) as u8;
    pat_pkt[18] = (crc >> 16) as u8;
    pat_pkt[19] = (crc >> 8) as u8;
    pat_pkt[20] = crc as u8;
    ts_data.extend_from_slice(&pat_pkt);

    // PMT packet (1 video + 1 audio)
    let mut pmt_pkt = [0xFFu8; 188];
    pmt_pkt[0] = 0x47;
    pmt_pkt[1] = 0x50; // PUSI, PID=0x1000
    pmt_pkt[2] = 0x00;
    pmt_pkt[3] = 0x10;
    pmt_pkt[4] = 0x00;
    pmt_pkt[5] = 0x02; // table_id = PMT
    let section_len = 9 + 10 + 4; // 9 fixed + 2 streams — 5 + CRC
    pmt_pkt[6] = 0xB0;
    pmt_pkt[7] = section_len as u8;
    pmt_pkt[8] = 0x00;
    pmt_pkt[9] = 0x01;
    pmt_pkt[10] = 0xC1;
    pmt_pkt[11] = 0x00;
    pmt_pkt[12] = 0x00;
    pmt_pkt[13] = 0xE1;
    pmt_pkt[14] = 0x00; // PCR PID = 0x100
    pmt_pkt[15] = 0xF0;
    pmt_pkt[16] = 0x00; // program_info_length = 0
    // Video: H.264, PID=0x100
    pmt_pkt[17] = 0x1B;
    pmt_pkt[18] = 0xE1;
    pmt_pkt[19] = 0x00;
    pmt_pkt[20] = 0xF0;
    pmt_pkt[21] = 0x00;
    // Audio: AAC, PID=0x101
    pmt_pkt[22] = 0x0F;
    pmt_pkt[23] = 0xE1;
    pmt_pkt[24] = 0x01;
    pmt_pkt[25] = 0xF0;
    pmt_pkt[26] = 0x00;
    let crc2 = crc32_mpeg2(&pmt_pkt[5..27]);
    pmt_pkt[27] = (crc2 >> 24) as u8;
    pmt_pkt[28] = (crc2 >> 16) as u8;
    pmt_pkt[29] = (crc2 >> 8) as u8;
    pmt_pkt[30] = crc2 as u8;
    ts_data.extend_from_slice(&pmt_pkt);

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts_data);

    assert!(demuxer.has_streams());
    assert_eq!(demuxer.streams.len(), 2);
    assert_eq!(demuxer.streams[0].kind, StreamKind::H264);
    assert_eq!(demuxer.streams[0].pid, 0x100);
    assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
    assert_eq!(demuxer.streams[1].pid, 0x101);
}

#[test]
fn adts_probe() {
    // Valid ADTS header: 48kHz, mono
    let adts = [0xFF, 0xF1, 0x4C, 0x40, 0x02, 0x1F, 0xFC];
    let meta = probe_audio(StreamKind::AacAdts, 0, 0x101, None, None, &adts);
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.channels, 1);
}

// --- Helpers shared by PMT version tests ---

/// Build a 188-byte TS PAT packet pointing to PMT PID 0x1000.
fn make_pat_ts_pkt() -> Vec<u8> {
    let mut pkt = vec![0xFFu8; 188];
    pkt[0] = 0x47;
    pkt[1] = 0x40; // PUSI, PID=0x0000
    pkt[2] = 0x00;
    pkt[3] = 0x10; // payload-only, CC=0
    pkt[4] = 0x00; // pointer_field
    // PAT section
    pkt[5] = 0x00; // table_id = PAT
    pkt[6] = 0xB0;
    pkt[7] = 13; // section_length
    pkt[8] = 0x00;
    pkt[9] = 0x01; // TSID = 1
    pkt[10] = 0xC1; // version=0, current_next=1
    pkt[11] = 0x00; // section_number
    pkt[12] = 0x00; // last_section_number
    // Program 1 -> PMT PID 0x1000
    pkt[13] = 0x00;
    pkt[14] = 0x01;
    pkt[15] = 0xF0; // 0xE0 | (0x1000 >> 8) = 0xF0
    pkt[16] = 0x00; // 0x1000 & 0xFF
    let crc = crc32_mpeg2(&pkt[5..17]);
    pkt[17] = (crc >> 24) as u8;
    pkt[18] = (crc >> 16) as u8;
    pkt[19] = (crc >> 8) as u8;
    pkt[20] = crc as u8;
    pkt
}

/// Build a 188-byte TS PMT packet at PID 0x1000 with the given version and
/// stream list. Each stream is `(stream_type, elementary_pid)`.
fn make_pmt_ts_pkt(version: u8, streams: &[(u8, u16)]) -> Vec<u8> {
    let mut pkt = vec![0xFFu8; 188];
    pkt[0] = 0x47;
    pkt[1] = 0x50; // PUSI, PID high (0x1000)
    pkt[2] = 0x00; // PID low
    pkt[3] = 0x10; // payload-only, CC=0
    pkt[4] = 0x00; // pointer_field
    // PMT section
    pkt[5] = 0x02; // table_id = PMT
    let section_len = 9 + (5 * streams.len()) + 4;
    pkt[6] = 0xB0;
    pkt[7] = section_len as u8;
    pkt[8] = 0x00; // program_number high
    pkt[9] = 0x01; // program_number low
    // version_number in bits 5..1, current_next_indicator in bit 0
    pkt[10] = 0xC0 | ((version & 0x1F) << 1) | 0x01;
    pkt[11] = 0x00; // section_number
    pkt[12] = 0x00; // last_section_number
    pkt[13] = 0xE1; // PCR_PID = 0x100
    pkt[14] = 0x00;
    pkt[15] = 0xF0; // program_info_length high
    pkt[16] = 0x00; // program_info_length low (= 0)
    let mut pos = 17usize;
    for &(stream_type, pid) in streams {
        pkt[pos] = stream_type;
        pkt[pos + 1] = 0xE0 | ((pid >> 8) as u8 & 0x1F);
        pkt[pos + 2] = (pid & 0xFF) as u8;
        pkt[pos + 3] = 0xF0; // ES_info_length = 0
        pkt[pos + 4] = 0x00;
        pos += 5;
    }
    let crc = crc32_mpeg2(&pkt[5..pos]);
    pkt[pos] = (crc >> 24) as u8;
    pkt[pos + 1] = (crc >> 16) as u8;
    pkt[pos + 2] = (crc >> 8) as u8;
    pkt[pos + 3] = crc as u8;
    pkt
}

// --- Regression: issue #3 — PMT version tracking ---

#[test]
fn pmt_retransmission_same_version_is_idempotent() {
    // Regression: the old guard `if !self.streams.is_empty() && pmt_expected == 0 { return }`
    // was replaced by explicit version tracking. A retransmission of the same
    // PMT version must NOT rebuild the stream map (no phantom duplicates).
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    let streams = [(0x1B, 0x100u16), (0x0F, 0x101u16)]; // H.264 + AAC
    data.extend_from_slice(&make_pmt_ts_pkt(0, &streams));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "initial PMT must produce 2 streams"
    );
    assert_eq!(demuxer.pmt_version, 0);

    // Feed the same PMT version again (broadcaster retransmits every ~100ms)
    let retransmit = make_pmt_ts_pkt(0, &streams);
    demuxer.feed(&retransmit);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "retransmitting the same PMT version must not rebuild the stream map"
    );
    assert_eq!(demuxer.pmt_version, 0);
}

#[test]
fn pmt_version_change_rebuilds_stream_map() {
    // Regression: the old code returned early on non-empty streams, silently
    // dropping genuine PMT version changes (e.g., broadcaster adds audio mid-stream).
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    // Version 0: video only
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    assert_eq!(demuxer.streams.len(), 1, "PMT v0 must have 1 stream");
    assert_eq!(demuxer.pmt_version, 0);

    // Version 1: broadcaster added an audio track
    let v1_pkt = make_pmt_ts_pkt(1, &[(0x1B, 0x100u16), (0x0F, 0x101u16)]);
    demuxer.feed(&v1_pkt);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "PMT version change must rebuild stream map so new audio PID is parsed"
    );
    assert_eq!(demuxer.pmt_version, 1);
    assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
}

// --- Regression: issue #12 — PCR negative guard ---

#[test]
fn write_pcr_clamps_negative_ts_to_zero() {
    // Regression: before the .max(0) fix, a negative DTS reaching write_pcr
    // would silently cast to a large u64, producing a nonsensical PCR value
    // that makes decoders stall or seek unexpectedly.
    let mut buf_neg = [0u8; 6];
    let mut buf_zero = [0u8; 6];
    write_pcr(&mut buf_neg, -1_000_000);
    write_pcr(&mut buf_zero, 0);
    assert_eq!(
        buf_neg, buf_zero,
        "negative ts_90k must clamp to 0, not wrap to a huge u64 PCR"
    );

    // Also verify an extreme negative value does not panic.
    let mut buf_min = [0u8; 6];
    write_pcr(&mut buf_min, i64::MIN);
    assert_eq!(buf_min, buf_zero);
}

// --- Regression: issue #6 (Round 3) — TsDemuxer remainder length cap ---
// Before the MAX_REMAINDER guard, feeding a stream of single-byte 0x47
// chunks would cause remainder to grow by 1 byte on every call — O(n)
// memory growth per byte of input, i.e. O(n²) overall processing cost
// for a corrupt / adversarial stream.  After the fix the remainder must
// never exceed TS_PACKET_SIZE - 1 = 187 bytes.
#[test]
fn feed_remainder_capped_on_corrupt_stream() {
    let mut dem = TsDemuxer::new();
    // Feed 500 isolated 0x47 bytes (each looks like a TS sync byte but is
    // never followed by 187 more bytes, so no packet can complete).
    for _ in 0..500 {
        dem.feed(&[0x47]);
    }
    assert!(
        dem.remainder.len() < TS_PACKET_SIZE,
        "remainder must be capped at TS_PACKET_SIZE-1 ({}) but was {}",
        TS_PACKET_SIZE - 1,
        dem.remainder.len()
    );
}

// --- Regression: issue #5 (Round 4) — PMT version rebuild preserves in-flight PES ---
// Before the fix, a PMT version change discarded ALL StreamInfo (including
// PesAccumulator buffers).  A partially-assembled video frame would be lost,
// producing a glitch until the next IDR.  After the fix, PES buffers for PIDs
// that survive into the new PMT are carried over.
#[test]
fn pmt_version_change_preserves_pes_for_unchanged_pid() {
    // Build a minimal TS PES packet for PID 0x100 that starts a new PES unit
    // (PUSI=1) but does NOT complete it (no second packet with the next PES
    // start, so the frame stays in the accumulator).
    fn make_pes_ts_pkt(pid: u16, pusi: bool, payload: &[u8]) -> Vec<u8> {
        let mut pkt = vec![0xFFu8; 188];
        pkt[0] = 0x47;
        pkt[1] = if pusi { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1F);
        pkt[2] = (pid & 0xFF) as u8;
        pkt[3] = 0x10; // payload_unit_start = pusi flag handled above; CC=0
        // When PUSI, prepend a minimal PES header: start code + stream_id +
        // length(0=unbounded) + flags + header_data_length(0)
        let pes_header: &[u8] = if pusi {
            &[0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x00, 0x00]
        } else {
            &[]
        };
        let data_offset = 4usize;
        let total = pes_header.len() + payload.len();
        pkt[data_offset..data_offset + total.min(184)]
            .iter_mut()
            .zip(pes_header.iter().chain(payload.iter()))
            .for_each(|(d, &s)| *d = s);
        pkt
    }

    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    // PMT v0: H.264 video at PID 0x100
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));
    // PES start for PID 0x100 — frame not yet complete
    data.extend_from_slice(&make_pes_ts_pkt(0x100, true, &[0xDE, 0xAD]));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    // Verify the PES buffer has data
    assert_eq!(demuxer.streams.len(), 1);
    let buf_before = demuxer.streams[0].pes.buf.clone();
    assert!(
        !buf_before.is_empty(),
        "PES buf must have partial frame data"
    );

    // Now the broadcaster sends a PMT version change for the same PID
    // (e.g., only the language descriptor changed).
    let v1_pkt = make_pmt_ts_pkt(1, &[(0x1B, 0x100u16)]);
    demuxer.feed(&v1_pkt);

    assert_eq!(
        demuxer.streams.len(),
        1,
        "stream map should still have 1 stream"
    );
    assert_eq!(demuxer.pmt_version, 1);
    assert_eq!(
        demuxer.streams[0].pes.buf, buf_before,
        "in-flight PES buffer must be preserved after PMT version change for same PID"
    );
}

#[test]
fn crc32_empty_data() {
    assert_eq!(crc32_mpeg2(&[]), 0xFFFF_FFFF);
}

#[test]
fn crc32_known_vector() {
    // CRC-32/MPEG-2 of "123456789" (classic check value)
    let data = b"123456789";
    assert_eq!(crc32_mpeg2(data), 0x0376_E6E7);
}

#[test]
fn crc32_idempotent_across_calls() {
    let data = b"hello world";
    let a = crc32_mpeg2(data);
    let b = crc32_mpeg2(data);
    assert_eq!(a, b);
}

#[test]
fn write_pcr_zero_ts() {
    let mut buf = [0xFFu8; 6];
    write_pcr(&mut buf, 0);
    // PCR at zero: base=0, extension=0, with the PCR marker bits set
    assert_eq!(buf[0], 0x00);
    assert_eq!(buf[1], 0x00);
    assert_eq!(buf[2], 0x00);
    assert_eq!(buf[3], 0x00);
}

// --- const CRC32_TABLE correctness ---

#[test]
fn crc32_table_is_compile_time_const() {
    // The const table and a freshly-computed runtime table must agree for all 256 entries.
    let runtime_table: [u32; 256] = {
        let mut t = [0u32; 256];
        for (i, entry) in t.iter_mut().enumerate() {
            let mut crc = (i as u32) << 24;
            for _ in 0..8 {
                if crc & 0x8000_0000 != 0 {
                    crc = (crc << 1) ^ 0x04C1_1DB7;
                } else {
                    crc <<= 1;
                }
            }
            *entry = crc;
        }
        t
    };
    assert_eq!(
        CRC32_TABLE, runtime_table,
        "compile-time CRC32_TABLE must match a freshly-computed runtime table"
    );
}

// --- Sentinel u8 for continuity counter and PMT version ---

#[test]
#[allow(clippy::assertions_on_constants)]
fn continuity_sentinel_is_not_a_valid_cc_value() {
    // Valid continuity counter values are 0–15 (4-bit field in TS header).
    assert_eq!(CC_UNSET, u8::MAX, "CC_UNSET must be u8::MAX");
    assert!(
        CC_UNSET > 15,
        "CC_UNSET must be out of the valid 0–15 range"
    );
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn pmt_version_sentinel_is_not_a_valid_version() {
    // Valid PMT version_number values are 0–31 (5-bit field in PMT header).
    assert_eq!(PMT_VER_UNSET, u8::MAX, "PMT_VER_UNSET must be u8::MAX");
    assert!(
        PMT_VER_UNSET > 31,
        "PMT_VER_UNSET must be out of the valid 0–31 range"
    );
}

#[test]
fn new_demuxer_pmt_version_is_unset() {
    let dem = TsDemuxer::new();
    assert_eq!(
        dem.pmt_version, PMT_VER_UNSET,
        "freshly constructed TsDemuxer must report PMT_VER_UNSET before any PMT is seen"
    );
}

#[test]
fn new_stream_continuity_is_unset() {
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

    let mut dem = TsDemuxer::new();
    dem.feed(&data);

    assert_eq!(dem.streams.len(), 1, "PMT must add one video stream");
    assert_eq!(
        dem.streams[0].continuity, CC_UNSET,
        "new stream continuity must be CC_UNSET before first TS packet arrives"
    );
}

// --- find_ts_sync / ts_sync_candidate_is_valid direct tests ---

#[test]
fn find_ts_sync_empty_returns_zero_length() {
    assert_eq!(find_ts_sync(&[]), 0);
}

#[test]
fn find_ts_sync_no_sync_byte_returns_len() {
    let data = vec![0x00u8; 200];
    assert_eq!(find_ts_sync(&data), data.len());
}

#[test]
fn find_ts_sync_sync_at_offset_zero_accepted() {
    // One packet followed by non-0x47 — optimistic accept at offset 0
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 2];
    data[0] = TS_SYNC_BYTE;
    data[TS_PACKET_SIZE] = TS_SYNC_BYTE; // confirm at +188
    assert_eq!(find_ts_sync(&data), 0);
}

#[test]
fn find_ts_sync_sync_at_offset_zero_short_buffer_optimistic() {
    // Buffer has only one packet (≤ TS_PACKET_SIZE bytes after candidate) → optimistic accept
    let mut data = vec![0x00u8; TS_PACKET_SIZE];
    data[0] = TS_SYNC_BYTE;
    assert_eq!(
        find_ts_sync(&data),
        0,
        "single-packet buffer must accept offset 0"
    );
}

#[test]
fn find_ts_sync_false_sync_skipped_real_sync_found() {
    // byte[5] = 0x47 but byte[5+188] ≠ 0x47 → rejected.
    // byte[10] = 0x47, +188 and +376 both confirmed → accepted.
    // Buffer is 4 packets so the triple-confirmation path is exercised.
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 4];
    data[5] = TS_SYNC_BYTE; // false candidate — no +188 confirm
    data[10] = TS_SYNC_BYTE;
    data[10 + TS_PACKET_SIZE] = TS_SYNC_BYTE;
    data[10 + TS_PACKET_SIZE * 2] = TS_SYNC_BYTE;
    assert_eq!(find_ts_sync(&data), 10);
}

#[test]
fn find_ts_sync_double_confirmed_sync() {
    // Two consecutive confirming sync bytes (triple-confirmation path)
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 4];
    data[3] = TS_SYNC_BYTE;
    data[3 + TS_PACKET_SIZE] = TS_SYNC_BYTE;
    data[3 + TS_PACKET_SIZE * 2] = TS_SYNC_BYTE;
    assert_eq!(find_ts_sync(&data), 3);
}

#[test]
fn ts_sync_candidate_not_sync_byte_returns_false() {
    let data = [0x00u8; 188];
    assert!(!ts_sync_candidate_is_valid(&data, 0));
}

#[test]
fn ts_sync_candidate_out_of_bounds_returns_false() {
    let data = [0x47u8; 10];
    assert!(!ts_sync_candidate_is_valid(&data, 20)); // candidate > data.len()
}
