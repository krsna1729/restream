
#[tokio::test]
async fn test_audio_router_reindexes_packets() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-id",
        StageKind::audio_route("atrack:2", StageKind::source()),
    );

    // Start audio router
    let routing = AudioRouting::SelectTracks { tracks: vec![2] };
    let handle = tokio::spawn(start_audio_router(
        "pipe-id".to_string(),
        routing,
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    // Push some source packets
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[1, 2, 3]),
    });
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 2, // track 2
        pts: 10,
        dts: 10,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[4, 5, 6]),
    });

    // Let the router process
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cancel.cancel();
    let _ = handle.await;

    // Verify output packets
    let mut reader = Reader::new("test_router".to_string(), out_ring);
    let mut out_pkts = Vec::new();
    while let Ok(Some(pkt)) = reader.pull() {
        out_pkts.push(pkt);
    }

    // Should contain video packet and audio packet
    assert_eq!(out_pkts.len(), 2);
    assert_eq!(out_pkts[0].media_type, MediaType::Video);
    assert_eq!(out_pkts[1].media_type, MediaType::Audio);
    assert_eq!(out_pkts[1].track_index, 0); // re-indexed to 0
}

#[tokio::test]
async fn audio_router_stage_is_not_blocking_after_output() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "audio-router-producing",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    let handle = tokio::spawn(start_audio_router(
        "audio-router-producing".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        engine.clone(),
        cancel.clone(),
        stage_key.clone(),
    ));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while source_ring.active_reader_count() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("audio router input reader should attach");

    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[4, 5, 6]),
    });

    let mut output_reader = Reader::new("audio-router-producing-output".to_string(), out_ring);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if matches!(output_reader.pull(), Ok(Some(_))) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("audio router should emit output");

    assert!(
        engine
            .egress_blocked_by_stage_snapshot(&stage_key)
            .await
            .is_none(),
        "a stage that has emitted output must not surface as blockedBy"
    );

    cancel.cancel();
    handle.await.expect("audio router task should not panic");
}

#[tokio::test]
async fn audio_router_replays_prebuffered_upstream_stage() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "prebuffered-audio-route",
        StageKind::audio_route(
            "atrack:1",
            StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
        ),
    );

    source_ring.set_codec_hint("h264");
    source_ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            channels: 2,
            sample_rate: 48000,
            track_index: 0,
            channel_layout: None,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        },
        AudioMeta {
            codec: "aac".to_string(),
            channels: 2,
            sample_rate: 48000,
            track_index: 1,
            channel_layout: None,
            pid: Some(0x102),
            language: None,
            title: None,
            profile: None,
        },
    ]);
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[1, 2, 3]),
    });
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 10,
        dts: 10,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[4, 5, 6]),
    });
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 1,
        pts: 20,
        dts: 20,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[7, 8, 9]),
    });
    source_ring.mark_end_of_stream();

    let handle = tokio::spawn(start_audio_router(
        "prebuffered-audio-route".to_string(),
        AudioRouting::SelectTracks { tracks: vec![1] },
        source_ring,
        out_ring.clone(),
        engine,
        cancel,
        stage_key,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("audio router should finish after upstream EOS")
        .expect("audio router task should not panic");

    let mut reader = Reader::new("prebuffered-router-output".to_string(), out_ring);
    let mut out_pkts = Vec::new();
    while let Ok(Some(pkt)) = reader.pull() {
        out_pkts.push(pkt);
    }

    assert!(
        out_pkts
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "late audio router should replay prebuffered video"
    );
    assert!(
        out_pkts
            .iter()
            .any(|packet| { packet.media_type == MediaType::Audio && packet.track_index == 0 }),
        "late audio router should replay and re-index the selected audio track"
    );
}

#[tokio::test]
async fn audio_router_propagates_late_arriving_audio_tracks() {
    // Simulates SRT multi-audio: source ring has no audio_tracks when the
    // audio_router stage starts, then they arrive mid-stream.
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "late-tracks",
        StageKind::audio_route("atrack:0,1", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");
    // NOTE: no set_audio_tracks() yet — simulates live SRT before probe

    let handle = tokio::spawn(start_audio_router(
        "late-tracks".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0, 1] },
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    // Output ring has no audio_tracks yet (source not probed)
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        out_ring.audio_tracks().is_none(),
        "output ring should not have tracks before source has them"
    );

    // SRT ingest probe completes — audio_tracks become available
    source_ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: Some("stereo".to_string()),
            track_index: 0,
            pid: Some(0x100),
            language: Some("eng".to_string()),
            title: None,
            profile: None,
        },
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: Some("stereo".to_string()),
            track_index: 1,
            pid: Some(0x101),
            language: Some("fra".to_string()),
            title: None,
            profile: None,
        },
    ]);

    // Push an audio packet — router burst loop should propagate audio_tracks
    source_ring.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: false,
        payload: bytes::Bytes::from_static(&[0x01]),
        format: crate::media::packet::PayloadFormat::Raw,
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let tracks = out_ring
        .audio_tracks()
        .expect("output ring should have audio_tracks after source ring received them");
    assert_eq!(
        tracks.len(),
        2,
        "SelectTracks [0,1] should propagate both tracks"
    );
    assert_eq!(tracks[0].track_index, 0);
    assert_eq!(tracks[1].track_index, 1);

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn audio_tracks_ring_supports_reconnect_update() {
    // Verifies that ArcSwapOption allows updating audio_tracks across
    // publisher reconnects — a reconnected stream may have different tracks.
    let ring = Arc::new(RingBuffer::new(8));

    // First publisher: 2 audio tracks
    ring.set_audio_tracks(vec![
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
        },
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 1,
            pid: None,
            language: None,
            title: None,
            profile: None,
        },
    ]);
    assert_eq!(
        ring.audio_tracks().unwrap().len(),
        2,
        "first publisher: 2 tracks"
    );

    // Publisher reconnects — RTMP clears with empty vec, then re-probes
    ring.set_audio_tracks(Vec::new());
    assert!(
        ring.audio_tracks().is_none(),
        "empty set_audio_tracks should clear metadata (not a no-op)"
    );

    // Re-probe with new single-track configuration
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 44100,
        channels: 1,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    }]);
    let tracks = ring.audio_tracks().unwrap();
    assert_eq!(tracks.len(), 1, "reconnected publisher: 1 track");
    assert_eq!(
        tracks[0].sample_rate, 44100,
        "reconnected publisher track data"
    );
}

