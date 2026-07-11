use super::*;
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::{AudioMeta, MediaEngine};
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::mpegts::TsDemuxer;
use crate::media::ring_buffer::{DtsEnforcer, MediaType, Reader, RingBuffer};
use crate::media::stage_runtime::{build_ffmpeg_stage_plan, wait_for_stage_metadata};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;
use tokio_util::sync::CancellationToken;

fn write_temp_ts_artifact(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "restream-external-transcoder-test-{}-{}",
        std::process::id(),
        name
    ));
    std::fs::create_dir_all(&dir).expect("create temp artifact dir");
    let path = dir.join("artifact.ts");
    std::fs::write(&path, bytes).expect("write temp TS artifact");
    path
}

fn assert_strict_video_dts<'a, I>(label: &str, packets: I)
where
    I: IntoIterator<Item = &'a crate::media::ring_buffer::MediaPacket>,
{
    let mut previous = None;
    let mut count = 0usize;
    for packet in packets
        .into_iter()
        .filter(|packet| packet.media_type == MediaType::Video)
    {
        if let Some(previous_dts) = previous {
            assert!(
                packet.dts > previous_dts,
                "{label} video DTS must be strictly increasing: {previous_dts} >= {}",
                packet.dts
            );
        }
        previous = Some(packet.dts);
        count += 1;
    }
    assert!(count > 0, "{label} should include video packets");
}

fn test_audio_track(track_index: u32) -> AudioMeta {
    AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: Some("stereo".to_string()),
        track_index,
        pid: None,
        language: None,
        title: None,
        profile: None,
    }
}

#[test]
fn external_stage_arg_preset_uses_preview_preset_not_stage_key_display() {
    let key = StageKey::new(
        "pipe-preview",
        StageKind::preview("720p", StageKind::source()),
    );
    let plan = build_ffmpeg_stage_plan(&key, None, Vec::new(), None, false)
        .expect("preview stage should produce an FFmpeg plan");

    assert_eq!(
        external_stage_arg_preset(&plan, &key.kind.to_string()),
        "720p"
    );
}

#[test]
fn external_output_stream_idx_routes_known_tracks_without_aliasing() {
    let audio_tracks = vec![
        test_audio_track(7),
        test_audio_track(2),
        test_audio_track(11),
    ];

    assert_eq!(
        external_output_stream_idx(MediaType::Video, 0, &audio_tracks, true),
        Some(0)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 7, &audio_tracks, true),
        Some(1)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 2, &audio_tracks, true),
        Some(2)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 11, &audio_tracks, true),
        Some(3)
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 99, &audio_tracks, true),
        None
    );
    assert_eq!(
        external_output_stream_idx(MediaType::Audio, 7, &audio_tracks, false),
        None
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn proptest_external_output_dts_routing_preserves_per_stream_monotonicity(
        track_set in proptest::collection::btree_set(0u32..64, 1..=6),
        events in proptest::collection::vec((0u8..4, 0usize..16, -10i64..40, -10i64..40), 1..160),
    ) {
        let audio_tracks = track_set
            .into_iter()
            .map(test_audio_track)
            .collect::<Vec<_>>();
        let mut enforcer = DtsEnforcer::new(1 + audio_tracks.len());
        let mut previous_by_stream = vec![None; 1 + audio_tracks.len()];

        for (kind, index_seed, pts, dts) in events {
            let (media_type, track_index, should_route) = match kind {
                0 => (MediaType::Video, 0, true),
                1 | 2 => {
                    let track = audio_tracks[index_seed % audio_tracks.len()].track_index;
                    (MediaType::Audio, track, true)
                }
                _ => (MediaType::Audio, 10_000 + index_seed as u32, false),
            };

            let stream_idx = external_output_stream_idx(
                media_type,
                track_index,
                &audio_tracks,
                true,
            );
            prop_assert_eq!(stream_idx.is_some(), should_route);

            if let Some(stream_idx) = stream_idx {
                let (out_pts, out_dts) = enforcer.enforce(stream_idx, pts, dts);
                prop_assert!(out_pts >= out_dts);
                if let Some(previous) = previous_by_stream[stream_idx] {
                    prop_assert!(out_dts > previous);
                }
                previous_by_stream[stream_idx] = Some(out_dts);
            }
        }
    }
}

