use super::*;
use crate::domain::stage::StageKind;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use bytes::Bytes;
use std::sync::Arc;

fn ready_video_meta(codec: &str) -> VideoMeta {
    VideoMeta {
        codec: codec.to_string(),
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
    }
}

fn ready_audio_meta(channels: u32) -> AudioMeta {
    AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    }
}

#[tokio::test]
async fn ensure_stage_creates_ring_and_returns_existing_on_reuse() {
    let engine = Arc::new(MediaEngine::new());
    let manager = StageRuntimeManager::new(engine.clone());
    let source = Arc::new(RingBuffer::new(16));
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));

    let (handle1, created1) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    assert!(created1);
    assert_eq!(handle1.ring.codec_hint_str(), "h264");

    let (handle2, created2) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    assert!(!created2);
    assert!(Arc::ptr_eq(&handle1.ring, &handle2.ring));
    let runtimes = engine.stages.runtimes.read().await;
    let runtime = runtimes.get(&key).expect("stage runtime registered");
    let runtime_ring = runtime.ring.as_ref().expect("ring-backed runtime");
    assert!(Arc::ptr_eq(runtime_ring, &handle1.ring));
    handle1.cancel.cancel();
    assert!(
        runtime.cancel.is_cancelled(),
        "runtime and handle must share one cancellation token"
    );
    assert!(Arc::ptr_eq(&runtime.lifecycle, &handle1.lifecycle));
    assert!(Arc::ptr_eq(&runtime.metrics, &handle1.metrics));
}

#[tokio::test]
async fn ensure_stage_replaces_cancelled_runtime() {
    let engine = Arc::new(MediaEngine::new());
    let manager = StageRuntimeManager::new(engine.clone());
    let source = Arc::new(RingBuffer::new(16));
    let key = StageKey::new("pipe-replace", StageKind::video_preset("720p"));

    let (handle1, created1) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    assert!(created1);
    handle1.cancel.cancel();

    let (handle2, created2) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;

    assert!(created2, "cancelled runtime should be replaced");
    assert!(!Arc::ptr_eq(&handle1.ring, &handle2.ring));
    assert!(!handle2.cancel.is_cancelled());
    let runtimes = engine.stages.runtimes.read().await;
    let runtime = runtimes.get(&key).expect("replacement runtime registered");
    let runtime_ring = runtime.ring.as_ref().expect("ring-backed runtime");
    assert!(Arc::ptr_eq(runtime_ring, &handle2.ring));
    assert!(!runtime.cancel.is_cancelled());
}

#[tokio::test]
async fn ensure_stage_uses_engine_typed_transcoder_ring_capacity() {
    let config = Arc::new(crate::AppConfig {
        transcoder_ring_capacity: 768,
        ..Default::default()
    });
    let engine = Arc::new(MediaEngine::new_with_config(config));
    let manager = StageRuntimeManager::new(engine);
    let source = Arc::new(RingBuffer::new(16));
    let key = StageKey::new("pipe-typed", StageKind::video_preset("720p"));

    let (handle, created) = manager.ensure_stage(key, source, None).await;
    assert!(created);
    assert_eq!(handle.ring.capacity(), 768);
}