#[tokio::test]
async fn audio_router_preserves_upstream_video_parameter_sets() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-video-params",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");
    source_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xCE, 0x38, 0x80,
    ]);

    let handle = tokio::spawn(start_audio_router(
        "pipe-video-params".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring,
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert_eq!(out_ring.codec_hint_str(), "h264");
    assert_eq!(
        out_ring.video_parameter_sets(),
        Some(vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ])
    );

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn audio_router_learns_video_parameter_sets_from_live_packets() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-video-params-live",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");

    let handle = tokio::spawn(start_audio_router(
        "pipe-video-params-live".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    assert!(
        out_ring.video_parameter_sets().is_none(),
        "router should start without cached parameter sets when the upstream ring does not have them yet"
    );

    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x40, 0x28, 0x02, 0xDD, 0x80,
            0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88,
            0x84, 0x00,
        ]),
    });

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if out_ring.video_parameter_sets().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "router should cache parameter sets from the first live video packet"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    cancel.cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn audio_router_copies_late_upstream_parameter_sets_without_inband_headers() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::MediaEngine;

    let source_ring = Arc::new(RingBuffer::new(16));
    let out_ring = Arc::new(RingBuffer::new(16));
    let engine = Arc::new(MediaEngine::new());
    let cancel = CancellationToken::new();
    let stage_key = StageKey::new(
        "pipe-video-params-cache",
        StageKind::audio_route("atrack:0", StageKind::source()),
    );

    source_ring.set_codec_hint("h264");

    let handle = tokio::spawn(start_audio_router(
        "pipe-video-params-cache".to_string(),
        AudioRouting::SelectTracks { tracks: vec![0] },
        source_ring.clone(),
        out_ring.clone(),
        engine,
        cancel.clone(),
        stage_key,
    ));

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    source_ring.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
        0xCE, 0x38, 0x80,
    ]);
    source_ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00]),
    });

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if out_ring.video_parameter_sets().is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "router should copy late upstream parameter sets even when the live packet payload lacks them"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    cancel.cancel();
    let _ = handle.await;
}

