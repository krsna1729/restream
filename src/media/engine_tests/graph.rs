use super::*;

#[tokio::test]
async fn pipeline_create_and_remove() {
    let engine = MediaEngine::new();
    let rb1 = engine.get_or_create_pipeline("p1").await;
    let rb2 = engine.get_or_create_pipeline("p1").await;
    // Same pipeline returns same buffer
    assert!(Arc::ptr_eq(&rb1, &rb2));

    engine.remove_pipeline("p1").await;
    let rb3 = engine.get_or_create_pipeline("p1").await;
    // After removal, new buffer is created
    assert!(!Arc::ptr_eq(&rb1, &rb3));
}

#[tokio::test]
async fn health_snapshot_includes_egress_under_correct_pipeline() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-a", "pipe-1", "rtmp://a.com/live/key")
        .await;
    engine
        .register_egress("out-b", "pipe-2", "rtmp://b.com/live/key")
        .await;
    engine
        .register_egress("out-c", "pipe-1", "srt://c.com?streamid=key")
        .await;

    let ids = vec!["pipe-1".to_string(), "pipe-2".to_string()];
    let rec = std::collections::HashMap::new();
    let snap = test_health_snapshot(&engine, &ids, &rec).await;

    let pipe1_outputs = &snap["pipelines"]["pipe-1"]["outputs"];
    assert!(pipe1_outputs.get("out-a").is_some());
    assert!(pipe1_outputs.get("out-c").is_some());
    assert!(pipe1_outputs.get("out-b").is_none());

    let pipe2_outputs = &snap["pipelines"]["pipe-2"]["outputs"];
    assert!(pipe2_outputs.get("out-b").is_some());
    assert!(pipe2_outputs.get("out-a").is_none());
}

#[tokio::test]
async fn recording_lifecycle() {
    let engine = MediaEngine::new();
    assert!(!engine.is_recording_active("p1").await);

    let token = engine.register_recording("p1").await.unwrap();
    assert!(engine.is_recording_active("p1").await);
    assert!(!token.is_cancelled());

    engine.unregister_recording("p1").await;
    assert!(!engine.is_recording_active("p1").await);
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn concurrent_register_recording_calls_never_both_succeed() {
    // Two near-simultaneous recording-start requests for the same pipeline
    // (e.g. a double-click or a retried API call) must not both register a
    // token: the loser would otherwise silently orphan the winner's token
    // (leaking its writer thread/file handle with no way to cancel it) and
    // both would go on to race `std::fs::File::create` on the same output
    // filename. Exactly one of the two racing calls must receive `Some`.
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "p-racing-recorders";

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let engine = engine.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            engine.register_recording(pipeline_id).await
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    let successes = results.iter().filter(|r| r.is_some()).count();
    assert_eq!(
        successes, 1,
        "exactly one of two racing register_recording calls must succeed, got {successes}"
    );

    let winner = results.into_iter().flatten().next().unwrap();
    assert!(engine.is_recording_active(pipeline_id).await);
    winner.cancel();
    assert!(!engine.is_recording_active(pipeline_id).await);
}

#[tokio::test]
async fn cancelled_recording_token_is_not_active() {
    let engine = MediaEngine::new();
    let token = engine.register_recording("p-cancelled-rec").await.unwrap();

    assert!(engine.is_recording_active("p-cancelled-rec").await);
    token.cancel();

    assert!(
        !engine.is_recording_active("p-cancelled-rec").await,
        "cancelled recording token must not be reported as active"
    );
}

#[tokio::test]
async fn health_snapshot_marks_cancelled_recording_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-rec-cancelled";
    let token = engine.register_recording(pipeline_id).await.unwrap();
    token.cancel();

    let mut recording_enabled = HashMap::new();
    recording_enabled.insert(pipeline_id.to_string(), true);
    let snapshot =
        test_health_snapshot(&engine, &[pipeline_id.to_string()], &recording_enabled).await;

    assert_eq!(
        snapshot["pipelines"][pipeline_id]["recording"]["active"],
        false
    );
}

