#[test]
fn h264_sequence_header_for_keyframe_uses_cached_parameter_sets() {
    let mut cache = Vec::new();
    cache_h264_parameter_sets(
        &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ],
        &mut cache,
    );
    let (sequence_header, sps) =
        h264_sequence_header_for_keyframe(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80], &cache)
            .expect("cached SPS/PPS should synthesize a sequence header");

    assert_eq!(sequence_header[0], 0x17);
    assert_eq!(sequence_header[1], 0x00);
    assert_eq!(
        sps,
        Some(vec![0x67, 0x42, 0x00, 0x1E, 0xAB]),
        "cached SPS should be reused when the keyframe carries only IDR data"
    );
}

#[test]
fn classify_flv_video_packet_distinguishes_config_and_media() {
    assert_eq!(
        classify_flv_video_packet(&[0x17, 0x00, 0x00, 0x00, 0x00]),
        Some(FlvVideoPacketKind::SequenceHeader)
    );
    assert_eq!(
        classify_flv_video_packet(&[0x17, 0x01, 0x00, 0x00, 0x28]),
        Some(FlvVideoPacketKind::Keyframe)
    );
    assert_eq!(
        classify_flv_video_packet(&[0x27, 0x01, 0x00, 0x00, 0x28]),
        Some(FlvVideoPacketKind::Interframe)
    );
    assert_eq!(classify_flv_video_packet(&[0x12, 0x01]), None);
}

#[test]
fn rtmp_video_droppability_matches_rml_contract() {
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x17, 0x00, 0x00, 0x00, 0x00], true),
        "AVC sequence headers must never be marked droppable"
    );
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x17, 0x01, 0x00, 0x00, 0x28], true),
        "keyframes must not be marked droppable because later frames depend on them"
    );
    assert!(
        rtmp_video_packet_can_be_dropped(&[0x27, 0x01, 0x00, 0x00, 0x28], false),
        "interframes may be marked droppable for future slow-consumer policies"
    );
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x27, 0x01, 0x00, 0x00, 0x28], true),
        "packet metadata wins over FLV tag classification when it says keyframe"
    );
    assert!(
        !rtmp_video_packet_can_be_dropped(&[0x12, 0x01], false),
        "unclassified video payloads must fail closed until a future drop policy can prove safety"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_rtmp_video_droppability_fails_closed(
        mut payload in proptest::collection::vec(any::<u8>(), 0..32),
        is_keyframe in any::<bool>(),
    ) {
        let expected = !is_keyframe
            && payload.len() >= 2
            && matches!(payload[0] & 0x0f, 7 | 12)
            && payload[1] != 0
            && (payload[0] >> 4) != 1;

        prop_assert_eq!(rtmp_video_packet_can_be_dropped(&payload, is_keyframe), expected);

        if !payload.is_empty() {
            payload[0] = (payload[0] & 0xf0) | 0x02;
            prop_assert!(
                !rtmp_video_packet_can_be_dropped(&payload, false),
                "unknown FLV video codecs must not be marked droppable"
            );
        }
    }
}

#[test]
fn startup_video_sequence_header_prefers_ring_parameter_sets() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);
    let ingest_sequence_header =
        Bytes::from_static(&[0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x1F]);

    let selected =
        startup_video_sequence_header(&ring, Some(ingest_sequence_header.clone()), false)
            .expect("ring parameter sets should synthesize a startup header");

    assert_ne!(
        selected, ingest_sequence_header,
        "startup header should come from the output ring, not the ingest cache"
    );
    assert_eq!(selected[0], 0x17);
    assert_eq!(selected[1], 0x00);
}

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
    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in bytes.chunks(1316) {
        demuxer.feed(chunk);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);
    let parameter_sets = packets
        .iter()
        .find_map(|packet| {
            (packet.media_type == crate::media::packet::MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        })
        .expect("fixture should carry HEVC parameter sets");
    ring.set_video_parameter_sets(parameter_sets);

    let selected = startup_video_sequence_header(&ring, None, true)
        .expect("HEVC parameter sets should synthesize an enhanced startup header");

    assert_eq!(&selected[..5], &[0x80, b'h', b'v', b'c', b'1']);
}

#[test]
fn raw_hevc_guard_does_not_flag_h264_non_idr_slice() {
    let h264_non_idr = [0, 0, 0, 1, 0x41, 0x9a, 0x22, 0x11];

    assert!(!raw_packet_starts_with_hevc_parameter_set(&h264_non_idr));
}

#[test]
fn enhanced_rtmp_connect_packet_advertises_hevc_fourcc_support() {
    let mut config = ClientSessionConfig::new();
    config.tc_url = Some("rtmp://example/live".to_string());

    let packet = enhanced_rtmp_connect_packet(&config, "live").unwrap();
    let mut deserializer = ChunkDeserializer::new();
    let payload = deserializer
        .get_next_message(&packet)
        .unwrap()
        .expect("connect packet should contain one RTMP message");
    let message = payload.to_rtmp_message().unwrap();

    let RtmpMessage::Amf0Command {
        command_name,
        transaction_id,
        command_object,
        ..
    } = message
    else {
        panic!("enhanced connect should be an AMF0 command");
    };
    assert_eq!(command_name, "connect");
    assert_eq!(transaction_id, 1.0);
    let Amf0Value::Object(properties) = command_object else {
        panic!("enhanced connect should use an object command payload");
    };
    assert_eq!(
        properties.get("fourCcList"),
        Some(&Amf0Value::StrictArray(vec![
            Amf0Value::Utf8String("hvc1".to_string()),
            Amf0Value::Utf8String("avc1".to_string()),
            Amf0Value::Utf8String("mp4a".to_string()),
        ]))
    );
    let Some(Amf0Value::Object(video_fourcc_info)) = properties.get("videoFourCcInfoMap") else {
        panic!("enhanced connect should advertise video FourCC capability info");
    };
    assert_eq!(
        video_fourcc_info.get("hvc1"),
        Some(&Amf0Value::Number(0x06 as f64))
    );
    assert_eq!(
        video_fourcc_info.get("avc1"),
        Some(&Amf0Value::Number(0x06 as f64))
    );
}

