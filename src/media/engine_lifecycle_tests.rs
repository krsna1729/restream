use super::*;

#[tokio::test]
async fn health_input_on_after_register_off_after_unregister() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "off");

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "on");

    engine.unregister_ingest("p1").await;
    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "off");
}

#[tokio::test]
async fn health_snapshot_preserves_recent_ingest_disconnect_details_after_unregister() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta("p1", None, None, Some("127.0.0.1:9000".to_string()))
        .await;
    engine.update_ingest_bytes("p1", 4096).await;
    engine
        .record_ingest_disconnect(
            "p1",
            Some("session"),
            Some("publisher disconnected".to_string()),
            false,
        )
        .await;
    engine.unregister_ingest("p1").await;

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snap["pipelines"]["p1"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["probeStatus"], "off");
    assert_eq!(input["lastSessionProtocol"], "rtmp");
    assert_eq!(input["lastDisconnectReason"], "publisher disconnected");
    assert_eq!(input["lastFailurePhase"], "session");
    assert_eq!(input["recentDisconnectError"], false);
    assert_eq!(input["disconnectGraceActive"], false);
    assert!(input["disconnectGraceRemainingMs"].is_null());
    assert_eq!(input["lastRemoteAddr"], "127.0.0.1:9000");
    assert_eq!(input["lastSessionBytesReceived"], 4096);
    assert!(input["lastDisconnectAt"].is_string());
    assert!(input["lastDisconnectAgeMs"].as_u64().is_some());
}

#[tokio::test]
async fn health_snapshot_exposes_disconnect_grace_window_fields() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .record_ingest_disconnect(
            "p1",
            Some("disconnect"),
            Some("publisher disconnected".to_string()),
            false,
        )
        .await;
    engine.unregister_ingest("p1").await;

    let snap =
        test_health_snapshot_with_disconnect_grace(&engine, &pipelines, &HashMap::new(), 5_000)
            .await;
    let input = &snap["pipelines"]["p1"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["disconnectGraceActive"], true);
    assert!(
        input["disconnectGraceRemainingMs"]
            .as_u64()
            .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
    );

    let no_grace = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(
        no_grace["pipelines"]["p1"]["input"]["disconnectGraceActive"],
        false
    );
    assert!(no_grace["pipelines"]["p1"]["input"]["disconnectGraceRemainingMs"].is_null());
}

#[tokio::test]
async fn recent_ingest_disconnect_respects_grace_window() {
    let engine = MediaEngine::new();
    let now_ms = MediaEngine::now_epoch_ms();

    engine.ingests.recent.write().await.insert(
        "inside".to_string(),
        RecentIngestOutcome {
            protocol: "rtmp".to_string(),
            disconnected_at_ms: now_ms,
            first_disconnect_at_ms: now_ms,
            disconnect_count: 1,
            reason: Some("publisher disconnected".to_string()),
            failure_phase: Some("disconnect".to_string()),
            had_error: false,
            remote_addr: Some("127.0.0.1:1935".to_string()),
            bytes_received: 1024,
        },
    );
    engine.ingests.recent.write().await.insert(
        "outside".to_string(),
        RecentIngestOutcome {
            protocol: "srt".to_string(),
            disconnected_at_ms: now_ms.saturating_sub(1_000),
            first_disconnect_at_ms: now_ms.saturating_sub(1_000),
            disconnect_count: 1,
            reason: Some("receiver stopped".to_string()),
            failure_phase: Some("receive".to_string()),
            had_error: true,
            remote_addr: Some("127.0.0.1:9000".to_string()),
            bytes_received: 2048,
        },
    );

    assert!(
        engine.has_recent_ingest_disconnect("inside", 250).await,
        "disconnects strictly inside the grace window should be treated as recent"
    );
    assert!(
        !engine.has_recent_ingest_disconnect("outside", 250).await,
        "disconnects older than the grace window should not count as recent"
    );
    assert!(
        !engine.has_recent_ingest_disconnect("inside", 0).await,
        "zero grace disables the recent-disconnect shortcut entirely"
    );
    assert!(
        !engine.has_recent_ingest_disconnect("missing", 250).await,
        "pipelines without a recent disconnect record must not be treated as recent"
    );
}

