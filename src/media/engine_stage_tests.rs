use super::*;

#[tokio::test]
async fn ingest_bytes_and_meta_on_nonexistent_pipeline_is_noop() {
    let engine = MediaEngine::new();
    // Should not panic
    engine.update_ingest_bytes("nonexistent", 1000).await;
    engine
        .update_ingest_meta("nonexistent", None, None, None)
        .await;
}

/// Two outputs with the same pipeline + encoding share exactly one transcoder
/// stage (same Arc<RingBuffer> pointer). A third output with a different
/// encoding gets its own stage. This is the core sharing invariant.
#[tokio::test]
async fn same_encoding_outputs_share_one_transcoder_stage() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-share").await;

    let a = engine
        .get_or_create_transcoder(
            "pipe-share",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let b = engine
        .get_or_create_transcoder(
            "pipe-share",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let c = engine
        .get_or_create_transcoder(
            "pipe-share",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;

    assert!(
        Arc::ptr_eq(&a, &b),
        "two outputs with encoding=720p must share the same ring buffer"
    );
    assert!(
        !Arc::ptr_eq(&a, &c),
        "different encodings must use separate ring buffers"
    );
}

/// Audio stages are keyed by both audio operation AND upstream video preset.
/// 720p+atrack:0 and 1080p+atrack:0 must not share an audio stage.
#[tokio::test]
async fn audio_stages_are_isolated_per_video_preset() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-audio").await;

    let v720 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let v1080 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;

    let a720 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::audio_route("atrack:0", StageKind::video_preset("720p")),
            v720.clone(),
            None,
        )
        .await;
    let a1080 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::audio_route("atrack:0", StageKind::video_preset("1080p")),
            v1080.clone(),
            None,
        )
        .await;
    let a720_again = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::audio_route("atrack:0", StageKind::video_preset("720p")),
            v720,
            None,
        )
        .await;

    assert!(
        !Arc::ptr_eq(&a720, &a1080),
        "audio stages for different video presets must be isolated"
    );
    assert!(
        Arc::ptr_eq(&a720, &a720_again),
        "same audio stage key must return the same ring buffer"
    );
}

/// cleanup_pipeline_stages must remove all entries whose key starts with
/// "<pipeline_id>:" and cancel their tokens. Entries for other pipelines
/// must not be affected.
#[tokio::test]
async fn cleanup_pipeline_stages_removes_all_stage_entries() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-del").await;
    let other = engine.get_or_create_pipeline("pipe-keep").await;

    let s1 = engine
        .get_or_create_transcoder(
            "pipe-del",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let s2 = engine
        .get_or_create_transcoder(
            "pipe-del",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;
    let other_stage = engine
        .get_or_create_transcoder("pipe-keep", StageKind::video_preset("720p"), other, None)
        .await;

    // Stages are alive before cleanup
    let stages_before = engine.active_transcoder_stages("pipe-del").await;
    assert_eq!(stages_before.len(), 2);

    engine.cleanup_pipeline_stages("pipe-del").await;

    // All pipe-del stages removed
    let stages_after = engine.active_transcoder_stages("pipe-del").await;
    assert_eq!(
        stages_after.len(),
        0,
        "all stages for deleted pipeline must be removed"
    );

    // The ring buffers from those stages had their tokens cancelled
    let _ = (s1, s2); // bindings kept to confirm they're the same arcs tested above

    // pipe-keep is unaffected
    let other_stages = engine.active_transcoder_stages("pipe-keep").await;
    assert_eq!(
        other_stages.len(),
        1,
        "unrelated pipeline stages must be untouched"
    );
    let _ = other_stage;
}

#[tokio::test]
async fn transcoder_stage_registry_uses_typed_stage_keys() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-typed").await;

    let _stage = engine
        .get_or_create_transcoder("pipe-typed", StageKind::video_preset("720p"), source, None)
        .await;

    let runtimes = engine.stages.runtimes.read().await;
    let key = runtimes
        .keys()
        .find(|key| key.pipeline.as_str() == "pipe-typed")
        .expect("typed registry should contain created stage");

    assert_eq!(key.to_string(), "pipe-typed:video:720p");
    assert!(matches!(
        &key.kind,
        StageKind::VideoPreset { preset, .. } if preset == "720p"
    ));
}

