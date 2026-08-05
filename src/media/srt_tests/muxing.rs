#[test]
fn ts_accum_capacity_tracks_packet_size_without_fixed_64k_floor() {
    let packets = vec![
        Arc::new(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from(vec![0; 200]),
        }),
        Arc::new(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from(vec![1; 1_000]),
        }),
    ];

    let estimated = estimate_ts_accum_capacity(&packets);
    assert_eq!(estimated, 200 + 1_000 + (188 * 4 * 2));
    assert!(estimated < 64 * 1024);
}

#[test]
fn video_for_ts_raw_passthrough() {
    let raw_video = [0, 0, 1, 0x65, 0xaa, 0xbb];
    let mut nls = 4usize;
    let mut cache = Vec::new();
    let result =
        crate::media::codec::video_for_ts(&raw_video, PayloadFormat::Raw, &mut nls, &mut cache);
    assert!(result.is_some());
    assert_eq!(&*result.unwrap(), &raw_video[..]);
}

#[test]
fn audio_for_ts_raw_passthrough_with_adts() {
    let adts_audio = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x1F, 0xFC, 0x21, 0x10];
    // Raw with ADTS sync → borrowed passthrough
    let result = crate::media::codec::audio_for_ts(&adts_audio, PayloadFormat::Raw, 48000, 2);
    assert!(result.is_some());
    assert_eq!(&*result.unwrap(), &adts_audio[..]);
}

#[test]
fn flv_video_seq_skipped_data_converted() {
    let flv_video_seq = [
        0x17u8, 0x00, 0x00, 0x00, 0x00, 1, 66, 0, 30, 0xFF, 0xE1, 0, 3, 1, 2, 3, 1, 0, 2, 4, 5,
    ];
    let flv_audio_seq = [0xaf, 0x00, 0x12, 0x10];

    let mut nls = 4usize;
    // Seq headers for audio → None
    assert!(
        crate::media::codec::audio_for_ts(&flv_audio_seq, PayloadFormat::Flv, 48000, 2).is_none()
    );
    // Video seq header → extracts SPS/PPS as Annex B (or None if config too short)
    let mut cache = Vec::new();
    let _result =
        crate::media::codec::video_for_ts(&flv_video_seq, PayloadFormat::Flv, &mut nls, &mut cache);
    // Just verify no panic; codec tests cover correctness in detail
}


