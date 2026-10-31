use super::*;
use std::time::Duration;

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

// ── stage leases ───────────────────────────────────────────────

/// Polls until `done` holds: a lease release runs on a spawned task.
async fn eventually(mut done: impl AsyncFnMut() -> bool) -> bool {
    for _ in 0..200 {
        if done().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

#[tokio::test]
async fn a_shared_muxer_lives_exactly_as_long_as_its_leases() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe").await;
    let (first_ring, first) = engine
        .get_or_create_ts_muxer_stage("pipe", "h264", source.clone())
        .await;
    let (second_ring, second) = engine
        .get_or_create_ts_muxer_stage("pipe", "h264", source.clone())
        .await;
    assert!(
        Arc::ptr_eq(&first_ring, &second_ring),
        "consumers share one muxer"
    );

    drop(first);
    tokio::task::yield_now().await;
    assert!(!first_ring.cancel.is_cancelled(), "one consumer remains");
    assert!(
        engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe:h264")
    );

    drop(second);
    assert!(
        first_ring.cancel.is_cancelled(),
        "the last consumer stops it at once"
    );
    assert!(
        eventually(async || !engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe:h264"))
        .await,
        "and its entry goes"
    );

    let (third_ring, _third) = engine
        .get_or_create_ts_muxer_stage("pipe", "h264", source)
        .await;
    assert!(
        !Arc::ptr_eq(&first_ring, &third_ring),
        "a new consumer gets a new muxer"
    );
    assert!(!third_ring.cancel.is_cancelled());
}

#[tokio::test]
async fn a_released_lease_never_removes_a_newer_stage_under_its_key() {
    let engine = Arc::new(MediaEngine::new());
    let manager = crate::stage_runtime::StageRuntimeManager::new(engine.clone());
    let key = StageKey::new("pipe", StageKind::video_preset_with_codec("720p", "h264"));
    let source = Arc::new(RingBuffer::new(16));

    let (old, old_lease, created) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    assert!(created);
    // The old stage ends on its own (backend exit) while its consumer
    // still holds the lease; the next consumer starts a new stage.
    old.cancel.cancel();
    let (new, _new_lease, created) = manager.ensure_stage(key.clone(), source, None).await;
    assert!(created, "a cancelled stage is never reused");

    drop(old_lease);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let runtimes = engine.stages.runtimes.read().await;
    let current = runtimes
        .get(&key)
        .expect("the newer stage stays registered");
    assert!(Arc::ptr_eq(&current.lifecycle, &new.lifecycle));
    assert!(!new.cancel.is_cancelled());
}

#[tokio::test]
async fn the_last_lease_removes_a_runtime_stage_and_its_metrics() {
    let engine = Arc::new(MediaEngine::new());
    let manager = crate::stage_runtime::StageRuntimeManager::new(engine.clone());
    let key = StageKey::new("pipe", StageKind::video_preset_with_codec("720p", "h264"));
    let (handle, lease, _) = manager
        .ensure_stage(key.clone(), Arc::new(RingBuffer::new(16)), None)
        .await;
    let (_, again, created) = manager
        .ensure_stage(key.clone(), Arc::new(RingBuffer::new(16)), None)
        .await;
    assert!(!created, "a live stage is shared");

    drop(lease);
    tokio::task::yield_now().await;
    assert!(!handle.cancel.is_cancelled());
    drop(again);
    assert!(handle.cancel.is_cancelled());
    assert!(
        eventually(async || {
            !engine.stages.runtimes.read().await.contains_key(&key)
                && !engine.stages.metrics.read().await.contains_key(&key)
                && !engine.stages.lifecycles.read().await.contains_key(&key)
        })
        .await
    );
}

/// An SRT player's lease drops on its Compio Owner thread, outside Tokio.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_lease_dropped_off_tokio_still_releases_its_stage() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe").await;
    let (ring, lease) = engine
        .get_or_create_ts_muxer_stage("pipe", "play", source)
        .await;
    std::thread::spawn(move || drop(lease)).join().unwrap();
    assert!(ring.cancel.is_cancelled());
    assert!(
        eventually(async || !engine
            .stages
            .ts_muxers
            .read()
            .await
            .contains_key("pipe:play"))
        .await
    );
}

/// Review of #284: an entry removed while its lease is held (pipeline
/// delete, an emptied SRT muxer shard) leaves no `Weak`, so the cell's
/// allocation can go to the next lease under the same key. The release
/// must still leave that newer stage alone.
#[tokio::test]
async fn a_release_after_its_entry_was_removed_spares_the_next_stage() {
    let engine = Arc::new(MediaEngine::new());
    let manager = crate::stage_runtime::StageRuntimeManager::new(engine.clone());
    let key = StageKey::new("pipe", StageKind::video_preset_with_codec("720p", "h264"));
    let source = Arc::new(RingBuffer::new(16));

    let (_, old_lease, _) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    engine.cleanup_pipeline_stages("pipe").await;
    // On this current-thread runtime the release task cannot run before the
    // next stage registers, whatever address its cell gets.
    drop(old_lease);
    let (new, _new_lease, created) = manager.ensure_stage(key.clone(), source, None).await;
    assert!(created);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let runtimes = engine.stages.runtimes.read().await;
    let current = runtimes.get(&key).expect("the next stage stays registered");
    assert!(Arc::ptr_eq(&current.lifecycle, &new.lifecycle));
}

/// Review of #284: a stage that ended stays registered (cancelled) until
/// its consumers' leases drop; its replacement must start with its own
/// lifecycle and metrics, not report the dead backend's progress.
#[tokio::test]
async fn a_replacement_stage_starts_with_fresh_lifecycle_and_metrics() {
    let engine = Arc::new(MediaEngine::new());
    let manager = crate::stage_runtime::StageRuntimeManager::new(engine.clone());
    let key = StageKey::new("pipe", StageKind::video_preset_with_codec("720p", "h264"));
    let source = Arc::new(RingBuffer::new(16));

    let (old, _old_lease, _) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    old.lifecycle.record_first_output();
    old.cancel.cancel();
    let (new, _new_lease, created) = manager.ensure_stage(key, source, None).await;
    assert!(created);
    assert!(!Arc::ptr_eq(&old.lifecycle, &new.lifecycle));
    assert!(!Arc::ptr_eq(&old.metrics, &new.metrics));
    assert_eq!(
        new.lifecycle.snapshot().phase,
        crate::stage_lifecycle::StagePhase::Registered
    );
}
