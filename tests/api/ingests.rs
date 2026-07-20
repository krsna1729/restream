use super::*;

#[tokio::test]
async fn ingest_crud_via_api() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Create
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/ingests",
            &cookie,
            Some(r#"{"filename":"test.mp4","streamKey":"key01","loopFlag":true}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let id = json["id"].as_str().unwrap().to_string();
    assert_eq!(json["filename"], "test.mp4");
    assert_eq!(json["loop"], true);

    // List
    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/ingests", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json.as_array().unwrap().len(), 1);

    // Delete
    let uri = format!("/api/v1/ingests/{}", id);
    let resp = app
        .clone()
        .oneshot(auth_req("DELETE", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn pipeline_file_ingest_is_scoped_to_pipeline_stream_key() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let create_pipeline = serde_json::json!({
        "name": "File Pipeline",
        "streamKey": "key01_6c71124cde80358ca7c13081"
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(&create_pipeline.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let pipeline_id = json["pipeline"]["id"].as_str().unwrap().to_string();

    let file_ingest = serde_json::json!({
        "filename": "clip.mp4",
        "loop": true,
        "startTime": "00:00:05",
        "liveOptimized": true,
        "targetGopSeconds": 4
    });
    let uri = format!("/api/v1/pipelines/{pipeline_id}/file-ingest");
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PUT",
            &uri,
            &cookie,
            Some(&file_ingest.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["configured"], true);
    assert_eq!(json["filename"], "clip.mp4");
    assert_eq!(json["streamKey"], "key01_6c71124cde80358ca7c13081");
    assert_eq!(json["loop"], true);
    assert_eq!(json["startTime"], "00:00:05");
    assert_eq!(json["liveOptimized"], true);
    assert_eq!(json["targetGopSeconds"], 4);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["configured"], true);
    assert_eq!(json["filename"], "clip.mp4");
    assert_eq!(json["liveOptimized"], true);
    assert_eq!(json["targetGopSeconds"], 4);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["pipelines"][0]["inputSource"], "file:clip.mp4");
    assert_eq!(json["pipelines"][0]["fileIngest"]["configured"], true);
    assert_eq!(json["pipelines"][0]["fileIngest"]["filename"], "clip.mp4");
    assert_eq!(json["pipelines"][0]["fileIngest"]["running"], false);
    assert_eq!(json["pipelines"][0]["fileIngest"]["liveOptimized"], true);
    assert_eq!(json["pipelines"][0]["fileIngest"]["targetGopSeconds"], 4);

    let resp = app
        .clone()
        .oneshot(auth_req("DELETE", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["configured"], false);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/settings", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert!(json["pipelines"][0]["inputSource"].is_null());
    assert_eq!(json["pipelines"][0]["fileIngest"]["configured"], false);
}

#[tokio::test]
async fn delete_ingest_returns_not_found_for_missing_row() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/ingests/missing-ingest",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ingest_create_start_time_too_long_rejected() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;

    let long_start = "0".repeat(65);
    let body = serde_json::json!({
        "filename": "clip.mp4",
        "streamKey": "testkey01",
        "startTime": long_start,
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/ingests",
            &cookie,
            Some(&body.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ingest_create_start_time_valid_accepted() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;

    let body = serde_json::json!({
        "filename": "clip.mp4",
        "streamKey": "testkey02",
        "startTime": "00:01:30",
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/ingests",
            &cookie,
            Some(&body.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn ingest_update_start_time_too_long_rejected() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;

    // Create ingest first
    let create_body = serde_json::json!({
        "filename": "clip.mp4",
        "streamKey": "testkey03",
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/ingests",
            &cookie,
            Some(&create_body.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let ingest_id = json["id"].as_str().unwrap().to_string();

    let long_start = "1".repeat(65);
    let update_body = serde_json::json!({
        "filename": "clip.mp4",
        "streamKey": "testkey03",
        "startTime": long_start,
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PUT",
            &format!("/api/v1/ingests/{}", ingest_id),
            &cookie,
            Some(&update_body.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ingest_start_requires_matching_pipeline() {
    let (app, _pool) = test_app().await;
    let cookie = login(&app).await;

    let create_body = serde_json::json!({
        "filename": "colorbar-timer-2v16a.mp4",
        "streamKey": "no-such-pipeline",
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/ingests",
            &cookie,
            Some(&create_body.to_string()),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let ingest_id = json["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/ingests/{ingest_id}/start"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_eq!(json["error"], "No pipeline found for stream key");
}
