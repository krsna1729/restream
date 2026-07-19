use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use restream::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use restream::domain::srt_ingest::SrtGlobalIngestConfig;
use restream::media::engine::MediaEngine;
use restream::media::security::IngestSecurityService;
use restream::{api, db};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tower::ServiceExt;

pub async fn authenticated_app() -> (axum::Router, SqlitePool, String) {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    db::setup_database_schema(&pool)
        .await
        .expect("schema setup");
    let sessions = Arc::new(RwLock::new(HashSet::new()));
    api::initialize_auth_for_test(&pool, &sessions, "admin").await;
    let security = Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG));
    let policies = Arc::new(restream::media::srt::SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &[],
    ));
    let (logs, _) = broadcast::channel(32);
    let state = Arc::new(api::AppState::test_new(
        pool.clone(),
        security,
        policies,
        sessions,
        Arc::new(MediaEngine::new()),
        logs,
    ));
    let app = api::create_router(state);
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/auth/login",
            None,
            Some(r#"{"password":"admin"}"#),
        ))
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie")
        .to_str()
        .expect("cookie text")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    (app, pool, cookie)
}

pub fn json_request(
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> Request<axum::body::Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("Cookie", cookie);
    }
    builder
        .body(match body {
            Some(body) => axum::body::Body::from(body.to_string()),
            None => axum::body::Body::empty(),
        })
        .expect("request")
}

pub async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json response")
}
