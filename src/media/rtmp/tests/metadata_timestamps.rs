#[test]
fn parse_flv_video_meta_empty_returns_none() {
    assert!(parse_flv_video_meta(&[]).is_none());
}

#[test]
fn parse_flv_video_meta_single_byte_returns_none() {
    assert!(parse_flv_video_meta(&[0x17]).is_none());
}

#[test]
fn parse_flv_video_meta_unknown_codec_id_returns_none() {
    // codec_id=5 (On2 VP6 with alpha) — not handled
    let data = [0x15u8, 0x01, 0x00, 0x00, 0x00];
    assert!(parse_flv_video_meta(&data).is_none());
}

#[test]
fn parse_flv_video_meta_vp6_returns_codec_name() {
    // frame_type=1, codec_id=4 (VP6) → meta returned with codec="vp6"
    let data = [0x14u8, 0x00];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "vp6");
    assert_eq!(meta.width, 0);
}

#[test]
fn parse_flv_video_meta_h265_returns_codec_name() {
    // frame_type=1, codec_id=12 (H.265/HEVC enhanced)
    let data = [0x1Cu8, 0x01, 0x00, 0x00, 0x00];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h265");
}

#[test]
fn parse_flv_video_meta_h264_seq_header_truncated_avcc() {
    // seq header (byte[1]=0) but AVCDecoderConfigurationRecord too short to extract profile/level
    // data.len() == 6: passes the > 12 check? No: 6 < 12 → skips SPS parsing, no panic
    let data = [0x17u8, 0x00, 0x00, 0x00, 0x00, 0x01];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h264");
    // profile/level not parsed (too short)
    assert!(meta.profile.is_none());
    assert!(meta.level.is_none());
    assert_eq!(meta.width, 0);
}

#[test]
fn parse_flv_video_meta_h264_seq_header_short_sps_length_field() {
    // 13 bytes: passes > 12 check. avc_config starts at data[5].
    // avc_config[5]=0xE1 (numSPS=1), avc_config[6..7]=SPS len = 0x0001 (1 byte),
    // but then we'd need avc_config[8 + 1] = 9 bytes total in avc_config.
    // avc_config len = 13-5 = 8 bytes → 8 < 9 → SPS resolution not parsed. No panic.
    let data = [
        0x17u8, 0x00, 0x00, 0x00, 0x00, // frame_type/codec, pkt_type, comp_time
        0x01, 0x64, 0x00, 0x1F, // version, profile, compat, level
        0xFF, 0xE1, // lengthSizeMinusOne, numSPS=1
        0x00, 0x01, // SPS length = 1 (only 0 bytes remain → out of bounds)
    ];
    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h264");
    assert_eq!(meta.profile.as_deref(), Some("High"));
    assert_eq!(meta.level.as_deref(), Some("3.1"));
    assert_eq!(meta.width, 0); // SPS not parsed, no panic
}

#[test]
fn parse_flv_video_meta_h264_seq_header_extracts_fps_from_sps_vui() {
    // libx264 AVCDecoderConfigurationRecord carrying a 1920x1080@50 SPS.
    #[rustfmt::skip]
        let data = [
            0x17u8, 0x00, 0x00, 0x00, 0x00, // keyframe, AVC sequence header
            0x01, 0x42, 0xc0, 0x2a, 0xff, 0xe1, 0x00, 0x18,
            0x67, 0x42, 0xc0, 0x2a, 0xda, 0x01, 0xe0, 0x08,
            0x9f, 0x97, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
            0x10, 0x00, 0x00, 0x06, 0x48, 0xf1, 0x83, 0x2a,
            0x01, 0x00, 0x04, 0x68, 0xce, 0x0f, 0xc8,
        ];

    let meta = parse_flv_video_meta(&data).unwrap();
    assert_eq!(meta.codec, "h264");
    assert_eq!(meta.width, 1920);
    assert_eq!(meta.height, 1080);
    assert!((meta.fps - 50.0).abs() < 0.01, "fps={}", meta.fps);
}

// --- FLV audio meta: malformed / truncated / non-AAC codecs ---

#[test]
fn parse_flv_audio_meta_empty_returns_none() {
    assert!(parse_flv_audio_meta(&[]).is_none());
}

