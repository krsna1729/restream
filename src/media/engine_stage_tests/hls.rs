use super::*;

// M2: get_hls_cancel_token must return None (not panic) when no HLS
// segmenter is registered for the pipeline. The reconciler's HLS egress
// path replaced an unwrap() with a None guard after this was identified.
#[tokio::test]
async fn get_hls_cancel_token_returns_none_with_no_segmenter() {
    let engine = Arc::new(MediaEngine::new());
    let token = engine.get_hls_cancel_token("no-such-pipeline").await;
    assert!(
        token.is_none(),
        "must return None, not panic, when segmenter is not registered"
    );
}

// M2 (continued): after ensure_hls_segmenter registers a segmenter, the
// token must be Some — confirming the None case above is not a permanent failure.
#[tokio::test]
async fn get_hls_cancel_token_returns_some_after_ensure() {
    let engine = Arc::new(MediaEngine::new());
    engine.ensure_hls_segmenter("pipe-hls").await;
    let token = engine.get_hls_cancel_token("pipe-hls").await;
    assert!(
        token.is_some(),
        "token must be Some after ensure_hls_segmenter registers the pipeline"
    );
    engine.shutdown_hls_segmenter("pipe-hls").await;
}

#[tokio::test]
async fn hls_stores_use_engine_typed_config() {
    let config = Arc::new(crate::AppConfig {
        hls_min_segment_ms: 0.25,
        hls_segment_capacity_bytes: 256 * 1024,
        hls_max_segments: 7,
        ..crate::AppConfig::default()
    });
    let engine = Arc::new(MediaEngine::new_with_config(config));

    let hls_store = engine.get_or_create_hls_store("pipe-hls-config").await;
    let preview_store = engine
        .get_or_create_hls_preview_store("pipe-hls-preview-config")
        .await;

    assert_eq!(
        hls_store.config(),
        crate::media::hls::HlsConfig {
            min_segment_secs: 0.25,
            segment_capacity: 256 * 1024,
            max_segments: 7,
        }
    );
    assert_eq!(preview_store.config(), hls_store.config());
}