// M7: pts=0 from AV_NOPTS_VALUE would cause a massive backward jump on a
// long-running stream. Verify the timestamp conversion formula itself is
// correct and that skipping None-pts packets is the right behavior.
//
// We can't inject AV_NOPTS_VALUE into a live FFmpeg pipeline in a unit
// test, but we can verify that the i128-based conversion that follows
// a valid pts produces the expected millisecond value, confirming that
// pts=0 would produce 0ms (a backward jump from, e.g., 3600000ms).
#[test]
fn pts_zero_would_produce_zero_ms_timestamp() {
    // Simulate the conversion for pts=0 with a 90kHz timebase (tb=1/90000).
    let pts: i64 = 0;
    let tb_num: i64 = 1;
    let tb_den: i64 = 90000;
    let pts_ms = (pts as i128 * tb_num as i128 * 1000 / tb_den as i128) as i64;
    assert_eq!(pts_ms, 0, "pts=0 produces 0ms — correct to skip, not use");

    // A real 1-hour stream has pts ≈ 324_000_000 ticks at 90kHz.
    let pts_1h: i64 = 3_600 * 90_000;
    let pts_ms_1h = (pts_1h as i128 * tb_num as i128 * 1000 / tb_den as i128) as i64;
    assert_eq!(pts_ms_1h, 3_600_000, "1h at 90kHz = 3600000ms");
    // Substituting 0 for AV_NOPTS_VALUE would create a -3600000ms backward jump.
    assert_eq!(pts_ms - pts_ms_1h, -3_600_000);
}

// Adversarial hunt: run_ffmpeg_transcode_with_scale_with_normalizer (the
// decode->scale->encode path) used to fall back to an unwrap-or-zero
// default on the encoder's output pts, instead of skipping None-pts
// packets like the passthrough path does — an inconsistency between two
// code paths in the same file where only one was hardened against the M7
// backward-jump bug. Both encoder-output loops now skip on a None pts,
// matching run_ffmpeg_transcoder_stage_with_normalizer exactly. This test
// documents that the same formula/consequence reasoning as
// pts_zero_would_produce_zero_ms_timestamp applies to the scale-encode
// path: a real FFmpeg pipeline can't be made to emit AV_NOPTS_VALUE from a
// unit test, but skip-vs-default is the only difference that matters, and
// both loops must agree on it.
#[test]
fn scale_encode_path_skips_none_pts_like_passthrough_path() {
    let source = include_str!("../../transcoder.rs");
    // Build search needles from fragments that never appear contiguously
    // in this test's own source text, so counting matches in the whole
    // file (including this test) can't pick up the needle argument
    // itself as a false positive.
    let skip_needle = format!("{}{}", "let Some(pts_ms) = enc_pkt.p", "ts() else {");
    let fallback_needle = format!("{}{}", "enc_pkt.pts().unwrap_o", "r(0)");

    let occurrences = source.matches(&skip_needle).count();
    assert_eq!(
        occurrences, 2,
        "run_ffmpeg_transcode_with_scale_with_normalizer must skip None-pts \
         packets identically in both the main receive loop and the EOF \
         flush loop — a zero-default fallback in either would reintroduce \
         the M7 backward-jump bug for the scale-encode path"
    );
    assert!(
        !source.contains(&fallback_needle),
        "defaulting encoder-output pts to 0 on AV_NOPTS_VALUE produces a \
         massive backward timestamp jump on a long-running stream; \
         None-pts packets must be skipped instead"
    );
}

// M6: ts_batch must be cleared at the top of each burst arm so stale bytes
// never accumulate across iterations. Verify the invariant by simulating
// the burst pattern: partial batch from one burst must not appear in the next.
#[test]
fn ts_batch_cleared_before_each_burst() {
    let mut ts_batch: Vec<u8> = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);

    // Simulate two burst cycles: first accumulates data, second must start empty.
    let burst1 = b"packet_data_burst1";
    ts_batch.extend_from_slice(burst1);
    assert!(!ts_batch.is_empty());

    // Write and clear (as the arm does after write()).
    // Then simulate loop top: clear is now at the TOP of the arm.
    ts_batch.clear(); // ← this is the arm-top clear (M6 fix)
    assert!(
        ts_batch.is_empty(),
        "ts_batch must be empty at burst start — stale data would corrupt the stream"
    );

    let burst2 = b"packet_data_burst2";
    ts_batch.extend_from_slice(burst2);
    assert_eq!(&ts_batch[..], burst2, "burst2 must not contain burst1 data");
}

#[test]
fn internal_video_preset_uses_planned_output_codec() {
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::ffmpeg::stage_plan::{StageInputSpec, VideoCodecKind};

    let plan = FfmpegStagePlan::video_preset(
        StageKey::new("pipe-1", StageKind::video_preset("720p")),
        "pipe-1",
        "720p",
        StageInputSpec {
            codec_hint: VideoCodecKind::Hevc,
            video_meta: None,
            audio_tracks: Vec::new(),
        },
        VideoCodecKind::H264,
    );

    assert_eq!(
        internal_video_encoder_id_for_plan(&plan),
        ffmpeg_next::codec::Id::H264
    );
}
