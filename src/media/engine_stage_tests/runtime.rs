use super::*;

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

/// Regression: a stage feeding only fabric SRT outputs has no `Reader`
/// registered (fabric consumes via `EgressFeed::read_from`, not the reader
/// registry), so the reader-count check alone would sweep it on the very
/// next reconcile tick regardless of how recently it started — starving the
/// fabric leaves of all media before they ever see a byte. Live evidence:
/// `docs/egress-implementation.md` Phase 4 status.
#[tokio::test]
async fn sweep_unused_stages_retains_stage_with_live_fabric_consumer_and_no_readers() {
    let engine = MediaEngine::new();
    let key = "pipeline:stage-fabric-sweep".to_string();
    let cancel = CancellationToken::new();
    let stage = Arc::new(TsChunkRing::new(16, cancel));

    engine
        .stages
        .ts_muxers
        .write()
        .await
        .insert(key.clone(), stage);

    // No Reader registered — mirrors the fabric consumption model — but a
    // live fabric runtime exists for the feed derived from this stage key.
    let feed_id = crate::media::egress::command::FeedId::new(format!("srt:{key}"));
    engine
        .fabric
        .srt
        .lock()
        .await
        .active_outputs
        .insert(feed_id, 1);

    engine.sweep_unused_stages().await;
    assert!(
        engine.stages.ts_muxers.read().await.contains_key(&key),
        "stage with a live fabric consumer must be retained even with zero readers"
    );

    engine.fabric.srt.lock().await.active_outputs.clear();
    engine.sweep_unused_stages().await;
    assert!(
        !engine.stages.ts_muxers.read().await.contains_key(&key),
        "stage must be swept once its fabric consumer is gone and it still has no readers"
    );
}
