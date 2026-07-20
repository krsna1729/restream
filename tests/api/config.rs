use super::*;

#[tokio::test]
async fn config_get_returns_structured_data() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://dest/live",
        None,
        DesiredOutputState::Running,
        &OutputConfig::default(),
    )
    .await
    .unwrap();
    db::create_job(
        &pool,
        "j1",
        "p1",
        "o1",
        Some(1234),
        restream::application::models::JobStatus::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["pipelines"].is_array());
    assert!(json["outputs"].is_array());
    assert!(json["jobs"].is_array());
    assert_eq!(json["jobs"][0]["id"], "j1");
    assert_eq!(json["jobs"][0]["pipelineId"], "p1");
    assert_eq!(json["jobs"][0]["outputId"], "o1");
    assert_eq!(json["jobs"][0]["pid"], 1234);
    assert_eq!(json["jobs"][0]["status"], "running");
    assert_eq!(json["jobs"][0]["startedAt"], "2024-01-01T00:00:00Z");
    assert!(json["serverName"].is_string());
    assert_eq!(json["ingestHost"], "");
    assert_eq!(json["recordingSettings"]["retainSourceTs"], false);
    assert_eq!(json["pipelines"][0]["fileIngest"]["configured"], false);
    assert_eq!(
        json["pipelines"][0]["ingestUrls"]["rtmp"],
        "rtmp://localhost:1935/live/key01"
    );
}

#[tokio::test]
async fn config_patch_server_name() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"serverName":"My Server"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["serverName"], "My Server");
}

#[tokio::test]
async fn config_patch_ingest_host_persists_and_updates_ingest_urls() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"ingestHost":"  ingest.example.com  "}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ingestHost"], "ingest.example.com");
    assert_eq!(
        db::get_ingest_host(&pool).await.unwrap().as_deref(),
        Some("ingest.example.com")
    );

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(json["ingestHost"], "ingest.example.com");
    assert_eq!(
        json["pipelines"][0]["ingestUrls"]["rtmp"],
        "rtmp://ingest.example.com:1935/live/key01"
    );
    assert_eq!(
        json["pipelines"][0]["ingestUrls"]["srt"],
        "srt://ingest.example.com:10080?streamid=publish:key01"
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"ingestHost":"   "}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ingestHost"], "");

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert_eq!(
        json["pipelines"][0]["ingestUrls"]["rtmp"],
        "rtmp://localhost:1935/live/key01"
    );
}

#[tokio::test]
async fn config_patch_recording_settings_persists() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["recordingSettings"]["retainSourceTs"], false);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"recordingSettings":{"retainSourceTs":true}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["recordingSettings"]["retainSourceTs"], true);

    let stored = restream::application::recording::load_recording_settings(
        &restream::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone()),
    )
    .await;
    assert!(stored.retain_source_ts);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["recordingSettings"]["retainSourceTs"], true);
}

#[tokio::test]
async fn config_patch_backend_policy_persists_and_updates_runtime() {
    let (app, pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    assert_eq!(
        engine.backend_policy(),
        restream::planner::BackendPolicy::default()
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(
                r#"{"backendPolicy":{"internalVideoPresets":true,"internalHevcToH264":false,"internalHlsPreview":true,"internalComplexAudio":false}}"#,
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["backendPolicy"]["internalVideoPresets"], true);
    assert_eq!(json["backendPolicy"]["internalHevcToH264"], false);
    assert_eq!(json["backendPolicy"]["internalHlsPreview"], true);
    assert_eq!(json["backendPolicy"]["internalComplexAudio"], false);

    let expected = restream::planner::BackendPolicy {
        internal_video_presets: true,
        internal_hevc_to_h264: false,
        internal_hls_preview: true,
        internal_complex_audio: false,
    };
    assert_eq!(engine.backend_policy(), expected);

    let stored = restream::application::settings::load_backend_policy(
        &restream::infrastructure::sqlite_ports::SqliteMetaStore::new(pool),
        restream::planner::BackendPolicy::default(),
    )
    .await;
    assert_eq!(stored, expected);
}

#[tokio::test]
async fn config_patch_ingest_security_persists() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ingestSecurity"]["failureLimit"], 10);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(
                r#"{"ingestSecurity":{"failureLimit":3,"failureWindowMs":15000,"banMs":45000,"trackedIpLimit":64}}"#,
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ingestSecurity"]["failureLimit"], 3);
    assert_eq!(json["ingestSecurity"]["failureWindowMs"], 15000);
    assert_eq!(json["ingestSecurity"]["banMs"], 45000);
    assert_eq!(json["ingestSecurity"]["trackedIpLimit"], 64);

    let stored = restream::application::ingest_security::load_ingest_security_config(
        &restream::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone()),
    )
    .await;
    assert_eq!(stored.failure_limit, 3);
    assert_eq!(stored.failure_window_ms, 15_000);
    assert_eq!(stored.ban_ms, 45_000);
    assert_eq!(stored.tracked_ip_limit, 64);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ingestSecurity"]["failureLimit"], 3);
}

