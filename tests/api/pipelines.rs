use super::*;

#[tokio::test]
async fn pipeline_crud_via_api() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Create
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"Test Pipeline","streamKey":"key01_6c71124cde80358ca7c13081"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let pipeline_id = json["pipeline"]["id"].as_str().unwrap().to_string();

    // List
    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/pipelines", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["pipelines"].as_array().unwrap().len(), 1);

    // Update
    let uri = format!("/api/v1/pipelines/{}", pipeline_id);
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            &uri,
            &cookie,
            Some(r#"{"name":"Updated Pipeline"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Delete
    let resp = app
        .clone()
        .oneshot(auth_req("DELETE", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn pipeline_create_generates_stream_key_when_omitted() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"Generated Key Pipeline"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let stream_key = json["pipeline"]["streamKey"].as_str().unwrap();
    assert!(stream_key.starts_with("sk_"));
    assert_eq!(stream_key.len(), 67);
    assert!(stream_key[3..].bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[tokio::test]
async fn duplicate_stream_keys_are_rejected() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p1", "P1", "unique-key", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P2","streamKey":"unique-key"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P2","streamKey":"unique-key-2"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/pipelines/p1",
            &cookie,
            Some(r#"{"name":"P1","streamKey":"unique-key-2"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn pipeline_logs_include_persisted_lifecycle_events_without_stream_keys() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    insert_app_log(
        &pool,
        AppLogEntry {
            ts: "2026-06-29T00:00:00Z".to_string(),
            level: "INFO".to_string(),
            target: "restream::lifecycle".to_string(),
            message: "RTMP publisher connected".to_string(),
            fields: Some(r#"{"protocol":"rtmp","pipelineId":"pipe-history"}"#.to_string()),
            pipeline_id: Some("pipe-history".to_string()),
            output_id: None,
            event_type: Some("ingest.connected".to_string()),
            event_class: Some("lifecycle".to_string()),
        },
    )
    .await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/logs?pipeline_id=pipe-history&event_class=lifecycle&limit=20",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["logs"][0]["pipelineId"], "pipe-history");
    assert_eq!(json["logs"][0]["eventType"], "ingest.connected");
    assert_eq!(json["logs"][0]["message"], "RTMP publisher connected");
    assert!(
        !serde_json::to_string(&json)
            .unwrap()
            .contains("secret-history-key")
    );
}

#[tokio::test]
async fn output_logs_lifecycle_filter_includes_persisted_egress_events() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    insert_app_log(
        &pool,
        AppLogEntry {
            ts: "2026-06-29T00:00:01Z".to_string(),
            level: "INFO".to_string(),
            target: "restream::lifecycle".to_string(),
            message: "Egress started".to_string(),
            fields: Some(r#"{"pipelineId":"pipe-history","outputId":"out-history"}"#.to_string()),
            pipeline_id: Some("pipe-history".to_string()),
            output_id: Some("out-history".to_string()),
            event_type: Some("egress.started".to_string()),
            event_class: Some("lifecycle".to_string()),
        },
    )
    .await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/logs?pipeline_id=pipe-history&output_id=out-history&event_class=lifecycle",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["logs"][0]["pipelineId"], "pipe-history");
    assert_eq!(json["logs"][0]["outputId"], "out-history");
    assert_eq!(json["logs"][0]["eventType"], "egress.started");
    let fields = json["logs"][0]["fields"].as_str().unwrap();
    let event_data: serde_json::Value = serde_json::from_str(fields).unwrap();
    assert_eq!(event_data["outputId"], "out-history");
}

#[tokio::test]
async fn restream_scope_logs_exclude_pipeline_and_output_entries() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    insert_app_log(
        &pool,
        AppLogEntry {
            ts: "2026-06-29T00:00:00Z".to_string(),
            level: "INFO".to_string(),
            target: "restream::runtime".to_string(),
            message: "dashboard API server listening".to_string(),
            fields: Some(r#"{"addr":"0.0.0.0:3030"}"#.to_string()),
            pipeline_id: None,
            output_id: None,
            event_type: Some("restream.http.ready".to_string()),
            event_class: Some("lifecycle".to_string()),
        },
    )
    .await;
    insert_app_log(
        &pool,
        AppLogEntry {
            ts: "2026-06-29T00:00:01Z".to_string(),
            level: "INFO".to_string(),
            target: "restream::lifecycle".to_string(),
            message: "publisher connected".to_string(),
            fields: None,
            pipeline_id: Some("pipe-history".to_string()),
            output_id: None,
            event_type: Some("ingest.connected".to_string()),
            event_class: Some("lifecycle".to_string()),
        },
    )
    .await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/logs?scope=restream&limit=20",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["logs"].as_array().unwrap().len(), 1);
    assert_eq!(json["logs"][0]["eventType"], "restream.http.ready");
    assert!(json["logs"][0]["pipelineId"].is_null());
}

#[tokio::test]
async fn delete_pipeline_storage_failure_is_internal_error() {
    let auth_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&auth_pool).await.unwrap();
    let pipeline_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pipeline_pool).await.unwrap();
    db::create_pipeline(
        &pipeline_pool,
        "p_delete_db_down",
        "P",
        "delete-db-down",
        None,
        None,
    )
    .await
    .unwrap();

    let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
    restream::infrastructure::bootstrap::initialize_auth_for_test(&auth_pool, &sessions, "admin")
        .await;
    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = broadcast::channel(32);
    let engine = Arc::new(MediaEngine::new());
    let mut state = api::AppState::test_new(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&auth_pool).compose(),
        security,
        ingest_policy_store,
        sessions,
        engine,
        log_broadcast,
    );
    state.pipeline_service =
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pipeline_pool)
            .pipeline_service();
    pipeline_pool.close().await;
    let app = api::create_router(Arc::new(state));
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/pipelines/p_delete_db_down",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn v1_overview_requires_auth() {
    let (app, _) = test_app().await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/overview")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn v1_overview_returns_summary_fields() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;
    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/overview", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["totalPipelines"].is_number());
    assert!(body["activePipelines"].is_number());
    assert!(body["degradedPipelines"].is_number());
    assert!(body["failedOutputs"].is_number());
    assert!(body["alertCount"]["critical"].is_number());
    assert!(body["alertCount"]["warning"].is_number());
    assert!(body["generatedAt"].is_string());
}

