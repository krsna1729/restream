use super::*;
use restream_engine::ingest_lease::IngestEnd;

#[tokio::test]
async fn stale_ingest_disconnect_cannot_poison_replacement_attempt() {
    let engine = Arc::new(MediaEngine::new());
    let pipelines = vec!["pipeline-race".to_string()];

    let first = engine
        .try_register_ingest("pipeline-race", "stream-key", "rtmp")
        .await
        .expect("first ingest should register");
    engine.unregister_ingest("pipeline-race").await;

    let replacement = engine
        .try_register_ingest("pipeline-race", "stream-key-2", "srt")
        .await
        .expect("replacement ingest should register");

    first
        .finish(IngestEnd::Disconnected {
            phase: "receive",
            reason: "stale disconnect".to_string(),
            had_error: true,
        })
        .await;
    let snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(
        snapshot["pipelines"]["pipeline-race"]["input"]["status"], "on",
        "a stale attempt's end must not release the replacement attempt"
    );

    replacement
        .finish(IngestEnd::Disconnected {
            phase: "disconnect",
            reason: "replacement disconnect".to_string(),
            had_error: false,
        })
        .await;

    let snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snapshot["pipelines"]["pipeline-race"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["lastSessionProtocol"], "srt");
    assert_eq!(input["lastDisconnectReason"], "replacement disconnect");
    assert_eq!(input["lastFailurePhase"], "disconnect");
    assert_eq!(input["recentDisconnectError"], false);
}
