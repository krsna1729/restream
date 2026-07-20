use super::*;

#[tokio::test]
async fn matrix_routing_ingest_to_source_reader() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("matrix-pipe").await;
    engine
        .try_register_ingest("matrix-pipe", "key", "rtmp")
        .await
        .unwrap();

    ring.push(test_video_packet(0, 0, true));
    ring.push(test_audio_packet(10, 10));
    ring.push(test_video_packet(33, 33, false));

    let mut reader = Reader::new("matrix-reader".to_string(), ring);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.media_type, MediaType::Video);
    assert!(p1.is_keyframe);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.media_type, MediaType::Audio);
    let p3 = reader.pull().unwrap().unwrap();
    assert_eq!(p3.pts, 33);
    assert!(reader.pull().unwrap().is_none());
}

#[tokio::test]
async fn matrix_routing_flv_and_raw_format_dispatch() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("fmt-pipe").await;

    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Flv,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x17, 0x01, 0, 0, 0]),
    });
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 33,
        dts: 33,
        payload: Bytes::from_static(&[0, 0, 0, 1, 0x41]),
    });

    let mut reader = Reader::new("fmt-reader".to_string(), ring);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.format, PayloadFormat::Flv);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.format, PayloadFormat::Raw);
}

#[tokio::test]
async fn matrix_routing_multi_reader_fan_out() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("fanout-pipe").await;

    ring.push(test_video_packet(0, 0, true));
    ring.push(test_audio_packet(10, 10));

    let mut r1 = Reader::new("reader-1".to_string(), ring.clone());
    let mut r2 = Reader::new("reader-2".to_string(), ring.clone());
    let mut r3 = Reader::new("reader-3".to_string(), ring);

    for reader in [&mut r1, &mut r2, &mut r3] {
        let p = reader.pull().unwrap().unwrap();
        assert_eq!(p.pts, 0);
        assert!(p.is_keyframe);
    }
}

#[tokio::test]
async fn matrix_routing_transcoder_stage_isolation() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("iso-pipe").await;

    source.push(test_video_packet(0, 0, true));

    let tc_ring = engine
        .get_or_create_transcoder(
            "iso-pipe",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;

    assert!(
        !Arc::ptr_eq(&source, &tc_ring),
        "transcoder output ring must differ from source ring"
    );

    let mut source_reader = Reader::new("src".to_string(), source);
    let p = source_reader.pull().unwrap().unwrap();
    assert_eq!(p.pts, 0);
}

// ── fault resilience: ingest lifecycle ──────────────────────────────
