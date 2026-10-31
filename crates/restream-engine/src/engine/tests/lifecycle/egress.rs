use super::*;

// ── fault resilience: egress error transitions ─────────────────────

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