#[test]
fn build_recent_ingest_outcome_resets_flap_streak_outside_window() {
    let now_ms = MediaEngine::now_epoch_ms();
    let previous = RecentIngestOutcome {
        protocol: "rtmp".to_string(),
        disconnected_at_ms: now_ms.saturating_sub(INGEST_FLAP_WINDOW_MS + 1),
        first_disconnect_at_ms: now_ms.saturating_sub(INGEST_FLAP_WINDOW_MS + 10_000),
        disconnect_count: 4,
        reason: Some("publisher disconnected".to_string()),
        failure_phase: Some("disconnect".to_string()),
        had_error: false,
        remote_addr: Some("127.0.0.1:1935".to_string()),
        bytes_received: 2048,
    };

    let next = MediaEngine::build_recent_ingest_outcome(
        Some(&previous),
        "rtmp".to_string(),
        Some("disconnect"),
        Some("publisher disconnected".to_string()),
        false,
        Some("127.0.0.1:1935".to_string()),
        4096,
    );

    assert_eq!(next.disconnect_count, 1);
    assert_eq!(next.first_disconnect_at_ms, next.disconnected_at_ms);
}

#[tokio::test]
async fn health_snapshot_surfaces_flapping_after_repeated_reconnects() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    for protocol in ["rtmp", "rtmp"] {
        engine
            .try_register_ingest("p1", "key", protocol)
            .await
            .expect("ingest registration should succeed");
        engine
            .record_ingest_disconnect(
                "p1",
                Some("disconnect"),
                Some("publisher disconnected".to_string()),
                false,
            )
            .await;
        engine.unregister_ingest("p1").await;
    }

    let off_snapshot =
        test_health_snapshot_with_disconnect_grace(&engine, &pipelines, &HashMap::new(), 5_000)
            .await;
    let off_input = &off_snapshot["pipelines"]["p1"]["input"];
    assert_eq!(off_input["recentDisconnectCount"], 2);
    assert_eq!(off_input["flapping"], true);

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .expect("reconnect registration should succeed");

    let on_snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let on_input = &on_snapshot["pipelines"]["p1"]["input"];
    assert_eq!(on_input["status"], "on");
    assert_eq!(on_input["recentDisconnectCount"], 2);
    assert_eq!(on_input["flapping"], true);
    assert!(on_input["lastSessionProtocol"].is_null());
    assert!(on_input["lastDisconnectReason"].is_null());
    assert!(on_input["lastFailurePhase"].is_null());
    assert!(on_input["lastDisconnectAt"].is_null());
    assert!(on_input["lastDisconnectAgeMs"].is_null());
}

#[tokio::test]
async fn unregister_ingest_preserves_recent_snapshot_without_explicit_error() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta("p1", None, None, Some("127.0.0.1:7000".to_string()))
        .await;
    engine.update_ingest_bytes("p1", 8192).await;

    engine.unregister_ingest("p1").await;

    assert!(
        engine.has_recent_ingest_disconnect("p1", 1_000).await,
        "plain unregister should still leave a recent disconnect marker for grace handling"
    );

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snap["pipelines"]["p1"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["probeStatus"], "off");
    assert_eq!(input["lastSessionProtocol"], "rtmp");
    assert!(input["lastDisconnectAt"].is_string());
    assert!(input["lastDisconnectAgeMs"].as_u64().is_some());
    assert_eq!(input["recentDisconnectError"], false);
    assert_eq!(input["disconnectGraceActive"], false);
    assert!(input["disconnectGraceRemainingMs"].is_null());
    assert_eq!(input["lastRemoteAddr"], "127.0.0.1:7000");
    assert_eq!(input["lastSessionBytesReceived"], 8192);
    assert!(input["lastDisconnectReason"].is_null());
    assert!(input["lastFailurePhase"].is_null());
}

