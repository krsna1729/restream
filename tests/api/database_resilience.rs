//! Real SQLite failures and caller cancellation at the API security boundary.

use super::*;
use restream::application::services::TeamService;
use restream::infrastructure::service_wiring::ControlPlane;
use tokio::sync::oneshot;

const CURRENT_TOKEN: &str = "database-resilience-current";
const OTHER_TOKEN: &str = "database-resilience-other";
const READ_DEADLINE: Duration = Duration::from_millis(100);

fn token_hash(token: &str) -> String {
    api::state::hash_session_token(token)
}

fn cookie(token: &str) -> String {
    format!("session={token}")
}

async fn fixture() -> (api::AppState, SqlitePool, Arc<TokioRwLock<HashSet<String>>>) {
    fixture_with(|services| services).await
}

/// [`fixture`] with `wire` applied to the service graph before the state is
/// built, so a test can swap one service for a failing one.
async fn fixture_with(
    wire: impl FnOnce(api::AppServices) -> api::AppServices,
) -> (api::AppState, SqlitePool, Arc<TokioRwLock<HashSet<String>>>) {
    // One connection makes exhaustion deterministic, without relying on an
    // acquire-timeout error or an artificial repository implementation.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    db::setup_database_schema(&pool).await.unwrap();
    let lifecycle = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&lifecycle).await.unwrap();
    let executor = db::DeadlineDb::new(pool.clone(), READ_DEADLINE);
    let services = wire(
        ControlPlane::new(
            executor,
            db::DeadlineDb::new(lifecycle, db::SHARED_LOOP_DEADLINE),
        )
        .services,
    );
    let sessions = Arc::new(TokioRwLock::new(HashSet::new()));
    for token in [CURRENT_TOKEN, OTHER_TOKEN] {
        let hash = token_hash(token);
        db::create_session(&pool, &hash, jiff::Timestamp::now().as_millisecond())
            .await
            .unwrap();
        sessions.write().await.insert(hash);
    }
    let state = api::AppState::test_new(
        services,
        Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG)),
        Arc::new(restream_engine::srt::SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[],
        )),
        sessions.clone(),
        Arc::new(MediaEngine::new()),
        broadcast::channel(8).0,
    );
    (state, pool, sessions)
}

#[tokio::test]
async fn exhausted_api_pool_returns_503_without_revoking_a_valid_session() {
    let (state, pool, sessions) = fixture().await;
    let app = api::create_router(Arc::new(state));
    let session_cookie = cookie(CURRENT_TOKEN);
    let cached_before = sessions.read().await.clone();
    let held = pool.acquire().await.unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        app.clone()
            .oneshot(auth_req("GET", "/api/v1/pipelines", &session_cookie, None)),
    )
    .await
    .expect("authentication must answer at the read deadline, not the pool acquire timeout")
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !response.headers().contains_key(header::SET_COOKIE),
        "database unavailability must not clear the caller's cookie"
    );
    assert!(body_json(response).await["error"].is_string());
    assert_eq!(*sessions.read().await, cached_before);

    drop(held);
    let response = app
        .oneshot(auth_req("GET", "/api/v1/pipelines", &session_cookie, None))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the same cookie recovers"
    );
    assert!(sessions.read().await.contains(&token_hash(CURRENT_TOKEN)));
}

