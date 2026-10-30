use super::*;

#[tokio::test]
async fn unprobed_media_opens_the_start_gate_only_after_the_probe_grace_from_its_first_byte() {
    use std::sync::atomic::Ordering;
    let engine = MediaEngine::new();
    assert_eq!(engine.ingest_tracks_ready("p1").await, None);
    engine
        .try_register_ingest("p1", "key", "srt")
        .await
        .unwrap();
    let first_media_ms = engine.ingests.active.read().await["p1"]
        .first_media_ms
        .clone();
    assert_eq!(engine.ingest_tracks_ready("p1").await, Some(false));

    engine.update_ingest_bytes("p1", 1316).await;
    let anchor = first_media_ms.load(Ordering::Relaxed);
    assert_ne!(anchor, 0, "the first media byte anchors the grace");
    assert_eq!(
        engine.ingest_tracks_ready("p1").await,
        Some(false),
        "media without a probe waits inside the grace"
    );

    // Just inside the bound, then at it; later bytes never move the anchor.
    first_media_ms.store(
        MediaEngine::now_epoch_ms()
            - (crate::media::engine_ingest_metadata::PROBE_GRACE_MS - 1_000),
        Ordering::Relaxed,
    );
    engine.update_ingest_bytes("p1", 1316).await;
    assert_eq!(engine.ingest_tracks_ready("p1").await, Some(false));
    first_media_ms.store(
        MediaEngine::now_epoch_ms() - crate::media::engine_ingest_metadata::PROBE_GRACE_MS,
        Ordering::Relaxed,
    );
    engine.update_ingest_bytes("p1", 1316).await;
    assert_eq!(
        engine.ingest_tracks_ready("p1").await,
        Some(true),
        "a source that never probes starts track-less after the grace"
    );
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