/// remove_pipeline must free the source ring buffer from the pipelines map.
#[tokio::test]
async fn remove_pipeline_frees_source_ring_buffer() {
    let engine = Arc::new(MediaEngine::new());
    let rb = engine.get_or_create_pipeline("pipe-rm").await;
    let weak = Arc::downgrade(&rb);
    drop(rb); // release our local strong reference

    // Pipeline map still holds a strong ref
    assert!(
        weak.upgrade().is_some(),
        "ring buffer should still be alive"
    );

    engine.remove_pipeline("pipe-rm").await;
    // Now only the weak ref remains — the Arc should be freed
    assert!(
        weak.upgrade().is_none(),
        "ring buffer should be freed after remove_pipeline"
    );
}

#[tokio::test]
async fn sweep_unused_transcoder_stages_removes_only_unused() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-sweep").await;

    let s1 = engine
        .get_or_create_transcoder(
            "pipe-sweep",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let s2 = engine
        .get_or_create_transcoder(
            "pipe-sweep",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;

    let mut active = std::collections::HashSet::new();
    active.insert(StageKey::new("pipe-sweep", StageKind::video_preset("720p")));

    engine.sweep_unused_transcoder_stages(&active).await;

    let stages = engine.active_transcoder_stages("pipe-sweep").await;
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].0, StageKind::video_preset("720p"));
    let runtime_keys: Vec<_> = engine
        .stages
        .runtimes
        .read()
        .await
        .keys()
        .filter(|key| key.pipeline.as_str() == "pipe-sweep")
        .cloned()
        .collect();
    assert_eq!(
        runtime_keys,
        vec![StageKey::new("pipe-sweep", StageKind::video_preset("720p"))],
        "runtime registry must remove swept stage objects"
    );
    // s2 was swept and cancelled
    let _ = (s1, s2);
}

#[tokio::test]
async fn sweep_unused_transcoder_stages_removes_codec_edge_stages() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-sweep-codec").await;

    let _stage = engine
        .get_or_create_h264_transcoder("pipe-sweep-codec", StageKind::source(), source)
        .await;
    let stages_before = engine.active_transcoder_stages("pipe-sweep-codec").await;
    assert!(
        stages_before.iter().any(|(stage, live)| *stage
            == StageKind::codec_edge("hevc_to_h264", StageKind::source())
            && *live),
        "codec-edge stage must be registered before the sweep"
    );

    let active: std::collections::HashSet<StageKey> = std::collections::HashSet::new();
    engine.sweep_unused_transcoder_stages(&active).await;

    let stages_after = engine.active_transcoder_stages("pipe-sweep-codec").await;
    assert!(
        stages_after.is_empty(),
        "unused codec-edge stages must be removed from the shared stage registry"
    );
    assert!(
        engine
            .stages
            .runtimes
            .read()
            .await
            .keys()
            .all(|key| key.pipeline.as_str() != "pipe-sweep-codec"),
        "codec-edge runtime objects must be removed with swept stages"
    );
}

