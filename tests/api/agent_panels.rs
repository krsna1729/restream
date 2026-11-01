//! The MCP panels' payload contract against the product's real results: every
//! tool that names a panel returns, through the API it is served from, every
//! field the panel declares (`restream_agent_contract::panels`).

use super::*;
use restream_agent_contract::AGENT_TOOLS;
use std::collections::HashMap;

async fn call(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> serde_json::Value {
    let body = body.map(|value| value.to_string());
    let response = app
        .clone()
        .oneshot(auth_req(method, path, cookie, body.as_deref()))
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{method} {path} answered {}",
        response.status()
    );
    body_json(response).await
}

#[tokio::test]
async fn every_panelled_tool_result_carries_its_panel_fields() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;
    let pipeline = call(
        &app,
        &cookie,
        "POST",
        "/api/v1/pipelines",
        Some(serde_json::json!({ "name": "panel-contract", "streamKey": "panel-contract-key" })),
    )
    .await;
    let pipeline_id = pipeline["pipeline"]["id"].as_str().unwrap().to_string();
    call(
        &app,
        &cookie,
        "POST",
        &format!("/api/v1/pipelines/{pipeline_id}/outputs"),
        Some(serde_json::json!({
            "name": "existing",
            "url": "rtmp://127.0.0.1:9/live/existing",
            "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}}
        })),
    )
    .await;
    let created = call(
        &app,
        &cookie,
        "POST",
        "/api/v1/agent/operations",
        Some(serde_json::json!({
            "intent": "add a backup output",
            "pipelineId": pipeline_id,
            "proposedChanges": [{
                "kind": "addOutput",
                "name": "backup",
                "url": "rtmp://127.0.0.1:9/live/backup",
                "config": {"video": {"mode": "source"}, "audio": {"mode": "all"}}
            }]
        })),
    )
    .await;
    let operation_id = created["operationId"].as_str().unwrap().to_string();

    // Each tool's result, as the HTTP route the MCP sidecar calls returns it.
    let results: HashMap<&str, serde_json::Value> = HashMap::from([
        (
            "get_agent_context",
            call(&app, &cookie, "GET", "/api/v1/agent/context", None).await,
        ),
        ("create_agent_operation", created),
        (
            "get_agent_operation",
            call(
                &app,
                &cookie,
                "GET",
                &format!("/api/v1/agent/operations/{operation_id}"),
                None,
            )
            .await,
        ),
    ]);

    for tool in AGENT_TOOLS {
        let Some(panel) = tool.panel else {
            continue;
        };
        let result = results.get(tool.name).unwrap_or_else(|| {
            panic!(
                "{} names {} but has no contract result here",
                tool.name, panel.uri
            )
        });
        assert_eq!(
            panel.mismatches(result),
            Vec::<String>::new(),
            "{} result does not fit {}",
            tool.name,
            panel.uri
        );
    }

    // The rows the panels show are populated here (the pipeline has no
    // publisher, which raises an alert), so their columns were checked
    // against real rows rather than passing on empty lists.
    let context = &results["get_agent_context"];
    assert_eq!(
        context["desiredVsActual"]["pipelines"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        !context["runtime"]["alerts"].as_array().unwrap().is_empty(),
        "{}",
        context["runtime"]["alerts"]
    );
    let operation = &results["get_agent_operation"];
    assert_eq!(
        operation["request"]["proposedChanges"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(operation["status"], "awaitingApproval");
}