#[test]
fn codec_edge_plan_can_passthrough_audio_tracks() {
    let key = StageKey::new(
        "pipe-codec-edge",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let video = VideoMeta {
        codec: "hevc".to_string(),
        width: 1920,
        height: 1080,
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
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let plan = build_ffmpeg_stage_plan(&key, Some(video), vec![audio], None, true)
        .expect("codec edge plan");

    assert!(plan.include_audio);
    assert_eq!(plan.input.audio_tracks.len(), 1);
    assert!(matches!(
        plan.video,
        VideoStageOp::CodecEdge {
            op: CodecEdgeOp::HevcToH264
        }
    ));
    assert!(matches!(plan.audio, AudioStageOp::Passthrough));
}

#[test]
fn external_and_internal_stage_plan_share_operation() {
    let key = StageKey::new("pipe-shared-plan", StageKind::video_preset("720p"));
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
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
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    let plan = build_ffmpeg_stage_plan(&key, Some(video), vec![audio], None, true)
        .expect("video preset plan");

    assert!(matches!(
        plan.video,
        VideoStageOp::ScalePreset { ref preset } if preset == "720p"
    ));
    assert!(matches!(plan.audio, AudioStageOp::Passthrough));
    assert_eq!(plan.output_codec, VideoCodecKind::H264);
    assert!(
        plan.startup.wait_for_first_keyframe,
        "shared FFmpeg plan should carry startup policy for both backends"
    );
}

#[test]
fn video_preset_plan_separates_input_and_output_codec() {
    let key = StageKey::new(
        "pipe-cross-codec",
        StageKind::video_preset_with_codec("720p", "h264"),
    );
    let video = VideoMeta {
        codec: "hevc".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let plan = build_ffmpeg_stage_plan(&key, Some(video), Vec::new(), Some("hevc"), true)
        .expect("video preset plan");

    assert_eq!(plan.input.codec_hint, VideoCodecKind::Hevc);
    assert_eq!(plan.output_codec, VideoCodecKind::H264);
}

#[tokio::test]
async fn snapshot_reflects_lifecycle_and_metrics() {
    let engine = Arc::new(MediaEngine::new());
    let manager = StageRuntimeManager::new(engine.clone());
    let source = Arc::new(RingBuffer::new(16));
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));

    let (handle, _) = manager.ensure_stage(key.clone(), source, None).await;
    handle.metrics.record_in_batch(2, 1024);
    handle.metrics.record_out(512);
    handle.lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    let snap = manager.snapshot(&key).await.expect("snapshot exists");
    assert_eq!(snap.key, key);
    assert_eq!(snap.bytes_in, 1024);
    assert_eq!(snap.bytes_out, 512);
    assert_eq!(snap.packets_in, 2);
    assert_eq!(snap.packets_out, 1);
    assert!(matches!(
        snap.phase,
        StagePhase::WaitingForCapacity {
            backend: StageBackendKind::ExternalFfmpeg,
        }
    ));
}

#[tokio::test]
async fn wait_for_stage_metadata_returns_none_immediately_when_already_cancelled() {
    let engine = Arc::new(MediaEngine::new());
    let source = Arc::new(RingBuffer::new(16));
    let cancel = CancellationToken::new();
    cancel.cancel();

    // No ingest is registered for this pipeline at all, so a buggy loop
    // that checked cancellation only after touching ingest state would
    // hang forever instead of returning promptly.
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        wait_for_stage_metadata(&engine, "pipe-cancel", &source, true, false, None, &cancel),
    )
    .await
    .expect("must not hang waiting on a pre-cancelled token");

    assert!(result.is_none());
}

#[tokio::test]
async fn wait_for_stage_metadata_resolves_once_ingest_metadata_becomes_ready() {
    let engine = Arc::new(MediaEngine::new());
    let source = Arc::new(RingBuffer::new(16));
    let cancel = CancellationToken::new();
    let pipeline_id = "pipe-race";

    engine
        .try_register_ingest_attempt(pipeline_id, "key", "rtmp")
        .await
        .expect("first registration for a fresh pipeline id must succeed");

    let engine_clone = engine.clone();
    let source_clone = source.clone();
    let cancel_clone = cancel.clone();
    let waiter = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_clone,
            pipeline_id,
            &source_clone,
            true,
            false,
            Some("h264"),
            &cancel_clone,
        )
        .await
    });

    // Give the poll loop several 25ms iterations to prove it keeps
    // waiting on the empty ingest record instead of resolving early.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(
        !waiter.is_finished(),
        "must keep waiting while no video metadata has been reported yet"
    );

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(ready_video_meta("h264")),
            Some(ready_audio_meta(2)),
            None,
        )
        .await;

    let result = tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
        .await
        .expect("must resolve soon after metadata becomes available")
        .expect("waiter task must not panic");

    let (resolved_video, resolved_audio) = result.expect("metadata must resolve to Some");
    assert_eq!(resolved_video.codec, "h264");
    assert_eq!(resolved_audio.len(), 1);
    assert_eq!(resolved_audio[0].channels, 2);
}

