use super::*;

async fn engine_with_pipeline(pipeline_id: &str) -> Arc<MediaEngine> {
    let engine = Arc::new(MediaEngine::new());
    let _ingest = engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    let _ = engine.get_or_create_pipeline(pipeline_id).await;
    engine
}

#[tokio::test]
async fn ensure_hls_preview_runtime_creates_and_reuses_runtime_store() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-hls-preview-runtime";
    let _ingest = engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    let _ = engine.get_or_create_pipeline(pipeline_id).await;

    let store = engine.ensure_hls_preview_runtime(pipeline_id).await;
    let reused = engine.ensure_hls_preview_runtime(pipeline_id).await;

    assert!(
        Arc::ptr_eq(&store, &reused),
        "preview runtime should reuse the existing fMP4 store"
    );
    assert!(engine.get_hls_preview_store(pipeline_id).await.is_some());
    assert!(
        engine
            .get_hls_preview_cancel_token(pipeline_id)
            .await
            .is_some()
    );

    engine.shutdown_hls_preview_segmenter(pipeline_id).await;
}

/// Regression test for a TOCTOU race: `ensure_hls_preview_runtime` used to
/// insert a consumer entry, drop the lock, then re-acquire it via
/// `get_hls_preview_cancel_token(...).await.unwrap()`. A concurrent
/// `shutdown_hls_preview_segmenter` call could remove the entry in that
/// window, turning the `unwrap()` into a panic. `ensure_hls_preview_segmenter`
/// now hands back the token it just inserted directly, so there is no
/// window for a concurrent shutdown to invalidate. Hammer ensure/shutdown
/// concurrently and confirm every task completes without panicking.
#[tokio::test]
async fn ensure_hls_preview_runtime_survives_concurrent_shutdown_race() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-hls-preview-race";
    let _ingest = engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    let _ = engine.get_or_create_pipeline(pipeline_id).await;

    let mut tasks = Vec::new();
    for _ in 0..64 {
        let ensure_engine = engine.clone();
        let pid = pipeline_id.to_string();
        tasks.push(tokio::spawn(async move {
            let _ = ensure_engine.ensure_hls_preview_runtime(&pid).await;
        }));

        let shutdown_engine = engine.clone();
        let pid = pipeline_id.to_string();
        tasks.push(tokio::spawn(async move {
            shutdown_engine.shutdown_hls_preview_segmenter(&pid).await;
        }));
    }

    for task in tasks {
        task.await
            .expect("no task should panic under the ensure/shutdown race");
    }

    engine.shutdown_hls_preview_segmenter(pipeline_id).await;
}

// Regression/documentation test: `resolve_hls_preview_graph` checks
// `cancel.is_cancelled()` once per loop iteration, before doing any
// codec-resolution wait. A pre-cancelled token on a pipeline with no
// resolvable codec must short-circuit to `None` on the very first
// iteration rather than falling through to the 100ms poll sleep or the
// 3s deadline.
#[tokio::test(start_paused = true)]
async fn returns_none_immediately_when_cancelled_before_codec_resolves() {
    let engine = engine_with_pipeline("pipe-preview-graph-cancel").await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let start = tokio::time::Instant::now();
    let graph = engine
        .resolve_hls_preview_graph("pipe-preview-graph-cancel", cancel)
        .await;

    assert!(graph.is_none(), "cancelled resolution must return None");
    assert_eq!(
        tokio::time::Instant::now(),
        start,
        "a pre-cancelled token must short-circuit before any deadline sleep"
    );
}

// A pipeline whose codec never resolves (no ingest video codec and no
// ring codec hint) must give up after the 3s deadline rather than
// polling forever.
#[tokio::test(start_paused = true)]
async fn returns_none_after_deadline_when_codec_never_resolves() {
    let engine = engine_with_pipeline("pipe-preview-graph-deadline").await;
    let cancel = CancellationToken::new();

    let start = tokio::time::Instant::now();
    let graph = engine
        .resolve_hls_preview_graph("pipe-preview-graph-deadline", cancel)
        .await;

    assert!(graph.is_none(), "an unresolved codec must time out to None");
    assert!(
        tokio::time::Instant::now() >= start + std::time::Duration::from_secs(3),
        "must not return before the 3s resolution deadline"
    );
}

/// A browser that decodes H.265 gets the source ring as it is: no
/// `hevc_to_h264` stage (no FFmpeg) is leased or planned for it.
#[tokio::test]
async fn a_passthrough_preview_reads_the_source_and_starts_no_stage() {
    let engine = engine_with_pipeline("pipe-preview-passthrough").await;
    let source = engine
        .get_or_create_pipeline("pipe-preview-passthrough")
        .await;
    source.set_codec_hint("hevc");
    let id = crate::preview_ids::hevc_passthrough_preview_id("pipe-preview-passthrough");
    assert_eq!(
        crate::preview_ids::preview_pipeline(&id),
        ("pipe-preview-passthrough", true)
    );
    assert_eq!(
        crate::preview_ids::preview_pipeline("pipe-preview-passthrough"),
        ("pipe-preview-passthrough", false)
    );
    let graph = engine
        .resolve_hls_preview_graph(&id, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        Arc::ptr_eq(&graph.video_ring, &source),
        "reads the source ring"
    );
    assert!(graph.stage.is_none(), "no transcode stage");
    assert!(
        engine.active_hls_preview_stage_keys().await.is_empty(),
        "nothing keeps a bridge alive for it"
    );
}

