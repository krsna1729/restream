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
    (
        status,
        serde_json::from_slice(&body_bytes(resp).await).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn a_schedule_lists_its_next_time_and_runs_its_action_on_demand() {
    let (app, cookie) = authenticated_app().await;
    let (_, created) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &cookie,
        Some(r#"{"name":"Show","streamKey":"key_sched_123456789"}"#),
    )
    .await;
    let pipeline = created["pipeline"]["id"].as_str().unwrap().to_string();
    let mut outputs = Vec::new();
    for name in ["A", "B"] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/v1/pipelines/{pipeline}/outputs"),
            &cookie,
            Some(&format!(r#"{{"name":"{name}","url":"rtmp://127.0.0.1:9/live/{name}","config":{{"video":{{"mode":"source"}},"audio":{{"mode":"all"}}}}}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        outputs.push(body["output"]["id"].as_str().unwrap().to_string());
    }

    let at = chrono::Utc::now().timestamp_millis() + 3_600_000;
    let draft = serde_json::json!({
        "pipelineId": pipeline,
        "name": "Go live on A",
        "action": "startOutputs",
        "outputIds": [outputs[0]],
        "atMs": at,
        "repeat": "daily"
    });
    let (status, schedule) = call(
        &app,
        "POST",
        "/api/v1/schedules",
        &cookie,
        Some(&draft.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{schedule}");
    assert_eq!(schedule["nextAtMs"], at);
    assert_eq!(schedule["lastRunMs"], serde_json::Value::Null);
    let id = schedule["id"].as_str().unwrap().to_string();

    let (status, run) = call(
        &app,
        "POST",
        &format!("/api/v1/schedules/{id}/run"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["result"], "1 of 1 destinations wanted running");
    let (_, settings) = call(&app, "GET", "/api/v1/settings", &cookie, None).await;
    let desired: Vec<_> = settings["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|output| {
            (
                output["name"].as_str().unwrap().to_string(),
                output["desiredState"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        desired.contains(&("A".into(), "running".into())),
        "{desired:?}"
    );
    assert!(
        desired.contains(&("B".into(), "stopped".into())),
        "only the named destination"
    );

    let (_, all) = call(&app, "GET", "/api/v1/schedules", &cookie, None).await;
    assert_eq!(all["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/v1/schedules/{id}"),
            &cookie,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn schedules_are_validated_and_need_an_operator_to_change() {
    let (app, admin) = authenticated_app().await;
    let (_, created) = call(
        &app,
        "POST",
        "/api/v1/pipelines",
        &admin,
        Some(r#"{"name":"P","streamKey":"key_sched_223456789"}"#),
    )
    .await;
    let pipeline = created["pipeline"]["id"].as_str().unwrap().to_string();
    let file_with_outputs = serde_json::json!({
        "pipelineId": pipeline, "name": "x", "action": "startFile", "outputIds": ["o"], "atMs": 1, "repeat": "once"
    });
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/schedules",
            &admin,
            Some(&file_with_outputs.to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let unknown_pipeline = serde_json::json!({
        "pipelineId": "nope", "name": "x", "action": "stopOutputs", "atMs": 1, "repeat": "once"
    });
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/schedules",
            &admin,
            Some(&unknown_pipeline.to_string())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // As a viewer: read, but not change.
    call(
        &app,
        "POST",
        "/api/v1/auth/view-as",
        &admin,
        Some(r#"{"role":"viewer"}"#),
    )
    .await;
    assert_eq!(
        call(&app, "GET", "/api/v1/schedules", &admin, None).await.0,
        StatusCode::OK
    );
    let ok_draft = serde_json::json!({
        "pipelineId": pipeline, "name": "x", "action": "stopOutputs", "atMs": 1, "repeat": "once"
    });
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/schedules",
            &admin,
            Some(&ok_draft.to_string())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
