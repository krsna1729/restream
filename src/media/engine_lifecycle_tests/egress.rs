use super::*;

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
