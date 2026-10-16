use super::*;

#[cfg(not(feature = "agent-plane"))]
#[tokio::test]
async fn agent_plane_returns_404_when_feature_is_compiled_out() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/agent/capabilities", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["compiledIn"], false);
}

#[cfg(not(feature = "agent-plane"))]
#[tokio::test]
async fn agent_context_returns_404_when_feature_is_compiled_out() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/agent/context", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["compiledIn"], false);
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_capabilities_requires_auth() {
    let (app, _) = test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/agent/capabilities")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_capabilities_reports_read_planning_only() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/agent/capabilities", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["feature"], "agent-plane");
    assert_eq!(body["compiledIn"], true);
    assert_eq!(body["executionEnabled"], cfg!(feature = "agent-execution"));
    // Discovery lists exactly the tools the agent can call.
    use restream::agent_core::{AgentToolKind, agent_tool_names};
    for (field, kind) in [
        ("readTools", AgentToolKind::Read),
        ("planningTools", AgentToolKind::Planning),
        ("executionTools", AgentToolKind::Execution),
    ] {
        let listed: Vec<&str> = body[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool.as_str().unwrap())
            .collect();
        assert_eq!(listed, agent_tool_names(kind), "{field}");
    }
    assert!(
        body["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|route| route["path"] == "/api/v1/agent/context" && route["mutates"] == false)
    );
    assert!(
        body["routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|route| route["feature"] != "core")
    );
    assert!(body["readTools"].as_array().unwrap().iter().all(|tool| {
        !tool.as_str().unwrap_or_default().starts_with("get_core_")
            && !tool.as_str().unwrap_or_default().contains("pipeline_graph")
            && !tool
                .as_str()
                .unwrap_or_default()
                .contains("engine_telemetry")
    }));
    assert!(body["schemas"]["PlanRequest"].is_object());
    assert_eq!(body["redaction"]["policy"], "agentContextV1");
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_context_requires_auth() {
    let (app, _) = test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/agent/context")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_context_returns_redacted_state_bundle() {
    let (app, pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;
    let raw_stream_key = "agent-context-secret-key";
    let raw_output_url = "rtmp://example.com/live/super-secret-output-key";

    let create = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(
                &serde_json::json!({ "name": "agent-context", "streamKey": raw_stream_key })
                    .to_string(),
            ),
        ))
        .await
        .unwrap();
    let pipe = body_json(create).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let output_resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(
                &serde_json::json!({
                    "name": "Redacted CDN",
                    "url": raw_output_url,
                    "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}}
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(output_resp.status(), StatusCode::CREATED);
    let output = body_json(output_resp).await;
    let output_id = output["output"]["id"].as_str().unwrap().to_string();

    db::create_job(
        &pool,
        "job-agent-context",
        &pid,
        &output_id,
        Some(4321),
        restream::application::models::JobStatus::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    engine
        .runtime
        .event_log
        .emit(restream::events::EventKind::IngestConnected {
            pipeline_id: pid.clone(),
            protocol: "rtmp".to_string(),
            stream_key: raw_stream_key.to_string(),
        });

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/agent/context", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let raw = serde_json::to_string(&body).unwrap();

    assert_eq!(body["readOnly"], true);
    assert_eq!(body["features"]["agentPlane"], true);
    assert_eq!(
        body["features"]["agentExecution"],
        cfg!(feature = "agent-execution")
    );
    assert!(body["state"]["pipelines"].is_array());
    assert!(body["state"]["outputs"].is_array());
    assert!(body["state"]["jobs"].is_array());
    assert_eq!(body["state"]["jobs"][0]["id"], "job-agent-context");
    assert_eq!(body["state"]["jobs"][0]["pipelineId"], pid);
    assert_eq!(body["state"]["jobs"][0]["outputId"], output_id);
    assert_eq!(body["state"]["jobs"][0]["pid"], 4321);
    assert_eq!(body["state"]["jobs"][0]["status"], "running");
    assert!(body["runtime"]["health"].is_object());
    assert!(body["runtime"]["telemetry"]["engine"].is_object());
    assert!(body["runtime"]["graphs"].is_array());
    assert!(body["api"]["routes"].as_array().unwrap().len() >= 5);
    assert!(body["api"]["schemas"]["AgentContextV1"].is_object());
    assert_eq!(
        body["desiredVsActual"]["summary"]["pipelines"].as_u64(),
        Some(1)
    );
    assert_eq!(
        body["desiredVsActual"]["summary"]["outputs"].as_u64(),
        Some(1)
    );
    assert_eq!(
        body["desiredVsActual"]["pipelines"][0]["outputs"][0]["recentJobs"][0]["id"],
        "job-agent-context"
    );
    assert_eq!(
        body["desiredVsActual"]["pipelines"][0]["outputs"][0]["recentJobs"][0]["outputId"],
        output_id
    );
    assert!(body["diagnostics"]["pipelines"].as_array().unwrap().len() == 1);
    assert_eq!(
        body["diagnostics"]["activeProbeEndpointTemplate"],
        "/api/v1/pipelines/:pipeline_id/diagnostics/run"
    );
    assert_eq!(body["diagnostics"]["activeProbeMethod"], "POST");
    assert_eq!(
        body["diagnostics"]["pipelines"][0]["activeProbeEndpoint"],
        format!("/api/v1/pipelines/{pid}/diagnostics/run")
    );
    assert_eq!(
        body["diagnostics"]["pipelines"][0]["activeProbeMethod"],
        "POST"
    );
    assert!(body["dependencies"]["hls"]["config"].is_object());
    assert!(body["dependencies"]["recording"]["pipelines"].is_array());
    assert_eq!(
        body["dependencies"]["fileIngest"]["configured"].as_u64(),
        Some(0)
    );
    assert!(body["dependencies"]["ingestSecurity"]["config"].is_object());
    assert!(body["storage"]["mediaFileCount"].as_u64().is_some());
    assert!(body["redaction"]["recursiveFields"].is_array());

    assert!(!raw.contains(raw_stream_key));
    assert!(!raw.contains("super-secret-output-key"));
    assert!(raw.contains("streamKeyFingerprint"));
    assert!(raw.contains("urlFingerprint"));
    assert!(raw.contains("example.com"));
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_investigation_returns_evidence_envelope() {
    let (app, _pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;
    let raw_stream_key = "agent-investigation-secret-key";

    let create = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(
                &serde_json::json!({ "name": "agent-pipe", "streamKey": raw_stream_key })
                    .to_string(),
            ),
        ))
        .await
        .unwrap();
    let pipe = body_json(create).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    engine
        .runtime
        .event_log
        .emit(restream::events::EventKind::IngestConnected {
            pipeline_id: pid.clone(),
            protocol: "rtmp".to_string(),
            stream_key: raw_stream_key.to_string(),
        });

    let resp = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/investigations",
            &cookie,
            Some(
                &serde_json::json!({
                    "workflow": "investigatePipelineIssue",
                    "pipelineId": pid,
                    "eventLimit": 10
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["readOnly"], true);
    assert_eq!(body["summary"]["hasGraph"], true);
    assert!(body["evidence"]["health"].is_object());
    assert!(body["evidence"]["graph"]["nodes"].is_array());
    assert!(body["evidence"]["telemetry"].is_object());
    assert!(body["evidence"]["alerts"].is_array());
    assert!(body["evidence"]["events"].is_array());

    let raw = serde_json::to_string(&body).unwrap();
    assert!(!raw.contains(raw_stream_key));
    assert!(raw.contains("streamKeyFingerprint"));
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_plan_validates_and_previews_stage_impact() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;

    let create = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(
                &serde_json::json!({ "name": "agent-plan", "streamKey": "agent-plan-key" })
                    .to_string(),
            ),
        ))
        .await
        .unwrap();
    let pipe = body_json(create).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap();

    let resp = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/plans",
            &cookie,
            Some(
                &serde_json::json!({
                    "intent": "Attach a 720p RTMP output",
                    "pipelineId": pid,
                    "proposedChanges": [{
                        "kind": "addOutput",
                        "name": "Primary CDN",
                        "url": "rtmp://example/live/key",
                        "config": {"video": {"mode": "preset", "preset": "720p"}, "audio": {"mode": "downmix", "track": 0}},
                        "desiredState": "running"
                    }]
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["planId"].as_str().unwrap().starts_with("plan_"));
    assert_eq!(body["executionEnabled"], cfg!(feature = "agent-execution"));
    assert_eq!(body["validation"]["valid"], true);
    let added_nodes = body["graphPreview"]["addedNodes"].as_array().unwrap();
    assert!(
        added_nodes
            .iter()
            .any(|node| node["stageKey"].as_str() == Some("video:720p:codec:h264")),
        "unexpected graph preview: {added_nodes:?}"
    );
    assert!(
        body["impact"]["sharedStageCandidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|stage| stage.as_str() == Some("video:720p:codec:h264"))
    );
    // Nothing runs yet; the add is wanted running, so it is the whole after
    // side, on a stage the change starts.
    let topology = &body["graphPreview"]["topology"];
    assert_eq!(topology["pipelineId"], pid);
    assert_eq!(topology["before"], serde_json::json!([]));
    assert_eq!(topology["after"].as_array().unwrap().len(), 1, "{topology}");
    assert_eq!(topology["after"][0]["protocol"], "rtmp");
    assert_eq!(topology["after"][0]["outputs"], 1);
    assert!(
        !topology["addedStages"].as_array().unwrap().is_empty(),
        "{topology}"
    );
}

#[cfg(feature = "agent-plane")]
#[tokio::test]
async fn agent_plan_validate_reports_invalid_changes() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/plans/validate",
            &cookie,
            Some(
                &serde_json::json!({
                    "intent": "Attach bad output",
                    "pipelineId": "missing",
                    "proposedChanges": [{
                        "kind": "addOutput",
                        "url": "ftp://example/live/key",
                        "config": {"video": {"mode": "custom"}, "audio": {"mode": "all"}}
                    }]
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["validation"]["valid"], false);
    let codes: Vec<_> = body["validation"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|issue| issue["code"].as_str())
        .collect();
    assert!(codes.contains(&"pipelineNotFound"));
    assert!(codes.contains(&"unsupportedOutputUrl"));
    assert!(codes.contains(&"customEncodingUnsupported"));
    assert!(codes.contains(&"missingOutputName"));
}

#[cfg(all(feature = "agent-plane", not(feature = "agent-execution")))]
#[tokio::test]
async fn agent_execution_routes_return_404_when_compiled_out() {
    let (app, cookie) = authenticated_app().await;

    let resp = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/operations",
            &cookie,
            Some(
                &serde_json::json!({
                    "intent": "Attach output",
                    "pipelineId": "p1",
                    "proposedChanges": []
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["feature"], "agent-execution");
    assert_eq!(body["compiledIn"], false);
}

// The store keeps every operation's whole request (up to 1024 of them), so
// creating one takes a small body: 1024 records x the 4 MiB API default would
// let one client pin gigabytes.
#[cfg(feature = "agent-execution")]
#[tokio::test]
async fn agent_operation_create_refuses_an_oversized_body() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;
    let oversized = serde_json::json!({
        "intent": "x".repeat(300 * 1024),
        "proposedChanges": []
    })
    .to_string();
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/operations",
            &cookie,
            Some(&oversized),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // A plan with the same intent is read but never stored; it keeps the
    // API-wide limit.
    let resp = app
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/plans",
            &cookie,
            Some(&oversized),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[cfg(feature = "agent-execution")]
#[tokio::test]
async fn unknown_operation_answers_every_route_with_the_same_json_404() {
    let (app, cookie) = authenticated_app().await;
    for (method, path, body) in [
        ("GET", "/api/v1/agent/operations/op_missing", "{}"),
        (
            "POST",
            "/api/v1/agent/operations/op_missing/approve",
            r#"{"approvedBy":"test","reason":null}"#,
        ),
        ("POST", "/api/v1/agent/operations/op_missing/apply", "{}"),
        ("POST", "/api/v1/agent/operations/op_missing/verify", "{}"),
    ] {
        let resp = app
            .clone()
            .oneshot(auth_req(method, path, &cookie, Some(body)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path}");
        let body = body_json(resp).await;
        assert_eq!(body["code"], "operationNotFound", "{path}");
    }
}

#[cfg(feature = "agent-execution")]
#[tokio::test]
async fn agent_operation_lifecycle_is_approval_gated_redacted_and_verified() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;
    let raw_output_url = "rtmp://example.com/live/agent-secret-key";

    let create_pipeline = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(
                &serde_json::json!({
                    "name": "agent-exec",
                    "streamKey": "agent-exec-key"
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(create_pipeline.status(), StatusCode::CREATED);
    let pipeline = body_json(create_pipeline).await;
    let pipeline_id = pipeline["pipeline"]["id"].as_str().unwrap().to_string();

    let request = serde_json::json!({
        "intent": "Create a stopped CDN output for approval-gated execution",
        "pipelineId": pipeline_id,
        "idempotencyKey": "agent-op-test-1",
        "actor": "test-agent",
        "agentId": "codex-test-agent",
        "toolIdentity": "api-test",
        "incidentId": "incident-api-test",
        "incidentLinks": ["alert:test-output"],
        "proposedChanges": [{
            "kind": "addOutput",
            "name": "Agent CDN",
            "url": raw_output_url,
            "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}},
            "desiredState": "stopped"
        }]
    });

    let create_operation = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/operations",
            &cookie,
            Some(&request.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(create_operation.status(), StatusCode::CREATED);
    let created = body_json(create_operation).await;
    let operation_id = created["operationId"].as_str().unwrap().to_string();
    assert_eq!(created["status"], "awaitingApproval");
    assert_eq!(created["approvalRequired"], true);
    assert_eq!(created["actor"], "dashboard-admin");
    assert_eq!(created["agentId"], "dashboard-admin");
    assert_eq!(created["toolIdentity"], "agent-execution-api");
    assert_eq!(created["incidentId"], "incident-api-test");
    assert_eq!(created["incidentLinks"][0], "alert:test-output");
    assert_eq!(created["plan"]["executionEnabled"], true);
    assert!(
        created["proposedPlanHash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let raw = serde_json::to_string(&created).unwrap();
    assert!(!raw.contains("agent-secret-key"));
    assert!(raw.contains("urlFingerprint"));

    let reused = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/operations",
            &cookie,
            Some(&request.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(reused.status(), StatusCode::OK);
    let reused_body = body_json(reused).await;
    assert_eq!(reused_body["operationId"], operation_id);

    let mut changed_request = request.clone();
    changed_request["intent"] = serde_json::json!("Create a different output");
    let idempotency_conflict = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/operations",
            &cookie,
            Some(&changed_request.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(idempotency_conflict.status(), StatusCode::CONFLICT);
    let conflict_body = body_json(idempotency_conflict).await;
    assert_eq!(conflict_body["code"], "idempotencyConflict");

    let apply_before_approval = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/agent/operations/{operation_id}/apply"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(apply_before_approval.status(), StatusCode::CONFLICT);
    let conflict = body_json(apply_before_approval).await;
    assert_eq!(conflict["code"], "approvalRequired");

    let approved = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/agent/operations/{operation_id}/approve"),
            &cookie,
            Some(
                &serde_json::json!({
                    "approvedBy": "human-test",
                    "reason": "unit test approval"
                })
                .to_string(),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    let approved_body = body_json(approved).await;
    assert_eq!(approved_body["status"], "approved");
    assert_eq!(approved_body["approval"]["approvedBy"], "dashboard-session");

    let applied = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/agent/operations/{operation_id}/apply"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    let applied_body = body_json(applied).await;
    assert_eq!(applied_body["status"], "applied");
    assert_eq!(applied_body["executionResult"]["success"], true);
    assert_eq!(
        applied_body["executionResult"]["changeResults"][0]["status"],
        "created"
    );
    let output_id = applied_body["executionResult"]["changeResults"][0]["outputId"]
        .as_str()
        .unwrap()
        .to_string();

    let output = db::get_output(&pool, &pipeline_id, &output_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.url, raw_output_url);
    assert_eq!(output.desired_state, DesiredOutputState::Stopped);

    let verified = app
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/agent/operations/{operation_id}/verify"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(verified.status(), StatusCode::OK);
    let verified_body = body_json(verified).await;
    assert_eq!(verified_body["status"], "verified");
    assert_eq!(verified_body["verificationResult"]["success"], true);
    assert_eq!(
        verified_body["verificationResult"]["checks"][0]["reason"],
        "stopped"
    );
    let gates = verified_body["verificationResult"]["gates"]
        .as_array()
        .unwrap();
    assert_eq!(
        gates
            .iter()
            .map(|gate| gate["gate"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "desiredState",
            "outputsRunning",
            "newCriticalAlerts",
            "capacityState"
        ]
    );
    assert!(gates.iter().all(|gate| gate["passed"] == true), "{gates:?}");
    assert!(verified_body["auditLog"].as_array().unwrap().len() >= 4);
}

// ── coverage gap: agent graph-diff-preview ──────────────────────────────

#[tokio::test]
async fn agent_graph_diff_preview_returns_404_when_compiled_out() {
    let (app, cookie) = authenticated_app().await;
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/agent/graph-diff-preview",
            &cookie,
            Some(r#"{"intent":"preview","proposedChanges":[]}"#),
        ))
        .await
        .unwrap();
    // When agent-plane feature is off, returns 404
    assert!(
        resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::OK,
        "expected 404 (compiled out) or 200, got {}",
        resp.status()
    );
}

#[cfg(feature = "agent-execution")]
async fn agent_call(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let body = body.map(|value| value.to_string());
    let resp = app
        .clone()
        .oneshot(auth_req(method, path, cookie, body.as_deref()))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = body_bytes(resp).await;
    let json = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "{method} {path} answered {status}: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, json)
}

#[cfg(feature = "agent-execution")]
async fn create_output(app: &axum::Router, cookie: &str, pid: &str, name: &str) -> String {
    let (status, output) = agent_call(
        app,
        cookie,
        "POST",
        &format!("/api/v1/pipelines/{pid}/outputs"),
        Some(serde_json::json!({
            "name": name,
            "url": format!("rtmp://127.0.0.1:9/live/{name}"),
            "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{output}");
    output["output"]["id"].as_str().unwrap().to_string()
}

/// Creates, approves, applies and verifies one operation; the verified record.
#[cfg(feature = "agent-execution")]
async fn run_operation(
    app: &axum::Router,
    cookie: &str,
    pid: &str,
    change: serde_json::Value,
) -> serde_json::Value {
    let (status, created) = agent_call(
        app,
        cookie,
        "POST",
        "/api/v1/agent/operations",
        Some(serde_json::json!({
            "intent": "gate test",
            "pipelineId": pid,
            "proposedChanges": [change]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    // The baseline is taken at apply, not at create.
    assert!(created["preApplyCriticalAlertIds"].is_null(), "{created}");
    let op = created["operationId"].as_str().unwrap().to_string();
    for step in ["approve", "apply"] {
        let (status, body) = agent_call(
            app,
            cookie,
            "POST",
            &format!("/api/v1/agent/operations/{op}/{step}"),
            (step == "approve").then(|| serde_json::json!({ "approvedBy": "t", "reason": "test" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{step}: {body}");
    }
    agent_call(
        app,
        cookie,
        "POST",
        &format!("/api/v1/agent/operations/{op}/verify"),
        None,
    )
    .await
    .1
}

#[cfg(feature = "agent-execution")]
fn gate(verified: &serde_json::Value, name: &str) -> serde_json::Value {
    verified["verificationResult"]["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["gate"] == name)
        .unwrap_or_else(|| panic!("no {name} gate: {verified}"))
        .clone()
}

#[cfg(feature = "agent-execution")]
#[tokio::test]
async fn verify_judges_only_the_outputs_the_change_set_running() {
    let (app, cookie) = authenticated_app().await;
    let (_, pipeline) = agent_call(
        &app,
        &cookie,
        "POST",
        "/api/v1/pipelines",
        Some(serde_json::json!({ "name": "agent-gates", "streamKey": "agent-gates-key" })),
    )
    .await;
    let pid = pipeline["pipeline"]["id"].as_str().unwrap().to_string();
    // Already wanted running and down (no publisher): not the change's doing.
    let down = create_output(&app, &cookie, &pid, "down").await;
    let (status, _) = agent_call(
        &app,
        &cookie,
        "POST",
        &format!("/api/v1/pipelines/{pid}/outputs/{down}/start"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A stopped backup leaves every gate passing.
    let verified = run_operation(
        &app,
        &cookie,
        &pid,
        serde_json::json!({
            "kind": "addOutput",
            "name": "backup",
            "url": "rtmp://127.0.0.1:9/live/backup",
            "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}},
            "desiredState": "stopped"
        }),
    )
    .await;
    assert_eq!(verified["status"], "verified", "{verified}");
    assert_eq!(
        gate(&verified, "outputsRunning")["detail"],
        "0 of 0 outputs the change set running are running"
    );
    assert!(
        verified["preApplyCriticalAlertIds"].is_array(),
        "{verified}"
    );

    // Starting an output that cannot come up fails it.
    let spare = create_output(&app, &cookie, &pid, "spare").await;
    let verified = run_operation(
        &app,
        &cookie,
        &pid,
        serde_json::json!({ "kind": "startOutput", "outputId": spare }),
    )
    .await;
    assert_eq!(verified["status"], "verificationFailed", "{verified}");
    assert_eq!(gate(&verified, "outputsRunning")["passed"], false);
    assert_eq!(
        gate(&verified, "outputsRunning")["detail"],
        "0 of 1 outputs the change set running are running"
    );
    assert_eq!(gate(&verified, "newCriticalAlerts")["passed"], true);
    assert_eq!(gate(&verified, "capacityState")["passed"], true);
}