#[tokio::test]
async fn v1_overview_counts_match_pipeline_count() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    // Create two pipelines
    for name in &["overview-p1", "overview-p2"] {
        app.clone()
            .oneshot(auth_req(
                "POST",
                "/api/v1/pipelines",
                &cookie,
                Some(&serde_json::json!({ "name": name, "streamKey": name }).to_string()),
            ))
            .await
            .unwrap();
    }
    let _ = pool; // keep alive

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/overview", &cookie, None))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["totalPipelines"].as_u64().unwrap(), 2);
    // No active ingests → 0 active, both pipelines show no-publisher alert → degraded = 2
    assert_eq!(body["activePipelines"].as_u64().unwrap(), 0);
    assert_eq!(body["degradedPipelines"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn v1_pipeline_summary_not_found_for_unknown_id() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;
    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/nonexistent/summary",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v1_pipeline_summary_returns_operator_fields() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Create a pipeline
    let create = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(
                &serde_json::json!({ "name": "summary-test", "streamKey": "smrykey" }).to_string(),
            ),
        ))
        .await
        .unwrap();
    let body = body_json(create).await;
    let pid = body["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{}/summary", pid),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["pipelineId"].as_str().unwrap(), pid);
    assert!(body["source"]["status"].is_string());
    assert_eq!(body["input"]["status"], body["source"]["status"]);
    assert!(body["outputs"]["total"].is_number());
    assert!(body["outputs"]["running"].is_number());
    assert_eq!(body["graph"]["hasGraph"], true);
    assert!(body["graph"]["nodes"].as_u64().unwrap() > 0);
    assert!(body["graph"]["edges"].is_number());
    assert!(body["graph"]["activeNodes"].is_number());
    assert!(body["alerts"].is_array());
    assert!(body["generatedAt"].is_string());
}