#[test]
fn parse_flv_audio_meta_mp3_no_asc() {
    // format_id=2 (MP3), rate=3 (44100), size=1, type=1 (stereo)
    let data = [0x2Fu8];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "mp3");
    assert_eq!(meta.sample_rate, 44100);
    assert_eq!(meta.channels, 2);
}

#[test]
fn parse_flv_audio_meta_speex_mono_11025() {
    // format_id=11 (Speex), rate=1 (11025), type=0 (mono)
    let data = [0xB4u8];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "speex");
    assert_eq!(meta.sample_rate, 11025);
    assert_eq!(meta.channels, 1);
    assert_eq!(meta.channel_layout.as_deref(), Some("mono"));
}

#[test]
fn parse_flv_audio_meta_aac_data_packet_not_seq_header() {
    // format_id=10 (AAC), byte[1]=1 (data packet, not seq header) → no ASC parsing
    let data = [0xAFu8, 0x01, 0x12, 0x10];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "aac");
    // sample_rate from FLV rate_id bits only (rate_id=3 → 44100)
    assert_eq!(meta.sample_rate, 44100);
}

#[test]
fn parse_flv_audio_meta_aac_seq_header_truncated_asc_one_byte() {
    // format_id=10, byte[1]=0 (seq header), only 1 byte of ASC → asc.len() < 2, no ASC parsing
    let data = [0xAFu8, 0x00, 0x12];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "aac");
    // Falls back to FLV header rates (rate_id=3 → 44100)
    assert_eq!(meta.sample_rate, 44100);
}

#[test]
fn parse_flv_audio_meta_aac_5_1_surround() {
    // object_type=2 (AAC-LC), freq_idx=3 (48000), ch_config=6 (5.1)
    // byte[0]: 0xAF (format=10, rate=3, size=1, channels=1 bit)
    // ASC: (2<<3)|(3>>1)=0x11, (3<<7)|(6<<3)=0xB0
    let data = [0xAFu8, 0x00, 0x11, 0xB0];
    let meta = parse_flv_audio_meta(&data).unwrap();
    assert_eq!(meta.codec, "aac");
    assert_eq!(meta.sample_rate, 48000);
    assert_eq!(meta.channels, 6);
    assert_eq!(meta.channel_layout.as_deref(), Some("5.1"));
}

#[test]
fn rtmp_timestamp_guard_bumps_repeated_video_dts() {
    let mut guard = RtmpTimestampGuard::new();

    assert_eq!(guard.enforce_ms(MediaType::Video, 41), 41);
    assert_eq!(guard.enforce_ms(MediaType::Video, 41), 42);
    assert_eq!(guard.enforce_ms(MediaType::Video, 40), 43);
}

#[test]
fn rtmp_timestamp_guard_bumps_repeated_audio_pts() {
    let mut guard = RtmpTimestampGuard::new();

    assert_eq!(guard.enforce_ms(MediaType::Audio, 26), 26);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 26), 27);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 25), 28);
}

#[test]
fn rtmp_timestamp_guard_keeps_audio_and_video_independent() {
    let mut guard = RtmpTimestampGuard::new();

    assert_eq!(guard.enforce_ms(MediaType::Video, 100), 100);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 100), 100);
    assert_eq!(guard.enforce_ms(MediaType::Video, 100), 101);
    assert_eq!(guard.enforce_ms(MediaType::Audio, 100), 101);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_rtmp_timestamp_guard_is_bounded_and_monotone_per_media(
        events in proptest::collection::vec((any::<bool>(), -1_000i64..=(u32::MAX as i64 + 1_000)), 1..128),
    ) {
        let mut guard = RtmpTimestampGuard::new();
        let mut expected_video = i64::MIN;
        let mut expected_audio = i64::MIN;

        for (is_video, input_ts) in events {
            let media_type = if is_video {
                MediaType::Video
            } else {
                MediaType::Audio
            };
            let expected_slot = if is_video {
                &mut expected_video
            } else {
                &mut expected_audio
            };

            let mut expected = input_ts.clamp(0, u32::MAX as i64);
            if expected <= *expected_slot {
                expected = (*expected_slot + 1).min(u32::MAX as i64);
            }
            *expected_slot = expected;

            let actual = guard.enforce_ms(media_type, input_ts);
            prop_assert_eq!(actual, expected);
            prop_assert!((0..=u32::MAX as i64).contains(&actual));
        }
    }

    #[test]
    fn proptest_refreshed_video_sequence_header_timestamp_precedes_media(
        media_ts in any::<u32>(),
    ) {
        let refreshed = refreshed_video_sequence_header_timestamp(RtmpTimestamp::new(media_ts));
        prop_assert_eq!(refreshed.value, media_ts.saturating_sub(1));
        prop_assert!(refreshed.value <= media_ts);
    }
}

