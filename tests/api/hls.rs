use super::*;

#[tokio::test]
async fn custom_encoding_endpoint_is_unavailable() {
    let (app, pool) = test_app().await;
    let cookie = login(&app).await;

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PUT",
            "/api/v1/encodings/custom",
            &cookie,
            Some(r#"{"ffmpegArgs":"-c:v libx264 -preset fast"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::GONE);
    assert_eq!(db::get_meta(&pool, "custom_encoding").await.unwrap(), None);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/encodings/custom", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::GONE);
    let json = body_json(resp).await;
    assert!(json["error"].as_str().unwrap().contains("not available"));
}

// --- HLS pull ---

#[tokio::test]
async fn hls_canonical_no_stream_returns_404() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;
    let resp = app
        .oneshot(auth_req(
            "GET",
            "/hls/nonexistent/index.m3u8",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"))
    );
    let json = body_json(resp).await;
    assert_eq!(json["error"], "No HLS stream");
    assert_eq!(json["status"], 404);
    assert_eq!(json["code"], "hlsNoStream");
}

#[tokio::test]
async fn hls_routes_require_authentication() {
    let (app, _, engine) = test_app_with_engine().await;
    engine.get_or_create_hls_store("test_pipe").await;

    for uri in [
        "/hls/test_pipe",
        "/hls/test_pipe/index.m3u8",
        "/hls/test_pipe/notasegment",
    ] {
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
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "uri={uri}");
    }
}

#[tokio::test]
async fn hls_playlist_routes_return_not_found_for_empty_store() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;
    engine.get_or_create_hls_preview_store("test_pipe").await;

    for uri in ["/hls/test_pipe", "/hls/test_pipe/index.m3u8"] {
        let resp = app
            .clone()
            .oneshot(auth_req("GET", uri, &cookie, None))
            .await
            .unwrap();

        // An existing empty store is a valid playlist route with no segments
        // yet. The generic segment handler returns 400 for "index.m3u8".
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "uri={uri}");
        assert!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json")),
            "uri={uri}"
        );
        let json = body_json(resp).await;
        assert_eq!(json["error"], "No segments yet", "uri={uri}");
        assert_eq!(json["status"], 404, "uri={uri}");
        assert_eq!(json["code"], "hlsNoSegments", "uri={uri}");
    }
}

#[tokio::test]
async fn hls_segment_bad_name_returns_400() {
    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;
    engine.get_or_create_hls_preview_store("test_pipe").await;

    for uri in ["/hls/test_pipe/notasegment"] {
        let resp = app
            .clone()
            .oneshot(auth_req("GET", uri, &cookie, None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "uri={uri}");
        assert!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json")),
            "uri={uri}"
        );
        let json = body_json(resp).await;
        assert_eq!(json["error"], "Invalid segment name", "uri={uri}");
        assert_eq!(json["status"], 400, "uri={uri}");
        assert_eq!(json["code"], "hlsInvalidSegmentName", "uri={uri}");
    }
}

#[tokio::test]
async fn hls_playlist_route_returns_blocked_stage_cause_when_applicable() {
    use restream::media::stage_lifecycle::StagePhase;
    use restream_core::metadata::VideoMeta;
    use restream_domain::domain::stage::StageKind;
    use restream_sync::RingBuffer;

    let (app, _, engine) = test_app_with_engine().await;
    let cookie = login(&app).await;

    // Register active ingest and preview consumer/store.
    engine
        .try_register_ingest("test_blocked_pipe", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "test_blocked_pipe",
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    engine.ensure_hls_preview_runtime("test_blocked_pipe").await;

    // Register a blocked preview stage
    let stage_key = StageKey::new(
        "test_blocked_pipe",
        StageKind::codec_edge("hevc_to_h264", StageKind::source()),
    );
    let source_ring = Arc::new(RingBuffer::new(16));
    let manager = restream::media::stage_runtime::StageRuntimeManager::new(engine.clone());
    let (handle, _lease205, _) = manager
        .ensure_stage(stage_key.clone(), source_ring, None)
        .await;

    // Transition stage to WaitingForCapacity
    handle.lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: restream::media::stage_lifecycle::StageBackendKind::ExternalFfmpeg,
    });

    let resp = app
        .oneshot(auth_req("GET", "/hls/test_blocked_pipe", &cookie, None))
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
    let body_str = body["error"].as_str().unwrap();
    assert!(
        body_str.contains("blocked by video stage:"),
        "body_str={body_str}"
    );
    assert!(
        body_str.contains("waitingForCapacity"),
        "body_str={body_str}"
    );
    assert_eq!(body["status"], 404);
    assert_eq!(body["code"], "hlsNoSegments");
    assert_eq!(body["blockedBy"]["phase"], "waitingForCapacity");
}

