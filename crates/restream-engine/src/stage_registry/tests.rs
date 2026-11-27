use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{SrtMuxerShardPool, StageRegistry};
use crate::pipe_metrics::PipeMetrics;
use crate::stage_lifecycle::{StageBackendKind, StagePhase};
use restream_domain::domain::stage::{StageKey, StageKind};

fn test_key() -> StageKey {
    StageKey::new("pipe-a", StageKind::source())
}

#[tokio::test]
async fn register_and_remove_pipe_metrics_round_trips_on_existing_runtime() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    stages
        .register_pipe_metrics(key.clone(), Arc::new(PipeMetrics::default()))
        .await;
    assert!(stages.runtimes.read().await[&key].pipe_metrics.is_some());

    stages.remove_pipe_metrics(&key).await;
    assert!(stages.runtimes.read().await[&key].pipe_metrics.is_none());
}

#[tokio::test]
async fn remove_pipe_metrics_is_noop_when_runtime_missing() {
    let stages = StageRegistry::new();
    stages.remove_pipe_metrics(&test_key()).await;
}

#[tokio::test]
async fn get_or_create_non_ring_stage_runtime_creates_on_first_call() {
    let stages = StageRegistry::new();
    let key = test_key();
    let (lifecycle, _metrics) = stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(lifecycle.current_phase(), StagePhase::Registered);
    assert_eq!(
        lifecycle.current_backend(),
        StageBackendKind::ExternalFfmpeg
    );
}

#[tokio::test]
async fn get_or_create_non_ring_stage_runtime_reuses_when_not_cancelled() {
    let stages = StageRegistry::new();
    let key = test_key();
    let (first, _) = stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    let (second, _) = stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(second.current_phase(), StagePhase::Registered);
}

#[tokio::test]
async fn get_or_create_non_ring_stage_runtime_replaces_when_cancelled() {
    let stages = StageRegistry::new();
    let key = test_key();
    let stale_cancel = CancellationToken::new();
    let (first, _) = stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::ExternalFfmpeg,
            stale_cancel.clone(),
        )
        .await;
    stale_cancel.cancel();

    let (second, _) = stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(second.current_phase(), StagePhase::Producing);
}

#[tokio::test]
async fn remove_stage_metrics_is_idempotent_on_missing_key() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages.remove_stage_metrics(&key).await;
    stages.remove_stage_metrics(&key).await;
}

#[tokio::test]
async fn remove_stage_metrics_removes_entry() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages.get_or_create_stage_metrics(key.clone()).await;
    assert!(stages.metrics.read().await.contains_key(&key));
    stages.remove_stage_metrics(&key).await;
    assert!(!stages.metrics.read().await.contains_key(&key));
}

#[tokio::test]
async fn remove_stage_runtime_is_idempotent_on_missing_key() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages.remove_stage_runtime(&key).await;
    stages.remove_stage_runtime(&key).await;
}

#[tokio::test]
async fn remove_stage_runtime_removes_entry() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert!(stages.runtimes.read().await.contains_key(&key));
    stages.remove_stage_runtime(&key).await;
    assert!(!stages.runtimes.read().await.contains_key(&key));
}

#[tokio::test]
async fn get_or_create_stage_lifecycle_with_backend_is_idempotent() {
    let stages = StageRegistry::new();
    let key = test_key();
    let first = stages
        .get_or_create_stage_lifecycle_with_backend(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::HlsSegmenter,
        )
        .await;
    let second = stages
        .get_or_create_stage_lifecycle_with_backend(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::Recording,
        )
        .await;
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(second.current_backend(), StageBackendKind::HlsSegmenter);
}

#[tokio::test]
async fn get_or_create_stage_lifecycle_with_backend_prefers_existing_runtime_lifecycle() {
    let stages = StageRegistry::new();
    let key = test_key();
    let (runtime_lifecycle, _) = stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    let lifecycle = stages
        .get_or_create_stage_lifecycle_with_backend(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::Recording,
        )
        .await;

    assert!(Arc::ptr_eq(&runtime_lifecycle, &lifecycle));
    assert!(!stages.lifecycles.read().await.contains_key(&key));
}