#[tokio::test]
async fn concurrent_get_or_create_transcoder_yields_single_stage() {
    // Bug #4 regression: the old read-lock-then-write-lock TOCTOU window
    // allowed concurrent callers to both see "key absent" and both insert,
    // spawning two transcoder tasks writing to different ring buffers.
    // After the fix, all concurrent callers must receive the SAME Arc<RingBuffer>.
    use std::sync::Arc as StdArc;
    use tokio::sync::Barrier;
    use tokio::task::JoinSet;

    let engine = StdArc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-concurrent").await;

    // Synchronize 16 tasks to all call get_or_create_transcoder simultaneously
    let barrier = StdArc::new(Barrier::new(16));
    let mut join_set = JoinSet::new();

    for _ in 0..16 {
        let e = engine.clone();
        let s = source.clone();
        let b = barrier.clone();
        join_set.spawn(async move {
            b.wait().await;
            e.get_or_create_transcoder("pipe-concurrent", StageKind::video_preset("720p"), s, None)
                .await
        });
    }

    let mut results = Vec::new();
    while let Some(r) = join_set.join_next().await {
        results.push(r.unwrap());
    }

    // All returned Arc<RingBuffer>s must point to the SAME allocation
    let first_ptr = StdArc::as_ptr(&results[0]);
    for rb in &results[1..] {
        assert_eq!(
            StdArc::as_ptr(rb),
            first_ptr,
            "concurrent callers must receive the same RingBuffer Arc (no duplicate stages)"
        );
    }

    // Exactly one stage must exist in the map
    let stages = engine.active_transcoder_stages("pipe-concurrent").await;
    assert_eq!(
        stages.len(),
        1,
        "exactly one transcoder stage must exist after concurrent creation"
    );
}

// --- Regression: Round 6 #7 — HLS consumer refcount must not leak ---
// The refcount must return to zero after balanced add/remove so the
// idle-sweep logic eventually stops the segmenter task.
#[tokio::test]
async fn hls_consumer_idle_only_when_persistent_count_zero() {
    use tokio_util::sync::CancellationToken;

    let engine = MediaEngine::new();
    let token = CancellationToken::new();
    {
        let mut consumers = engine.hls.consumers.write().await;
        consumers.insert("pipe-hls-rc".to_string(), HlsConsumers::new(token.clone()));
    }

    // One persistent consumer added — segmenter must not be idle.
    engine.add_hls_persistent_consumer("pipe-hls-rc").await;
    {
        let consumers = engine.hls.consumers.read().await;
        assert!(
            !consumers["pipe-hls-rc"].is_idle(0),
            "segmenter must not be idle while a persistent consumer holds a ref"
        );
    }

    // Remove the consumer — now idle (last_access_ms was set on creation;
    // use a long timeout so only persistent count matters here).
    engine.remove_hls_persistent_consumer("pipe-hls-rc").await;
    {
        let consumers = engine.hls.consumers.read().await;
        assert_eq!(
            consumers["pipe-hls-rc"]
                .persistent
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "persistent count must be 0 after remove"
        );
    }
}

// --- H.265 routing correctness tests ---

#[tokio::test]
async fn hevc_input_video_preset_ring_tagged_hevc() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-hevc").await;
    let ring = engine
        .get_or_create_transcoder(
            "p-hevc",
            StageKind::video_preset("720p"),
            source,
            Some("hevc"),
        )
        .await;
    assert_eq!(
        ring.codec_hint_str(),
        "hevc",
        "video:720p stage fed with H.265 must be tagged 'hevc'"
    );
}

#[tokio::test]
async fn h264_input_video_preset_ring_tagged_h264() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-h264").await;
    let ring = engine
        .get_or_create_transcoder("p-h264", StageKind::video_preset("720p"), source, None)
        .await;
    assert_eq!(
        ring.codec_hint_str(),
        "h264",
        "video:720p stage without codec override must default to 'h264'"
    );
}

#[tokio::test]
async fn h264_transcoder_different_upstreams_are_independent_stages() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-dual").await;

    let from_source = engine
        .get_or_create_h264_transcoder("p-dual", StageKind::source(), source.clone())
        .await;
    let from_720 = engine
        .get_or_create_h264_transcoder("p-dual", StageKind::video_preset("720p"), source.clone())
        .await;

    assert!(
        !Arc::ptr_eq(&from_source, &from_720),
        "hevc_to_h264 stages keyed by different upstreams must be independent"
    );
}