/// File ingest (an external `ffmpeg` child) feeds the dashboard preview: the
/// HLS master lists video and all 16 alternate audio renditions, and the
/// video and audio segments are served.
#[tokio::test]
async fn file_ingest_preview_hls_serves_playlist_and_segment() {
    let (app, cookie, media_dir, pool, _engine) =
        authenticated_app_with_temp_media_and_engine().await;
    let pipeline_id = "pipe-file-preview";
    let stream_key = "file-preview-key";
    db::create_pipeline(&pool, pipeline_id, "File Preview", stream_key, None, None)
        .await
        .expect("create pipeline");
    std::fs::copy(
        restream::media::test_fixtures::checked_in_fixture(
            "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
        )
        .expect("checked-in integration media fixture"),
        media_dir.join("colorbar-timer-2v16a.mp4"),
    )
    .expect("copy fixture into media dir");

    let create = serde_json::json!({
        "filename": "colorbar-timer-2v16a.mp4",
        "streamKey": stream_key,
        "loop": true,
    });
    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            "/api/v1/ingests",
            &cookie,
            Some(&create.to_string()),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "create ingest: {}",
        resp.status()
    );
    let ingest_id = body_json(resp).await["id"].as_str().unwrap().to_string();
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
    assert!(
        resp.status().is_success(),
        "start ingest: {}",
        resp.status()
    );

    let mut playlist_body = None;
    for _ in 0..40 {
        let resp = app
            .clone()
            .oneshot(auth_req(
                "GET",
                &format!("/hls/{pipeline_id}/index.m3u8"),
                &cookie,
                None,
            ))
            .await
            .unwrap();

        if resp.status() == StatusCode::OK {
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8(bytes.to_vec()).expect("playlist utf8");
            if body
                .lines()
                .any(|line| !line.starts_with('#') && !line.is_empty())
            {
                playlist_body = Some(body);
                break;
            }
        }

        sleep(Duration::from_millis(250)).await;
    }

    let playlist = playlist_body.expect("playlist with at least one segment");
    let segment = playlist
        .lines()
        .find(|line| !line.starts_with('#') && !line.is_empty())
        .expect("segment path in playlist");

    let mut master_playlist_body = None;
    for _ in 0..40 {
        let resp = app
            .clone()
            .oneshot(auth_req(
                "GET",
                &format!("/hls/{pipeline_id}/master.m3u8"),
                &cookie,
                None,
            ))
            .await
            .unwrap();

        if resp.status() == StatusCode::OK {
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8(bytes.to_vec()).expect("master playlist utf8");
            if body.matches("#EXT-X-MEDIA:TYPE=AUDIO").count() >= 16
                && body.contains("video/index.m3u8")
                && body.contains("audio/15/index.m3u8")
            {
                master_playlist_body = Some(body);
                break;
            }
        }

        sleep(Duration::from_millis(250)).await;
    }

    let master_playlist =
        master_playlist_body.expect("master playlist with video and 16 alternate audio tracks");
    assert_eq!(
        master_playlist.matches("#EXT-X-MEDIA:TYPE=AUDIO").count(),
        16
    );
    assert!(master_playlist.contains("video/index.m3u8"));
    assert!(master_playlist.contains("audio/15/index.m3u8"));

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/hls/{pipeline_id}/{segment}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let segment_body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        !segment_body.is_empty(),
        "segment payload should not be empty"
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/hls/{pipeline_id}/video/index.m3u8"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let audio_playlist_resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/hls/{pipeline_id}/audio/15/index.m3u8"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(audio_playlist_resp.status(), StatusCode::OK);
    let audio_playlist_body = audio_playlist_resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let audio_playlist =
        String::from_utf8(audio_playlist_body.to_vec()).expect("audio playlist utf8");
    let audio_segment = audio_playlist
        .lines()
        .find(|line| !line.starts_with('#') && !line.is_empty())
        .expect("audio segment path in playlist");

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            &format!("/hls/{pipeline_id}/audio/15/{audio_segment}"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let audio_segment_body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        !audio_segment_body.is_empty(),
        "audio segment payload should not be empty"
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "POST",
            &format!("/api/v1/ingests/{ingest_id}/stop"),
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success(), "stop ingest: {}", resp.status());
}
