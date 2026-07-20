use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::domain::stage::{StageKey, StageKind};
use crate::media::avio::MemoryQueue;
use crate::media::engine::MediaEngine;
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

fn test_key() -> StageKey {
    StageKey::new("pipe-a", StageKind::source())
}

#[tokio::test]
async fn register_input_queue_is_noop_when_runtime_missing() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .register_input_queue(key.clone(), Arc::new(MemoryQueue::new()))
        .await;
    assert!(engine.stages.runtimes.read().await.get(&key).is_none());
}

#[tokio::test]
async fn register_and_remove_input_queue_round_trips_on_existing_runtime() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    engine
        .register_input_queue(key.clone(), Arc::new(MemoryQueue::new()))
        .await;
    assert!(
        engine.stages.runtimes.read().await[&key]
            .input_queue
            .is_some()
    );

    engine.remove_input_queue(&key).await;
    assert!(
        engine.stages.runtimes.read().await[&key]
            .input_queue
            .is_none()
    );
}

#[tokio::test]
async fn remove_input_queue_is_noop_when_runtime_missing() {
    let engine = MediaEngine::new();
    engine.remove_input_queue(&test_key()).await;
}

#[tokio::test]
async fn register_and_remove_pipe_metrics_round_trips_on_existing_runtime() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    engine
        .register_pipe_metrics(key.clone(), Arc::new(PipeMetrics::default()))
        .await;
    assert!(
        engine.stages.runtimes.read().await[&key]
            .pipe_metrics
            .is_some()
    );

    engine.remove_pipe_metrics(&key).await;
    assert!(
        engine.stages.runtimes.read().await[&key]
            .pipe_metrics
            .is_none()
    );
}

#[tokio::test]
async fn remove_pipe_metrics_is_noop_when_runtime_missing() {
    let engine = MediaEngine::new();
    engine.remove_pipe_metrics(&test_key()).await;
}

#[tokio::test]
async fn get_or_create_non_ring_stage_runtime_creates_on_first_call() {
    let engine = MediaEngine::new();
    let key = test_key();
    let (lifecycle, _metrics) = engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(lifecycle.current_phase(), StagePhase::Registered);
    assert_eq!(
        lifecycle.current_backend(),
        StageBackendKind::InternalFfmpeg
    );
}

#[tokio::test]
async fn get_or_create_non_ring_stage_runtime_reuses_when_not_cancelled() {
    let engine = MediaEngine::new();
    let key = test_key();
    let (first, _) = engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    let (second, _) = engine
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
    let engine = MediaEngine::new();
    let key = test_key();
    let stale_cancel = CancellationToken::new();
    let (first, _) = engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            stale_cancel.clone(),
        )
        .await;
    stale_cancel.cancel();

    let (second, _) = engine
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
    let engine = MediaEngine::new();
    let key = test_key();
    engine.remove_stage_metrics(&key).await;
    engine.remove_stage_metrics(&key).await;
}

#[tokio::test]
async fn remove_stage_metrics_removes_entry() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine.get_or_create_stage_metrics(key.clone()).await;
    assert!(engine.stages.metrics.read().await.contains_key(&key));
    engine.remove_stage_metrics(&key).await;
    assert!(!engine.stages.metrics.read().await.contains_key(&key));
}

#[tokio::test]
async fn remove_stage_runtime_is_idempotent_on_missing_key() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine.remove_stage_runtime(&key).await;
    engine.remove_stage_runtime(&key).await;
}

#[tokio::test]
async fn remove_stage_runtime_removes_entry() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert!(engine.stages.runtimes.read().await.contains_key(&key));
    engine.remove_stage_runtime(&key).await;
    assert!(!engine.stages.runtimes.read().await.contains_key(&key));
}

#[tokio::test]
async fn get_or_create_stage_lifecycle_with_backend_is_idempotent() {
    let engine = MediaEngine::new();
    let key = test_key();
    let first = engine
        .get_or_create_stage_lifecycle_with_backend(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::HlsSegmenter,
        )
        .await;
    let second = engine
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
    let engine = MediaEngine::new();
    let key = test_key();
    let (runtime_lifecycle, _) = engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Registered,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    let lifecycle = engine
        .get_or_create_stage_lifecycle_with_backend(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::Recording,
        )
        .await;

    assert!(Arc::ptr_eq(&runtime_lifecycle, &lifecycle));
    assert!(!engine.stages.lifecycles.read().await.contains_key(&key));
}

#[tokio::test]
async fn remove_stage_lifecycle_is_idempotent_on_missing_key() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine.remove_stage_lifecycle(&key).await;
    engine.remove_stage_lifecycle(&key).await;
}

#[tokio::test]
async fn remove_stage_lifecycle_removes_entry() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;
    assert!(engine.stages.lifecycles.read().await.contains_key(&key));
    engine.remove_stage_lifecycle(&key).await;
    assert!(!engine.stages.lifecycles.read().await.contains_key(&key));
}

#[tokio::test]
async fn stage_lifecycle_snapshot_is_none_when_key_is_absent_from_both_maps() {
    let engine = MediaEngine::new();
    assert!(engine.stage_lifecycle_snapshot(&test_key()).await.is_none());
}

#[tokio::test]
async fn stage_lifecycle_snapshot_falls_back_to_lifecycles_map_when_no_runtime() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Producing)
        .await;

    let snapshot = engine
        .stage_lifecycle_snapshot(&key)
        .await
        .expect("lifecycle-only entry should be found via fallback");
    assert_eq!(snapshot.phase, StagePhase::Producing);
}

#[tokio::test]
async fn stage_lifecycle_snapshot_prefers_runtime_map_when_both_present() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Failed)
        .await;
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    let snapshot = engine.stage_lifecycle_snapshot(&key).await.unwrap();
    assert_eq!(snapshot.phase, StagePhase::Producing);
}

#[tokio::test]
async fn stage_runtime_snapshot_has_no_capacity_fields_outside_capacity_phases() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    let snapshot = engine.stage_runtime_snapshot(&key).await.unwrap();
    assert!(snapshot.capacity_permits_total.is_none());
    assert!(snapshot.capacity_permits_available.is_none());
    assert!(snapshot.capacity_wait_ms.is_none());
}

#[tokio::test]
async fn stage_runtime_snapshot_has_capacity_fields_during_waiting_for_capacity() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::WaitingForCapacity {
                backend: StageBackendKind::ExternalFfmpeg,
            },
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;

    let snapshot = engine.stage_runtime_snapshot(&key).await.unwrap();
    assert!(snapshot.capacity_permits_total.is_some());
    assert!(snapshot.capacity_permits_available.is_some());
    assert!(snapshot.capacity_wait_ms.is_some());
}

#[tokio::test]
async fn egress_blocked_by_stage_snapshot_is_none_for_missing_key() {
    let engine = MediaEngine::new();
    assert!(
        engine
            .egress_blocked_by_stage_snapshot(&test_key())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn egress_blocked_by_stage_snapshot_is_none_when_producing() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    assert!(
        engine
            .egress_blocked_by_stage_snapshot(&key)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn egress_blocked_by_stage_snapshot_is_some_when_not_yet_producing() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::FirstInput,
            StageBackendKind::InternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    let snapshot = engine
        .egress_blocked_by_stage_snapshot(&key)
        .await
        .expect("non-producing stage should report as blocking");
    assert_eq!(snapshot.phase, StagePhase::FirstInput);
}
