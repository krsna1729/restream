use super::*;

#[tokio::test]
async fn stale_ingest_disconnect_cannot_poison_replacement_attempt() {
    let engine = MediaEngine::new();

    let first = engine
        .try_register_ingest_attempt("pipeline-race", "stream-key", "rtmp")
        .await
        .expect("first ingest should register");
    engine.unregister_ingest("pipeline-race").await;

    let replacement = engine
        .try_register_ingest_attempt("pipeline-race", "stream-key-2", "srt")
        .await
        .expect("replacement ingest should register");

    assert!(
        !engine
            .record_ingest_disconnect_if_current(
                "pipeline-race",
                &first,
                Some("receive"),
                Some("stale disconnect".to_string()),
                true,
            )
            .await,
        "stale disconnect metadata must not attach to a replacement ingest attempt"
    );
    assert!(
        engine
            .record_ingest_disconnect_if_current(
                "pipeline-race",
                &replacement,
                Some("disconnect"),
                Some("replacement disconnect".to_string()),
                false,
            )
            .await,
        "current attempt should still be able to publish disconnect metadata"
    );
    assert!(
        engine
            .unregister_ingest_if_current("pipeline-race", &replacement)
            .await,
        "replacement attempt should be able to unregister cleanly"
    );

    let pipelines = vec!["pipeline-race".to_string()];
    let snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snapshot["pipelines"]["pipeline-race"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["lastSessionProtocol"], "srt");
    assert_eq!(input["lastDisconnectReason"], "replacement disconnect");
    assert_eq!(input["lastFailurePhase"], "disconnect");
    assert_eq!(input["recentDisconnectError"], false);
}