#[tokio::test]
async fn re_register_ingest_clears_recent_disconnect_details() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .record_ingest_disconnect(
            "p1",
            Some("receive"),
            Some("connection reset by peer".to_string()),
            true,
        )
        .await;
    engine.unregister_ingest("p1").await;

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["probeStatus"], "failed");
    assert_eq!(
        snap["pipelines"]["p1"]["input"]["lastDisconnectReason"],
        "connection reset by peer"
    );

    engine
        .try_register_ingest("p1", "key", "srt")
        .await
        .unwrap();
    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "on");
    assert!(snap["pipelines"]["p1"]["input"]["lastSessionProtocol"].is_null());
    assert!(snap["pipelines"]["p1"]["input"]["lastDisconnectReason"].is_null());
    assert_eq!(
        snap["pipelines"]["p1"]["input"]["disconnectGraceActive"],
        false
    );
    assert!(snap["pipelines"]["p1"]["input"]["disconnectGraceRemainingMs"].is_null());
}

#[tokio::test]
async fn double_register_ingest_rejected() {
    let engine = MediaEngine::new();
    let first = engine.try_register_ingest("p1", "key", "rtmp").await;
    assert!(first.is_some());

    let second = engine.try_register_ingest("p1", "key2", "srt").await;
    assert!(
        second.is_none(),
        "second register must be rejected while first is active"
    );
}

#[tokio::test]
async fn re_register_ingest_after_unregister() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    let t1 = engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine.unregister_ingest("p1").await;
    assert!(t1.is_cancelled());

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "off");

    let t2 = engine.try_register_ingest("p1", "key", "srt").await;
    assert!(t2.is_some(), "re-register after unregister must succeed");

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "on");
    assert_eq!(
        snap["pipelines"]["p1"]["input"]["publisher"]["protocol"],
        "srt"
    );
}

// ── fault resilience: egress error transitions ─────────────────────

#[tokio::test]
async fn egress_error_during_sending_transitions_to_failed() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 5000).await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["phase"], "sending");

    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
}

#[tokio::test]
async fn egress_cleaned_up_after_unregister() {
    let engine = MediaEngine::new();
    let token = engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 2048).await;

    assert!(
        crate::api_runtime_views::output_status(&engine, "out-1")
            .await
            .is_some()
    );

    engine.unregister_egress("out-1").await;
    assert!(token.is_cancelled());
    assert!(
        crate::api_runtime_views::output_status(&engine, "out-1")
            .await
            .is_some(),
        "output_status must preserve the last classified egress state after unregister"
    );
    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "stopped");
    assert_eq!(status["rawStatus"], "stopped");
    assert_eq!(status["phase"], "stopped");
    assert_eq!(status["bytesOut"], 2048);
    assert_eq!(status["totalSize"], 2048);
    assert!(status["endedAt"].is_string());
}

#[tokio::test]
async fn recent_egress_failure_survives_unregister_and_preserves_error_fields() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 2048).await;
    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;

    engine.unregister_egress("out-1").await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "failed");
    assert_eq!(status["rawStatus"], "running");
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
    assert_eq!(status["bytesOut"], 2048);
    assert_eq!(status["totalSize"], 2048);
    assert!(status["lastErrorAt"].is_string());
    assert!(status["endedAt"].is_string());
    assert!(status["endedAgeMs"].as_u64().is_some());
}

#[tokio::test]
async fn health_snapshot_keeps_recent_egress_status_visible_after_unregister() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipe-1".to_string();
    engine.get_or_create_pipeline(&pipeline_id).await;
    engine
        .register_egress(
            "out-1",
            &pipeline_id,
            "srt://example.com:10080?streamid=live/test",
        )
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine
        .record_egress_error("out-1", "connect", "connection failed")
        .await;

    engine.unregister_egress("out-1").await;

    let snapshot = test_health_snapshot(&engine, &[pipeline_id], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "connect");
    assert_eq!(output["lastError"], "connection failed");
    assert!(output["endedAt"].is_string());
}

