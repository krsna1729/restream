#[tokio::test]
async fn stage_metadata_prefers_upstream_ring_tracks_and_codec_hint() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-stage-meta", "stream-key", "srt")
        .await
        .unwrap();

    let ingest_audio = vec![
        crate::media::metadata::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        },
        crate::media::metadata::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 1,
            pid: Some(0x102),
            language: None,
            title: None,
            profile: None,
        },
    ];
    engine
        .update_ingest_meta(
            "pipe-stage-meta",
            Some(crate::media::metadata::VideoMeta {
                codec: "hevc".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: Some(0x100),
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            ingest_audio.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-stage-meta", ingest_audio)
        .await;

    let upstream_ring = Arc::new(RingBuffer::new(1024));
    upstream_ring.set_codec_hint("h264");
    upstream_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);
    upstream_ring.set_audio_tracks(vec![crate::media::metadata::AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: Some(0x101),
        language: None,
        title: None,
        profile: None,
    }]);

    let cancel = CancellationToken::new();
    let (video, audio_tracks) = wait_for_stage_metadata(
        &engine,
        "pipe-stage-meta",
        &upstream_ring,
        true,
        true,
        None,
        &cancel,
    )
    .await
    .expect("stage metadata");

    assert_eq!(video.codec, "h264");
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0].track_index, 0);
    assert_eq!(audio_tracks[0].pid, Some(0x101));
}

#[tokio::test]
async fn stage_metadata_waits_for_complete_audio_tracks() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-stage-audio-ready", "stream-key", "srt")
        .await
        .unwrap();

    engine
        .update_ingest_meta(
            "pipe-stage-audio-ready",
            Some(crate::media::metadata::VideoMeta {
                codec: "hevc".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: Some(0x100),
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            Some(crate::media::metadata::AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 0,
                channels: 0,
                channel_layout: None,
                track_index: 0,
                pid: Some(0x101),
                language: None,
                title: None,
                profile: None,
            }),
            None,
        )
        .await;

    let upstream_ring = Arc::new(RingBuffer::new(1024));
    upstream_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ]);
    let cancel = CancellationToken::new();
    let engine_for_wait = engine.clone();
    let ring_for_wait = upstream_ring.clone();
    let cancel_for_wait = cancel.clone();
    let wait = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_for_wait,
            "pipe-stage-audio-ready",
            &ring_for_wait,
            true,
            true,
            None,
            &cancel_for_wait,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !wait.is_finished(),
        "stage metadata should wait until audio sample rate and channels are known"
    );

    let ready_audio = crate::media::metadata::AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: Some(0x101),
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta(
            "pipe-stage-audio-ready",
            None,
            Some(ready_audio.clone()),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-stage-audio-ready", vec![ready_audio.clone()])
        .await;

    let (video, audio_tracks) = wait
        .await
        .expect("wait task should join")
        .expect("stage metadata should become ready");
    assert_eq!(video.width, 1920);
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0].sample_rate, 48000);
    assert_eq!(audio_tracks[0].channels, 2);
}

#[tokio::test]
async fn stage_metadata_waits_for_raw_parameter_sets_on_srt_inputs() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-stage-params", "stream-key", "srt")
        .await
        .unwrap();

    let ready_audio = crate::media::metadata::AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: Some(0x101),
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta(
            "pipe-stage-params",
            Some(crate::media::metadata::VideoMeta {
                codec: "hevc".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: Some(0x100),
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            Some(ready_audio.clone()),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-stage-params", vec![ready_audio])
        .await;

    let upstream_ring = Arc::new(RingBuffer::new(1024));
    upstream_ring.set_codec_hint("hevc");
    let cancel = CancellationToken::new();
    let engine_for_wait = engine.clone();
    let ring_for_wait = upstream_ring.clone();
    let cancel_for_wait = cancel.clone();
    let wait = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_for_wait,
            "pipe-stage-params",
            &ring_for_wait,
            true,
            true,
            None,
            &cancel_for_wait,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !wait.is_finished(),
        "stage metadata should wait until raw parameter sets are cached on the source ring"
    );

    upstream_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ]);

    let (video, audio_tracks) = wait
        .await
        .expect("wait task should join")
        .expect("stage metadata should become ready");
    assert_eq!(video.codec, "hevc");
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0].track_index, 0);
}

#[tokio::test]
async fn stage_metadata_waits_for_raw_parameter_sets_on_file_inputs() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-stage-file-params", "stream-key", "file")
        .await
        .unwrap();

    let ready_audio = crate::media::metadata::AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: Some(0x101),
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta(
            "pipe-stage-file-params",
            Some(crate::media::metadata::VideoMeta {
                codec: "h264".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: Some(0x100),
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            Some(ready_audio.clone()),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-stage-file-params", vec![ready_audio])
        .await;

    let upstream_ring = Arc::new(RingBuffer::new(1024));
    upstream_ring.set_codec_hint("h264");
    let cancel = CancellationToken::new();

    let engine_for_wait = engine.clone();
    let ring_for_wait = upstream_ring.clone();
    let cancel_for_wait = cancel.clone();
    let wait = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_for_wait,
            "pipe-stage-file-params",
            &ring_for_wait,
            true,
            true,
            None,
            &cancel_for_wait,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !wait.is_finished(),
        "file stage metadata should wait until raw parameter sets are cached on the source ring"
    );

    upstream_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE,
        0x38, 0x80,
    ]);

    let (video, audio_tracks) = wait
        .await
        .expect("wait task should join")
        .expect("stage metadata should become ready");

    assert_eq!(video.codec, "h264");
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0].track_index, 0);
}

#[tokio::test]
async fn stage_metadata_requires_raw_parameter_sets_for_hevc_codec_edge_stages() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-stage-codec-edge", "stream-key", "file")
        .await
        .unwrap();

    let ready_audio = crate::media::metadata::AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: Some(0x101),
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta(
            "pipe-stage-codec-edge",
            Some(crate::media::metadata::VideoMeta {
                codec: "hevc".to_string(),
                width: 1280,
                height: 720,
                fps: 30.0,
                bw: None,
                pid: Some(0x100),
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            Some(ready_audio.clone()),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-stage-codec-edge", vec![ready_audio])
        .await;

    let upstream_ring = Arc::new(RingBuffer::new(1024));
    upstream_ring.set_codec_hint("hevc");
    let cancel = CancellationToken::new();
    let engine_for_wait = engine.clone();
    let ring_for_wait = upstream_ring.clone();
    let cancel_for_wait = cancel.clone();
    let wait = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_for_wait,
            "pipe-stage-codec-edge",
            &ring_for_wait,
            true,
            true,
            None,
            &cancel_for_wait,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !wait.is_finished(),
        "HEVC codec-edge stages should wait until upstream raw parameter sets are cached"
    );

    assert!(
        crate::media::codec::annexb_parameter_sets(&[
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ])
        .is_none(),
        "partial HEVC parameter sets should be rejected before they reach the ring cache"
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !wait.is_finished(),
        "HEVC codec-edge stages should keep waiting until VPS/SPS/PPS are all cached"
    );

    upstream_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ]);

    let (video, audio_tracks) = wait
        .await
        .expect("wait task should join")
        .expect("codec-edge stage metadata should become ready once parameter sets exist");

    assert_eq!(video.codec, "hevc");
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0].track_index, 0);
}
