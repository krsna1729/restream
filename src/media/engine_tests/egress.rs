use super::*;

#[tokio::test]
async fn unregister_ingest_cancels_token() {
    let engine = MediaEngine::new();
    let token = engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    assert!(!token.is_cancelled());

    engine.unregister_ingest("p1").await;
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn unregister_ingest_idempotent() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine.unregister_ingest("p1").await;
    // Second unregister should not panic
    engine.unregister_ingest("p1").await;
}

#[tokio::test]
async fn egress_register_and_cancel() {
    let engine = MediaEngine::new();
    let token = engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    assert!(!token.is_cancelled());

    engine.unregister_egress("out-1").await;
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn egress_unregister_idempotent() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine.unregister_egress("out-1").await;
    engine.unregister_egress("out-1").await;
}

#[tokio::test]
async fn stale_egress_unregister_cannot_clobber_replacement_attempt() {
    let engine = MediaEngine::new();

    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/one", None)
        .await;
    engine.unregister_egress("out-race").await;

    let replacement = engine
        .register_egress_attempt("out-race", "pipe-1", "srt://example.com:10080", None)
        .await;

    assert!(
        !engine
            .unregister_egress_if_current("out-race", &first)
            .await,
        "stale cleanup from the old egress attempt must not remove the replacement"
    );
    assert!(
        engine
            .with_active_egress("out-race", |egress| egress.attempt_id)
            .await
            .is_some_and(|attempt_id| attempt_id == replacement.attempt_id),
        "replacement egress must remain active after stale unregister"
    );
}

#[tokio::test]
async fn egress_registration_stores_terminal_stage_key() {
    let engine = MediaEngine::new();
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let reg = engine
        .register_egress_attempt(
            "out-1",
            "pipe-1",
            "rtmp://example.com/live/key",
            Some(key.clone()),
        )
        .await;
    assert!(reg.attempt_id > 0);
    let stored = engine
        .with_active_egress("out-1", |e| e.terminal_stage_key.clone())
        .await;
    assert_eq!(stored, Some(Some(key)));
}

#[tokio::test]
async fn egress_blocked_by_phase_reports_waiting_upstream_stage() {
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        external_ffmpeg_permits: 3,
        ..Default::default()
    }));
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let lc = engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;
    lc.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    engine
        .register_egress_attempt(
            "out-1",
            "pipe-1",
            "rtmp://example.com/live/key",
            Some(key.clone()),
        )
        .await;

    let blocked = {
        let egresses = engine.egresses.active.read().await;
        let egress = egresses.get("out-1").unwrap();
        engine.egress_blocked_by_snapshot(egress).await
    };
    assert!(
        matches!(
            blocked,
            Some(restream_domain::runtime::stage::StageRuntimeSnapshot {
                phase: StagePhase::WaitingForCapacity { .. },
                capacity_permits_total: Some(3),
                ..
            })
        ),
        "expected blocked by WaitingForCapacity with configured permits, got {blocked:?}"
    );

    lc.transition(StagePhase::Producing);
    let blocked = {
        let egresses = engine.egresses.active.read().await;
        let egress = egresses.get("out-1").unwrap();
        engine.egress_blocked_by_snapshot(egress).await
    };
    assert_eq!(blocked, None, "producing stage must not block egress");
}

#[tokio::test]
async fn stage_runtime_snapshot_reads_runtime_after_side_maps_removed() {
    use crate::media::stage_lifecycle::StagePhase;
    use crate::media::stage_runtime::StageRuntimeManager;
    use restream_sync::RingBuffer;

    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let manager = StageRuntimeManager::new(engine.clone());
    let (handle, _lease27, _) = manager
        .ensure_stage(key.clone(), Arc::new(RingBuffer::new(4)), None)
        .await;
    handle.metrics.record_in(42);
    handle.lifecycle.transition(StagePhase::RunningNoOutputYet);
    engine.stages.metrics.write().await.remove(&key);
    engine.stages.lifecycles.write().await.remove(&key);

    let snapshot = engine
        .stage_runtime_snapshot(&key)
        .await
        .expect("runtime-backed stage should not depend on side maps");
    assert_eq!(snapshot.phase, StagePhase::RunningNoOutputYet);
    assert_eq!(snapshot.bytes_in, 42);

    let metrics = engine.get_or_create_stage_metrics(key.clone()).await;
    let lifecycle = engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;

    assert!(Arc::ptr_eq(&metrics, &handle.metrics));
    assert!(Arc::ptr_eq(&lifecycle, &handle.lifecycle));
}

#[tokio::test]
async fn egress_bytes_counter() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;

    engine.update_egress_bytes("out-1", 1000).await;
    engine.update_egress_bytes("out-1", 500).await;
    assert_eq!(engine.egress_bytes("out-1").await, 1500);

    // Non-existent egress returns 0
    assert_eq!(engine.egress_bytes("out-nonexistent").await, 0);
}

#[tokio::test]
async fn egress_failure_event_survives_unregister() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine
        .record_egress_error("out-1", "connect", "connection refused")
        .await;
    engine.unregister_egress("out-1").await;

    let events = engine.runtime.event_log.recent(10, Some("pipe-1"));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        crate::media::events::EventKind::EgressFailed {
            output_id,
            phase,
            error,
            ..
        } if output_id == "out-1" && phase == "connect" && error == "connection refused"
    )));
}

#[tokio::test]
async fn egress_has_recorded_progress_only_after_progress_update() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;

    assert!(!engine.egress_has_recorded_progress("out-1").await);

    engine.record_egress_progress("out-1", 188).await;

    assert!(engine.egress_has_recorded_progress("out-1").await);
}

#[tokio::test]
async fn egress_attribution_is_a_no_op_for_an_unregistered_output() {
    let engine = MediaEngine::new();

    // Must not panic when the output was never registered (or was already
    // unregistered by the time the caller's fabric-routing decision lands).
    let registration = engine
        .register_egress_attempt("other", "pipe-1", "rtmp://example.com/live/key", None)
        .await;
    engine
        .set_egress_fabric_attribution(
            "missing-output",
            &registration,
            crate::media::engine::FabricSlot {
                feed: crate::media::egress::command::FeedId::new("rtmp:pipe-1:source"),
                shard: 1,
            },
        )
        .await;
}
