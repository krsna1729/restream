use super::*;

#[tokio::test]
async fn rtmps_output_is_accepted_by_api() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_rtmps", "P", "key_rtmps", None, None)
        .await
        .unwrap();

    // rtmps:// must be accepted (used by Facebook, YouTube, etc.)
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_rtmps/outputs",
            &cookie,
            Some(r#"{"name":"FB","url":"rtmps://live-api-s.facebook.com:443/rtmp/test","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "rtmps:// output should be accepted"
    );

    // Verify roundtrip
    let json = body_json(resp).await;
    assert_eq!(
        json["output"]["url"],
        "rtmps://live-api-s.facebook.com:443/rtmp/test"
    );
}

#[tokio::test]
async fn local_hls_output_is_accepted_by_api() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_hls", "P", "key_hls", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_hls/outputs",
            &cookie,
            Some(r#"{"name":"Local HLS","url":"hls://localhost/hls/key_hls","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(json["output"]["url"], "hls://localhost/hls/key_hls");
}

#[tokio::test]
async fn sink_output_is_accepted_by_api() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_sink", "P", "key_sink", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_sink/outputs",
            &cookie,
            Some(r#"{"name":"Discard","url":" SINK://LOCAL/blackhole ","config":{"video":{"mode":"source","codec":"h265"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    let output_id = json["output"]["id"].as_str().unwrap();
    assert_eq!(json["output"]["url"], "sink://local/blackhole");

    let stored = db::get_output(&pool, "p_sink", output_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, "sink://local/blackhole");
}

