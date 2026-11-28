use super::*;

#[tokio::test]
async fn unregister_ingest_cancels_token() {
    let engine = Arc::new(MediaEngine::new());
    let lease = engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    let token = &lease.registration().cancel_token;
    assert!(!token.is_cancelled());

    engine.unregister_ingest("p1").await;
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn unregister_ingest_idempotent() {
    let engine = Arc::new(MediaEngine::new());
    let _ingest = engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine.unregister_ingest("p1").await;
    // Second unregister should not panic
    engine.unregister_ingest("p1").await;
}

#[tokio::test]
async fn egress_register_and_cancel() {
    let engine = Arc::new(MediaEngine::new());
    let token = engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    assert!(!token.is_cancelled());

    engine.unregister_egress("out-1").await;
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn egress_unregister_idempotent() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine.unregister_egress("out-1").await;
    engine.unregister_egress("out-1").await;
}

#[tokio::test]
async fn stale_egress_unregister_cannot_clobber_replacement_attempt() {
    let engine = Arc::new(MediaEngine::new());

    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/one", None)
        .await;
    engine.record_egress_progress("out-race", 1024).await;
    engine
        .record_egress_error("out-race", "send", "old attempt failed")
        .await;
    engine.unregister_egress("out-race").await;
    let settled = engine
        .recent_egress_outcome("out-race")
        .await
        .expect("first attempt must settle a recent outcome");

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
    let after = engine
        .recent_egress_outcome("out-race")
        .await
        .expect("recent outcome must survive stale unregister");
    assert_eq!(after.ended_at_ms, settled.ended_at_ms);
    assert_eq!(after.bytes_sent, settled.bytes_sent);
    assert_eq!(after.failure_count, settled.failure_count);
    assert_eq!(after.status, settled.status);
    assert_eq!(after.raw_status, settled.raw_status);
    assert!(
        engine.egress_retry_state("out-race").await.is_none(),
        "stale unregister must not publish retry state for the replacement"
    );
}

#[tokio::test]
async fn repeated_egress_unregister_keeps_settled_outcome_stable() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .register_egress("out-stable", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine.record_egress_progress("out-stable", 512).await;
    engine
        .record_egress_error("out-stable", "send", "connection reset by peer")
        .await;
    engine.unregister_egress("out-stable").await;
    let first = engine
        .recent_egress_outcome("out-stable")
        .await
        .expect("first unregister must settle a recent outcome");

    engine.unregister_egress("out-stable").await;
    let second = engine
        .recent_egress_outcome("out-stable")
        .await
        .expect("repeated unregister must not clear the settled outcome");
    assert_eq!(second.ended_at_ms, first.ended_at_ms);
    assert_eq!(second.bytes_sent, first.bytes_sent);
    assert_eq!(second.failure_count, first.failure_count);
    assert_eq!(second.status, first.status);
    assert_eq!(second.raw_status, first.raw_status);
    assert_eq!(second.last_error, first.last_error);
}

#[tokio::test]
async fn late_attempt_settlement_cannot_overwrite_replacement_outcome() {
    let engine = Arc::new(MediaEngine::new_with_config(Arc::new(
        crate::config::EngineConfig {
            srt_egress_muxer_max_outputs_per_shard: 1,
            srt_egress_muxer_max_shards: 1,
            ..crate::config::EngineConfig::default()
        },
    )));
    engine
        .register_egress("out-race", "pipe-1", "srt://example.com:10080")
        .await;
    engine.record_egress_progress("out-race", 1024).await;
    engine
        .record_egress_error("out-race", "send", "old attempt failed")
        .await;

    // Hold the SRT muxer shard registry so the old settlement pauses inside
    // unregister_egress after it removes the active attempt.
    let shard_guard = engine.stages.srt_muxer_shards.write().await;
    let settling = {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move {
            engine.unregister_egress("out-race").await;
        })
    };
    tokio::task::yield_now().await;

    engine
        .register_egress("out-race", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine.record_egress_progress("out-race", 2048).await;
    engine
        .record_egress_error("out-race", "send", "new attempt failed")
        .await;
    engine.unregister_egress("out-race").await;

    drop(shard_guard);
    settling.await.expect("settlement task");

    let recent = engine
        .recent_egress_outcome("out-race")
        .await
        .expect("recent outcome");
    assert_eq!(
        recent.bytes_sent, 2048,
        "late settlement from the older attempt must not overwrite the newer outcome"
    );
    assert_eq!(recent.last_error.as_deref(), Some("new attempt failed"));
}

#[tokio::test]
async fn retained_egress_records_keep_configured_outputs_only() {
    let engine = Arc::new(MediaEngine::new());
    for output_id in ["out-kept", "out-gone"] {
        engine
            .register_egress(output_id, "pipe-1", "rtmp://example.com/live/key")
            .await;
        engine
            .record_egress_error(output_id, "send", "connection reset by peer")
            .await;
        engine.unregister_egress(output_id).await;
        engine
            .update_egress_retry_state(output_id, 2, 5_000, 4_000)
            .await;
    }

    let keep = std::collections::HashSet::from(["out-kept".to_string()]);
    engine.retain_egress_records(&keep).await;

    assert!(engine.recent_egress_outcome("out-kept").await.is_some());
    assert!(engine.egress_retry_state("out-kept").await.is_some());
    assert!(engine.recent_egress_outcome("out-gone").await.is_none());
    assert!(engine.egress_retry_state("out-gone").await.is_none());
}

#[tokio::test]
async fn stale_attempt_cannot_clear_or_replace_newer_retry_state() {
    let engine = Arc::new(MediaEngine::new());
    let first = engine
        .register_egress_attempt("out-retry", "pipe-1", "rtmp://example.com/live/one", None)
        .await;
    assert!(
        engine
            .update_egress_retry_state_if_current("out-retry", &first, 2, 5_000, 4_000)
            .await
    );
    assert!(
        engine
            .clear_egress_retry_state_if_current("out-retry", &first)
            .await
    );
    assert!(engine.egress_retry_state("out-retry").await.is_none());

    // A later attempt settled its retry state after its active registration
    // was already gone (a stop/replacement removed it).
    let second = engine
        .register_egress_attempt("out-retry", "pipe-1", "rtmp://example.com/live/two", None)
        .await;
    engine.unregister_egress("out-retry").await;
    engine
        .update_egress_retry_state("out-retry", 3, 5_000, 4_000)
        .await;

    assert!(
        !engine
            .update_egress_retry_state_if_current("out-retry", &first, 9, 10_000, 9_000)
            .await,
        "a stale attempt must not replace the published retry state"
    );
    assert!(
        !engine
            .clear_egress_retry_state_if_current("out-retry", &first)
            .await,
        "a stale attempt must not clear the published retry state"
    );
    let kept = engine
        .egress_retry_state("out-retry")
        .await
        .expect("newer retry state must survive a stale clear");
    assert_eq!(kept.attempts, 3);
    assert_eq!(kept.backoff_ms, 5_000);
    assert!(
        !engine
            .clear_egress_retry_state_if_current("out-retry", &second)
            .await,
        "only an active attempt registration may clear retry state"
    );
    assert!(engine.egress_retry_state("out-retry").await.is_some());
}

#[tokio::test]
async fn egress_registration_stores_terminal_stage_key() {
    let engine = Arc::new(MediaEngine::new());
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
    use crate::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new_with_config(Arc::new(crate::config::EngineConfig {
        external_ffmpeg_permits: 3,
        ..Default::default()
    }));
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let lc = engine
        .stages
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
    use crate::stage_lifecycle::StagePhase;
    use restream_sync::RingBuffer;

    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let manager = engine.stage_runtime();
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

    let metrics = engine.stages.get_or_create_stage_metrics(key.clone()).await;
    let lifecycle = engine
        .stages
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;

    assert!(Arc::ptr_eq(&metrics, &handle.metrics));
    assert!(Arc::ptr_eq(&lifecycle, &handle.lifecycle));
}

#[tokio::test]
async fn egress_bytes_counter() {
    let engine = Arc::new(MediaEngine::new());
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
    let engine = Arc::new(MediaEngine::new());
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
        crate::events::EventKind::EgressFailed {
            output_id,
            phase,
            error,
            ..
        } if output_id == "out-1" && phase == "connect" && error == "connection refused"
    )));
}

#[tokio::test]
async fn egress_has_recorded_progress_only_after_progress_update() {
    let engine = Arc::new(MediaEngine::new());
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;

    assert!(!engine.egress_has_recorded_progress("out-1").await);

    engine.record_egress_progress("out-1", 188).await;

    assert!(engine.egress_has_recorded_progress("out-1").await);
}

#[tokio::test]
async fn egress_attribution_is_a_no_op_for_an_unregistered_output() {
    let engine = Arc::new(MediaEngine::new());

    // Must not panic when the output was never registered (or was already
    // unregistered by the time the caller's fabric-routing decision lands).
    let registration = engine
        .register_egress_attempt("other", "pipe-1", "rtmp://example.com/live/key", None)
        .await;
    engine
        .set_egress_fabric_attribution(
            "missing-output",
            &registration,
            crate::engine_registries::FabricSlot {
                feed: crate::egress::command::FeedId::new("rtmp:pipe-1:source"),
                shard: 1,
            },
        )
        .await;
}
