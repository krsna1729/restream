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
async fn stale_egress_error_cannot_poison_replacement_attempt() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "stream-key", "rtmp")
        .await
        .unwrap();

    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/one", None)
        .await;
    engine.unregister_egress("out-race").await;

    let replacement = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/two", None)
        .await;

    assert!(
        !engine
            .record_egress_error_if_current("out-race", &first, "send", "stale failure",)
            .await,
        "stale failure metadata must not attach to a replacement egress attempt"
    );
    engine
        .record_egress_progress_if_current("out-race", &replacement, 2048)
        .await;
    assert!(
        engine
            .record_egress_error_if_current(
                "out-race",
                &replacement,
                "connect",
                "replacement failure",
            )
            .await,
        "current attempt should still publish its own failure metadata"
    );
    assert!(
        engine
            .unregister_egress_if_current("out-race", &replacement)
            .await,
        "replacement attempt should unregister cleanly"
    );

    let pipelines = vec!["pipe-1".to_string()];
    let snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-race"];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["failurePhase"], "connect");
    assert_eq!(output["lastError"], "replacement failure");
    assert_eq!(output["totalSize"], 2048);
}

#[tokio::test]
async fn stale_egress_queue_removal_cannot_drop_replacement_queue() {
    let engine = MediaEngine::new();
    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "srt://example.com:10080", None)
        .await;
    let first_queue = Arc::new(MemoryQueue::new());
    assert!(
        engine
            .register_egress_queue_if_current("out-race", &first, first_queue)
            .await
    );
    engine.unregister_egress("out-race").await;

    let replacement = engine
        .register_egress_attempt("out-race", "pipe-1", "srt://example.com:10081", None)
        .await;
    let replacement_queue = Arc::new(MemoryQueue::new());
    assert!(
        engine
            .register_egress_queue_if_current("out-race", &replacement, replacement_queue.clone(),)
            .await
    );
    assert!(
        !engine
            .remove_egress_queue_if_current("out-race", &first)
            .await,
        "stale cleanup must not remove the replacement queue"
    );
    assert!(Arc::ptr_eq(
        &engine
            .egresses
            .queues
            .read()
            .await
            .get("out-race")
            .expect("replacement queue should stay registered")
            .clone(),
        &replacement_queue
    ));
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
            Some(crate::runtime::stage::StageRuntimeSnapshot {
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
async fn health_snapshot_drops_egress_registry_before_stage_blocked_lookup() {
    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", Some(key))
        .await;

    let stage_write = engine.stages.runtimes.write().await;
    let health = {
        let engine = engine.clone();
        tokio::spawn(async move {
            test_health_snapshot(&engine, &[String::from("pipe-1")], &HashMap::new()).await
        })
    };

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let active_write = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        engine.egresses.active.write(),
    )
    .await
    .expect("health snapshot must not hold egresses.active while awaiting stage registries");
    drop(active_write);

    drop(stage_write);
    health.await.expect("health snapshot task should not panic");
}

#[tokio::test]
async fn stage_runtime_snapshot_reads_runtime_after_side_maps_removed() {
    use crate::media::ring_buffer::RingBuffer;
    use crate::media::stage_lifecycle::StagePhase;
    use crate::media::stage_runtime::StageRuntimeManager;

    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let manager = StageRuntimeManager::new(engine.clone());
    let (handle, _) = manager
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
async fn health_snapshot_includes_blocked_by_for_waiting_terminal_stage() {
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "stream-key", "srt")
        .await
        .unwrap();
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let lc = engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;
    lc.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", Some(key))
        .await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["blockedBy"]["phase"], "waitingForCapacity");
    assert_eq!(output["blockedBy"]["backend"], "externalFfmpeg");
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
async fn health_snapshot_exposes_egress_progress_and_error_state() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .register_egress(
            "out-1",
            "pipe-1",
            "srt://example.com:10080?streamid=live/key",
        )
        .await;
    engine
        .update_egress_target_addr("out-1", "203.0.113.10:10080".to_string())
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine
        .update_egress_quality(
            "out-1",
            PublisherQuality {
                tcp_congestion_algorithm: Some("cubic".to_string()),
                mbps_send_rate: Some(3.2),
                packets_sent_retrans: Some(2),
                srt_bonded: Some(true),
                srt_group_member_count: Some(2),
                srt_group_active_members: Some(1),
                ..PublisherQuality::default()
            },
        )
        .await;
    engine.record_egress_progress("out-1", 1316).await;
    engine
        .record_egress_error("out-1", "send", "synthetic send failure")
        .await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];

    assert_eq!(output["protocol"], "srt");
    assert_eq!(output["status"], "failed");
    assert_eq!(output["targetAddr"], "203.0.113.10:10080");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
    assert_eq!(output["lastError"], "synthetic send failure");
    assert_eq!(output["totalSize"], 1316);
    assert_eq!(output["quality"]["mbpsSendRate"], 3.2);
    assert_eq!(output["quality"]["tcpCongestionAlgorithm"], "cubic");
    assert_eq!(output["quality"]["packetsSentRetrans"], 2);
    assert_eq!(output["quality"]["srtBonded"], true);
    assert_eq!(output["quality"]["srtGroupMemberCount"], 2);
    assert_eq!(output["quality"]["srtGroupActiveMembers"], 1);
    assert!(!output["lastProgressAt"].is_null());
    assert!(!output["lastErrorAt"].is_null());
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
        crate::events::EventKind::EgressFailed {
            output_id,
            phase,
            error,
            ..
        } if output_id == "out-1" && phase == "connect" && error == "connection refused"
    )));
}

#[tokio::test]
async fn egress_progress_after_error_clears_failed_phase() {
    let engine = MediaEngine::new();
    engine
        .register_egress(
            "out-1",
            "pipe-1",
            "https://upload.example.com/live/out.m3u8?token=abc",
        )
        .await;
    engine
        .record_egress_error("out-1", "upload_segment", "temporary sink outage")
        .await;
    engine.record_egress_progress("out-1", 4096).await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];

    assert_eq!(output["phase"], "uploading");
    assert!(output["failurePhase"].is_null());
    assert!(output["lastError"].is_null());
    assert!(output["lastErrorAt"].is_null());
    assert_eq!(output["totalSize"], 4096);
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
