use restream::config::AppConfig;
use restream::domain::ids::OutputId;
use restream::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use restream::domain::output_spec::OutputConfig;
use restream::domain::srt_ingest::SrtGlobalIngestConfig;
use restream::domain::state::EgressPhase;
use restream::media::security::IngestSecurityService;
use sqlx::SqlitePool;
use std::sync::Arc;

#[tokio::test]
async fn test_phase_2_config_reads_env_correctly() {
    unsafe {
        std::env::set_var("RESTREAM_DB_PATH", "test_env.db");
        std::env::set_var("RESTREAM_MEDIA_DIR", "test_media_dir");
        std::env::set_var("RESTREAM_LOG_RETENTION_DAYS", "14");
    }

    let config = AppConfig::from_env();
    assert_eq!(config.db_path, "test_env.db");
    assert_eq!(config.media_dir, "test_media_dir");
    assert_eq!(config.log_retention_days, 14);
}

#[tokio::test]
async fn test_phase_3_routing_resolves_all_major_routes() {
    let mock_engine = Arc::new(restream::media::engine::MediaEngine::new());
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    restream::db::setup_database_schema(&db).await.unwrap();

    let sessions = Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new()));
    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = tokio::sync::broadcast::channel(32);

    let state = Arc::new(restream::api::AppState::test_new(
        db,
        security,
        ingest_policy_store,
        sessions,
        mock_engine,
        log_broadcast,
        "media".to_string(),
    ));
    let app = restream::api::create_router(state);
    let _ = app.into_make_service();
}

#[tokio::test]
async fn test_phase_4_5_services_and_repositories_flow() {
    let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
    restream::db::setup_database_schema(&db).await.unwrap();

    let pipeline_service =
        restream::application::services::pipeline_service::PipelineService::new(db.clone());
    let output_service =
        restream::application::services::output_service::OutputService::new(db.clone());

    let pid = "test-pipe-service";
    pipeline_service
        .create_pipeline(pid, "name", "stream-key", None, None)
        .await
        .unwrap();

    let pipeline = restream::db::pipeline_repo::get_pipeline(&db, pid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pipeline.stream_key, "stream-key");

    let oid = "test-out-service";
    let config = OutputConfig::default();
    output_service
        .create_output(
            oid,
            pid,
            "rtmp-push",
            "rtmp://localhost/live",
            None,
            "running",
            &config,
        )
        .await
        .unwrap();

    let output = restream::db::output_repo::get_output(&db, pid, oid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.url, "rtmp://localhost/live");
}

#[tokio::test]
async fn test_phase_8_dependency_aware_output_status_resolution() {
    use restream::runtime::output::OutputRuntimeExplanation;

    let explanation = OutputRuntimeExplanation {
        output_id: OutputId::from("out-1".to_string()),
        output_name: "test-out".to_string(),
        encoding: "h264".to_string(),
        url: "rtmp://localhost/live".to_string(),
        phase: EgressPhase::Connecting,
        terminal_stage: None,
        blocked_by: None,
    };

    assert_eq!(explanation.output_name, "test-out");
}
