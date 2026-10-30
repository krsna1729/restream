#[test]
fn startup_video_sequence_header_builds_enhanced_hevc_header() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("hevc");
    let fixture = crate::test_fixtures::av_marker_transport_fixture_for_bframes(
        "h265",
        false,
        crate::test_fixtures::AvMarkerBframeMode::Bf0,
    )
    .expect("checked-in HEVC BF0 fixture");
    let bytes = std::fs::read(fixture).expect("read HEVC BF0 fixture");
    let mut demuxer = restream_mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in bytes.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);
    let parameter_sets = packets
        .iter()
        .find_map(|packet| {
            (packet.media_type == restream_core::MediaType::Video)
                .then(|| restream_core::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        })
        .expect("fixture should carry HEVC parameter sets");
    ring.set_video_parameter_sets(parameter_sets);

    let (selected, _) = startup_video_sequence_header(&ring, None, true)
        .expect("HEVC parameter sets should synthesize an enhanced startup header");

    assert_eq!(&selected[..5], &[0x80, b'h', b'v', b'c', b'1']);
}

#[tokio::test]
async fn rtmp_metadata_uses_terminal_ring_codec_for_hevc_codec_edge() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p-codec-edge", "key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "p-codec-edge",
            Some(VideoMeta {
                codec: "hevc".into(),
                width: 1920,
                height: 1080,
                fps: 50.0,
                ..Default::default()
            }),
            None,
            None,
        )
        .await;

    let output_ring = Arc::new(RingBuffer::new(1024));
    output_ring.set_codec_hint("h264");
    let audio = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    };

    let metadata = rtmp_publish_metadata(&engine, "p-codec-edge", &output_ring, Some(&audio))
        .await
        .expect("codec-edge RTMP output should publish metadata");

    assert_eq!(
        metadata.video_codec_id,
        Some(7),
        "HEVC ingest converted by hevc_to_h264 must advertise AVC on RTMP"
    );
    assert_eq!(metadata.video_width, Some(1920));
    assert_eq!(metadata.video_height, Some(1080));
    assert_eq!(metadata.audio_codec_id, Some(10));
}

#[tokio::test]
async fn rtmp_metadata_advertises_unconverted_hevc_as_hvc1_fourcc() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p-hevc-source", "key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "p-hevc-source",
            Some(VideoMeta {
                codec: "hevc".into(),
                width: 1920,
                height: 1080,
                fps: 50.0,
                ..Default::default()
            }),
            None,
            None,
        )
        .await;

    let output_ring = Arc::new(RingBuffer::new(1024));
    let audio = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        track_index: 0,
        ..Default::default()
    };

    let metadata = rtmp_publish_metadata(&engine, "p-hevc-source", &output_ring, Some(&audio))
        .await
        .expect("audio metadata should still be present");

    assert_eq!(
        metadata.video_codec_id,
        Some(RTMP_METADATA_VIDEO_CODEC_ID_HEVC),
        "without a terminal H.264 ring, HEVC must be advertised as hvc1, not AVC"
    );
    assert_eq!(metadata.video_width, Some(1920));
    assert_eq!(metadata.video_height, Some(1080));
    assert_eq!(metadata.audio_codec_id, Some(10));
}

#[tokio::test]
async fn detects_h265_from_ingest_video_meta() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p1", "key", "srt")
        .await
        .unwrap();

    // No video meta yet → not H.265
    {
        let ingests = engine.ingests.active.read().await;
        let is_h265 = ingests
            .get("p1")
            .and_then(|ingest| ingest.metadata().video)
            .map(|v| v.codec == "hevc")
            .unwrap_or(false);
        assert!(!is_h265, "no video meta should not be hevc");
    }

    // H.264 meta → not H.265
    engine
        .update_ingest_meta(
            "p1",
            Some(VideoMeta {
                codec: "h264".into(),
                width: 0,
                height: 0,
                fps: 0.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    {
        let ingests = engine.ingests.active.read().await;
        let is_h265 = ingests
            .get("p1")
            .and_then(|ingest| ingest.metadata().video)
            .map(|v| v.codec == "hevc")
            .unwrap_or(false);
        assert!(!is_h265, "h264 meta should not be hevc");
    }

    // H.265 meta → is H.265
    engine
        .update_ingest_meta(
            "p1",
            Some(VideoMeta {
                codec: "hevc".into(),
                width: 0,
                height: 0,
                fps: 0.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            None,
            None,
        )
        .await;
    {
        let ingests = engine.ingests.active.read().await;
        let is_h265 = ingests
            .get("p1")
            .and_then(|ingest| ingest.metadata().video)
            .map(|v| v.codec == "hevc")
            .unwrap_or(false);
        assert!(is_h265, "hevc meta should be detected");
    }

    engine.unregister_ingest("p1").await;
}

#[tokio::test]
async fn h265_detection_waits_for_probe_meta() {
    let engine = Arc::new(MediaEngine::new());
    let engine_clone = engine.clone();
    let pipeline_id = "p2".to_string();

    engine
        .try_register_ingest(&pipeline_id, "key", "srt")
        .await
        .unwrap();

    // Spawn a task that sets the video meta after a delay (simulating probe arrival)
    let delayed_engine = engine.clone();
    let delayed_pid = pipeline_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        delayed_engine
            .update_ingest_meta(
                &delayed_pid,
                Some(VideoMeta {
                    codec: "hevc".into(),
                    width: 0,
                    height: 0,
                    fps: 0.0,
                    bw: None,
                    pid: None,
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                None,
                None,
            )
            .await;
    });

    // Now run the same probe-wait logic that start_rtmp_egress uses
    let is_h265 = 'probe: {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let ingests = engine_clone.ingests.active.read().await;
            let meta = ingests
                .get(&pipeline_id)
                .and_then(|ingest| ingest.metadata().video);
            match meta {
                Some(v) if v.codec == "hevc" => break 'probe true,
                Some(_) => break 'probe false,
                None => {}
            }
            drop(ingests);
            if std::time::Instant::now() >= deadline {
                break 'probe false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    assert!(is_h265, "should detect hevc after probe arrives");

    engine.unregister_ingest(&pipeline_id).await;
}

#[tokio::test]
async fn resolved_output_audio_tracks_falls_back_when_ring_metadata_is_empty() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-audio", "key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_audio_tracks(
            "pipe-audio",
            vec![
                AudioMeta {
                    codec: "aac".into(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 0,
                    ..Default::default()
                },
                AudioMeta {
                    codec: "aac".into(),
                    sample_rate: 48_000,
                    channels: 2,
                    track_index: 1,
                    ..Default::default()
                },
            ],
        )
        .await;

    let ring = Arc::new(RingBuffer::new(16));
    ring.set_audio_tracks(Vec::new());

    let tracks = resolved_output_audio_tracks(&engine, "pipe-audio", &ring).await;

    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].track_index, 0);
    assert_eq!(tracks[1].track_index, 1);

    engine.unregister_ingest("pipe-audio").await;
}
