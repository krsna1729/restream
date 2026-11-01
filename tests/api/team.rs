use super::*;

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(auth_req(method, uri, cookie, body))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = body_bytes(resp).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn member_login(app: &axum::Router, username: &str, password: &str) -> Option<String> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({ "username": username, "password": password }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    (resp.status() == StatusCode::OK).then(|| {
        resp.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    })
}

async fn add_member(app: &axum::Router, admin: &str, username: &str, role: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/users",
        admin,
        Some(&serde_json::json!({ "username": username, "password": "correct-horse-1", "role": role }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn roles_gate_reads_and_changes_by_the_route_policy() {
    let (app, admin) = authenticated_app().await;
    add_member(&app, &admin, "watcher", "viewer").await;
    add_member(&app, &admin, "runner", "operator").await;
    let viewer = member_login(&app, "watcher", "correct-horse-1")
        .await
        .unwrap();
    let operator = member_login(&app, "runner", "correct-horse-1")
        .await
        .unwrap();

    let (status, me) = call(&app, "GET", "/api/v1/auth/me", &viewer, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (me["username"].as_str(), me["role"].as_str()),
        (Some("watcher"), Some("viewer"))
    );

    // A viewer reads the plant but changes nothing.
    assert_eq!(
        call(&app, "GET", "/api/v1/pipelines", &viewer, None)
            .await
            .0,
        StatusCode::OK
    );
    let create = Some(r#"{"name":"from viewer"}"#);
    let (status, body) = call(&app, "POST", "/api/v1/pipelines", &viewer, create).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["requiredRole"], "operator");
    assert_eq!(
        call(&app, "GET", "/api/v1/settings", &viewer, None).await.0,
        StatusCode::FORBIDDEN
    );

    // An operator runs the plant but cannot manage settings or the team.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &operator,
        Some(r#"{"name":"from operator"}"#),
    )
    .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(
        call(&app, "GET", "/api/v1/settings", &operator, None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "PATCH", "/api/v1/settings", &operator, Some("{}"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/users", &operator, None).await.0,
        StatusCode::FORBIDDEN
    );

    // Without a session the handler's own 401 still answers.
    assert_eq!(
        call(&app, "POST", "/api/v1/pipelines", "session=nope", create)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn viewing_as_a_lower_role_is_enforced_until_exited() {
    let (app, admin) = authenticated_app().await;
    let (status, me) = call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        &admin,
        Some(r#"{"role":"viewer"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (me["role"].as_str(), me["effectiveRole"].as_str()),
        (Some("admin"), Some("viewer"))
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/pipelines",
            &admin,
            Some(r#"{"name":"x"}"#)
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "the preview is truthful: the server refuses what a viewer cannot do"
    );
    assert_eq!(
        call(&app, "GET", "/api/v1/users", &admin, None).await.0,
        StatusCode::FORBIDDEN
    );
    // Exiting the view is always allowed.
    let (status, me) = call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        &admin,
        Some(r#"{"role":null}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["effectiveRole"], "admin");
    assert_eq!(
        call(&app, "GET", "/api/v1/users", &admin, None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn disabling_a_member_ends_their_sessions_and_logins() {
    let (app, admin) = authenticated_app().await;
    let id = add_member(&app, &admin, "temp", "operator").await;
    let session = member_login(&app, "temp", "correct-horse-1").await.unwrap();
    assert_eq!(member_login(&app, "temp", "wrong-password-1").await, None);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/v1/users/{id}"),
        &admin,
        Some(r#"{"disabled":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        call(&app, "GET", "/api/v1/auth/me", &session, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(member_login(&app, "temp", "correct-horse-1").await, None);
}

#[tokio::test]
async fn every_change_is_audited_with_who_and_how_it_ended() {
    let (app, admin) = authenticated_app().await;
    add_member(&app, &admin, "watcher", "viewer").await;
    let viewer = member_login(&app, "watcher", "correct-horse-1")
        .await
        .unwrap();
    call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &viewer,
        Some(r#"{"name":"nope"}"#),
    )
    .await;
    let (status, body) = call(&app, "GET", "/api/v1/audit?limit=10", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = body["entries"].as_array().unwrap();
    let summary: Vec<_> = entries
        .iter()
        .map(|e| {
            format!(
                "{} {} {} {}",
                e["username"].as_str().unwrap(),
                e["method"].as_str().unwrap(),
                e["path"].as_str().unwrap(),
                e["status"]
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            "watcher POST /api/v1/pipelines 403",
            "admin POST /api/v1/users 201"
        ],
        "reads are not audited; refusals are"
    );
}

/// Each change of an agent operation is approved by a member whose role
/// reaches the change's; a lower role is refused and audited, and the
/// operation is approved with its last change.
#[cfg(feature = "agent-execution")]
#[tokio::test]
async fn an_operation_change_is_approved_only_from_its_role_up() {
    let (app, admin) = authenticated_app().await;
    let (_, pipe) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(r#"{"name":"p"}"#),
    )
    .await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();
    let mut outputs = Vec::new();
    for name in ["keep", "drop"] {
        let body = serde_json::json!({
            "name": name,
            "url": format!("rtmp://127.0.0.1:9/live/{name}"),
            "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}}
        });
        let path = format!("/api/v1/pipelines/{pid}/outputs");
        let (status, output) = call(&app, "POST", &path, &admin, Some(&body.to_string())).await;
        assert_eq!(status, StatusCode::CREATED, "{output}");
        outputs.push(output["output"]["id"].as_str().unwrap().to_string());
    }
    add_member(&app, &admin, "ops", "operator").await;
    let operator = member_login(&app, "ops", "correct-horse-1").await.unwrap();

    let request = serde_json::json!({
        "intent": "stop one destination, remove another",
        "pipelineId": pid,
        "proposedChanges": [
            {"kind": "stopOutput", "pipelineId": pid, "outputId": outputs[0]},
            {"kind": "removeOutput", "pipelineId": pid, "outputId": outputs[1]}
        ]
    });
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/agent/operations",
        &operator,
        Some(&request.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"], "awaitingApproval", "{created}");
    assert_eq!(created["actions"][0]["requiredRole"], "operator");
    assert_eq!(created["actions"][1]["requiredRole"], "admin");
    let id = created["operationId"].as_str().unwrap();
    let action = |index: usize| format!("/api/v1/agent/operations/{id}/actions/{index}/approve");
    let whole = format!("/api/v1/agent/operations/{id}/approve");

    let (status, refused) = call(&app, "POST", &action(1), &operator, Some("{}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(
        (
            refused["requiredRole"].as_str(),
            refused["effectiveRole"].as_str()
        ),
        (Some("admin"), Some("operator"))
    );
    let claimed = r#"{"approvedBy":"ops"}"#;
    assert_eq!(
        call(&app, "POST", &whole, &operator, Some(claimed)).await.0,
        StatusCode::FORBIDDEN,
        "approving everything at once needs the most demanding change's role"
    );

    let (status, first) = call(&app, "POST", &action(0), &operator, Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["status"], "awaitingApproval");
    assert_eq!(first["actions"][0]["approval"]["approvedBy"], "ops");

    // Viewing as an operator approves as one, even for an admin.
    let view = |role: &'static str| format!(r#"{{"role":{role}}}"#);
    call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        &admin,
        Some(&view(r#""operator""#)),
    )
    .await;
    assert_eq!(
        call(&app, "POST", &action(1), &admin, Some("{}")).await.0,
        StatusCode::FORBIDDEN
    );
    call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        &admin,
        Some(&view("null")),
    )
    .await;

    let (status, last) = call(
        &app,
        "POST",
        &action(1),
        &admin,
        Some(r#"{"reason":"retired"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{last}");
    assert_eq!(last["status"], "approved");
    assert_eq!(last["approval"]["approvedBy"], "admin");
    assert_eq!(last["actions"][1]["approval"]["role"], "admin");

    let (_, audit) = call(&app, "GET", "/api/v1/audit?limit=20", &admin, None).await;
    let refusals: Vec<_> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["status"] == 403)
        .map(|e| {
            format!(
                "{} {}",
                e["username"].as_str().unwrap(),
                e["path"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        refusals,
        [
            format!("admin {}", action(1)),
            format!("ops {whole}"),
            format!("ops {}", action(1))
        ],
        "each refused approval is audited, newest first"
    );
}