#[tokio::test]
async fn health_snapshot_marks_cleanly_unregistered_egress_stopped() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipe-1".to_string();
    engine.get_or_create_pipeline(&pipeline_id).await;
    engine
        .register_egress("out-1", &pipeline_id, "rtmp://127.0.0.1/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 4096).await;

    engine.unregister_egress("out-1").await;

    let snapshot = test_health_snapshot(&engine, &[pipeline_id], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "stopped");
    assert_eq!(output["rawStatus"], "stopped");
    assert_eq!(output["phase"], "stopped");
    assert_eq!(output["totalSize"], 4096);
    assert!(output["endedAt"].is_string());
}

#[tokio::test]
async fn re_register_egress_clears_recent_snapshot() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "connect", "connection refused")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;
    assert!(engine.recent_egress_outcome("out-1").await.is_some());
    assert!(engine.egress_retry_state("out-1").await.is_some());

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;

    let recent = engine
        .recent_egress_outcome("out-1")
        .await
        .expect("recent failure window should stay visible across restart");
    assert_eq!(recent.failure_count, 1);
    assert!(engine.egress_retry_state("out-1").await.is_none());
}

#[tokio::test]
async fn late_retry_state_update_is_ignored_after_output_restarts() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;
    engine.unregister_egress("out-1").await;

    // Simulate the reconciler starting a fresh output session before the
    // old task's cleanup path gets to publish its retry backoff state.
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;

    assert!(engine.egress_retry_state("out-1").await.is_none());

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "running");
    assert_eq!(status["retrying"], false);
    assert!(status["retryAttempts"].is_null());
    assert!(status["retryBackoffMs"].is_null());
    assert!(status["retryRemainingMs"].is_null());
}

#[tokio::test]
async fn repeated_late_retry_updates_cannot_poison_newest_output_attempt() {
    let engine = MediaEngine::new();

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "attempt 1 failed")
        .await;
    engine.unregister_egress("out-1").await;

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .update_egress_retry_state("out-1", 1, 10_000, 8_000)
        .await;
    assert!(
        engine.egress_retry_state("out-1").await.is_none(),
        "the first stale retry publication must be ignored once a replacement attempt is active"
    );

    engine
        .record_egress_error("out-1", "connect", "attempt 2 failed")
        .await;
    engine.unregister_egress("out-1").await;

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 4096).await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;
    engine
        .update_egress_retry_state("out-1", 3, 40_000, 35_000)
        .await;

    assert!(
        engine.egress_retry_state("out-1").await.is_none(),
        "stale retry publications from any older attempt must not reattach retry state"
    );
    assert!(
        engine.recent_egress_outcome("out-1").await.is_some(),
        "the newest active attempt should retain the recent failure window for flapping visibility"
    );

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "running");
    assert_eq!(status["phase"], "sending");
    assert_eq!(status["bytesOut"], 4096);
    assert_eq!(status["retrying"], false);
    assert!(status["retryAttempts"].is_null());
    assert!(status["retryBackoffMs"].is_null());
    assert!(status["retryRemainingMs"].is_null());
    assert!(status["lastError"].is_null());
    assert!(status["failurePhase"].is_null());
    assert_eq!(status["recentFailureCount"], 2);
    assert_eq!(status["flapping"], true);
}

