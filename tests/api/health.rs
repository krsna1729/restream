use super::*;

#[tokio::test]
async fn health_shows_registered_egress() {
    let (_, pool, engine) = test_app_with_engine().await;
    let app = {
        let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
        restream::infrastructure::bootstrap::initialize_auth_for_test(&pool, &sessions, "admin")
            .await;
        let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
        let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[],
        ));
        let (log_broadcast, _) = broadcast::channel(32);
        let state = Arc::new(api::AppState::test_new(
            restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose(),
            security,
            ingest_policy_store,
            sessions,
            engine.clone(),
            log_broadcast,
        ));
        api::create_router(state)
    };
    let cookie = login(&app).await;

    // Create pipeline and output
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13081"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(r#"{"name":"O","url":"rtmp://dest/live/k","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let out = body_json(resp).await;
    let oid = out["output"]["id"].as_str().unwrap().to_string();

    // Register an ingest + egress in the engine (simulates reconciler start with active publisher)
    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13081", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;

    // Health endpoint should show the output under the correct pipeline
    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health = body_json(resp).await;
    assert!(health["srtListener"]["bondingAvailable"].is_boolean());
    let outputs = &health["pipelines"][&pid]["outputs"];
    assert!(
        outputs[&oid].is_object(),
        "egress should appear under its pipeline in /health: {outputs}"
    );
    assert_eq!(outputs[&oid]["status"], "running");
}

#[tokio::test]
async fn output_status_and_health_preserve_recent_egress_failure_after_unregister() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13082"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(r#"{"name":"O","url":"rtmp://dest/live/k","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let out = body_json(resp).await;
    let oid = out["output"]["id"].as_str().unwrap().to_string();

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13082", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    engine.update_egress_phase(&oid, EgressPhase::Sending).await;
    engine.record_egress_progress(&oid, 1316).await;
    engine
        .record_egress_error(&oid, "send", "connection reset by peer")
        .await;
    engine.unregister_egress(&oid).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{pid}/outputs/{oid}/status"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = body_json(resp).await;
    assert_eq!(status["status"], "failed");
    assert_eq!(status["rawStatus"], "running");
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
    assert!(status["lastErrorAt"].is_string());
    assert!(status["endedAt"].is_string());

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health = body_json(resp).await;
    let output = &health["pipelines"][&pid]["outputs"][&oid];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
    assert_eq!(output["lastError"], "connection reset by peer");
    assert!(output["endedAt"].is_string());
}

#[tokio::test]
async fn active_output_status_ignores_stale_retry_state_after_restart() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13084"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(r#"{"name":"O","url":"rtmp://dest/live/k","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let out = body_json(resp).await;
    let oid = out["output"]["id"].as_str().unwrap().to_string();

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13084", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    engine
        .record_egress_error(&oid, "send", "connection reset by peer")
        .await;
    engine.unregister_egress(&oid).await;

    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    engine.update_egress_phase(&oid, EgressPhase::Sending).await;
    engine.record_egress_progress(&oid, 2048).await;
    engine
        .update_egress_retry_state(&oid, 2, 20_000, 15_000)
        .await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{pid}/outputs/{oid}/status"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = body_json(resp).await;
    assert_eq!(status["status"], "running");
    assert_eq!(status["phase"], "sending");
    assert_eq!(status["recentFailureCount"], 1);
    assert_eq!(status["flapping"], false);
    assert_eq!(status["retrying"], false);
    assert!(status["retryAttempts"].is_null());
    assert!(status["retryBackoffMs"].is_null());
    assert!(status["retryRemainingMs"].is_null());

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health = body_json(resp).await;
    let output = &health["pipelines"][&pid]["outputs"][&oid];
    assert_eq!(output["status"], "running");
    assert_eq!(output["phase"], "sending");
    assert_eq!(output["recentFailureCount"], 1);
    assert_eq!(output["flapping"], false);
    assert_eq!(output["retrying"], false);
    assert!(output["retryAttempts"].is_null());
    assert!(output["retryBackoffMs"].is_null());
    assert!(output["retryRemainingMs"].is_null());
}

