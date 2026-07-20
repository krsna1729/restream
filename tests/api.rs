use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use restream::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use restream::domain::output_spec::OutputConfig;
use restream::domain::srt_ingest::SrtGlobalIngestConfig;
use restream::domain::stage::{StageKey, StageKind};
use restream::domain::state::{DesiredOutputState, EgressPhase};
use restream::logging::types::AppLogEntry;
use restream::media::engine::MediaEngine;
use restream::media::metadata::{AudioMeta, VideoMeta};
use restream::media::security::IngestSecurityService;
use restream::{api, db};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{RwLock as TokioRwLock, broadcast};
use tokio::time::{Duration, sleep};
use tower::ServiceExt;

#[path = "api/agent.rs"]
mod agent;
#[path = "api/auth.rs"]
mod auth;
#[path = "api/config.rs"]
mod config;
#[path = "api/diagnostics.rs"]
mod diagnostics;
#[path = "api/health.rs"]
mod health;
#[path = "api/hls.rs"]
mod hls;
#[path = "api/ingests.rs"]
mod ingests;
#[path = "api/media.rs"]
mod media;
#[path = "api/observability.rs"]
mod observability;
#[path = "api/outputs.rs"]
mod outputs;
#[path = "api/pipelines.rs"]
mod pipelines;

async fn test_app() -> (axum::Router, SqlitePool) {
    let (app, pool, _) = test_app_with_engine().await;
    (app, pool)
}

async fn test_app_with_engine() -> (axum::Router, SqlitePool, Arc<MediaEngine>) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();

    let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
    restream::infrastructure::bootstrap::initialize_auth_for_test(&pool, &sessions, "admin").await;

    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = broadcast::channel(32);
    let engine = Arc::new(MediaEngine::new());

    let state = Arc::new(api::AppState::test_new(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose(),
        security,
        ingest_policy_store,
        sessions,
        engine.clone(),
        log_broadcast,
    ));

    (api::create_router(state), pool, engine)
}

async fn test_app_with_secure_cookies() -> axum::Router {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();

    let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
    restream::infrastructure::bootstrap::initialize_auth_for_test(&pool, &sessions, "admin").await;

    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = broadcast::channel(32);
    let engine = Arc::new(MediaEngine::new());

    let mut state = api::AppState::test_new(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose(),
        security,
        ingest_policy_store,
        sessions,
        engine,
        log_broadcast,
    );
    state.set_secure_session_cookies_for_test(true);

    api::create_router(Arc::new(state))
}

async fn authenticated_app() -> (axum::Router, String) {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;
    (app, cookie)
}

async fn authenticated_app_with_temp_media()
-> (axum::Router, String, std::path::PathBuf, SqlitePool) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();

    let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
    restream::infrastructure::bootstrap::initialize_auth_for_test(&pool, &sessions, "admin").await;

    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = broadcast::channel(32);
    let engine = Arc::new(MediaEngine::new());
    let temp_dir =
        std::env::temp_dir().join(format!("restream-api-media-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let media_dir = temp_dir.to_string_lossy().to_string();

    let state = Arc::new(api::AppState::test_new_with_media_dir(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose(),
        security,
        ingest_policy_store,
        sessions,
        engine,
        log_broadcast,
        media_dir,
    ));

    let app = api::create_router(state);
    let cookie = login(&app).await;
    (app, cookie, temp_dir, pool)
}

async fn authenticated_app_with_temp_media_and_engine() -> (
    axum::Router,
    String,
    std::path::PathBuf,
    SqlitePool,
    Arc<MediaEngine>,
) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();

    let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
    restream::infrastructure::bootstrap::initialize_auth_for_test(&pool, &sessions, "admin").await;

    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let ingest_policy_store = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (log_broadcast, _) = broadcast::channel(32);
    let engine = Arc::new(MediaEngine::new());
    let temp_dir =
        std::env::temp_dir().join(format!("restream-api-media-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let media_dir = temp_dir.to_string_lossy().to_string();

    let state = Arc::new(api::AppState::test_new_with_media_dir(
        restream::infrastructure::service_wiring::SqliteServiceFactory::new(&pool).compose(),
        security,
        ingest_policy_store,
        sessions,
        engine.clone(),
        log_broadcast,
        media_dir,
    ));

    let app = api::create_router(state);
    let cookie = login(&app).await;
    (app, cookie, temp_dir, pool, engine)
}

async fn login(app: &axum::Router) -> String {
    login_with_password(app, "admin").await
}

async fn login_with_password(app: &axum::Router, password: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({ "password": password }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    cookie.split(';').next().unwrap().to_string()
}

fn auth_req(
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> Request<axum::body::Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Cookie", cookie)
        .header("Content-Type", "application/json");
    if let Some(b) = body {
        builder.body(axum::body::Body::from(b.to_string())).unwrap()
    } else {
        builder.body(axum::body::Body::empty()).unwrap()
    }
}

fn auth_req_with_header(
    method: &str,
    uri: &str,
    cookie: &str,
    name: &'static str,
    value: &'static str,
) -> Request<axum::body::Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Cookie", cookie)
        .header(name, value)
        .body(axum::body::Body::empty())
        .unwrap()
}

fn media_upload_req(cookie: &str, filename: &str, contents: &[u8]) -> Request<axum::body::Body> {
    let boundary = "restream-upload-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(contents);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    Request::builder()
        .method("POST")
        .uri("/api/v1/media/upload")
        .header("Cookie", cookie)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body))
        .unwrap()
}

async fn body_json(resp: axum::http::Response<axum::body::Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn body_bytes(resp: axum::http::Response<axum::body::Body>) -> bytes::Bytes {
    resp.into_body().collect().await.unwrap().to_bytes()
}

async fn insert_app_log(pool: &SqlitePool, entry: AppLogEntry) {
    db::append_app_log_batch(pool, &[entry]).await.unwrap();
}