#[tokio::test]
async fn stage_metadata_prefers_upstream_ring_tracks_and_codec_hint() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-stage-meta", "stream-key", "srt")
        .await
        .unwrap();

    let ingest_audio = vec![
        crate::media::engine::AudioMeta {
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
        crate::media::engine::AudioMeta {
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
            Some(crate::media::engine::VideoMeta {
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
    upstream_ring.set_audio_tracks(vec![crate::media::engine::AudioMeta {
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
            Some(crate::media::engine::VideoMeta {
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
            Some(crate::media::engine::AudioMeta {
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

    let ready_audio = crate::media::engine::AudioMeta {
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

    let ready_audio = crate::media::engine::AudioMeta {
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
            Some(crate::media::engine::VideoMeta {
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

    let ready_audio = crate::media::engine::AudioMeta {
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
            Some(crate::media::engine::VideoMeta {
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

    let ready_audio = crate::media::engine::AudioMeta {
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
            Some(crate::media::engine::VideoMeta {
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

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_hevc_sample() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let _ = tracing_subscriber::fmt::try_init();
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-preview", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-preview",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-preview", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    // Extract parameter sets from the pre-demuxed packets so the stage's
    // metadata wait loop can find them (required for HEVC).
    if let Some(ps) = packets.iter().find_map(|p| {
        (p.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&p.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(ps);
    }
    let stage_key = StageKey::new(
        "pipe-ext-preview",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external 720p stage reader did not attach to the source ring in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // Feed all input.  With 18 streams (2v + 16a) FFmpeg holds output
    // until stdin closes, so mark EOS and let the pump drain naturally.
    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|p| p.media_type == MediaType::Video && p.is_keyframe)
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p HEVC preview stage should emit video packets after close (got {} packets)",
        output_packets.len()
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "external 720p HEVC preview stage should emit a keyframe after close (got {} packets)",
        output_packets.len()
    );
    cancel.cancel();
}

#[tokio::test]
async fn chained_hevc_preview_stages_emit_live_h264_packets() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-preview-chain", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-preview-chain",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-preview-chain", audio_tracks.clone())
        .await;

    let source_ring = engine
        .get_or_create_pipeline("pipe-ext-preview-chain")
        .await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }

    let hevc_stage_key = StageKey::new("pipe-ext-preview-chain", StageKind::video_preset("1080p"));
    let hevc_preview_upstream = engine
        .get_or_create_transcoder(
            "pipe-ext-preview-chain",
            StageKind::video_preset("1080p"),
            source_ring.clone(),
            Some("hevc"),
        )
        .await;
    let h264_stage_key = StageKey::new(
        "pipe-ext-preview-chain",
        StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("1080p")),
    );
    let h264_preview_ring = engine
        .get_or_create_h264_transcoder(
            "pipe-ext-preview-chain",
            StageKind::video_preset("1080p"),
            hevc_preview_upstream.clone(),
        )
        .await;
    let mut hevc_reader = Reader::new_live(
        "test_ext_preview_chain_mid".to_string(),
        hevc_preview_upstream.clone(),
    );
    let mut reader = Reader::new_live(
        "test_ext_preview_chain_output".to_string(),
        h264_preview_ring.clone(),
    );

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let source_attached = source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&hevc_stage_key.to_string()));
        if source_attached {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "preview upstream reader did not attach in time: source={:?} expected_source={} hevc_stage={:?}",
            source_ring.reader_snapshots(),
            hevc_stage_key,
            engine.stage_runtime_snapshot(&hevc_stage_key).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
    let mut hevc_packets = Vec::new();
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = hevc_reader.pull() {
            hevc_packets.push(packet);
        }
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    assert_eq!(
        h264_preview_ring.codec_hint_str(),
        "h264",
        "preview codec-edge ring should advertise H.264 output"
    );
    assert!(
        hevc_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "preview chain should first emit live HEVC packets from the 1080p stage"
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "chained HEVC preview stages should emit live video packets; h264_stage={:?}",
        engine.stage_runtime_snapshot(&h264_stage_key).await
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "chained HEVC preview stages should emit a live keyframe"
    );
}

#[tokio::test]
async fn hevc_scaled_rtmp_audio_routes_emit_both_selected_tracks() {
    let fixture = crate::test_fixtures::av_marker_transport_fixture("h265", true)
        .expect("multi-audio HEVC marker fixture");
    let fixture_bytes = std::fs::read(fixture).expect("read multi-audio HEVC marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in fixture_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer
        .take_probe()
        .expect("probe multi-audio HEVC marker fixture");
    let video = probe.video.expect("sample should contain video");
    let audio_tracks = probe.audio_tracks;
    assert!(
        audio_tracks.len() >= 2,
        "fixture must expose at least two audio tracks"
    );

    let pipeline_id = "pipe-ext-scaled-audio-routes";
    let engine = Arc::new(MediaEngine::new());
    let _ = tracing_subscriber::fmt::try_init();
    engine
        .try_register_ingest(pipeline_id, "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(pipeline_id, audio_tracks.clone())
        .await;

    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }

    let scaled_hevc = engine
        .get_or_create_transcoder(
            pipeline_id,
            StageKind::video_preset("720p"),
            source_ring.clone(),
            Some("hevc"),
        )
        .await;
    let scaled_h264 = engine
        .get_or_create_h264_transcoder(
            pipeline_id,
            StageKind::video_preset("720p"),
            scaled_hevc.clone(),
        )
        .await;
    let track0_ring = engine
        .get_or_create_transcoder(
            pipeline_id,
            StageKind::audio_route(
                "atrack:0",
                StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
            ),
            scaled_h264.clone(),
            None,
        )
        .await;
    let track1_ring = engine
        .get_or_create_transcoder(
            pipeline_id,
            StageKind::audio_route(
                "atrack:1",
                StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
            ),
            scaled_h264.clone(),
            None,
        )
        .await;

    let mut scaled_hevc_reader =
        Reader::new_live("test_ext_scaled_hevc_mid".to_string(), scaled_hevc.clone());
    let mut scaled_h264_reader =
        Reader::new_live("test_ext_scaled_h264_mid".to_string(), scaled_h264.clone());
    let mut track0_reader =
        Reader::new_live("test_ext_scaled_audio_track0".to_string(), track0_ring);
    let mut track1_reader =
        Reader::new_live("test_ext_scaled_audio_track1".to_string(), track1_ring);

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let source_attached = source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains("video:720p"));
        let h264_attached = scaled_h264.reader_snapshots().len() >= 2;
        if source_attached && h264_attached {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "scaled RTMP selected-audio chain readers did not attach in time: \
             source={:?} scaled_hevc={:?} scaled_h264={:?}",
            source_ring.reader_snapshots(),
            scaled_hevc.reader_snapshots(),
            scaled_h264.reader_snapshots()
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut scaled_hevc_packets = Vec::new();
    let mut scaled_h264_packets = Vec::new();
    let mut track0_packets = Vec::new();
    let mut track1_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = scaled_hevc_reader.pull() {
            scaled_hevc_packets.push(packet);
        }
        while let Ok(Some(packet)) = scaled_h264_reader.pull() {
            scaled_h264_packets.push(packet);
        }
        while let Ok(Some(packet)) = track0_reader.pull() {
            track0_packets.push(packet);
        }
        while let Ok(Some(packet)) = track1_reader.pull() {
            track1_packets.push(packet);
        }
        let track0_ready = selected_track_output_ready(&track0_packets);
        let track1_ready = selected_track_output_ready(&track1_packets);
        if track0_ready && track1_ready {
            break;
        }
        assert!(
            tokio::time::Instant::now() < output_deadline,
            "scaled RTMP selected-audio routes did not both emit video and selected audio: \
             source_write={} source_eos={} scaled_hevc_write={} scaled_hevc_eos={} \
             scaled_h264_write={} scaled_h264_eos={} scaled_hevc_packets={} \
             scaled_h264_packets={} track0_packets={} track1_packets={} stages={:?}",
            source_ring.get_write_idx(),
            source_ring.is_end_of_stream(),
            scaled_hevc.get_write_idx(),
            scaled_hevc.is_end_of_stream(),
            scaled_h264.get_write_idx(),
            scaled_h264.is_end_of_stream(),
            scaled_hevc_packets.len(),
            scaled_h264_packets.len(),
            track0_packets.len(),
            track1_packets.len(),
            engine.pipeline_stage_runtime_snapshots(pipeline_id).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn selected_track_output_ready(
    packets: &[std::sync::Arc<crate::media::ring_buffer::MediaPacket>],
) -> bool {
    packets
        .iter()
        .any(|packet| packet.media_type == MediaType::Video)
        && packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Audio && packet.track_index == 0)
        && packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Audio)
            .all(|packet| packet.track_index == 0)
}

#[tokio::test]
async fn hevc_hls_preview_stage_uses_hevc_input_and_emits_h264() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
    engine
        .try_register_ingest("pipe-hevc-preview-input", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-hevc-preview-input",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-hevc-preview-input", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }

    let stage_key = StageKey::new(
        "pipe-hevc-preview-input",
        StageKind::preview("720p", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    assert_eq!(
        output_ring.codec_hint_str(),
        "h264",
        "preview output ring should advertise browser-safe H.264"
    );
    let mut reader = Reader::new_live("test_hevc_preview_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_preview_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "HEVC preview stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    cancel.cancel();

    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "HEVC preview stage should demux HEVC input and emit H.264 video"
    );
    assert!(
        output_packets
            .iter()
            .all(|packet| packet.media_type == MediaType::Video),
        "preview stage should drop audio packets"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_h264_marker_fixture() {
    let path = crate::test_fixtures::av_marker_transport_fixture("h264", false)
        .expect("H.264 marker fixture");
    let file_bytes = std::fs::read(&path).expect("read H.264 marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer.take_probe().expect("probe H.264 marker fixture");
    let video = probe.video.expect("marker fixture should contain video");
    let audio_tracks = probe.audio_tracks;

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-h264-marker", "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-h264-marker",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-h264-marker", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("h264");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new("pipe-ext-h264-marker", StageKind::video_preset("720p"));
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_marker_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_stage(handle, source_ring.clone(), None);

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external H.264 marker stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external H.264 marker stage should emit live video packets"
    );
    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "external H.264 marker stage should emit a live keyframe"
    );
    assert!(
        source_ring.video_parameter_sets().is_some(),
        "source ring should cache raw parameter sets for the marker fixture"
    );
    assert!(
        reader.current_ring().video_parameter_sets().is_some(),
        "external H.264 marker stage output ring should cache raw parameter sets"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_h264_marker_fixture_without_eos() {
    let path = crate::test_fixtures::av_marker_transport_fixture_for_bframes(
        "h264",
        false,
        crate::test_fixtures::AvMarkerBframeMode::Bf0,
    )
    .expect("H.264 marker fixture");
    let file_bytes = std::fs::read(&path).expect("read H.264 marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer.take_probe().expect("probe H.264 marker fixture");
    let video = probe.video.expect("marker fixture should contain video");
    let audio_tracks = probe.audio_tracks;

    let pipeline_id = "pipe-ext-h264-marker-live";
    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest(pipeline_id, "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks(pipeline_id, audio_tracks.clone())
        .await;

    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new(pipeline_id, StageKind::video_preset("720p"));
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_marker_live_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_stage(handle, source_ring.clone(), None);

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external live H.264 marker stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    let mut sent = 0usize;
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        let end = (sent + 8).min(packets.len());
        if sent < end {
            source_ring.push_batch(packets[sent..end].iter().cloned());
            sent = end;
        }
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        if sent == packets.len() {
            sent = 0;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external H.264 marker stage should emit live video packets without requiring EOS"
    );
}

#[tokio::test]
async fn external_1080p_stage_remuxes_marker_fixture_with_monotone_dts() {
    let path = crate::test_fixtures::av_marker_transport_fixture("h264", false)
        .expect("H.264 marker fixture");
    let file_bytes = std::fs::read(&path).expect("read H.264 marker fixture");
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);

    let probe = demuxer.take_probe().expect("probe H.264 marker fixture");
    let video = probe.video.expect("marker fixture should contain video");
    let audio_tracks = probe.audio_tracks;

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-h264-marker-1080p", "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-h264-marker-1080p",
            Some(video.clone()),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-h264-marker-1080p", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("h264");
    source_ring.set_audio_tracks(audio_tracks.clone());
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new(
        "pipe-ext-h264-marker-1080p",
        StageKind::video_preset("1080p"),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_marker_1080p_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_stage(handle, source_ring.clone(), None);

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external H.264 marker 1080p stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    source_ring.mark_end_of_stream();
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .filter(|packet| packet.media_type == MediaType::Video)
            .count()
            >= 120
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 1080p H.264 marker stage should emit live video packets"
    );

    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();
    for packet in &output_packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }
    assert_strict_video_dts(
        "stage output",
        output_packets.iter().map(std::sync::Arc::as_ref),
    );

    let mut remux_demuxer = TsDemuxer::new();
    let mut remuxed_packets = Vec::new();
    for chunk in ts_bytes.chunks(1316) {
        remux_demuxer.feed(chunk);
        remux_demuxer.drain_into(&mut remuxed_packets);
    }
    remux_demuxer.flush();
    remux_demuxer.drain_into(&mut remuxed_packets);
    assert_strict_video_dts("remuxed output", remuxed_packets.iter());
}

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_single_audio_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-hevc-single-audio", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-hevc-single-audio",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-hevc-single-audio", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new(
        "pipe-ext-hevc-single-audio",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_single_audio_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external 720p single-audio HEVC stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p single-audio HEVC stage should emit live video packets"
    );
    assert!(
        reader.current_ring().video_parameter_sets().is_some(),
        "external 720p HEVC stage output ring should cache raw parameter sets for chained stages"
    );
}

#[tokio::test]
async fn external_h264_stage_emits_live_packets_for_single_audio_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-hevc-source-h264", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-hevc-source-h264",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-hevc-source-h264", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    let stage_key = StageKey::new(
        "pipe-ext-hevc-source-h264",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_h264_single_audio_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external source-h264 single-audio HEVC stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));
    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external source-h264 single-audio HEVC stage should emit live video packets"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_video_for_prebuffered_single_audio_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");
    let continuation = packets
        .iter()
        .rev()
        .take(96)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-hevc-prebuffered", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-hevc-prebuffered",
            Some(video.clone()),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-hevc-prebuffered", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(16_384));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks.clone());
    if let Some(parameter_sets) = packets.iter().find_map(|packet| {
        (packet.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(parameter_sets);
    }
    source_ring.push_batch(packets.drain(..));

    let stage_key = StageKey::new(
        "pipe-ext-hevc-prebuffered",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_prebuffered_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    source_ring.push_batch(continuation);

    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p HEVC stage should emit video once a prebuffered join receives live continuation"
    );
}

#[tokio::test]
async fn external_720p_stage_emits_live_packets_for_canonical_hevc_fixture() {
    let (video, audio_tracks, mut packets) =
        crate::test_fixtures::primary_av_packets_for_codec("h265")
            .expect("single-audio HEVC fixture");

    let engine = Arc::new(MediaEngine::new());
    engine
        .try_register_ingest("pipe-ext-preview-long", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-ext-preview-long",
            Some(video),
            audio_tracks.first().cloned(),
            None,
        )
        .await;
    engine
        .update_ingest_audio_tracks("pipe-ext-preview-long", audio_tracks.clone())
        .await;

    let source_ring = Arc::new(RingBuffer::new(32_768));
    source_ring.set_codec_hint("hevc");
    source_ring.set_audio_tracks(audio_tracks);
    // Extract parameter sets from the pre-demuxed packets so the stage's
    // metadata wait loop can find them (required for HEVC).
    if let Some(ps) = packets.iter().find_map(|p| {
        (p.media_type == MediaType::Video)
            .then(|| crate::media::codec::annexb_parameter_sets(&p.payload))
            .flatten()
    }) {
        source_ring.set_video_parameter_sets(ps);
    }
    let stage_key = StageKey::new(
        "pipe-ext-preview-long",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, is_new) = manager
        .ensure_stage(stage_key.clone(), source_ring.clone(), None)
        .await;
    assert!(is_new);
    let output_ring = handle.ring.clone();
    let mut reader = Reader::new_live("test_ext_720p_long_output".to_string(), output_ring);
    let cancel = handle.cancel.clone();

    manager.spawn_codec_edge_stage(handle, source_ring.clone());

    let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if source_ring
            .reader_snapshots()
            .iter()
            .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < ready_deadline,
            "external 720p long-input HEVC stage reader did not attach in time"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    source_ring.push_batch(packets.drain(..));

    let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(16);
    let mut output_packets = Vec::new();
    loop {
        while let Ok(Some(packet)) = reader.pull() {
            output_packets.push(packet);
        }
        if output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video)
        {
            break;
        }
        if tokio::time::Instant::now() >= output_deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    cancel.cancel();

    assert!(
        output_packets
            .iter()
            .any(|packet| packet.media_type == MediaType::Video),
        "external 720p HEVC preview stage should emit live video packets for the canonical fixture (got {} packets)",
        output_packets.len()
    );
}

#[test]
fn feeder_remuxed_single_audio_hevc_fixture_decodes_as_ts_file() {
    let (video, audio_tracks, packets) = crate::test_fixtures::primary_av_packets_for_codec("h265")
        .expect("single-audio HEVC fixture");
    let mut feeder = TsPacketFeeder::new(
        Some(&video),
        std::sync::Arc::new(audio_tracks),
        PacketFeedConfig::default(),
    );
    let mut ts_bytes = Vec::new();
    let mut packet_buf = Vec::new();

    for packet in &packets {
        packet_buf.clear();
        if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
            ts_bytes.extend_from_slice(&packet_buf);
        }
    }

    assert!(
        !ts_bytes.is_empty(),
        "remuxed HEVC fixture should produce TS bytes"
    );

    let ts_path = write_temp_ts_artifact("hevc-feeder-remux", &ts_bytes);
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            ts_path.to_str().expect("utf-8 ts path"),
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("spawn ffmpeg decode check");

    assert!(
        output.status.success(),
        "ffmpeg should decode feeder-remuxed HEVC TS: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