#[test]
fn refreshed_video_sequence_header_uses_current_media_timestamp() {
    let timestamp = refreshed_video_sequence_header_timestamp(RtmpTimestamp::new(42));

    assert_eq!(timestamp.value, 41);
}

#[test]
fn refreshed_video_sequence_header_consumes_a_video_timestamp_slot() {
    let mut guard = RtmpTimestampGuard::new();
    let sequence_header_ts = RtmpTimestamp::new(guard.enforce_ms(MediaType::Video, 42) as u32);
    let packet_ts = RtmpTimestamp::new(
        guard.enforce_ms(MediaType::Video, sequence_header_ts.value as i64) as u32,
    );

    assert_eq!(
        refreshed_video_sequence_header_timestamp(sequence_header_ts).value,
        41
    );
    assert_eq!(
        packet_ts.value, 43,
        "the following keyframe must advance past the refreshed sequence header DTS"
    );
}

#[test]
fn refreshed_video_sequence_header_keeps_zero_timestamp_for_first_keyframe() {
    let timestamp = refreshed_video_sequence_header_timestamp(RtmpTimestamp::new(0));

    assert_eq!(timestamp.value, 0);
}

#[test]
fn validate_rtmp_output_audio_tracks_accepts_single_track() {
    let track = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 1,
        ..Default::default()
    };

    assert!(validate_rtmp_output_audio_tracks(&[track]).is_ok());
}

#[test]
fn validate_rtmp_output_audio_tracks_rejects_multitrack_outputs() {
    let track0 = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    };
    let track1 = AudioMeta {
        track_index: 1,
        ..track0.clone()
    };

    let error = validate_rtmp_output_audio_tracks(&[track0, track1]).unwrap_err();
    assert!(error.contains("exactly one audio track"));
    assert!(error.contains("subset"));
}

#[test]
fn validate_rtmp_output_audio_packet_track_accepts_track_zero() {
    assert!(validate_rtmp_output_audio_packet_track(0).is_ok());
}

#[test]
fn validate_rtmp_output_audio_packet_track_rejects_nonzero_track() {
    let error = validate_rtmp_output_audio_packet_track(1).unwrap_err();
    assert!(error.contains("single routed audio track"));
    assert!(error.contains("track index 1"));
}

// --- FLV composition time: edge cases ---

#[test]
fn flv_composition_time_too_short_returns_zero() {
    assert_eq!(flv_video_composition_time_ms(&[]), 0);
    assert_eq!(flv_video_composition_time_ms(&[0x17, 0x01, 0x00, 0x00]), 0); // 4 bytes < 5
}

#[test]
fn flv_composition_time_sequence_header_returns_zero() {
    // packet_type=0 (seq header) → composition time is always 0 per spec
    let data = [0x17u8, 0x00, 0x00, 0x00, 0x28];
    assert_eq!(flv_video_composition_time_ms(&data), 0);
}

#[test]
fn flv_composition_time_h265_nalu_packet() {
    // codec_id=12 (H.265), packet_type=1 (NALU), positive offset = 40ms
    let data = [0x1Cu8, 0x01, 0x00, 0x00, 0x28];
    assert_eq!(flv_video_composition_time_ms(&data), 40);
}

#[test]
fn flv_composition_time_audio_byte_returns_zero() {
    // FLV audio tag (codec_id=10, i.e. byte[0]&0x0F=10, not 7 or 12) → 0
    let data = [0xAFu8, 0x01, 0x00, 0x00, 0x28];
    assert_eq!(flv_video_composition_time_ms(&data), 0);
}