#[tokio::test]
async fn database_failure_is_not_a_browser_login_redirect() {
    let (state, pool, _) = fixture().await;
    let app = api::create_router(Arc::new(state));
    let held = pool.acquire().await.unwrap();
    for path in ["/", "/login", "/index.html"] {
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            app.clone()
                .oneshot(auth_req("GET", path, &cookie(CURRENT_TOKEN), None)),
        )
        .await
        .expect("browser authentication must answer at its read deadline")
        .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert!(!response.headers().contains_key(header::LOCATION), "{path}");
    }
    drop(held);
    let response = app
        .oneshot(auth_req("GET", "/", &cookie(CURRENT_TOKEN), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unavailable_principal_store_cannot_bypass_role_checks_and_mutate() {
    // Session validation is healthy. Only the independent role/principal
    // repository fails, exercising the middleware's separate error branch.
    let principal_pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&principal_pool).await.unwrap();
    let team_service = TeamService::new(db::DeadlineDb::new(principal_pool.clone(), READ_DEADLINE));
    let (state, pool, _) = fixture_with(|services| api::AppServices {
        team_service,
        ..services
    })
    .await;
    assert!(state.validate_session(CURRENT_TOKEN).await.unwrap());
    principal_pool.close().await;
    let app = api::create_router(Arc::new(state));

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        app.oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie(CURRENT_TOKEN),
            Some(r#"{"name":"Test Pipeline","streamKey":"key01_6c71124cde80358ca7c13081"}"#),
        )),
    )
    .await
    .expect("a failed principal lookup must fail closed promptly")
    .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "SAMEORIGIN");
    assert!(db::list_pipelines(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn middleware_authentication_errors_keep_security_headers() {
    let (state, _pool, _) = fixture().await;
    let response = api::create_router(Arc::new(state))
        .oneshot(auth_req("GET", "/api/v1/settings", "", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "SAMEORIGIN");
}

#[tokio::test]
async fn public_api_misses_do_not_consult_unavailable_authentication() {
    let (state, pool, _) = fixture().await;
    let app = api::create_router(Arc::new(state));
    let _held = pool.acquire().await.unwrap();
    for (method, path) in [
        ("POST", "/api/v1/not-a-real-route"),
        ("GET", "/api/v1/users/not-a-real-route/extra"),
    ] {
        for session in ["".to_owned(), cookie(CURRENT_TOKEN)] {
            let response = tokio::time::timeout(
                Duration::from_secs(1),
                app.clone().oneshot(auth_req(method, path, &session, None)),
            )
            .await
            .expect("the public API miss must not wait on authentication")
            .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            assert!(!response.headers().contains_key(header::LOCATION));
        }
    }
}

/// Pauses the API pool's one connection inside the request's session
/// revocation (`DELETE FROM sessions`), after the password write committed
/// in its own earlier statement and before any session row is gone. Sends
/// the tables written since the hook was set, then waits for `resume`.
async fn pause_at_session_revocation(
    pool: &SqlitePool,
) -> (oneshot::Receiver<Vec<String>>, std::sync::mpsc::Sender<()>) {
    let (reached_tx, reached_rx) = oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let mut pause = Some((reached_tx, resume_rx));
    let mut written = Vec::new();
    let mut connection = pool.acquire().await.unwrap();
    connection
        .lock_handle()
        .await
        .unwrap()
        .set_update_hook(move |change| {
            let revoking = change.table == "sessions"
                && matches!(change.operation, sqlx::sqlite::SqliteOperation::Delete);
            if !revoking {
                if pause.is_some() {
                    written.push(change.table.to_owned());
                }
            } else if let Some((reached, resume)) = pause.take() {
                let _ = reached.send(std::mem::take(&mut written));
                let _ = resume.recv();
            }
        });
    (reached_rx, resume_tx)
}

#[tokio::test]
async fn cancelling_password_change_after_commit_still_revokes_sessions_and_audits() {
    let (state, pool, sessions) = fixture().await;
    let initial_password = format!("initial-{}", rand::random::<u64>());
    let updated_password = format!("updated-{}-password", rand::random::<u64>());
    restream::infrastructure::bootstrap::initialize_auth_for_test(
        &pool,
        &sessions,
        &initial_password,
    )
    .await;
    let original_hash = db::get_meta(&pool, "dashboardPasswordHash")
        .await
        .unwrap()
        .unwrap();
    let (committed_rx, resume_tx) = pause_at_session_revocation(&pool).await;
    let state = Arc::new(state);
    let app = api::create_router(state.clone());
    let payload = serde_json::json!({
        "currentPassword": initial_password,
        "newPassword": updated_password,
    })
    .to_string();
    let request = tokio::spawn(app.clone().oneshot(auth_req(
        "POST",
        "/api/v1/auth/change-password",
        &cookie(CURRENT_TOKEN),
        Some(&payload),
    )));
    let written = tokio::time::timeout(Duration::from_secs(15), committed_rx)
        .await
        .expect("the request must reach session revocation")
        .unwrap();
    assert_eq!(
        written,
        ["meta"],
        "the cancellation point is after the password write committed and \
         before session invalidation removed a row"
    );

    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let _ = resume_tx.send(());

    let current_hash = token_hash(CURRENT_TOKEN);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let persisted = db::list_sessions(&pool).await.unwrap();
            let cached = sessions.read().await.clone();
            let audit = state.audit_entries(20, None).await.unwrap();
            if persisted == vec![current_hash.clone()]
                && cached == HashSet::from([current_hash.clone()])
                && audit.iter().any(|entry| {
                    entry.username == "admin"
                        && entry.method == "POST"
                        && entry.path == "/api/v1/auth/change-password"
                        && entry.status == StatusCode::OK.as_u16()
                })
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the detached mutation must finish durable/cache revocation and its success audit");
    assert_ne!(
        db::get_meta(&pool, "dashboardPasswordHash").await.unwrap(),
        Some(original_hash),
        "the password change committed"
    );

    let revoked = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines",
            &cookie(OTHER_TOKEN),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    let current = app
        .oneshot(auth_req(
            "GET",
            "/api/v1/pipelines",
            &cookie(CURRENT_TOKEN),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(current.status(), StatusCode::OK);
}
