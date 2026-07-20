use super::*;

#[tokio::test]
async fn pipeline_graph_returns_dag() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Create a pipeline first
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"graph-test","streamKey":"gkey"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipeline = body_json(resp).await;
    let pid = pipeline["pipeline"]["id"].as_str().unwrap();

    // Get the graph (no active ingests/egresses, should still return structure)
    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{}/graph", pid),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let graph = body_json(resp).await;
    assert!(graph["nodes"].is_array());
    assert!(graph["edges"].is_array());
    assert_eq!(graph["desiredGraph"]["pipelineId"], pid);
    assert!(graph["desiredGraph"]["stages"].is_array());
    assert!(graph["desiredGraph"]["edges"].is_array());
    assert!(graph["desiredOutputGraphs"].is_array());
    assert!(graph["runtimeGraph"]["nodes"].is_array());
    assert!(graph["runtimeGraph"]["edges"].is_array());
    // Source ring buffer node should always be present
    let nodes = graph["nodes"].as_array().unwrap();
    assert!(nodes.iter().any(|n| n["type"] == "ring_buffer"));
}

#[tokio::test]
async fn pipeline_graph_stage_nodes_include_lifecycle_details() {
    let (app, pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    db::create_pipeline(
        &pool,
        "pipe-graph-life",
        "Graph Life",
        "graph-life-key",
        None,
        None,
    )
    .await
    .unwrap();
    db::create_output(
        &pool,
        "out-graph-life",
        "pipe-graph-life",
        "RTMP 720p",
        "rtmp://example.test/live/graph-life",
        None,
        DesiredOutputState::Running,
        &OutputConfig::preset("720p"),
    )
    .await
    .unwrap();

    engine
        .try_register_ingest("pipe-graph-life", "graph-life-key", "rtmp")
        .await
        .unwrap();
    let stage_key = StageKey::new(
        "pipe-graph-life",
        StageKind::video_preset_with_codec("720p", "h264"),
    );
    let manager = restream::media::stage_runtime::StageRuntimeManager::new(engine);
    let (handle, _) = manager
        .ensure_stage(
            stage_key.clone(),
            Arc::new(restream::media::ring_buffer::RingBuffer::new(8)),
            None,
        )
        .await;
    handle.lifecycle.transition(
        restream::media::stage_lifecycle::StagePhase::WaitingForCapacity {
            backend: restream::media::stage_lifecycle::StageBackendKind::ExternalFfmpeg,
        },
    );

    let resp = app
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/pipe-graph-life/graph",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let graph = body_json(resp).await;
    let stage_node = graph["runtimeGraph"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["stageKey"] == stage_key.kind.to_string())
        .expect("runtime graph should expose the video stage node");

    assert_eq!(stage_node["details"]["phase"], "waitingForCapacity");
    assert_eq!(stage_node["details"]["backend"], "externalFfmpeg");
    assert!(stage_node["details"]["capacityWaitMs"].is_u64());
}

#[tokio::test]
async fn pipeline_diagnostics_context_returns_causal_bundle() {
    let (app, pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    db::create_pipeline(
        &pool,
        "pipe-diagctx",
        "Diag Context",
        "diagctx-key",
        None,
        None,
    )
    .await
    .unwrap();
    db::create_output(
        &pool,
        "out-diagctx",
        "pipe-diagctx",
        "RTMP 720p",
        "rtmp://example.test/live/diagctx",
        None,
        DesiredOutputState::Running,
        &OutputConfig::preset("720p"),
    )
    .await
    .unwrap();
    db::create_output(
        &pool,
        "out-diagctx-hls",
        "pipe-diagctx",
        "HLS Upload",
        "https://upload.example.test/live/out.m3u8",
        None,
        DesiredOutputState::Running,
        &OutputConfig::source(),
    )
    .await
    .unwrap();
    db::append_app_log_batch(
        &pool,
        &[AppLogEntry {
            ts: "2026-07-09T00:00:00Z".to_string(),
            level: "WARN".to_string(),
            target: "restream::media::external_transcoder".to_string(),
            message: "[ext-transcoder] ffmpeg stderr (video:720p): synthetic warning".to_string(),
            fields: None,
            pipeline_id: Some("pipe-diagctx".to_string()),
            output_id: None,
            event_type: Some("stage.stderr".to_string()),
            event_class: Some("lifecycle".to_string()),
        }],
    )
    .await
    .unwrap();
    engine
        .runtime
        .event_log
        .emit(restream::events::EventKind::StageRegistered {
            pipeline_id: "pipe-diagctx".to_string(),
            encoding: "video:720p".to_string(),
        });

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/pipe-diagctx/diagnostics/context",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    assert_eq!(body["pipelineId"], "pipe-diagctx");
    assert_eq!(body["graph"]["desired"]["pipelineId"], "pipe-diagctx");
    assert!(body["graph"]["desired"]["stages"].as_array().unwrap().len() >= 2);
    let desired_outputs = body["graph"]["desiredOutputs"].as_array().unwrap();
    assert!(desired_outputs.iter().any(|graph| {
        graph["role"]["kind"] == "hlsOutput" && graph["role"]["outputId"] == "out-diagctx-hls"
    }));
    assert!(body["graph"]["runtime"]["nodes"].is_array());
    assert!(body["health"]["pipelines"]["pipe-diagctx"].is_object());
    assert!(body["alerts"].is_array());
    assert_eq!(body["recentEvents"].as_array().unwrap().len(), 1);
    assert_eq!(body["recentLogs"].as_array().unwrap().len(), 1);
    assert_eq!(body["backendStderrTail"].as_array().unwrap().len(), 1);
    assert!(
        body["backendStderrTail"][0]["message"]
            .as_str()
            .unwrap()
            .contains("synthetic warning")
    );
}

#[tokio::test]
async fn diagnostics_requires_active_ingest() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines/inactive/diagnostics/run")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/inactive/diagnostics/run",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let wrong_method = app
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/inactive/diagnostics/run",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn diagnostics_supports_active_file_ingest() {
    let (app, cookie, media_dir, pool, engine) =
        authenticated_app_with_temp_media_and_engine().await;

    db::create_pipeline(
        &pool,
        "pipe-file-diag",
        "File Diagnostics",
        "file-diag-key",
        Some("file"),
        None,
    )
    .await
    .unwrap();
    db::create_ingest(
        &pool,
        "ingest-file-diag",
        "source.mp4",
        "file-diag-key",
        true,
        "00:00:02",
        false,
        2,
    )
    .await
    .unwrap();

    std::fs::copy(
        restream::test_fixtures::sparse_gop_mp4_fixture().unwrap(),
        media_dir.join("source.mp4"),
    )
    .unwrap();

    engine
        .try_register_ingest("pipe-file-diag", "file-diag-key", "file")
        .await
        .expect("file ingest should register");

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/pipe-file-diag/diagnostics/run",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );

    let body: serde_json::Value = serde_json::from_slice(&body_bytes(resp).await).unwrap();
    assert_eq!(body["protocol"], "file");
    assert!(body["totalDurationMs"].is_number());
    let checks = body["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 9);
    assert_eq!(checks[0]["index"], 0);
    assert_eq!(checks[8]["index"], 8);
    let names: Vec<_> = checks.iter().map(|check| &check["name"]).collect();
    assert!(names.contains(&&serde_json::json!("File Source")));
    assert!(names.contains(&&serde_json::json!("File Ingest Runtime")));
    assert!(names.contains(&&serde_json::json!("Preview & Recording")));
    assert!(!names.contains(&&serde_json::json!("Publisher Transport")));
    assert!(!names.contains(&&serde_json::json!("Network Bandwidth")));
    assert!(!names.contains(&&serde_json::json!("SRT Listener Socket")));

    let semaphore = engine.get_or_create_diag_semaphore("pipe-file-diag").await;
    let _held_permit = semaphore.try_acquire_owned().unwrap();
    let busy = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/pipe-file-diag/diagnostics/run",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    let busy_body: serde_json::Value = serde_json::from_slice(&body_bytes(busy).await).unwrap();
    assert_eq!(
        busy_body["error"],
        "A diagnostic is already running for this pipeline"
    );
}