#[tokio::test]
async fn wait_for_stage_metadata_eager_parameter_sets_waits_for_ring_data() {
    let engine = Arc::new(MediaEngine::new());
    let source = Arc::new(RingBuffer::new(16));
    let cancel = CancellationToken::new();
    let pipeline_id = "pipe-eager";

    engine
        .try_register_ingest_attempt(pipeline_id, "key", "srt")
        .await
        .expect("first registration for a fresh pipeline id must succeed");
    engine
        .update_ingest_meta(pipeline_id, Some(ready_video_meta("h264")), None, None)
        .await;

    let engine_clone = engine.clone();
    let source_clone = source.clone();
    let cancel_clone = cancel.clone();
    let waiter = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_clone,
            pipeline_id,
            &source_clone,
            false,
            true,
            Some("h264"),
            &cancel_clone,
        )
        .await
    });

    // Ready video dimensions alone must not satisfy the eager
    // raw-parameter-set gate for a parameter-set codec: no ring
    // parameter sets, no engine sequence header, and no ring packets
    // yet.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(
        !waiter.is_finished(),
        "h264 must wait for parameter sets, a sequence header, or a ring packet"
    );

    source.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x67]),
    });

    let result = tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
        .await
        .expect("must resolve once the ring reports at least one packet")
        .expect("waiter task must not panic");

    assert!(result.is_some());
}

#[tokio::test]
async fn wait_for_stage_metadata_gates_on_audio_track_readiness() {
    let engine = Arc::new(MediaEngine::new());
    let source = Arc::new(RingBuffer::new(16));
    let cancel = CancellationToken::new();
    let pipeline_id = "pipe-audio-gate";

    engine
        .try_register_ingest_attempt(pipeline_id, "key", "rtmp")
        .await
        .expect("first registration for a fresh pipeline id must succeed");
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(ready_video_meta("h264")),
            Some(ready_audio_meta(0)),
            None,
        )
        .await;

    let engine_clone = engine.clone();
    let source_clone = source.clone();
    let cancel_clone = cancel.clone();
    let waiter = tokio::spawn(async move {
        wait_for_stage_metadata(
            &engine_clone,
            pipeline_id,
            &source_clone,
            true,
            false,
            Some("h264"),
            &cancel_clone,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(
        !waiter.is_finished(),
        "an audio track reporting zero channels must not be considered ready"
    );

    engine
        .update_ingest_meta(pipeline_id, None, Some(ready_audio_meta(2)), None)
        .await;

    let result = tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
        .await
        .expect("must resolve once the audio track reports real channels")
        .expect("waiter task must not panic");

    let (_, resolved_audio) = result.expect("metadata must resolve to Some");
    assert_eq!(resolved_audio.len(), 1);
    assert_eq!(resolved_audio[0].channels, 2);
}

#[tokio::test]
async fn wait_for_stage_metadata_backfills_bandwidth_from_observed_ring_bitrate() {
    let engine = Arc::new(MediaEngine::new());
    let source = Arc::new(RingBuffer::new(16));
    let cancel = CancellationToken::new();
    let pipeline_id = "pipe-bw";

    engine
        .try_register_ingest_attempt(pipeline_id, "key", "rtmp")
        .await
        .expect("first registration for a fresh pipeline id must succeed");
    engine
        .update_ingest_meta(pipeline_id, Some(ready_video_meta("h264")), None, None)
        .await;

    // Two packets spanning >= the estimator's 250ms minimum observation
    // window so `observed_payload_bitrate_bps` returns Some.
    source.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from(vec![0u8; 1000]),
    });
    source.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 300,
        dts: 300,
        payload: Bytes::from(vec![0u8; 1000]),
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        wait_for_stage_metadata(
            &engine,
            pipeline_id,
            &source,
            false,
            false,
            Some("h264"),
            &cancel,
        ),
    )
    .await
    .expect("must resolve promptly once video metadata is ready")
    .expect("metadata must resolve to Some");

    let (resolved_video, _) = result;
    let observed = source
        .observed_payload_bitrate_bps()
        .expect("ring must report an observed bitrate for this fixture");
    assert_eq!(resolved_video.bw, Some(observed as f64));
}
