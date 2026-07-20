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