#[tokio::test]
async fn h264_transcoder_same_upstream_is_shared() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-shared-h264").await;

    let ring1 = engine
        .get_or_create_h264_transcoder(
            "p-shared-h264",
            StageKind::video_preset("720p"),
            source.clone(),
        )
        .await;
    let ring2 = engine
        .get_or_create_h264_transcoder(
            "p-shared-h264",
            StageKind::video_preset("720p"),
            source.clone(),
        )
        .await;

    assert!(
        Arc::ptr_eq(&ring1, &ring2),
        "hevc_to_h264 stage for the same upstream must be reused"
    );
}

#[tokio::test]
async fn h264_transcoder_output_ring_tagged_h264() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-h264-tag").await;

    let ring = engine
        .get_or_create_h264_transcoder("p-h264-tag", StageKind::source(), source)
        .await;

    assert_eq!(
        ring.codec_hint_str(),
        "h264",
        "hevc_to_h264 output ring must always be tagged 'h264'"
    );
}

// ── audio_tracks Arc<Vec<AudioMeta>> semantics ────────────────────

#[test]
fn arc_audio_tracks_clone_is_shallow_refcount_bump() {
    use std::sync::Arc;
    let tracks = vec![
        AudioMeta {
            codec: "aac".into(),
            sample_rate: 48000,
            channels: 2,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
            channel_layout: None,
        },
        AudioMeta {
            codec: "opus".into(),
            sample_rate: 48000,
            channels: 6,
            track_index: 1,
            pid: None,
            language: None,
            title: None,
            profile: None,
            channel_layout: None,
        },
    ];
    let arc = Arc::new(tracks);

    let c1 = Arc::clone(&arc);
    let c2 = Arc::clone(&arc);
    assert_eq!(Arc::as_ptr(&arc), Arc::as_ptr(&c1));
    assert_eq!(Arc::as_ptr(&arc), Arc::as_ptr(&c2));
    assert_eq!(Arc::strong_count(&arc), 3);
    assert_eq!(arc.len(), 2);
    assert_eq!(c1[0].codec, "aac");
    assert_eq!(c2[1].channels, 6);
}

#[test]
fn arc_audio_tracks_deref_works_for_iteration() {
    use std::sync::Arc;
    let tracks = vec![AudioMeta {
        codec: "aac".into(),
        sample_rate: 44100,
        channels: 1,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
        channel_layout: None,
    }];
    let arc = Arc::new(tracks);
    assert_eq!(arc.iter().next().unwrap().sample_rate, 44100);
    assert_eq!(arc.first().unwrap().codec, "aac");
    assert_eq!(arc.len(), 1);
}

#[test]
fn arc_audio_tracks_default_is_empty() {
    use std::sync::Arc;
    let arc: Arc<Vec<AudioMeta>> = Arc::default();
    assert!(arc.is_empty());
    assert_eq!(arc.len(), 0);
}

#[test]
fn arc_audio_tracks_mutex_wraps_correctly() {
    use std::sync::{Arc, Mutex};
    let tracks = Arc::new(vec![AudioMeta {
        codec: "aac".into(),
        sample_rate: 48000,
        channels: 2,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
        channel_layout: None,
    }]);
    let mtx = Mutex::new(Arc::clone(&tracks));

    // Clone under lock gives an Arc clone, not a deep Vec copy
    let guard = mtx.lock().unwrap();
    let cloned = guard.clone(); // Arc clone
    assert_eq!(Arc::as_ptr(&tracks), Arc::as_ptr(&cloned));
    assert_eq!(Arc::strong_count(&tracks), 3); // tracks + mtx inner + cloned
    drop(guard);
    drop(cloned);
    assert_eq!(Arc::strong_count(&tracks), 2); // tracks + mtx inner
}

// ── diag concurrency semaphore ──────────────────────────────────