#[tokio::test]
async fn remove_stage_lifecycle_is_idempotent_on_missing_key() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages.remove_stage_lifecycle(&key).await;
    stages.remove_stage_lifecycle(&key).await;
}

#[tokio::test]
async fn remove_stage_lifecycle_removes_entry() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;
    assert!(stages.lifecycles.read().await.contains_key(&key));
    stages.remove_stage_lifecycle(&key).await;
    assert!(!stages.lifecycles.read().await.contains_key(&key));
}

#[tokio::test]
async fn stage_lifecycle_snapshot_is_none_when_key_is_absent_from_both_maps() {
    let stages = StageRegistry::new();
    assert!(stages.stage_lifecycle_snapshot(&test_key()).await.is_none());
}

#[tokio::test]
async fn stage_lifecycle_snapshot_falls_back_to_lifecycles_map_when_no_runtime() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Producing)
        .await;

    let snapshot = stages
        .stage_lifecycle_snapshot(&key)
        .await
        .expect("lifecycle-only entry should be found via fallback");
    assert_eq!(snapshot.phase, StagePhase::Producing);
}

#[tokio::test]
async fn stage_lifecycle_snapshot_prefers_runtime_map_when_both_present() {
    let stages = StageRegistry::new();
    let key = test_key();
    stages
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Failed)
        .await;
    stages
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    let snapshot = stages.stage_lifecycle_snapshot(&key).await.unwrap();
    assert_eq!(snapshot.phase, StagePhase::Producing);
}

#[test]
fn retiring_empty_srt_muxer_shard_is_not_reused_until_cleanup_finishes() {
    let mut pool = SrtMuxerShardPool::default();

    let first = pool.assign("out-1", 1, 1, 8);
    assert_eq!(first.shard_index, 0);

    let release = pool.release("out-1", 1).expect("assignment released");
    assert_eq!(release.shard_index, 0);
    assert!(release.shard_empty);

    let during_cleanup = pool.assign("out-2", 1, 1, 8);
    assert_eq!(during_cleanup.shard_index, 1);

    pool.finish_retiring(0);
    let after_cleanup = pool.assign("out-3", 1, 1, 8);
    assert_eq!(after_cleanup.shard_index, 0);
}

#[test]
fn assign_overflow_does_not_reuse_a_still_retiring_shard() {
    // At max_shards capacity with the sole shard mid-teardown (retiring),
    // a new output must not land on that shard's index: its backing
    // muxer may be concurrently torn down by a racing release, so
    // reusing the index would hand the output a dead stage. The pool
    // must grow past max_shards instead of falling back to a retiring
    // shard.
    let mut pool = SrtMuxerShardPool::default();
    let first = pool.assign("out-1", 1, 1, 1);
    assert_eq!(first.shard_index, 0);

    let release = pool.release("out-1", 1).expect("assignment released");
    assert_eq!(release.shard_index, 0);
    assert!(release.shard_empty);

    let second = pool.assign("out-2", 1, 1, 1);
    assert_ne!(
        second.shard_index, 0,
        "out-2 must not be assigned to shard 0 while it is still retiring"
    );

    let (_, occupancy, retiring) = pool.test_snapshot();
    assert!(retiring.contains(&0));
    assert_eq!(occupancy[0], 0);
}

#[test]
fn assign_same_output_and_attempt_is_idempotent_and_does_not_double_occupy() {
    let mut pool = SrtMuxerShardPool::default();

    let first = pool.assign("out-1", 1, 4, 8);
    let second = pool.assign("out-1", 1, 4, 8);

    assert_eq!(first.shard_index, second.shard_index);
    // Re-asserting the same (output, attempt) pair must not consume a
    // second occupancy slot on the shard.
    let (_, occupancy, _) = pool.test_snapshot();
    assert_eq!(occupancy[first.shard_index], 1);
    assert_eq!(second.shard_occupancy, 1);
}