#[tokio::test]
async fn shutdown_hls_segmenter_removes_consumer_and_store() {
    let engine = Arc::new(MediaEngine::new());
    let (store, already_running) = engine.ensure_hls_segmenter("pipe-hls-clean").await;
    assert!(!already_running);
    store.push_segment(1.0, bytes::Bytes::from_static(b"segment"));

    assert!(engine.get_hls_store("pipe-hls-clean").await.is_some());
    assert!(
        engine
            .get_hls_cancel_token("pipe-hls-clean")
            .await
            .is_some()
    );

    engine.shutdown_hls_segmenter("pipe-hls-clean").await;

    assert!(engine.get_hls_store("pipe-hls-clean").await.is_none());
    assert!(
        engine
            .get_hls_cancel_token("pipe-hls-clean")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn shutdown_hls_preview_segmenter_removes_consumer_and_store() {
    let engine = Arc::new(MediaEngine::new());
    let (store, already_running, _cancel_token) = engine
        .ensure_hls_preview_segmenter("pipe-hls-preview-clean")
        .await;
    assert!(!already_running);
    store.push_video_segment(0, 1.0, bytes::Bytes::from_static(b"segment"));

    assert!(
        engine
            .get_hls_preview_store("pipe-hls-preview-clean")
            .await
            .is_some()
    );
    assert!(
        engine
            .get_hls_preview_cancel_token("pipe-hls-preview-clean")
            .await
            .is_some()
    );

    engine
        .shutdown_hls_preview_segmenter("pipe-hls-preview-clean")
        .await;

    assert!(
        engine
            .get_hls_preview_store("pipe-hls-preview-clean")
            .await
            .is_none(),
        "preview store must be dropped on shutdown, not leaked in the registry"
    );
    assert!(
        engine
            .get_hls_preview_cancel_token("pipe-hls-preview-clean")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn hls_segmenter_without_ingest_is_immediately_shutdown_candidate() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-hls-no-ingest";

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    engine.touch_hls_preview(pipeline_id).await;

    assert!(
        engine
            .should_shutdown_hls_preview_segmenter(pipeline_id, 60_000)
            .await,
        "HLS preview should stop promptly when ingest disappears, regardless of idle timeout"
    );
}

#[tokio::test]
async fn ensure_hls_segmenter_is_idempotent_and_preserves_identity() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-hls-idempotent";

    let (store1, already_running1) = engine.ensure_hls_segmenter(pipeline_id).await;
    assert!(!already_running1);
    let token1 = engine
        .get_hls_cancel_token(pipeline_id)
        .await
        .expect("token registered after first ensure");

    let (store2, already_running2) = engine.ensure_hls_segmenter(pipeline_id).await;
    assert!(
        already_running2,
        "second ensure on the same pipeline_id must report already_running"
    );
    assert!(
        Arc::ptr_eq(&store1, &store2),
        "second ensure must return the same store, not recreate it"
    );
    let token2 = engine
        .get_hls_cancel_token(pipeline_id)
        .await
        .expect("token still registered after second ensure");
    assert!(
        token1.is_cancelled() == token2.is_cancelled() && !token1.is_cancelled(),
        "cancel token identity must be preserved across idempotent ensure calls"
    );

    engine.shutdown_hls_segmenter(pipeline_id).await;
}

#[tokio::test]
async fn ensure_hls_preview_segmenter_is_idempotent_and_preserves_identity() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-hls-preview-idempotent";

    let (store1, already_running1, token1) = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    assert!(!already_running1);

    let (store2, already_running2, token2) = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    assert!(
        already_running2,
        "second ensure on the same preview pipeline_id must report already_running"
    );
    assert!(
        Arc::ptr_eq(&store1, &store2),
        "second ensure must return the same preview store, not recreate it"
    );
    assert!(
        !token1.is_cancelled() && !token2.is_cancelled(),
        "neither token should be cancelled before shutdown"
    );
    token1.cancel();
    assert!(
        token2.is_cancelled(),
        "both handles must refer to the same underlying cancel token"
    );

    engine.shutdown_hls_preview_segmenter(pipeline_id).await;
}

#[tokio::test]
async fn hls_pipeline_ids_and_preview_pipeline_ids_do_not_cross_contaminate() {
    let engine = Arc::new(MediaEngine::new());

    engine.ensure_hls_segmenter("pipe-regular-only").await;
    engine
        .ensure_hls_preview_segmenter("pipe-preview-only")
        .await;
    // Same underlying pipeline_id registered in both registries.
    engine.ensure_hls_segmenter("pipe-both").await;
    engine.ensure_hls_preview_segmenter("pipe-both").await;

    let mut regular_ids = engine.hls_pipeline_ids().await;
    regular_ids.sort();
    assert_eq!(regular_ids, vec!["pipe-both", "pipe-regular-only"]);

    let mut preview_ids = engine.hls_preview_pipeline_ids().await;
    preview_ids.sort();
    assert_eq!(preview_ids, vec!["pipe-both", "pipe-preview-only"]);

    engine.shutdown_all_hls_segmenters().await;
}

#[tokio::test]
async fn shutdown_all_hls_segmenters_cleans_up_mixed_pool() {
    let engine = Arc::new(MediaEngine::new());

    engine.ensure_hls_segmenter("pipe-mix-regular").await;
    engine
        .ensure_hls_preview_segmenter("pipe-mix-preview")
        .await;
    engine.ensure_hls_segmenter("pipe-mix-both").await;
    engine.ensure_hls_preview_segmenter("pipe-mix-both").await;

    engine.shutdown_all_hls_segmenters().await;

    assert!(engine.hls_pipeline_ids().await.is_empty());
    assert!(engine.hls_preview_pipeline_ids().await.is_empty());
    for pipeline_id in ["pipe-mix-regular", "pipe-mix-preview", "pipe-mix-both"] {
        assert!(engine.get_hls_store(pipeline_id).await.is_none());
        assert!(engine.get_hls_preview_store(pipeline_id).await.is_none());
        assert!(engine.get_hls_cancel_token(pipeline_id).await.is_none());
        assert!(
            engine
                .get_hls_preview_cancel_token(pipeline_id)
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn hls_persistent_consumer_add_and_remove_are_safe_noops_when_unregistered() {
    let engine = Arc::new(MediaEngine::new());
    // No ensure_hls_segmenter call for this pipeline_id: the consumer entry
    // does not exist. Both calls must be silent no-ops, not panics.
    engine.add_hls_persistent_consumer("no-such-pipeline").await;
    engine
        .remove_hls_persistent_consumer("no-such-pipeline")
        .await;

    assert!(
        engine
            .get_hls_cancel_token("no-such-pipeline")
            .await
            .is_none(),
        "no-op calls on an unregistered pipeline_id must not create a registry entry"
    );
}

// ── Matrix routing with synthetic packets (Phase 0 re-tier) ─────