#[tokio::test]
async fn concurrent_retry_publish_cannot_outlive_a_racing_registration() {
    let engine = Arc::new(MediaEngine::new());

    // Hold cancel_tokens open so a concurrent retry-state publish can
    // observe "not active" and a fresh registration can clear+block right
    // behind it, then release in a controlled order. This reproduces the
    // exact interleaving a slow WaitRetry publish can race against a
    // reconciler-driven StartNow for the same output.
    let cancel_tokens_write = engine.egresses.cancel_tokens.write().await;

    let retry_engine = engine.clone();
    let retry_task = tokio::spawn(async move {
        retry_engine
            .update_egress_retry_state("out-1", 1, 10_000, 8_000)
            .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let register_engine = engine.clone();
    let register_task = tokio::spawn(async move {
        register_engine
            .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", None)
            .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    drop(cancel_tokens_write);
    retry_task
        .await
        .expect("retry publish task should not panic");
    register_task.await.expect("register task should not panic");

    assert!(
        engine.egress_retry_state("out-1").await.is_none(),
        "a retry publish racing a fresh registration must not leave stale retry state \
         behind once the registration has completed"
    );
}

#[tokio::test]
async fn build_recent_egress_outcome_resets_flap_streak_outside_window() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "attempt 1 failed")
        .await;
    engine.unregister_egress("out-1").await;

    let previous = engine
        .recent_egress_outcome("out-1")
        .await
        .expect("recent egress outcome");
    let expired = RecentEgressOutcome {
        ended_at_ms: MediaEngine::now_epoch_ms() - EGRESS_FLAP_WINDOW_MS - 1,
        ..previous
    };

    engine
        .register_egress("out-2", "pipe-1", "rtmp://127.0.0.1:1935/live/other")
        .await;
    engine
        .record_egress_error("out-2", "connect", "attempt 2 failed")
        .await;
    let next = {
        let egresses = engine.egresses.active.read().await;
        let active = egresses.get("out-2").expect("active egress should exist");
        MediaEngine::build_recent_egress_outcome(Some(&expired), active, true, false)
    };

    assert_eq!(next.failure_count, 1);
    assert_eq!(next.first_failure_at_ms, next.ended_at_ms);
}

#[tokio::test]
async fn health_snapshot_surfaces_flapping_after_repeated_egress_recoveries() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "key01_recent_egress_flapping", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "attempt 1 failed")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "connect", "attempt 2 failed")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 4096).await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "running");
    assert!(status["lastError"].is_null());
    assert_eq!(status["recentFailureCount"], 2);
    assert_eq!(status["flapping"], true);

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "running");
    assert_eq!(output["recentFailureCount"], 2);
    assert_eq!(output["flapping"], true);
}

#[tokio::test]
async fn output_status_surfaces_retry_backoff_after_failure() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "key01_retry_engine", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "retrying");
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
    assert_eq!(status["retrying"], true);
    assert_eq!(status["retryAttempts"], 2);
    assert_eq!(status["retryBackoffMs"], 20_000);
    assert!(status["nextRetryAt"].is_string());
    assert!(status["retryRemainingMs"].as_u64().unwrap_or(0) > 0);

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "retrying");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
    assert_eq!(output["lastError"], "connection reset by peer");
    assert_eq!(output["retrying"], true);
    assert_eq!(output["retryAttempts"], 2);
    assert_eq!(output["retryBackoffMs"], 20_000);
    assert!(output["nextRetryAt"].is_string());
    assert!(output["retryRemainingMs"].as_u64().unwrap_or(0) > 0);
}