#[tokio::test]
async fn pipeline_recirculation_rejects_media_transforms() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_pipe_src", "Source", "key_pipe_src", None, None)
        .await
        .unwrap();
    db::create_pipeline(&pool, "p_pipe_tgt", "Target", "key_pipe_tgt", None, None)
        .await
        .unwrap();
    db::create_pipeline_input(&pool, "backup", "p_pipe_tgt", "Backup", "key_pipe_backup")
        .await
        .unwrap();

    for config in [
        r#"{"video":{"mode":"preset","preset":"720p"},"audio":{"mode":"all"}}"#,
        r#"{"video":{"mode":"source","codec":"h264"},"audio":{"mode":"all"}}"#,
        r#"{"video":{"mode":"source"},"audio":{"mode":"selectTracks","tracks":[0]}}"#,
    ] {
        let body = format!(
            r#"{{"name":"Recirc","url":"pipeline://p_pipe_tgt/backup","config":{config}}}"#
        );
        let resp = app
            .clone()
            .oneshot(auth_req(
                "POST",
                "/api/v1/pipelines/p_pipe_src/outputs",
                &cookie,
                Some(&body),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn pipeline_recirculation_output_lifecycle_uses_api_status_and_cleanup() {
    let (app, pool, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_life_src", "Source", "key_life_src", None, None)
        .await
        .unwrap();
    db::create_pipeline(&pool, "p_life_tgt", "Target", "key_life_tgt", None, None)
        .await
        .unwrap();
    db::create_pipeline_input(&pool, "backup-a", "p_life_tgt", "Backup A", "key_life_a")
        .await
        .unwrap();
    db::create_pipeline_input(&pool, "backup-b", "p_life_tgt", "Backup B", "key_life_b")
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_life_src/outputs",
            &cookie,
            Some(r#"{"name":"Recirc","url":"pipeline://p_life_tgt/backup-a","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let output_id = json["output"]["id"].as_str().unwrap().to_string();
    assert_eq!(json["output"]["url"], "pipeline://p_life_tgt/backup-a");
    assert_eq!(json["output"]["desiredState"], "stopped");

    let uri = format!("/api/v1/pipelines/p_life_src/outputs/{output_id}");
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            &uri,
            &cookie,
            Some(r#"{"name":"Recirc B","url":"pipeline://p_life_tgt/backup-b","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["output"]["name"], "Recirc B");
    assert_eq!(json["output"]["url"], "pipeline://p_life_tgt/backup-b");

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/p_life_src/outputs/{output_id}/start"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["desiredState"], "running");

    let token = engine
        .register_egress(&output_id, "p_life_src", "pipeline://p_life_tgt/backup-b")
        .await;
    engine
        .update_egress_target_addr(&output_id, "pipeline://p_life_tgt/backup-b".to_string())
        .await;
    engine
        .update_egress_phase(&output_id, EgressPhase::Sending)
        .await;
    engine.record_egress_progress(&output_id, 256).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/api/v1/pipelines/p_life_src/outputs/{output_id}/status"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = body_json(resp).await;
    assert_eq!(status["status"], "running");
    assert_eq!(status["phase"], "sending");
    assert_eq!(status["targetAddr"], "pipeline://p_life_tgt/backup-b");
    assert_eq!(status["bytesOut"], 256);

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/p_life_src/outputs/{output_id}/stop"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["desiredState"], "stopped");

    let resp = app
        .clone()
        .oneshot(auth_req("DELETE", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(token.is_cancelled());
    assert!(
        db::get_output(&pool, "p_life_src", &output_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn output_urls_are_parsed_normalized_and_host_required() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_url_norm", "P", "key_url_norm", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_url_norm/outputs",
            &cookie,
            Some(r#"{"name":"Normalized","url":" RTMP://DEST.EXAMPLE/live/key ","monitoringUrl":" HTTPS://MONITOR.EXAMPLE/live ","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let output_id = json["output"]["id"].as_str().unwrap();
    assert_eq!(json["output"]["url"], "rtmp://dest.example/live/key");
    assert_eq!(
        json["output"]["monitoringUrl"],
        "https://monitor.example/live"
    );

    let stored = db::get_output(&pool, "p_url_norm", output_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.url, "rtmp://dest.example/live/key");
    assert_eq!(
        stored.monitoring_url.as_deref(),
        Some("https://monitor.example/live")
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            &format!("/api/v1/pipelines/p_url_norm/outputs/{output_id}"),
            &cookie,
            Some(r#"{"name":"Normalized","url":" SRT://SINK.EXAMPLE:9000?streamid=publish:key ","monitoringUrl":null,"config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(
        json["output"]["url"],
        "srt://sink.example:9000?streamid=publish:key"
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_url_norm/outputs",
            &cookie,
            Some(r#"{"name":"Bad","url":"rtmp:///live/key","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn custom_output_encoding_is_rejected_by_api() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_custom", "P", "key_custom", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_custom/outputs",
            &cookie,
            Some(
                r#"{"name":"Custom","url":"rtmp://dest/live/key","config":{"video":{"mode":"custom"},"audio":{"mode":"all"}}}"#,
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let json = body_json(resp).await;
    assert!(
        json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Custom output encoding is not available yet")
    );

    let output = db::create_output(
        &pool,
        "out_custom_update",
        "p_custom",
        "O",
        "rtmp://dest/live/key",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();
    let uri = format!("/api/v1/pipelines/p_custom/outputs/{}", output.id);
    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            &uri,
            &cookie,
            Some(
                r#"{"name":"Custom","url":"rtmp://dest/live/key","config":{"video":{"mode":"custom"},"audio":{"mode":"downmix","track":0}}}"#,
            ),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_hls_upload_output_is_accepted_by_api() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_http_hls", "P", "key_http_hls", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p_http_hls/outputs",
            &cookie,
            Some(r#"{"name":"Remote HLS","url":"https://a.upload.youtube.com/http_upload_hls?cid=abc&copy=0&file=out.m3u8","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let json = body_json(resp).await;
    assert_eq!(
        json["output"]["url"],
        "https://a.upload.youtube.com/http_upload_hls?cid=abc&copy=0&file=out.m3u8"
    );
}

#[tokio::test]
async fn output_crud_via_api() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();

    // Create output
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines/p1/outputs",
            &cookie,
            Some(r#"{"name":"YouTube","url":"rtmp://yt/live","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let output_id = json["output"]["id"].as_str().unwrap().to_string();

    // Start
    let uri = format!("/api/v1/pipelines/p1/outputs/{}/start", output_id);
    let resp = app
        .clone()
        .oneshot(auth_req("POST", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["desiredState"], "running");

    // Verify desired state persisted in DB
    let output = db::get_output(&pool, "p1", &output_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.desired_state, DesiredOutputState::Running);

    // Stop
    let uri = format!("/api/v1/pipelines/p1/outputs/{}/stop", output_id);
    let resp = app
        .clone()
        .oneshot(auth_req("POST", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Delete
    let uri = format!("/api/v1/pipelines/p1/outputs/{}", output_id);
    let resp = app
        .clone()
        .oneshot(auth_req("DELETE", &uri, &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn delete_output_cancels_egress() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/pipelines",
            &cookie,
            Some(r#"{"name":"P","streamKey":"key01_6c71124cde80358ca7c13081"}"#),
        ))
        .await
        .unwrap();
    let pipe = body_json(resp).await;
    let pid = pipe["pipeline"]["id"].as_str().unwrap().to_string();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/pipelines/{pid}/outputs"),
            &cookie,
            Some(r#"{"name":"O","url":"rtmp://dest/live/k","config":{"video":{"mode":"source"},"audio":{"mode":"all"}}}"#),
        ))
        .await
        .unwrap();
    let out = body_json(resp).await;
    let oid = out["output"]["id"].as_str().unwrap().to_string();

    let token = engine
        .register_egress(&oid, &pid, "rtmp://dest/live/k")
        .await;
    assert!(!token.is_cancelled());

    // Delete the output
    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            &format!("/api/v1/pipelines/{pid}/outputs/{oid}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Egress cancellation token should be cancelled
    assert!(token.is_cancelled(), "deleting output should cancel egress");
}

#[tokio::test]
async fn delete_output_returns_not_found_for_missing_row() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    db::create_pipeline(&pool, "p_delete_missing", "P", "delete-missing", None, None)
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/pipelines/p_delete_missing/outputs/missing-output",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