#[test]
fn startup_video_sequence_header_skips_ingest_header_for_empty_raw_ring() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    let ingest_sequence_header =
        Bytes::from_static(&[0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x1F]);

    let selected = startup_video_sequence_header(&ring, Some(ingest_sequence_header), false);

    assert!(
        selected.is_none(),
        "empty raw output rings should wait for their own keyframe/config"
    );
}

#[test]
fn startup_audio_waits_for_video_on_empty_transcoded_ring() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");

    assert!(
        rtmp_output_waits_for_video(&ring),
        "transcoded RTMP output rings should wait for their own video startup"
    );
    assert!(
        should_defer_audio_until_video_ready(false, &ring),
        "audio packets must be deferred until the codec-edge ring has emitted video"
    );
    assert!(
        !should_send_startup_audio_sequence_header(false, &ring),
        "startup AAC config must not be sent before video is ready on an empty transcoded ring"
    );
}

#[test]
fn startup_audio_allows_passthrough_audio_only_ring() {
    let ring = Arc::new(RingBuffer::new(1024));

    assert!(
        !rtmp_output_waits_for_video(&ring),
        "audio-only or unknown rings should not be forced to wait for video startup"
    );
    assert!(
        !should_defer_audio_until_video_ready(false, &ring),
        "audio packets should flow immediately when the ring is not video-gated"
    );
    assert!(
        should_send_startup_audio_sequence_header(false, &ring),
        "audio-only startup should still emit AAC config immediately"
    );
}

#[test]
fn startup_audio_unblocks_once_parameter_sets_exist() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);

    assert!(
        should_send_startup_audio_sequence_header(false, &ring),
        "once the output ring has H.264 parameter sets, startup audio can follow the video config"
    );
}

#[test]
fn warmup_does_not_unlock_codec_edge_output_on_audio_only_burst() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    let packets = vec![Arc::new(MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x11, 0x22]),
    })];

    assert!(
        !rtmp_warmup_ready(&ring, &packets),
        "codec-edge warmup should keep waiting until video startup data exists"
    );
}

#[test]
fn warmup_unlocks_codec_edge_output_once_video_startup_arrives() {
    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");
    let video_packets = vec![Arc::new(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65]),
    })];

    assert!(
        rtmp_warmup_ready(&ring, &video_packets),
        "seeing a video burst should unlock RTMP warmup even before parameter sets are cached"
    );

    let parameter_set_ring = Arc::new(RingBuffer::new(1024));
    parameter_set_ring.set_codec_hint("h264");
    parameter_set_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);

    assert!(
        rtmp_warmup_ready(&parameter_set_ring, &[]),
        "cached parameter sets should also satisfy RTMP warmup readiness"
    );
}

#[test]
fn deferred_audio_sequence_header_prefers_cached_flv_header() {
    let cached = Bytes::from_static(&[0xaf, 0x00, 0x12, 0x10]);
    let track = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        ..Default::default()
    };

    assert_eq!(
        resolve_deferred_audio_sequence_header(Some(&cached), Some(&track)),
        Some(cached)
    );
}

#[test]
fn deferred_audio_sequence_header_synthesizes_aac_when_cache_missing() {
    let track = AudioMeta {
        codec: "aac".into(),
        sample_rate: 48_000,
        channels: 2,
        ..Default::default()
    };

    let header = resolve_deferred_audio_sequence_header(None, Some(&track))
        .expect("AAC tracks should synthesize a deferred sequence header");

    assert_eq!(header[0], 0xaf);
    assert_eq!(header[1], 0x00);
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
async fn start_rtmp_egress_waits_for_ring_data_before_connecting() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let ring = Arc::new(RingBuffer::new(1024));
    ring.set_codec_hint("h264");

    let engine = Arc::new(MediaEngine::new());
    let registration = engine
        .register_egress_attempt("out-warmup", "pipe-warmup", "rtmp://ignored/app/key", None)
        .await;

    let ring_c = ring.clone();
    let engine_c = engine.clone();
    let registration_c = registration.clone();
    tokio::spawn(async move {
        start_rtmp_egress(
            "out-warmup".to_string(),
            "pipe-warmup".to_string(),
            format!("rtmp://{}/live/key", addr),
            ring_c,
            engine_c,
            registration_c,
            crate::domain::output_spec::RtmpOutputMode::Legacy,
        )
        .await;
    });

    // No data on the ring yet: the pre-connect warmup gate must hold off
    // connecting, or MediaMTX would accept an idle publisher and later
    // drop it for inactivity (the root cause of the RTMP reconnect storm
    // under high output fanout).
    let early_accept =
        tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
    assert!(
        early_accept.is_err(),
        "must not connect before the ring has data"
    );

    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Flv,
        payload: bytes::Bytes::from_static(&[0x17, 0x01, 0x00, 0x00, 0x00]),
    });

    let late_accept =
        tokio::time::timeout(std::time::Duration::from_secs(2), listener.accept()).await;
    assert!(late_accept.is_ok(), "must connect once the ring has data");

    registration.cancel_token.cancel();
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
