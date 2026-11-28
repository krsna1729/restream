use tokio_util::sync::CancellationToken;

use crate::engine::MediaEngine;
use crate::stage_lifecycle::{StageBackendKind, StagePhase};
use restream_domain::domain::stage::{StageKey, StageKind};

fn test_key() -> StageKey {
    StageKey::new("pipe-a", StageKind::source())
}

#[tokio::test]
async fn stage_runtime_snapshot_has_no_capacity_fields_outside_capacity_phases() {
    let engine = MediaEngine::new();
    let key = test_key();
    engine
        .get_or_create_non_ring_stage_runtime(
            key.clone(),
            StagePhase::Producing,
            StageBackendKind::ExternalFfmpeg,
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
            StageBackendKind::ExternalFfmpeg,
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
            StageBackendKind::ExternalFfmpeg,
            CancellationToken::new(),
        )
        .await;
    let snapshot = engine
        .egress_blocked_by_stage_snapshot(&key)
        .await
        .expect("non-producing stage should report as blocking");
    assert_eq!(snapshot.phase, StagePhase::FirstInput);
}