/// A passthrough preview's segmenter stage is keyed by its preview id;
/// it must count as its pipeline's runtime, or the topology sweep tears
/// down a "pipeline" that is not configured and kills the preview.
#[tokio::test]
async fn a_passthrough_preview_stage_counts_as_its_pipeline() {
    let engine = engine_with_pipeline("pipe-preview-owner").await;
    let id = crate::preview_ids::hevc_passthrough_preview_id("pipe-preview-owner");
    let key = StageKey::new(id.as_str(), StageKind::hls());
    let _runtime = engine
        .get_or_create_non_ring_stage_runtime(
            key,
            crate::stage_lifecycle::StagePhase::Registered,
            crate::stage_lifecycle::StageBackendKind::HlsSegmenter,
            CancellationToken::new(),
        )
        .await;
    let ids = engine.runtime_pipeline_ids().await;
    assert!(ids.contains("pipe-preview-owner"));
    assert!(!ids.contains(&id), "{ids:?}");
}

/// An input's two preview forms read one preview ring: the passthrough
/// one plans no stage, and stopping either keeps the ring while the
/// other still plays.
#[tokio::test]
async fn an_input_passthrough_preview_shares_the_ring_and_starts_no_stage() {
    let engine = Arc::new(MediaEngine::new());
    let _ingest = engine
        .try_register_pipeline_input_attempt("pipe", "input-a", "key", "srt", true)
        .await
        .expect("register input");
    let ring = engine
        .ingests
        .ensure_input_preview_ring("input-a")
        .await
        .unwrap();
    ring.set_codec_hint("hevc");
    let copy = crate::preview_ids::input_hls_preview_resource_id("input-a");
    let passthrough = crate::preview_ids::input_hls_preview_resource_id(
        &crate::preview_ids::hevc_passthrough_preview_id("input-a"),
    );
    assert_eq!(
        crate::preview_ids::input_id_from_hls_preview_resource_id(&passthrough),
        Some("input-a")
    );
    let graph = engine
        .resolve_input_hls_preview_graph(
            &passthrough,
            "input-a",
            ring.clone(),
            CancellationToken::new(),
        )
        .await;
    assert!(Arc::ptr_eq(&graph.video_ring, &ring) && graph.stage.is_none());

    engine.ensure_hls_preview_segmenter(&copy).await;
    engine.ensure_hls_preview_segmenter(&passthrough).await;
    engine.shutdown_hls_preview_segmenter(&passthrough).await;
    assert!(
        engine
            .ingests
            .ensure_input_preview_ring("input-a")
            .await
            .is_some_and(|kept| Arc::ptr_eq(&kept, &ring)),
        "the copy still plays from the same ring"
    );
    engine.shutdown_hls_preview_segmenter(&copy).await;
    let after = engine
        .ingests
        .ensure_input_preview_ring("input-a")
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&after, &ring), "released once neither plays");
}

/// The reconciler keeps a passthrough preview while its pipeline is live
/// and someone watches: the preview id is not the pipeline id.
#[tokio::test]
async fn a_watched_passthrough_preview_of_a_live_pipeline_is_kept() {
    let engine = engine_with_pipeline("pipe-preview-kept").await;
    let id = crate::preview_ids::hevc_passthrough_preview_id("pipe-preview-kept");
    engine.ensure_hls_preview_segmenter(&id).await;
    engine.touch_hls_preview(&id).await;
    assert!(
        !engine
            .should_shutdown_hls_preview_segmenter(&id, 60_000)
            .await,
        "live and just watched"
    );
    engine.unregister_ingest("pipe-preview-kept").await;
    assert!(
        engine
            .should_shutdown_hls_preview_segmenter(&id, 60_000)
            .await,
        "its pipeline went away"
    );
}

#[test]
fn hevc_preview_meta_keeps_source_resolution() {
    let source = VideoMeta {
        codec: "hevc".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        profile: Some("Main".to_string()),
        level: Some("4.0".to_string()),
        pixel_format: Some("yuv420p10le".to_string()),
        ..Default::default()
    };
    let preview = h264_preview_video_meta(Some(source));
    assert_eq!(preview.codec, "h264");
    assert_eq!(preview.width, 1920);
    assert_eq!(preview.height, 1080);
    assert!(preview.profile.is_none());
    assert!(preview.level.is_none());
    assert!(preview.pixel_format.is_none());
}