#[tokio::test]
async fn config_patch_ingest_security_does_not_mutate_runtime_when_persist_fails() {
    let auth_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&auth_pool).await.unwrap();
    let settings_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&settings_pool).await.unwrap();

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
        security.clone(),
        ingest_policy_store,
        sessions,
        engine,
        log_broadcast,
    );
    state.settings_service =
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&settings_pool)
            .settings_service();
    settings_pool.close().await;
    let app = api::create_router(Arc::new(state));
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(
                r#"{"ingestSecurity":{"failureLimit":3,"failureWindowMs":15000,"banMs":45000,"trackedIpLimit":64}}"#,
            ),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        security.get_config().failure_limit,
        DEFAULT_INGEST_SECURITY_CONFIG.failure_limit
    );
}

#[tokio::test]
async fn config_patch_rejects_invalid_ingest_security() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(
                r#"{"ingestSecurity":{"failureLimit":0,"failureWindowMs":15000,"banMs":45000,"trackedIpLimit":64}}"#,
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        std::str::from_utf8(&body_bytes(resp).await).unwrap(),
        "ingestSecurity.failureLimit must be >= 1"
    );

    let stored = restream::application::ingest_security::load_ingest_security_config(
        &restream::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone()),
    )
    .await;
    assert_eq!(
        stored.failure_limit,
        DEFAULT_INGEST_SECURITY_CONFIG.failure_limit
    );
}

// --- Audio caps ---

#[tokio::test]
async fn audio_caps_requires_auth() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/audio-caps")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn audio_caps_returns_caps_when_authenticated() {
    let (app, cookie) = authenticated_app().await;
    let resp = app
        .oneshot(auth_req("GET", "/api/v1/audio-caps", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["caps"].is_object());
    assert!(json["platformLabels"].is_object());
}

// --- Stream keys ---

#[tokio::test]
async fn stream_keys_requires_auth() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/stream-keys")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn stream_keys_returns_array() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;
    db::set_ingest_host(&pool, "ingest.example.com")
        .await
        .unwrap();
    db::create_pipeline(&pool, "p1", "Publisher", "configured-key", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/stream-keys", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let keys = json.as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["key"], "configured-key");
    assert_eq!(keys[0]["label"], "Publisher");
    assert_eq!(
        keys[0]["ingestUrls"]["rtmp"],
        "rtmp://ingest.example.com:1935/live/configured-key"
    );
    assert_eq!(
        keys[0]["ingestUrls"]["srt"],
        "srt://ingest.example.com:10080?streamid=publish:configured-key"
    );
}

// --- Round 7 #4: transcode profile field validation ---

#[tokio::test]
async fn config_patch_invalid_transcode_profile_rejected() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Patch with an invalid preset
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"transcodeProfiles":{"h264":{"preset":"garbage","tune":"zerolatency","crf":23}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Patch with an invalid tune
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"transcodeProfiles":{"h264":{"preset":"ultrafast","tune":"badtune","crf":23}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Patch with an invalid CRF
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(r#"{"transcodeProfiles":{"h264":{"preset":"ultrafast","tune":"zerolatency","crf":100}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn config_patch_custom_transcode_profiles_keep_built_ins_visible() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let body = serde_json::json!({
        "transcodeProfiles": {
            "4k60": {
                "preset": "ultrafast",
                "tune": "zerolatency",
                "crf": 23,
                "gop": 60,
                "bframes": 0,
                "bitrate": 20000000,
                "maxBitrate": 24000000,
                "width": 3840,
                "height": 2160
            }
        }
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/settings",
            &cookie,
            Some(&body.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["transcodeProfiles"]["h264"].is_object());
    assert!(json["transcodeProfiles"]["720p"].is_object());
    assert!(json["transcodeProfiles"]["1080p"].is_object());
    assert_eq!(json["transcodeProfiles"]["4k60"]["width"], 3840);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["transcodeProfiles"]["h264"].is_object());
    assert!(json["transcodeProfiles"]["720p"].is_object());
    assert!(json["transcodeProfiles"]["1080p"].is_object());
    assert_eq!(json["transcodeProfiles"]["4k60"]["height"], 2160);
}