#[tokio::test]
async fn recovered_output_surfaces_flapping_after_repeated_sink_failures() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13085"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(r#"{"name":"O","url":"rtmp://dest/live/k","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let out = body_json(resp).await;
    let oid = out["output"]["id"].as_str().unwrap().to_string();

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13085", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    engine
        .record_egress_error(&oid, "send", "attempt 1 failed")
        .await;
    engine.unregister_egress(&oid).await;

    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    engine
        .record_egress_error(&oid, "connect", "attempt 2 failed")
        .await;
    engine.unregister_egress(&oid).await;

    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    engine.update_egress_phase(&oid, EgressPhase::Sending).await;
    engine.record_egress_progress(&oid, 4096).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/{pid}/outputs/{oid}/status"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = body_json(resp).await;
    assert_eq!(status["status"], "running");
    assert!(status["lastError"].is_null());
    assert_eq!(status["recentFailureCount"], 2);
    assert_eq!(status["flapping"], true);
    assert_eq!(status["retrying"], false);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let health = body_json(resp).await;
    let output = &health["pipelines"][&pid]["outputs"][&oid];
    assert_eq!(output["status"], "running");
    assert_eq!(output["recentFailureCount"], 2);
    assert_eq!(output["flapping"], true);
}

#[tokio::test]
async fn health_and_dashboard_runtime_fail_when_pipeline_list_fails() {
    let auth_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&auth_pool).await.unwrap();
    let pipeline_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pipeline_pool).await.unwrap();

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

    for uri in ["/api/v1/engine/health", "/api/v1/dashboard/runtime"] {
        let resp = app
            .clone()
            .oneshot(auth_req("GET", uri, &cookie, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
    }
}

#[tokio::test]
async fn health_endpoint_exposes_probe_and_egress_fault_fields() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13081"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(r#"{"name":"O","url":"rtmp://dest/live/k","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let out = body_json(resp).await;
    let oid = out["output"]["id"].as_str().unwrap().to_string();

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13081", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pending = body_json(resp).await;
    let pending_input = &pending["pipelines"][&pid]["input"];
    assert_eq!(pending_input["probeReady"], false);
    assert_eq!(pending_input["probeStatus"], "pending");
    assert!(pending_input["probePendingMs"].as_u64().is_some());

    let audio = AudioMeta {
        track_index: 0,
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta(
            &pid,
            Some(VideoMeta {
                codec: "h264".to_string(),
                width: 1920,
                height: 1080,
                fps: 30.0,
                bw: None,
                pid: None,
                language: None,
                title: None,
                profile: None,
                level: None,
                pixel_format: None,
            }),
            Some(audio.clone()),
            None,
        )
        .await;
    engine.update_ingest_audio_tracks(&pid, vec![audio]).await;
    engine.record_egress_progress(&oid, 1316).await;
    engine
        .record_egress_error(&oid, "send", "connection reset by peer")
        .await;
    let (store, _, _) = engine.ensure_hls_preview_segmenter(&pid).await;
    engine.touch_hls_preview(&pid).await;
    store.put_video_init_segment(bytes::Bytes::from_static(b"init"));
    store.push_video_segment(0, 2.0, bytes::Bytes::from_static(b"segment"));

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ready = body_json(resp).await;
    let ready_input = &ready["pipelines"][&pid]["input"];
    assert_eq!(ready_input["probeReady"], true);
    assert_eq!(ready_input["probeStatus"], "ready");
    assert!(ready_input["probePendingMs"].is_null());

    let output = &ready["pipelines"][&pid]["outputs"][&oid];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["rawStatus"], "running");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
    assert_eq!(output["lastError"], "connection reset by peer");
    assert!(output["lastErrorAt"].is_string());
    assert!(output["lastProgressAt"].is_string());
    assert!(output["lastProgressAgeMs"].as_u64().is_some());

    let hls_preview = &ready["pipelines"][&pid]["hlsPreview"];
    assert_eq!(hls_preview["active"], true);
    assert_eq!(hls_preview["persistentConsumers"], 0);
    assert!(hls_preview["lastAccessAgeMs"].as_u64().is_some());
    assert_eq!(hls_preview["segments"], 1);
    assert!(hls_preview["playlistBytes"].as_u64().unwrap_or(0) > 0);

    engine
        .record_ingest_disconnect(
            &pid,
            Some("disconnect"),
            Some("publisher disconnected".to_string()),
            false,
        )
        .await;
    engine.unregister_ingest(&pid).await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let disconnected = body_json(resp).await;
    let disconnected_input = &disconnected["pipelines"][&pid]["input"];
    assert_eq!(disconnected_input["status"], "off");
    assert_eq!(disconnected_input["probeStatus"], "off");
    assert_eq!(disconnected_input["lastSessionProtocol"], "rtmp");
    assert_eq!(
        disconnected_input["lastDisconnectReason"],
        "publisher disconnected"
    );
    assert_eq!(disconnected_input["lastFailurePhase"], "disconnect");
    assert_eq!(disconnected_input["recentDisconnectError"], false);
    assert_eq!(disconnected_input["recentDisconnectCount"], 1);
    assert_eq!(disconnected_input["flapping"], false);
    assert_eq!(disconnected_input["disconnectGraceActive"], true);
    assert!(
        disconnected_input["disconnectGraceRemainingMs"]
            .as_u64()
            .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
    );
    assert!(disconnected_input["lastDisconnectAt"].is_string());
    assert!(disconnected_input["lastDisconnectAgeMs"].as_u64().is_some());
}

#[tokio::test]
async fn health_endpoint_clears_recent_disconnect_details_after_reconnect() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13082"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13082", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .record_ingest_disconnect(
            &pid,
            Some("disconnect"),
            Some("publisher disconnected".to_string()),
            false,
        )
        .await;
    engine.unregister_ingest(&pid).await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let disconnected = body_json(resp).await;
    let disconnected_input = &disconnected["pipelines"][&pid]["input"];
    assert_eq!(disconnected_input["status"], "off");
    assert_eq!(disconnected_input["probeStatus"], "off");
    assert_eq!(
        disconnected_input["lastDisconnectReason"],
        "publisher disconnected"
    );
    assert_eq!(disconnected_input["lastFailurePhase"], "disconnect");
    assert_eq!(disconnected_input["disconnectGraceActive"], true);
    assert!(
        disconnected_input["disconnectGraceRemainingMs"]
            .as_u64()
            .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
    );

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13082", "srt")
        .await
        .expect("reconnect registration should succeed");

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let reconnected = body_json(resp).await;
    let input = &reconnected["pipelines"][&pid]["input"];
    assert_eq!(input["status"], "on");
    assert_eq!(input["probeStatus"], "pending");
    assert_eq!(input["probeReady"], false);
    assert!(input["lastSessionProtocol"].is_null());
    assert!(input["lastDisconnectReason"].is_null());
    assert!(input["lastFailurePhase"].is_null());
    assert!(input["lastDisconnectAt"].is_null());
    assert!(input["lastDisconnectAgeMs"].is_null());
    assert_eq!(input["recentDisconnectError"], false);
    assert_eq!(input["recentDisconnectCount"], 1);
    assert_eq!(input["flapping"], false);
    assert_eq!(input["disconnectGraceActive"], false);
    assert!(input["disconnectGraceRemainingMs"].is_null());
}

#[tokio::test]
async fn health_endpoint_surfaces_repeated_transient_disconnects_as_flapping() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13083"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    for _ in 0..2 {
        engine
            .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13083", "rtmp")
            .await
            .expect("ingest registration should succeed");
        engine
            .record_ingest_disconnect(
                &pid,
                Some("disconnect"),
                Some("publisher disconnected".to_string()),
                false,
            )
            .await;
        engine.unregister_ingest(&pid).await;
    }

    engine
        .try_register_ingest(&pid, "key01_6c71124cde80358ca7c13083", "rtmp")
        .await
        .expect("reconnect registration should succeed");

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/engine/health", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let input = &body["pipelines"][&pid]["input"];
    assert_eq!(input["status"], "on");
    assert_eq!(input["recentDisconnectCount"], 2);
    assert_eq!(input["flapping"], true);
    assert!(input["lastSessionProtocol"].is_null());
    assert!(input["lastDisconnectReason"].is_null());
    assert!(input["lastFailurePhase"].is_null());
    assert!(input["lastDisconnectAt"].is_null());
    assert!(input["lastDisconnectAgeMs"].is_null());
}
