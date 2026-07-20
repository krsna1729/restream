use super::*;

#[tokio::test]
async fn base_path_script_is_served_as_static_asset() {
    let (app, _) = test_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/base-path.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/javascript"))
    );
    let body = String::from_utf8(body_bytes(resp).await.to_vec()).unwrap();
    assert!(body.contains("__RESTREAM_BASE_PATH__"));
}

#[tokio::test]
async fn static_assets_use_cache_validators() {
    let (app, _) = test_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/base-path.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("public, max-age=3600")
    );
    let etag = resp.headers().get(header::ETAG).cloned().unwrap();
    assert!(!body_bytes(resp).await.is_empty());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/base-path.js")
                .header(header::IF_NONE_MATCH, etag)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    assert!(body_bytes(resp).await.is_empty());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-cache")
    );
    assert!(resp.headers().contains_key(header::ETAG));
}

#[tokio::test]
async fn login_page_uses_base_path_aware_api_and_redirects() {
    let (app, _) = test_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(body_bytes(resp).await.to_vec()).unwrap();
    assert!(body.contains(r#"<script src="base-path.js"></script>"#));
    assert!(body.contains(r#"<script src="login.js"></script>"#));
    assert!(!body.contains(r#"onclick="loginBtn()"#));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = String::from_utf8(body_bytes(resp).await.to_vec()).unwrap();
    assert!(body.contains(r#"fetch(withBasePath("/api/v1/auth/login")"#));
    assert!(body.contains(r#"const fallbackReturnPath = () => withBasePath("/")"#));
    assert!(body.contains(r#"window.location.href = safeReturnPath(returnPath)"#));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login.html")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("login")
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/settings.html")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("./?mode=settings")
    );
}

#[tokio::test]
async fn secure_session_cookies_are_opt_in() {
    let (default_app, _) = test_app().await;
    let default_resp = default_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"admin"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let default_cookie = default_resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(!default_cookie.contains("; Secure"));

    let secure_app = test_app_with_secure_cookies().await;
    let secure_resp = secure_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"admin"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let secure_cookie = secure_resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(secure_cookie.contains("; Secure"));
}

#[tokio::test]
async fn healthz_no_auth() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn login_wrong_password() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rate_limit_state_lists_and_resets_failed_auth_attempts() {
    let (app, _) = test_app().await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let cookie = login(&app).await;
    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/security/rate-limits",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let attempts = json["attempts"].as_array().unwrap();
    assert!(attempts.iter().any(|attempt| {
        attempt["scope"] == "dashboard-login"
            && attempt["ip"] == "unknown"
            && attempt["failureCount"].as_u64() == Some(1)
            && attempt["banned"] == false
    }));

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/security/rate-limits/reset",
            &cookie,
            Some(r#"{"scope":"dashboard-login","ip":"unknown"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["removed"], 1);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/security/rate-limits",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    let json = body_json(resp).await;
    assert!(json["attempts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn login_success_and_logout() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;
    assert!(cookie.starts_with("session="));

    let resp = app
        .clone()
        .oneshot(auth_req("POST", "/api/v1/auth/logout", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn password_change_revokes_other_sessions_and_keeps_current_session() {
    let (app, _) = test_app().await;
    let current_cookie = login(&app).await;
    let other_cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/auth/change-password",
            &current_cookie,
            Some(r#"{"current_password":"admin","new_password":"newpass12345"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &current_cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &other_cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"admin"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let new_cookie = login_with_password(&app, "newpass12345").await;
    assert!(new_cookie.starts_with("session="));
}

#[tokio::test]
async fn unauthenticated_returns_401() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/settings")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn representative_authenticated_routes_reject_missing_session() {
    let (app, _, engine) = test_app_with_engine().await;
    engine.get_or_create_hls_store("auth_pipe").await;

    for (method, uri, body) in [
        ("GET", "/api/v1/settings", None),
        ("GET", "/api/v1/security/rate-limits", None),
        ("POST", "/api/v1/security/rate-limits/reset", Some("{}")),
        ("GET", "/api/v1/audio-caps", None),
        ("GET", "/api/v1/stream-keys", None),
        ("GET", "/api/v1/dashboard/runtime", None),
        ("GET", "/api/v1/pipelines", None),
        ("GET", "/api/v1/logs", None),
        ("GET", "/api/v1/agent/context", None),
        ("GET", "/api/v1/engine", None),
        ("GET", "/api/v1/media", None),
        ("GET", "/media/missing.ts", None),
        ("GET", "/hls/auth_pipe/index.m3u8", None),
    ] {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("Content-Type", "application/json");
        }
        let resp = app
            .clone()
            .oneshot(
                builder
                    .body(axum::body::Body::from(body.unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn unauthenticated_app_pages_redirect_to_login() {
    let (app, _) = test_app().await;
    for uri in ["/"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_redirection(), "{uri} should redirect");
        assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "login");
    }
}

#[tokio::test]
async fn unauthenticated_static_assets_remain_available() {
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/base-path.js")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn change_password() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Change password
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/auth/change-password",
            &cookie,
            Some(r#"{"current_password":"admin","new_password":"newpass12345"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Old password should fail
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"admin"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // New password should work
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(r#"{"password":"newpass12345"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn change_password_rejects_short_new_password() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/auth/change-password",
            &cookie,
            Some(r#"{"current_password":"admin","new_password":"short"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_eq!(json["error"], "New password must be at least 12 characters");
}

// --- Regression: Round 6 #2 — Security headers ---

#[tokio::test]
async fn security_headers_present_on_api_response() {
    // Every API response must carry X-Content-Type-Options and X-Frame-Options
    // to defend against MIME-sniffing and clickjacking (Round 6 finding #2).
    let (app, _) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .map(|v| v.as_bytes()),
        Some(b"nosniff" as &[u8]),
        "X-Content-Type-Options: nosniff must be present"
    );
    assert_eq!(
        resp.headers().get("x-frame-options").map(|v| v.as_bytes()),
        Some(b"SAMEORIGIN" as &[u8]),
        "X-Frame-Options: SAMEORIGIN must be present"
    );
}

#[tokio::test]
async fn security_headers_present_on_hls_response_without_wildcard_cors() {
    let (app, _, engine) = test_app_with_engine().await;
    engine.get_or_create_hls_store("test_pipe").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/hls/test_pipe")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .map(|v| v.as_bytes()),
        Some(b"nosniff" as &[u8])
    );
    assert_eq!(
        resp.headers().get("x-frame-options").map(|v| v.as_bytes()),
        Some(b"SAMEORIGIN" as &[u8])
    );
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "cookie-authenticated HLS should not advertise wildcard CORS"
    );
}

#[tokio::test]
async fn unknown_api_paths_return_json_not_spa_html() {
    let (app, _) = test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/not-a-real-route")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"))
    );
    let body = body_json(resp).await;
    assert_eq!(body["error"], "API route not found");
    assert_eq!(body["path"], "/api/v1/not-a-real-route");
    assert_eq!(body["status"], 404);
}
