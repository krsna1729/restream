use super::*;

#[tokio::test]
async fn adapt_pipeline_ring_no_op_when_default_is_sufficient() {
    // 1080p30 + 1 audio = 80 pkt/s → needed = ceil(80 × 6) = 480 < default 1024
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let result = engine.adapt_pipeline_ring("p", 30.0, 1).await;
    assert!(
        result.is_none(),
        "no resize needed for single-track 1080p30"
    );

    let ring = engine.get_or_create_pipeline("p").await;
    assert_eq!(ring.capacity(), engine.config.ring_capacity);
    let depth = ring.buffer_depth_secs().unwrap();
    assert!((12.0..=13.0).contains(&depth), "depth={depth}");
}

#[tokio::test]
async fn source_ring_uses_engine_typed_config_capacity() {
    let config = Arc::new(crate::AppConfig {
        ring_capacity: 2048,
        ..Default::default()
    });
    let engine = MediaEngine::new_with_config(config);

    let ring = engine.get_or_create_pipeline("typed-ring").await;
    assert_eq!(ring.capacity(), 2048);
}

#[tokio::test]
async fn ts_muxer_ring_uses_engine_typed_config_capacity() {
    let config = Arc::new(crate::AppConfig {
        ts_ring_capacity: 96,
        ..Default::default()
    });
    let engine = Arc::new(MediaEngine::new_with_config(config));
    let source = Arc::new(RingBuffer::new(16));

    let ts_ring = engine
        .get_or_create_ts_muxer_stage("typed-ts", "source", source)
        .await;

    assert_eq!(ts_ring.ring.capacity(), 96);
    ts_ring.cancel.cancel();
}

#[tokio::test]
async fn adapt_pipeline_ring_resizes_for_multi_track_stream() {
    // 2v16a: 30 fps + 16 audio × 50 = 830 pkt/s → needed = ceil(830 × 6) = 4980
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("ring must be resized for 830 pkt/s");

    assert_eq!(new_ring.capacity(), 4980);
    let depth = new_ring.buffer_depth_secs().unwrap();
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
    assert_eq!(engine.get_or_create_pipeline("p").await.capacity(), 4980);
}

#[tokio::test]
async fn adapt_pipeline_ring_4k60_single_audio_no_resize() {
    // 4K 60fps + 1 audio = 110 pkt/s → needed = 660 < default 1024
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let result = engine.adapt_pipeline_ring("p", 60.0, 1).await;
    assert!(
        result.is_none(),
        "default 1024 already covers 4K60 single-track"
    );
}

#[tokio::test]
async fn adapt_pipeline_ring_4k60_multi_audio_resizes() {
    // 4K 60fps + 16 audio = 860 pkt/s → needed = ceil(860 × 6) = 5160
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let new_ring = engine
        .adapt_pipeline_ring("p", 60.0, 16)
        .await
        .expect("resize needed for 4K60 + 16 audio");

    assert_eq!(new_ring.capacity(), 5160);
    let depth = new_ring.buffer_depth_secs().unwrap();
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
}

#[tokio::test]
async fn get_or_create_pipeline_preserves_adapted_ring_across_calls() {
    // The adapted ring must be returned by all subsequent get_or_create_pipeline
    // calls so egress readers and TS mux stages attach to the correctly-sized ring.
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;
    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("should resize for 830 pkt/s");
    assert_eq!(new_ring.capacity(), 4980);

    let ring2 = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring2.capacity(),
        4980,
        "adapted ring must persist across calls"
    );

    let _reader = crate::media::ring_buffer::Reader::new("hold".to_string(), ring2.clone());
    let ring3 = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring3.capacity(),
        4980,
        "ring must not change with active reader"
    );
}

#[tokio::test]
async fn adapt_pipeline_ring_lighter_republish_updates_rate_not_capacity() {
    // A lighter re-publish (1v1a after 2v16a) does not shrink the ring —
    // it just updates estimated_pkt_rate so bufferDepthSecs is correct.
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;
    engine.adapt_pipeline_ring("p", 30.0, 16).await; // → 4980 for 830 pkt/s

    // Lighter re-publish: 1v1a = 80 pkt/s → needed = 480 < 4980 → no resize.
    let result = engine.adapt_pipeline_ring("p", 30.0, 1).await;
    assert!(
        result.is_none(),
        "no resize when ring is already large enough"
    );

    let ring = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring.capacity(),
        4980,
        "capacity preserved from heavier session"
    );
    let depth = ring.buffer_depth_secs().unwrap();
    // telemetry now reflects the lighter stream's real depth: 4980/80 ≈ 62 s
    assert!(depth > 60.0, "4980/80 ≈ 62.3 s; got {depth}");
}

#[tokio::test]
async fn adapt_pipeline_ring_preserves_codec_and_track_metadata() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("p").await;
    ring.set_codec_hint("hevc");
    ring.set_video_parameter_sets(vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01]);
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: Some("stereo".to_string()),
        track_index: 3,
        pid: Some(257),
        language: Some("eng".to_string()),
        title: Some("Program".to_string()),
        profile: Some("LC".to_string()),
    }]);

    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("ring must be resized for metadata preservation proof");

    assert_eq!(new_ring.codec_hint_str(), "hevc");
    assert_eq!(
        new_ring.video_parameter_sets(),
        Some(vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01])
    );
    let tracks = new_ring
        .audio_tracks()
        .expect("resized ring should preserve audio tracks");
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].codec, "aac");
    assert_eq!(tracks[0].sample_rate, 48_000);
    assert_eq!(tracks[0].channels, 2);
    assert_eq!(tracks[0].track_index, 3);
    assert_eq!(tracks[0].pid, Some(257));
    assert_eq!(tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(tracks[0].title.as_deref(), Some("Program"));
    assert_eq!(tracks[0].profile.as_deref(), Some("LC"));
}

#[tokio::test]
async fn health_input_protocol_matches_registration() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    for proto in ["rtmp", "srt", "file"] {
        engine
            .try_register_ingest("p1", "key", proto)
            .await
            .unwrap();
        let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
        assert_eq!(
            snap["pipelines"]["p1"]["input"]["publisher"]["protocol"], proto,
            "protocol mismatch for {proto}"
        );
        engine.unregister_ingest("p1").await;
    }
}