#[tokio::test]
async fn processing_graph_marks_cancelled_recording_and_hls_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-graph-cancelled";
    let rec_token = engine.register_recording(pipeline_id).await.unwrap();
    rec_token.cancel();

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    let hls_token = engine
        .get_hls_preview_cancel_token(pipeline_id)
        .await
        .unwrap();
    hls_token.cancel();

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[]).await;
    let nodes = graph["nodes"].as_array().unwrap();

    let recording = nodes
        .iter()
        .find(|node| node["type"] == "recording")
        .expect("recording node should remain visible while registered");
    assert_eq!(recording["active"], false);

    let hls = nodes
        .iter()
        .find(|node| node["type"] == "hls")
        .expect("HLS node should remain visible while its store exists");
    assert_eq!(hls["active"], false);
}

#[tokio::test]
async fn processing_graph_routes_srt_egress_through_ts_mux() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-srt-graph";
    let _source = engine.get_or_create_pipeline(pipeline_id).await;
    let output = crate::application::models::Output {
        id: "out-srt".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "SRT Target".to_string(),
        url: "srt://example.com:9000?streamid=publish:test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::source(),
    };

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[output]).await;
    let nodes = graph["nodes"].as_array().unwrap();
    let edges = graph["edges"].as_array().unwrap();

    assert!(
        nodes
            .iter()
            .any(|node| node["type"] == "demux" && node["label"] == "Demux/probe idle"),
        "graph should expose the ingest demux/probe boundary"
    );
    assert!(
        nodes
            .iter()
            .any(|node| node["type"] == "packetizer" && node["label"] == "MPEG-TS mux: source"),
        "SRT egress should expose MPEG-TS packetization"
    );
    assert!(
        edges.iter().any(|edge| edge["label"] == "SRT send"),
        "SRT egress should include an explicit sender edge"
    );
    assert!(
        !edges.iter().any(|edge| edge["label"] == "FLV passthrough"),
        "SRT egress must not be labeled as FLV passthrough"
    );
}

#[tokio::test]
async fn processing_graph_marks_failed_egress_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-failed-output-graph";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("out-failed", pipeline_id, "rtmp://example/live/test")
        .await;
    engine
        .record_egress_error("out-failed", "send", "connection refused")
        .await;

    let output = crate::application::models::Output {
        id: "out-failed".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Failed Target".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::source(),
    };

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[output]).await;
    let nodes = graph["nodes"].as_array().unwrap();
    let egress = nodes
        .iter()
        .find(|node| node["type"] == "egress")
        .expect("egress node");

    assert_eq!(egress["active"], false);
    assert_eq!(egress["details"]["status"], "failed");
    assert_eq!(egress["details"]["phase"], "failed");
    assert_eq!(egress["details"]["failurePhase"], "send");
}

#[tokio::test]
async fn processing_graph_omits_stale_codec_edge_when_output_no_longer_needs_it() {
    let engine = std::sync::Arc::new(MediaEngine::new());
    let pipeline_id = "pipeline-graph-stale-codec";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;

    let stale_source_output = crate::application::models::Output {
        id: "out-graph-stale-codec".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Graph RTMP".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::source(),
    };
    let _ = crate::application::egress::prepare_output_ring(&engine, &stale_source_output).await;

    let output = crate::application::models::Output {
        id: "out-graph-stale-codec".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Graph RTMP".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::preset("h264").with_audio(
            crate::domain::audio_routing::AudioRouting::SelectTracks { tracks: vec![1] },
        ),
    };

    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    // Re-prepare after the codec flip, as reconciliation would: this creates
    // the h264-only path while the stale HEVC-era stages stay registered.
    let _ = crate::application::egress::prepare_output_ring(&engine, &output).await;

    let stages = engine.active_transcoder_stages(pipeline_id).await;
    let stale_source = StageKind::codec_edge("hevc_to_h264", StageKind::source());
    assert!(
        stages
            .iter()
            .any(|(stage, live)| *live && *stage == stale_source),
        "test precondition: stale codec-edge stage should still exist in the engine registry"
    );

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[output]).await;
    let nodes = graph["nodes"].as_array().unwrap();

    assert!(
        nodes
            .iter()
            .any(|node| node["stageKey"] == "video:h264:codec:h264"),
        "current output path should still render its video stage"
    );
    assert!(
        nodes
            .iter()
            .any(|node| node["stageKey"] == "audio:atrack:1:from:video:h264:codec:h264"),
        "current output path should still render its audio routing stage"
    );
    assert!(
        !nodes
            .iter()
            .any(|node| node["stageKey"] == "hevc_to_h264:from:source"),
        "graph should omit stale codec-edge stages that no longer belong to the output path"
    );
}