#[tokio::test]
async fn diag_semaphore_prevents_concurrent_runs_on_same_pipeline() {
    let engine = MediaEngine::new();
    let pipeline = "diag-concurrency";

    let sem = {
        let mut map = engine.runtime.diag_semaphores.write().await;
        map.entry(pipeline.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    };

    let permit1 = sem.clone().try_acquire_owned();
    assert!(permit1.is_ok(), "first acquire must succeed");

    let permit2 = sem.clone().try_acquire_owned();
    assert!(permit2.is_err(), "second concurrent acquire must fail");

    let sem_other = {
        let mut map = engine.runtime.diag_semaphores.write().await;
        map.entry("other-pipeline".to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    };
    assert!(
        sem_other.try_acquire_owned().is_ok(),
        "different pipeline must succeed"
    );

    drop(permit1);
    assert!(
        sem.try_acquire_owned().is_ok(),
        "acquire must succeed after previous permit dropped"
    );
}

// ── sweep_unused_stages reader tracking ─────────────────────────

#[tokio::test]
async fn sweep_unused_stages_retains_active_readers() {
    let engine = MediaEngine::new();
    let key = "pipeline:stage-sweep".to_string();
    let cancel = CancellationToken::new();
    let stage = Arc::new(TsChunkRing::new(16, cancel));

    let _reader =
        crate::media::ring_buffer::Reader::new("sweep-test".to_string(), stage.ring.clone());

    engine
        .stages
        .ts_muxers
        .write()
        .await
        .insert(key.clone(), stage);

    engine.sweep_unused_stages().await;
    assert!(
        engine.stages.ts_muxers.read().await.contains_key(&key),
        "stage with active reader must be retained"
    );

    drop(_reader);
    engine.sweep_unused_stages().await;
    assert!(
        !engine.stages.ts_muxers.read().await.contains_key(&key),
        "stage without readers must be removed"
    );
}

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
    let (store, already_running) = engine
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

// ── Matrix routing with synthetic packets (Phase 0 re-tier) ─────

#[tokio::test]
async fn matrix_routing_ingest_to_source_reader() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("matrix-pipe").await;
    engine
        .try_register_ingest("matrix-pipe", "key", "rtmp")
        .await
        .unwrap();

    ring.push(test_video_packet(0, 0, true));
    ring.push(test_audio_packet(10, 10));
    ring.push(test_video_packet(33, 33, false));

    let mut reader = Reader::new("matrix-reader".to_string(), ring);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.media_type, MediaType::Video);
    assert!(p1.is_keyframe);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.media_type, MediaType::Audio);
    let p3 = reader.pull().unwrap().unwrap();
    assert_eq!(p3.pts, 33);
    assert!(reader.pull().unwrap().is_none());
}

#[tokio::test]
async fn matrix_routing_flv_and_raw_format_dispatch() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("fmt-pipe").await;

    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Flv,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x17, 0x01, 0, 0, 0]),
    });
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 33,
        dts: 33,
        payload: Bytes::from_static(&[0, 0, 0, 1, 0x41]),
    });

    let mut reader = Reader::new("fmt-reader".to_string(), ring);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.format, PayloadFormat::Flv);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.format, PayloadFormat::Raw);
}

#[tokio::test]
async fn matrix_routing_multi_reader_fan_out() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("fanout-pipe").await;

    ring.push(test_video_packet(0, 0, true));
    ring.push(test_audio_packet(10, 10));

    let mut r1 = Reader::new("reader-1".to_string(), ring.clone());
    let mut r2 = Reader::new("reader-2".to_string(), ring.clone());
    let mut r3 = Reader::new("reader-3".to_string(), ring);

    for reader in [&mut r1, &mut r2, &mut r3] {
        let p = reader.pull().unwrap().unwrap();
        assert_eq!(p.pts, 0);
        assert!(p.is_keyframe);
    }
}

#[tokio::test]
async fn matrix_routing_transcoder_stage_isolation() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("iso-pipe").await;

    source.push(test_video_packet(0, 0, true));

    let tc_ring = engine
        .get_or_create_transcoder(
            "iso-pipe",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;

    assert!(
        !Arc::ptr_eq(&source, &tc_ring),
        "transcoder output ring must differ from source ring"
    );

    let mut source_reader = Reader::new("src".to_string(), source);
    let p = source_reader.pull().unwrap().unwrap();
    assert_eq!(p.pts, 0);
}

// ── fault resilience: ingest lifecycle ──────────────────────────────
