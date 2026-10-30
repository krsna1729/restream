use super::*;

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
