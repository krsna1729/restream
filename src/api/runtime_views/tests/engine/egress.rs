use super::*;

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
async fn health_snapshot_includes_blocked_by_for_waiting_terminal_stage() {
    use restream_engine::stage_lifecycle::{StageBackendKind, StagePhase};

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
async fn sink_discard_progress_marks_active_without_network_bytes() {
    let engine = MediaEngine::new();
    let registration = engine
        .register_egress_attempt("sink-1", "pipe-1", "sink://local/discard", None)
        .await;
    engine
        .record_egress_error_if_current("sink-1", &registration, "discard", "temporary stall")
        .await;

    assert!(
        engine
            .record_egress_discard_progress_if_current("sink-1", &registration)
            .await
    );

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["sink-1"];
    assert_eq!(output["protocol"], "sink");
    assert_eq!(output["phase"], "discarding");
    assert_eq!(output["totalSize"], 0);
    assert!(output["failurePhase"].is_null());
    assert!(output["lastError"].is_null());
    assert!(!output["lastProgressAt"].is_null());
}

#[tokio::test]
async fn unplaced_egress_has_no_fabric_attribution() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["fabric"], false);
    assert!(output["shardId"].is_null());
}

#[tokio::test]
async fn egress_attribution_reflects_the_placed_shard_of_the_current_attempt() {
    let engine = MediaEngine::new();
    let stale = engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", None)
        .await;
    let current = engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", None)
        .await;
    let slot = |shard| restream_engine::engine::FabricSlot {
        feed: restream_engine::egress::command::FeedId::new("rtmp:pipe-1:source"),
        shard,
    };

    engine
        .set_egress_fabric_attribution("out-1", &current, slot(3))
        .await;
    engine
        .set_egress_fabric_attribution("out-1", &stale, slot(1))
        .await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["fabric"], true);
    assert_eq!(output["fabricFeed"], "rtmp:pipe-1:source");
    assert_eq!(
        output["shardId"], 3,
        "a replaced attempt does not overwrite it"
    );
}
