mod support;

use axum::http::StatusCode;
use restream::db;
use tower::ServiceExt;

use support::{authenticated_app, json_request, response_json};

async fn create_pipeline(app: &axum::Router, cookie: &str) -> String {
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            "/api/v1/pipelines",
            Some(cookie),
            Some(r#"{"name":"Event"}"#),
        ))
        .await
        .expect("pipeline response");
    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await["pipeline"]["id"]
        .as_str()
        .expect("pipeline id")
        .to_string()
}

#[tokio::test]
async fn pipeline_inputs_list_exposes_primary_ingest_urls() {
    let (app, pool, cookie) = authenticated_app().await;
    db::set_ingest_host(&pool, "ingest.example.com")
        .await
        .expect("ingest host");
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let response = app
        .clone()
        .oneshot(json_request(
            "GET",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("inputs response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let inputs = body["inputs"].as_array().expect("inputs array");
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0]["role"], "primary");
    assert_eq!(inputs[0]["selected"], true);
    assert_eq!(inputs[0]["runtime"]["connected"], false);
    assert_eq!(
        inputs[0]["previewUrl"],
        format!(
            "/hls/inputs/{}/master.m3u8",
            inputs[0]["id"].as_str().expect("input id")
        )
    );
    let key = inputs[0]["streamKey"].as_str().expect("stream key");
    assert_eq!(
        inputs[0]["ingestUrls"]["rtmp"],
        format!("rtmp://ingest.example.com:1935/live/{key}")
    );
    assert_eq!(
        inputs[0]["ingestUrls"]["srt"],
        format!("srt://ingest.example.com:10080?streamid=publish:{key}")
    );
}

#[tokio::test]
async fn adding_inputs_enforces_pipeline_limit_and_unique_credentials() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;

    let mut keys = Vec::new();
    for label in ["Encoder B", "Encoder C", "Encoder D"] {
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
                Some(&cookie),
                Some(&serde_json::json!({ "label": label }).to_string()),
            ))
            .await
            .expect("create input response");
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = response_json(response).await;
        keys.push(
            body["input"]["streamKey"]
                .as_str()
                .expect("stream key")
                .to_string(),
        );
    }

    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 3);
    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            Some(r#"{"label":"Encoder E"}"#),
        ))
        .await
        .expect("limit response");
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn promotion_moves_selection_without_changing_input_role() {
    let (app, _, cookie) = authenticated_app().await;
    let pipeline_id = create_pipeline(&app, &cookie).await;
    let create_response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            Some(&cookie),
            Some(r#"{"label":"Backup encoder"}"#),
        ))
        .await
        .expect("create input response");
    let created = response_json(create_response).await;
    let input_id = created["input"]["id"]
        .as_str()
        .expect("input id")
        .to_string();

    let response = app
        .clone()
        .oneshot(json_request(
            "POST",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs/{input_id}/promote"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("promotion response");

    assert_eq!(response.status(), StatusCode::OK);
    let promoted = response_json(response).await;
    assert_eq!(promoted["input"]["selected"], true);
    assert_eq!(promoted["input"]["role"], "backup");
    assert_eq!(promoted["connected"], false);
    let delete_response = app
        .clone()
        .oneshot(json_request(
            "DELETE",
            &format!("/api/v1/pipelines/{pipeline_id}/inputs/{input_id}"),
            Some(&cookie),
            None,
        ))
        .await
        .expect("delete response");
    assert_eq!(delete_response.status(), StatusCode::CONFLICT);
}