proptest! {
    #[test]
    fn prop_ingest_lifecycle_preserves_health_invariants(
        actions in proptest::collection::vec(ingest_lifecycle_action_strategy(), 1..64)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async move {
            let engine = MediaEngine::new();
            let pipeline_id = "pipe-1".to_string();
            let mut model = IngestLifecycleModel::default();

            for action in actions {
                match action {
                    IngestLifecycleAction::Register { protocol } => {
                        let registered =
                            engine.try_register_ingest("pipe-1", "prop-ingest-key", protocol).await;
                        if registered.is_some() {
                            model.active = true;
                            model.protocol = Some(protocol);
                            model.remote_addr = None;
                            model.bytes_received = 0;
                        }
                    }
                    IngestLifecycleAction::UpdateRemoteAddr(remote_addr) => {
                        engine
                            .update_ingest_meta(
                                "pipe-1",
                                None,
                                None,
                                remote_addr.map(str::to_string),
                            )
                            .await;
                        if model.active && remote_addr.is_some() {
                            model.remote_addr = remote_addr;
                        }
                    }
                    IngestLifecycleAction::RecordBytes(bytes) => {
                        engine.update_ingest_bytes("pipe-1", bytes).await;
                        if model.active {
                            model.bytes_received += bytes;
                        }
                    }
                    IngestLifecycleAction::DisconnectAndUnregister {
                        phase,
                        message,
                        had_error,
                    } => {
                        engine
                            .record_ingest_disconnect(
                                "pipe-1",
                                phase,
                                message.map(str::to_string),
                                had_error,
                            )
                            .await;
                        if model.active {
                            model.recent_visible = true;
                            model.recent_protocol = model.protocol.take();
                            model.recent_remote_addr = model.remote_addr.take();
                            model.recent_bytes_received = std::mem::take(&mut model.bytes_received);
                            model.recent_phase = phase;
                            model.recent_message = message;
                            model.recent_had_error = had_error;
                            model.recent_disconnect_count =
                                model.recent_disconnect_count.saturating_add(1);
                            model.active = false;
                        }
                        engine.unregister_ingest("pipe-1").await;
                    }
                    IngestLifecycleAction::Unregister => {
                        engine.unregister_ingest("pipe-1").await;
                        if model.active {
                            model.active = false;
                            if !model.recent_visible {
                                model.recent_visible = true;
                                model.recent_protocol = model.protocol.take();
                                model.recent_remote_addr = model.remote_addr.take();
                                model.recent_bytes_received =
                                    std::mem::take(&mut model.bytes_received);
                                model.recent_phase = None;
                                model.recent_message = None;
                                model.recent_had_error = false;
                                model.recent_disconnect_count = 1;
                            } else {
                                model.protocol = None;
                                model.remote_addr = None;
                                model.bytes_received = 0;
                            }
                        }
                    }
                }

                let plain_snapshot =
                    test_health_snapshot(&engine, std::slice::from_ref(&pipeline_id), &HashMap::new())
                        .await;
                let grace_snapshot = test_health_snapshot_with_disconnect_grace(
                    &engine,
                    std::slice::from_ref(&pipeline_id),
                    &HashMap::new(),
                    5_000,
                )
                .await;
                let plain_input = &plain_snapshot["pipelines"]["pipe-1"]["input"];
                let grace_input = &grace_snapshot["pipelines"]["pipe-1"]["input"];

                assert_ingest_lifecycle_invariants(&model, plain_input, grace_input);
            }
        });
    }

    #[test]
    fn prop_egress_lifecycle_preserves_runtime_and_health_invariants(
        actions in proptest::collection::vec(egress_lifecycle_action_strategy(), 1..64)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async move {
            let engine = MediaEngine::new();
            engine
                .try_register_ingest("pipe-1", "prop-egress-key", "rtmp")
                .await
                .expect("ingest registration should succeed");
            let mut model = EgressLifecycleModel::default();

            for action in actions {
                match action {
                    EgressLifecycleAction::Register => {
                        engine
                            .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
                            .await;
                        model = EgressLifecycleModel {
                            active: true,
                            recent_visible: model.recent_visible,
                            retry_visible: false,
                            bytes_sent: 0,
                            phase: "starting",
                            last_error: None,
                            retry_attempts: None,
                            retry_backoff_ms: None,
                        };
                    }
                    EgressLifecycleAction::RecordError { phase, message } => {
                        engine.record_egress_error("out-1", phase, message).await;
                        if model.active {
                            model.phase = "failed";
                            model.last_error = Some((phase, message));
                        }
                    }
                    EgressLifecycleAction::RecordProgress(bytes) => {
                        engine.record_egress_progress("out-1", bytes).await;
                        if model.active {
                            model.bytes_sent += bytes;
                            model.phase = "sending";
                            model.last_error = None;
                        }
                    }
                    EgressLifecycleAction::Unregister => {
                        engine.unregister_egress("out-1").await;
                        if model.active {
                            model.active = false;
                            model.recent_visible = true;
                        }
                    }
                    EgressLifecycleAction::RetryState {
                        attempts,
                        backoff_ms,
                        remaining_ms,
                    } => {
                        engine
                            .update_egress_retry_state("out-1", attempts, backoff_ms, remaining_ms)
                            .await;
                        if model.active {
                            model.retry_visible = false;
                            model.retry_attempts = None;
                            model.retry_backoff_ms = None;
                        } else {
                            model.retry_visible = true;
                            model.retry_attempts = Some(attempts);
                            model.retry_backoff_ms = Some(backoff_ms);
                        }
                    }
                    EgressLifecycleAction::ClearRetry => {
                        engine.clear_egress_retry_state("out-1").await;
                        model.retry_visible = false;
                        model.retry_attempts = None;
                        model.retry_backoff_ms = None;
                    }
                }

                let status = crate::api_runtime_views::output_status(&engine, "out-1").await;
                let snapshot =
                    test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new())
                        .await;
                let snapshot_output = snapshot["pipelines"]["pipe-1"]["outputs"].get("out-1");
                let recent = engine.recent_egress_outcome("out-1").await;
                let retry = engine.egress_retry_state("out-1").await;

                assert_egress_lifecycle_invariants(
                    &model,
                    status.as_ref(),
                    snapshot_output,
                    recent.as_ref(),
                    retry.as_ref(),
                );
            }
        });
    }
}