#[tokio::test]
async fn v1_engine_and_health_endpoints_require_auth() {
    let (app, _) = test_app().await;

    for uri in ["/api/v1/engine", "/api/v1/engine/health"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn v1_engine_and_settings_endpoints_return_structured_payloads() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let engine = body_json(resp).await;
    assert!(engine["restream"]["version"].is_string());
    assert!(engine["os"].is_object());

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health = body_json(resp).await;
    assert!(health["generatedAt"].is_string());
    assert!(health["pipelines"].is_object());

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let settings = body_json(resp).await;
    assert!(settings["serverName"].is_string());
    assert!(settings["recordingSettings"].is_object());
    assert!(settings["transcodeProfiles"].is_object());
}

#[tokio::test]
async fn v1_pipeline_list_detail_and_graph_endpoints_return_payloads() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(
        &pool,
        "pipe-v1",
        "Pipeline V1",
        "key_v1",
        None,
        Some("source"),
    )
    .await
    .unwrap();
    db::create_output(
        &pool,
        "out-v1",
        "pipe-v1",
        "Output V1",
        "rtmp://example/live/key",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/pipelines", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    assert!(list["pipelines"].is_array());

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/pipelines/pipe-v1", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let detail = body_json(resp).await;
    assert_eq!(detail["pipeline"]["id"], "pipe-v1");
    assert_eq!(detail["outputs"].as_array().unwrap().len(), 1);
    assert_eq!(detail["outputs"][0]["id"], "out-v1");
    assert_eq!(detail["outputs"][0]["desiredState"], "stopped");
    assert_eq!(detail["outputs"][0]["url"], "rtmp://example/live/key");

    let resp = app
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/pipe-v1/graph",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let graph = body_json(resp).await;
    assert!(graph["nodes"].is_array());
    assert!(graph["edges"].is_array());
    assert_eq!(graph["desiredGraph"]["pipelineId"], "pipe-v1");
    assert!(graph["desiredGraph"]["stages"].is_array());
    assert!(graph["runtimeGraph"]["nodes"].is_array());
    assert!(graph["runtimeGraph"]["edges"].is_array());
}

#[tokio::test]
async fn v1_pipeline_detail_and_diagnostics_return_404_for_unknown_pipeline() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/pipelines/missing", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/missing/graph",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/missing/diagnostics/run",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let (app, cookie) = authenticated_app().await;
    let resp = app
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines/missing/diagnostics/context",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// --- Lifecycle events endpoint ---

#[tokio::test]
async fn v1_events_requires_auth() {
    let (app, _) = test_app().await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/events")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn v1_events_returns_envelope_and_events_array() {
    let (app, _pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    // Emit a synthetic event directly on the engine's event log
    engine
        .runtime
        .event_log
        .emit(restream::events::EventKind::IngestConnected {
            pipeline_id: "test-pipeline".to_string(),
            protocol: "rtmp".to_string(),
            stream_key: "key01".to_string(),
        });

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/events", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["generatedAt"].is_string());
    assert!(body["count"].as_u64().unwrap() >= 1);
    assert!(body["events"].is_array());
    let events = body["events"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["kind"].as_str() == Some("ingestConnected"))
    );
}

#[tokio::test]
async fn v1_events_filters_by_pipeline_id() {
    let (app, _pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    engine
        .runtime
        .event_log
        .emit(restream::events::EventKind::IngestConnected {
            pipeline_id: "pipe-a".to_string(),
            protocol: "rtmp".to_string(),
            stream_key: "key01".to_string(),
        });
    engine
        .runtime
        .event_log
        .emit(restream::events::EventKind::IngestConnected {
            pipeline_id: "pipe-b".to_string(),
            protocol: "srt".to_string(),
            stream_key: "key02".to_string(),
        });

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/events?pipeline_id=pipe-a",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["pipelineId"].as_str().unwrap(), "pipe-a");
}
