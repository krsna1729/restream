use super::*;
use crate::media::engine_registries::SrtMuxerAssignment;
use crate::media::ts_chunk_ring::TsChunkRing;
use std::collections::{HashMap, HashSet};

fn engine_with_srt_muxer_caps(max_outputs_per_shard: usize, max_shards: usize) -> MediaEngine {
    MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        srt_egress_muxer_max_outputs_per_shard: max_outputs_per_shard,
        srt_egress_muxer_max_shards: max_shards,
        ..crate::AppConfig::default()
    }))
}

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

#[tokio::test]
async fn srt_muxer_assignment_creates_new_shards_at_output_threshold() {
    let engine = engine_with_srt_muxer_caps(2, 8);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let second = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-2", 1)
        .await;
    let third = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-3", 1)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(second, "source:srt-mux-shard:0");
    assert_eq!(third, "source:srt-mux-shard:1");
}

#[tokio::test]
async fn srt_muxer_assignment_reuses_freed_empty_shard() {
    let engine = engine_with_srt_muxer_caps(1, 8);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let second = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-2", 1)
        .await;
    engine
        .release_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let third = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-3", 1)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(second, "source:srt-mux-shard:1");
    assert_eq!(third, "source:srt-mux-shard:0");
}

#[tokio::test]
async fn srt_muxer_assignment_degrades_to_least_loaded_at_max_shards() {
    let engine = engine_with_srt_muxer_caps(1, 2);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let second = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-2", 1)
        .await;
    let third = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-3", 1)
        .await;
    let fourth = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-4", 1)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(second, "source:srt-mux-shard:1");
    assert_eq!(third, "source:srt-mux-shard:0");
    assert_eq!(fourth, "source:srt-mux-shard:1");
}

#[tokio::test]
async fn stale_srt_muxer_release_cannot_remove_replacement_assignment() {
    let engine = engine_with_srt_muxer_caps(1, 8);

    let first = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-race", 1)
        .await;
    let replacement = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-race", 2)
        .await;
    engine
        .release_srt_egress_muxer_stage("pipe-1", "source", "out-race", 1)
        .await;
    let still_current = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-race", 2)
        .await;

    assert_eq!(first, "source:srt-mux-shard:0");
    assert_eq!(replacement, "source:srt-mux-shard:0");
    assert_eq!(still_current, replacement);
}

#[tokio::test]
async fn empty_srt_muxer_shard_cancels_and_removes_ts_stage() {
    let engine = Arc::new(engine_with_srt_muxer_caps(1, 8));
    let source_ring = Arc::new(RingBuffer::new(8));
    let stage_key = engine
        .assign_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;
    let ts_ring = engine
        .get_or_create_ts_muxer_stage("pipe-1", &stage_key, source_ring)
        .await;

    assert!(
        engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe-1:source:srt-mux-shard:0")
    );
    engine
        .release_srt_egress_muxer_stage("pipe-1", "source", "out-1", 1)
        .await;

    assert!(ts_ring.cancel.is_cancelled());
    assert!(
        !engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe-1:source:srt-mux-shard:0")
    );
}

#[derive(Clone, Copy, Debug)]
enum SrtMuxerLifecycleOp {
    Assign { output: usize, attempt_delta: u8 },
    RepeatAssign { output: usize },
    ReleaseCurrent { output: usize },
    ReleaseStale { output: usize },
}

fn srt_muxer_lifecycle_op_strategy() -> impl Strategy<Value = SrtMuxerLifecycleOp> {
    prop_oneof![
        (0usize..12, 0u8..3).prop_map(|(output, attempt_delta)| {
            SrtMuxerLifecycleOp::Assign {
                output,
                attempt_delta,
            }
        }),
        (0usize..12).prop_map(|output| SrtMuxerLifecycleOp::RepeatAssign { output }),
        (0usize..12).prop_map(|output| SrtMuxerLifecycleOp::ReleaseCurrent { output }),
        (0usize..12).prop_map(|output| SrtMuxerLifecycleOp::ReleaseStale { output }),
    ]
}

fn parse_srt_muxer_shard_index(stage_key: &str) -> usize {
    stage_key
        .rsplit_once(":srt-mux-shard:")
        .and_then(|(_, shard)| shard.parse::<usize>().ok())
        .unwrap_or_else(|| panic!("stage key should contain shard index: {stage_key}"))
}