// ── adaptive ring sizing ──────────────────────────────────────────────────

#[tokio::test]
async fn adapt_pipeline_ring_no_op_when_default_is_sufficient() {
    // 1080p30 + 1 audio = 80 pkt/s → needed = ceil(80 × 6) = 480 < default 1024
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let result = engine.adapt_pipeline_ring("p", 30.0, 1).await;
    assert!(
        result.is_none(),
        "no resize needed for single-track 1080p30"
    );

    let ring = engine.get_or_create_pipeline("p").await;
    assert_eq!(ring.capacity(), engine.config.ring_capacity);
    let depth = ring.buffer_depth_secs().unwrap();
    assert!((12.0..=13.0).contains(&depth), "depth={depth}");
}

#[tokio::test]
async fn source_ring_uses_engine_typed_config_capacity() {
    let config = Arc::new(crate::AppConfig {
        ring_capacity: 2048,
        ..Default::default()
    });
    let engine = MediaEngine::new_with_config(config);

    let ring = engine.get_or_create_pipeline("typed-ring").await;
    assert_eq!(ring.capacity(), 2048);
}

#[tokio::test]
async fn ts_muxer_ring_uses_engine_typed_config_capacity() {
    let config = Arc::new(crate::AppConfig {
        ts_ring_capacity: 96,
        ..Default::default()
    });
    let engine = Arc::new(MediaEngine::new_with_config(config));
    let source = Arc::new(RingBuffer::new(16));

    let ts_ring = engine
        .get_or_create_ts_muxer_stage("typed-ts", "source", source)
        .await;

    assert_eq!(ts_ring.ring.capacity(), 96);
    ts_ring.cancel.cancel();
}

#[tokio::test]
async fn adapt_pipeline_ring_resizes_for_multi_track_stream() {
    // 2v16a: 30 fps + 16 audio × 50 = 830 pkt/s → needed = ceil(830 × 6) = 4980
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("ring must be resized for 830 pkt/s");

    assert_eq!(new_ring.capacity(), 4980);
    let depth = new_ring.buffer_depth_secs().unwrap();
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
    assert_eq!(engine.get_or_create_pipeline("p").await.capacity(), 4980);
}

#[tokio::test]
async fn adapt_pipeline_ring_4k60_single_audio_no_resize() {
    // 4K 60fps + 1 audio = 110 pkt/s → needed = 660 < default 1024
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let result = engine.adapt_pipeline_ring("p", 60.0, 1).await;
    assert!(
        result.is_none(),
        "default 1024 already covers 4K60 single-track"
    );
}

