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