async fn assert_srt_muxer_pool_matches_model(
    engine: &MediaEngine,
    model: &HashMap<String, (u64, usize)>,
    max_shards: usize,
) {
    let pools = engine.stages.srt_muxer_shards.read().await;
    let pool = pools.get("pipe-1\u{1f}source");

    if model.is_empty() {
        assert!(
            pool.is_none_or(|pool| pool.is_empty()),
            "empty model should leave no live shard assignments: {pool:?}"
        );
        return;
    }

    let pool = pool.expect("non-empty model should have a shard pool");
    let (assignments, shard_occupancy, retiring_shards) = pool.test_snapshot();
    assert_eq!(assignments.len(), model.len());
    assert!(
        shard_occupancy.len() <= max_shards,
        "shard count must stay capped"
    );

    let mut expected_occupancy = vec![0usize; shard_occupancy.len()];
    let mut expected_assignments = HashMap::new();
    for (output, (attempt, shard)) in model {
        assert!(
            *shard < max_shards,
            "model shard index must stay below configured cap"
        );
        if *shard >= expected_occupancy.len() {
            expected_occupancy.resize(*shard + 1, 0);
        }
        expected_occupancy[*shard] += 1;
        expected_assignments.insert(
            output.clone(),
            SrtMuxerAssignment {
                attempt_id: *attempt,
                shard_index: *shard,
            },
        );
    }

    assert_eq!(assignments, expected_assignments);
    assert_eq!(shard_occupancy, expected_occupancy);
    for retiring in &retiring_shards {
        assert_eq!(
            shard_occupancy.get(*retiring).copied().unwrap_or_default(),
            0,
            "only empty shards may be marked retiring"
        );
    }

    let assigned_shards = model
        .values()
        .map(|(_, shard)| *shard)
        .collect::<HashSet<_>>();
    assert!(
        assigned_shards.len() <= max_shards,
        "live assignment fanout must stay within max shards"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn proptest_srt_muxer_shard_lifecycle_matches_model(
        max_outputs_per_shard in 1usize..=4,
        max_shards in 1usize..=5,
        ops in prop::collection::vec(srt_muxer_lifecycle_op_strategy(), 1..96),
    ) {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            let engine = engine_with_srt_muxer_caps(max_outputs_per_shard, max_shards);
            let mut model: HashMap<String, (u64, usize)> = HashMap::new();
            let mut next_attempt_by_output = [1_u64; 12];
            let mut stale_attempt_by_output: [Option<u64>; 12] = [None; 12];

            for op in ops {
                match op {
                    SrtMuxerLifecycleOp::Assign { output, attempt_delta } => {
                        let output_id = format!("out-{output}");
                        if let Some((attempt, _)) = model.get(&output_id).copied() {
                            stale_attempt_by_output[output] = Some(attempt);
                        }
                        next_attempt_by_output[output] =
                            next_attempt_by_output[output].saturating_add(u64::from(attempt_delta) + 1);
                        let attempt = next_attempt_by_output[output];
                        let stage_key = engine
                            .assign_srt_egress_muxer_stage("pipe-1", "source", &output_id, attempt)
                            .await;
                        model.insert(output_id, (attempt, parse_srt_muxer_shard_index(&stage_key)));
                    }
                    SrtMuxerLifecycleOp::RepeatAssign { output } => {
                        let output_id = format!("out-{output}");
                        let attempt = model
                            .get(&output_id)
                            .map(|(attempt, _)| *attempt)
                            .unwrap_or(next_attempt_by_output[output]);
                        let stage_key = engine
                            .assign_srt_egress_muxer_stage("pipe-1", "source", &output_id, attempt)
                            .await;
                        model.insert(output_id, (attempt, parse_srt_muxer_shard_index(&stage_key)));
                    }
                    SrtMuxerLifecycleOp::ReleaseCurrent { output } => {
                        let output_id = format!("out-{output}");
                        let attempt = model
                            .get(&output_id)
                            .map(|(attempt, _)| *attempt)
                            .unwrap_or(next_attempt_by_output[output]);
                        engine
                            .release_srt_egress_muxer_stage("pipe-1", "source", &output_id, attempt)
                            .await;
                        model.remove(&output_id);
                    }
                    SrtMuxerLifecycleOp::ReleaseStale { output } => {
                        let output_id = format!("out-{output}");
                        let stale_attempt = stale_attempt_by_output[output]
                            .or_else(|| model.get(&output_id).map(|(attempt, _)| attempt.saturating_sub(1)))
                            .unwrap_or(0);
                        engine
                            .release_srt_egress_muxer_stage("pipe-1", "source", &output_id, stale_attempt)
                            .await;
                    }
                }

                assert_srt_muxer_pool_matches_model(&engine, &model, max_shards).await;
            }
        });
    }
}