#[tokio::test]
async fn adapt_pipeline_ring_4k60_multi_audio_resizes() {
    // 4K 60fps + 16 audio = 860 pkt/s → needed = ceil(860 × 6) = 5160
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let new_ring = engine
        .adapt_pipeline_ring("p", 60.0, 16)
        .await
        .expect("resize needed for 4K60 + 16 audio");

    assert_eq!(new_ring.capacity(), 5160);
    let depth = new_ring.buffer_depth_secs().unwrap();
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
}

#[tokio::test]
async fn get_or_create_pipeline_preserves_adapted_ring_across_calls() {
    // The adapted ring must be returned by all subsequent get_or_create_pipeline
    // calls so egress readers and TS mux stages attach to the correctly-sized ring.
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;
    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("should resize for 830 pkt/s");
    assert_eq!(new_ring.capacity(), 4980);

    let ring2 = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring2.capacity(),
        4980,
        "adapted ring must persist across calls"
    );

    let _reader = crate::media::ring_buffer::Reader::new("hold".to_string(), ring2.clone());
    let ring3 = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring3.capacity(),
        4980,
        "ring must not change with active reader"
    );
}

#[tokio::test]
async fn adapt_pipeline_ring_lighter_republish_updates_rate_not_capacity() {
    // A lighter re-publish (1v1a after 2v16a) does not shrink the ring —
    // it just updates estimated_pkt_rate so bufferDepthSecs is correct.
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;
    engine.adapt_pipeline_ring("p", 30.0, 16).await; // → 4980 for 830 pkt/s

    // Lighter re-publish: 1v1a = 80 pkt/s → needed = 480 < 4980 → no resize.
    let result = engine.adapt_pipeline_ring("p", 30.0, 1).await;
    assert!(
        result.is_none(),
        "no resize when ring is already large enough"
    );

    let ring = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring.capacity(),
        4980,
        "capacity preserved from heavier session"
    );
    let depth = ring.buffer_depth_secs().unwrap();
    // telemetry now reflects the lighter stream's real depth: 4980/80 ≈ 62 s
    assert!(depth > 60.0, "4980/80 ≈ 62.3 s; got {depth}");
}

#[tokio::test]
async fn adapt_pipeline_ring_preserves_codec_and_track_metadata() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("p").await;
    ring.set_codec_hint("hevc");
    ring.set_video_parameter_sets(vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01]);
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: Some("stereo".to_string()),
        track_index: 3,
        pid: Some(257),
        language: Some("eng".to_string()),
        title: Some("Program".to_string()),
        profile: Some("LC".to_string()),
    }]);

    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("ring must be resized for metadata preservation proof");

    assert_eq!(new_ring.codec_hint_str(), "hevc");
    assert_eq!(
        new_ring.video_parameter_sets(),
        Some(vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01])
    );
    let tracks = new_ring
        .audio_tracks()
        .expect("resized ring should preserve audio tracks");
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].codec, "aac");
    assert_eq!(tracks[0].sample_rate, 48_000);
    assert_eq!(tracks[0].channels, 2);
    assert_eq!(tracks[0].track_index, 3);
    assert_eq!(tracks[0].pid, Some(257));
    assert_eq!(tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(tracks[0].title.as_deref(), Some("Program"));
    assert_eq!(tracks[0].profile.as_deref(), Some("LC"));
}

#[tokio::test]
async fn health_input_protocol_matches_registration() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    for proto in ["rtmp", "srt", "file"] {
        engine
            .try_register_ingest("p1", "key", proto)
            .await
            .unwrap();
        let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
        assert_eq!(
            snap["pipelines"]["p1"]["input"]["publisher"]["protocol"], proto,
            "protocol mismatch for {proto}"
        );
        engine.unregister_ingest("p1").await;
    }
}
