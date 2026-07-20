use super::*;

#[tokio::test]
async fn test_hls_consumers_monotonic_idle() {
    let cancel = CancellationToken::new();
    let hc = HlsConsumers::new(cancel);
    assert!(!hc.is_idle(60000));

    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    hc.touch();
    let last = hc.last_access_ms.load(Ordering::Relaxed);
    assert!(last > 0);

    tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
    assert!(!hc.is_idle(60000));
    assert!(hc.is_idle(10));
}

#[tokio::test]
async fn runtime_helpers_expose_registered_ingest_and_egress() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-runtime", "stream-key", "rtmp")
        .await
        .expect("register ingest");
    engine.update_ingest_bytes("pipe-runtime", 2048).await;

    let ingest = engine
        .with_active_ingest("pipe-runtime", |ingest| {
            (
                ingest.protocol.clone(),
                ingest.bytes_received.load(Ordering::Relaxed),
            )
        })
        .await;
    assert_eq!(ingest, Some(("rtmp".to_string(), 2048)));
    assert_eq!(engine.with_active_ingest("missing", |_| true).await, None);

    engine
        .register_egress("out-runtime", "pipe-runtime", "rtmp://example.com/live/key")
        .await;
    engine.record_egress_progress("out-runtime", 1316).await;

    let egress = engine
        .with_active_egress("out-runtime", |egress| {
            (
                egress.protocol.clone(),
                egress.bytes_sent.load(Ordering::Relaxed),
                egress
                    .phase
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .to_string(),
            )
        })
        .await;
    assert_eq!(
        egress,
        Some(("rtmp".to_string(), 1316, "sending".to_string()))
    );
    assert_eq!(engine.with_active_egress("missing", |_| true).await, None);
}

#[tokio::test]
async fn hls_dependency_snapshot_reflects_store_and_consumer_state() {
    let engine = MediaEngine::new();
    let (store, already_running, _cancel_token) = engine
        .ensure_hls_preview_segmenter("pipe-hls-snapshot")
        .await;
    assert!(!already_running);

    engine.touch_hls_preview("pipe-hls-snapshot").await;
    store.push_video_segment(0, 2.0, Bytes::from_static(b"segment"));

    let snapshot = engine.hls_dependency_snapshot("pipe-hls-snapshot").await;
    assert!(snapshot.store_exists);
    assert!(snapshot.active);
    assert_eq!(snapshot.persistent_consumers, 0);
    assert!(snapshot.last_access_age_ms.is_some());
    assert_eq!(snapshot.segments, 1);
    assert!(snapshot.playlist_bytes > 0);
}

#[tokio::test]
async fn active_hls_preview_stage_keys_include_hevc_preview_codec_edge() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-preview-hevc", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-preview-hevc",
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    let _ = engine
        .ensure_hls_preview_segmenter("pipe-preview-hevc")
        .await;

    let stages = engine.active_hls_preview_stage_keys().await;

    assert!(stages.contains(&StageKey::new(
        "pipe-preview-hevc",
        StageKind::preview("720p", StageKind::source())
    )));
    assert!(stages.contains(&StageKey::new(
        "pipe-preview-hevc",
        StageKind::hls_segmenter(StageKind::preview("720p", StageKind::source()))
    )));
}

#[tokio::test]
async fn active_hls_preview_stage_keys_include_h264_segmenter() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-preview-h264", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-preview-h264",
            Some(VideoMeta {
                codec: "h264".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    let _ = engine
        .ensure_hls_preview_segmenter("pipe-preview-h264")
        .await;

    let stages = engine.active_hls_preview_stage_keys().await;

    assert_eq!(stages.len(), 1);
    assert!(stages.contains(&StageKey::new(
        "pipe-preview-h264",
        StageKind::hls_segmenter(StageKind::source())
    )));
}

#[tokio::test]
async fn preview_blocked_by_snapshot_uses_graph_planned_keys() {
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        external_ffmpeg_permits: 3,
        ..Default::default()
    }));
    let pipeline_id = "pipe-preview-blocked";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;

    let rogue_key = StageKey::new(
        pipeline_id,
        StageKind::preview("720p", StageKind::video_preset("rogue")),
    );
    let rogue_lifecycle = engine
        .get_or_create_stage_lifecycle(rogue_key, StagePhase::Registered)
        .await;
    rogue_lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    assert_eq!(
        engine.preview_blocked_by_snapshot(pipeline_id).await,
        None,
        "unplanned preview-looking stages must not drive HLS preview blocked cause"
    );

    let planned_key = StageKey::new(pipeline_id, StageKind::preview("720p", StageKind::source()));
    let planned_lifecycle = engine
        .get_or_create_stage_lifecycle(planned_key.clone(), StagePhase::Registered)
        .await;
    planned_lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    let blocked = engine
        .preview_blocked_by_snapshot(pipeline_id)
        .await
        .expect("planned preview stage should be reported as blocked");
    assert_eq!(blocked.key, planned_key);
    assert_eq!(blocked.capacity_permits_total, Some(3));
}

#[tokio::test]
async fn file_ingest_dependency_snapshot_reflects_active_and_child_state() {
    let engine = MediaEngine::new();
    engine
        .mark_file_ingest_running("file-ingest-snapshot")
        .await;

    let child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep child");
    engine
        .file_ingests
        .children
        .write()
        .await
        .insert("file-ingest-snapshot".to_string(), child);

    let snapshot = engine
        .file_ingest_dependency_snapshot("file-ingest-snapshot")
        .await;
    assert!(snapshot.marked_active);
    assert!(snapshot.child_registered);

    assert!(engine.stop_file_ingest_child("file-ingest-snapshot").await);
    engine
        .clear_file_ingest_running("file-ingest-snapshot")
        .await;

    let snapshot = engine
        .file_ingest_dependency_snapshot("file-ingest-snapshot")
        .await;
    assert!(!snapshot.marked_active);
    assert!(!snapshot.child_registered);
}
