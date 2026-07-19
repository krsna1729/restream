use restream::media::engine::MediaEngine;
use restream::media::input_gate::{InputForwardState, InputPacketBoundary};
use restream::media::snapshots::{AudioMeta, VideoMeta};
use std::sync::Arc;

#[tokio::test]
async fn pipeline_accepts_selected_and_standby_input_sessions() {
    let engine = MediaEngine::new();
    let primary = engine
        .try_register_pipeline_input_attempt("pipeline", "primary", "primary-key", "rtmp", true)
        .await
        .expect("primary registration");

    let backup = engine
        .try_register_pipeline_input_attempt("pipeline", "backup", "backup-key", "srt", false)
        .await
        .expect("backup registration");

    assert_eq!(primary.gate.state(), InputForwardState::Active);
    assert_eq!(backup.gate.state(), InputForwardState::Standby);
    assert!(primary.gate.try_enter(InputPacketBoundary::Other).is_some());
    assert!(backup.gate.try_enter(InputPacketBoundary::Other).is_none());
    assert_eq!(engine.connected_input_count("pipeline").await, 2);
}

#[tokio::test]
async fn promotion_hands_writer_lease_to_backup_at_keyframe() {
    let engine = MediaEngine::new();
    let primary = engine
        .try_register_pipeline_input_attempt("pipeline", "primary", "key-a", "rtmp", true)
        .await
        .expect("primary registration");
    let backup = engine
        .try_register_pipeline_input_attempt("pipeline", "backup", "key-b", "rtmp", false)
        .await
        .expect("backup registration");

    let connected = engine.select_pipeline_input("pipeline", "backup").await;

    assert!(connected);
    assert_eq!(primary.gate.state(), InputForwardState::Standby);
    assert_eq!(backup.gate.state(), InputForwardState::AwaitingKeyframe);
    assert!(backup.gate.try_enter(InputPacketBoundary::Other).is_none());
    assert!(
        backup
            .gate
            .try_enter(InputPacketBoundary::VideoKeyframe)
            .is_some()
    );
    assert_eq!(backup.gate.state(), InputForwardState::Active);
}

#[tokio::test]
async fn stale_disconnect_cannot_unregister_promoted_replacement() {
    let engine = MediaEngine::new();
    let primary = engine
        .try_register_pipeline_input_attempt("pipeline", "primary", "key-a", "rtmp", true)
        .await
        .expect("primary registration");
    let backup = engine
        .try_register_pipeline_input_attempt("pipeline", "backup", "key-b", "srt", false)
        .await
        .expect("backup registration");
    assert!(engine.select_pipeline_input("pipeline", "backup").await);

    let removed = engine
        .unregister_ingest_if_current("pipeline", &primary)
        .await;

    assert!(removed);
    let selected = engine
        .with_active_ingest("pipeline", |ingest| ingest.input_id.clone())
        .await;
    assert_eq!(selected.as_deref(), Some("backup"));
    assert_eq!(backup.gate.state(), InputForwardState::AwaitingKeyframe);
}

#[tokio::test]
async fn selected_reconnect_waits_for_keyframe_when_pipeline_timeline_exists() {
    let engine = MediaEngine::new();
    let first = engine
        .try_register_pipeline_input_attempt("pipeline", "primary", "key-a", "rtmp", true)
        .await
        .expect("first registration");
    first
        .last_forwarded_dts
        .store(4_000, std::sync::atomic::Ordering::Release);
    assert!(
        engine
            .unregister_ingest_if_current("pipeline", &first)
            .await
    );

    let second = engine
        .try_register_pipeline_input_attempt("pipeline", "primary", "key-a", "rtmp", true)
        .await
        .expect("second registration");

    assert_eq!(second.gate.state(), InputForwardState::AwaitingKeyframe);
    assert!(second.gate.try_enter(InputPacketBoundary::Other).is_none());
}

#[tokio::test]
async fn input_preview_ring_exists_only_while_preview_is_requested() {
    let engine = MediaEngine::new();
    let registration = engine
        .try_register_pipeline_input_attempt("pipeline", "backup", "key-b", "srt", false)
        .await
        .expect("backup registration");
    assert!(registration.preview_ring.load_full().is_none());

    let ring = engine
        .ensure_input_preview_ring("backup")
        .await
        .expect("connected input preview ring");
    assert!(registration.preview_ring.load_full().is_some());
    engine.release_input_preview_ring("backup").await;

    assert!(registration.preview_ring.load_full().is_none());
    assert_eq!(Arc::strong_count(&ring), 1);
}

#[tokio::test]
async fn standby_metadata_does_not_replace_selected_input_metadata() {
    let engine = MediaEngine::new();
    let pipeline_ring = engine.get_or_create_pipeline("pipeline").await;
    let primary = engine
        .try_register_pipeline_input_attempt("pipeline", "primary", "key-a", "rtmp", true)
        .await
        .expect("primary registration");
    let backup = engine
        .try_register_pipeline_input_attempt("pipeline", "backup", "key-b", "file", false)
        .await
        .expect("backup registration");
    let primary_video = VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        ..Default::default()
    };
    let backup_video = VideoMeta {
        codec: "hevc".to_string(),
        width: 1280,
        height: 720,
        ..Default::default()
    };
    let backup_audio = AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        ..Default::default()
    };

    engine
        .update_ingest_session_meta(
            "pipeline",
            &primary,
            Some(primary_video.clone()),
            None,
            None,
        )
        .await;
    engine
        .update_ingest_session_meta(
            "pipeline",
            &backup,
            Some(backup_video.clone()),
            Some(backup_audio),
            None,
        )
        .await;

    let selected = engine
        .with_active_ingest("pipeline", |ingest| ingest.metadata())
        .await
        .expect("selected ingest metadata");
    let standby = engine
        .with_ingest_session(&backup, |ingest| ingest.metadata())
        .await
        .expect("standby ingest metadata");
    let selected_video = selected.video.expect("selected video metadata");
    let standby_video = standby.video.expect("standby video metadata");
    assert_eq!(selected_video.codec, primary_video.codec);
    assert_eq!(selected_video.width, primary_video.width);
    assert_eq!(standby_video.codec, backup_video.codec);
    assert_eq!(standby_video.width, backup_video.width);
    assert_eq!(pipeline_ring.codec_hint_str(), "h264");
}