#[test]
fn assign_overflow_boundary_uses_strict_greater_than_on_idempotent_path() {
    // Fill a single shard to exactly max_outputs_per_shard, then
    // re-assert the same (output, attempt) pair already on that shard.
    // The idempotent-return branch reports `overflowed` only when
    // occupancy is strictly greater than capacity, so sitting exactly
    // at capacity must not be flagged as overflowed.
    let mut pool = SrtMuxerShardPool::default();
    pool.assign("out-1", 1, 1, 1);

    let reassert = pool.assign("out-1", 1, 1, 1);
    assert_eq!(reassert.shard_occupancy, 1);
    assert!(!reassert.overflowed);
}

#[test]
fn assign_reconnect_with_new_attempt_id_frees_old_shard_immediately() {
    // A publisher reconnect reuses the same output_id but gets a new
    // attempt_id; the stale assignment must be released (not leaked)
    // and the freed shard must be immediately reusable without going
    // through the retiring-shard cleanup path.
    let mut pool = SrtMuxerShardPool::default();
    let first = pool.assign("out-1", 1, 1, 1);
    assert_eq!(first.shard_index, 0);

    let reconnected = pool.assign("out-1", 2, 1, 1);
    assert_eq!(reconnected.shard_index, 0);
    assert_eq!(reconnected.shard_occupancy, 1);

    let (assignments, occupancy, retiring) = pool.test_snapshot();
    assert_eq!(assignments.get("out-1").unwrap().attempt_id, 2);
    assert_eq!(occupancy[0], 1);
    assert!(retiring.is_empty());
}

#[test]
fn assign_least_occupied_shard_selection_prefers_first_tied_minimum() {
    let mut pool = SrtMuxerShardPool::default();
    let a = pool.assign("out-1", 1, 1, 3);
    let b = pool.assign("out-2", 1, 1, 3);
    let c = pool.assign("out-3", 1, 1, 3);

    assert_eq!((a.shard_index, b.shard_index, c.shard_index), (0, 1, 2));
}

#[test]
fn assign_overflow_warns_only_once_when_shards_and_capacity_are_exhausted() {
    let mut pool = SrtMuxerShardPool::default();
    let first = pool.assign("out-1", 1, 1, 1);
    assert!(!first.overflowed);

    let second = pool.assign("out-2", 1, 1, 1);
    assert!(second.overflowed);
    assert!(second.should_warn_overflow);

    let third = pool.assign("out-3", 1, 1, 1);
    assert!(third.overflowed);
    assert!(!third.should_warn_overflow);
}

#[test]
fn release_with_mismatched_attempt_id_is_a_stale_noop() {
    // A cleanup task from a superseded attempt racing after a
    // reconnect must not be able to release the current assignment.
    let mut pool = SrtMuxerShardPool::default();
    pool.assign("out-1", 1, 4, 8);

    assert_eq!(pool.release("out-1", 999), None);

    let (assignments, occupancy, _) = pool.test_snapshot();
    assert_eq!(assignments.get("out-1").unwrap().attempt_id, 1);
    assert_eq!(occupancy[0], 1);
}

#[test]
fn release_unknown_output_returns_none_without_panicking() {
    let mut pool = SrtMuxerShardPool::default();
    assert_eq!(pool.release("never-assigned", 1), None);
}

#[test]
fn finish_retiring_unknown_shard_is_a_noop() {
    let mut pool = SrtMuxerShardPool::default();
    // Must not panic even though shard 7 was never allocated, let
    // alone marked retiring.
    pool.finish_retiring(7);
    assert!(pool.is_empty());
}

// `assign`'s zero-argument checks are `debug_assert!`, not `assert!`, so
// the engine never panics on this caller bug in a release-like build
// (`--profile bench` disables debug_assertions, same as release) —
// these tests only observe the panic where debug_assertions are on.
#[test]
#[should_panic]
#[cfg(debug_assertions)]
fn assign_panics_on_zero_max_shards_invariant() {
    let mut pool = SrtMuxerShardPool::default();
    let _ = crate::test_support::with_expected_panic_suppressed(|| pool.assign("out-1", 1, 4, 0));
}

#[test]
#[should_panic]
#[cfg(debug_assertions)]
fn assign_panics_on_zero_max_outputs_per_shard_invariant() {
    let mut pool = SrtMuxerShardPool::default();
    let _ = crate::test_support::with_expected_panic_suppressed(|| pool.assign("out-1", 1, 0, 4));
}
