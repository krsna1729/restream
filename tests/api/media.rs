use super::*;

#[tokio::test]
async fn media_analysis_reports_sparse_checked_in_fixture() {
    let (app, cookie, temp_dir, _pool) = authenticated_app_with_temp_media().await;
    let fixture = restream::test_fixtures::sparse_gop_mp4_fixture()
        .expect("checked-in sparse GOP fixture should exist");
    let target = temp_dir.join("sparse-gop-5s.mp4");
    tokio::fs::copy(&fixture, &target).await.unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req(
            "GET",
            "/api/v1/media/sparse-gop-5s.mp4/analysis",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(json["videoCodec"], "h264");
    assert_eq!(json["keyframeCount"], 3);
    assert_eq!(json["averageKeyframeIntervalSec"], 5.0);
    assert_eq!(json["maxKeyframeIntervalSec"], 5.0);
    assert_eq!(json["sparseForLive"], true);
    assert_eq!(json["liveGopTargetSeconds"], 2);

    let _ = std::fs::remove_dir_all(temp_dir);
}

// --- Round 7 #1: media delete path traversal guard ---

#[tokio::test]
async fn media_delete_path_traversal_blocked() {
    let (app, _) = test_app().await;
    let cookie = login(&app).await;

    // Test a normal non-existent file: should return NOT_FOUND (404)
    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/media/nonexistent.mp4",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Test path traversal attempt: should return BAD_REQUEST (400) or NOT_FOUND (404)
    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/media/..%2f..%2fetc%2fpasswd",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert!(resp.status() == StatusCode::BAD_REQUEST || resp.status() == StatusCode::NOT_FOUND);
}

#[test]
fn media_destination_path_allows_plain_name_under_relative_media_root() {
    let media_dir = format!("target/restream-media-{}", rand::random::<u64>());
    std::fs::create_dir_all(&media_dir).unwrap();

    let destination =
        restream::api::media_library::media_destination_path_under_root(&media_dir, "renamed.mp4")
            .unwrap();
    let expected = std::fs::canonicalize(&media_dir)
        .unwrap()
        .join("renamed.mp4");

    assert_eq!(destination, expected);
    let _ = std::fs::remove_dir_all(media_dir);
}

#[tokio::test]
async fn media_library_classifies_serves_and_deletes_files() {
    let (app, cookie, temp_dir, pool) = authenticated_app_with_temp_media().await;
    let recording_path = temp_dir.join("sample-recording.mp4");
    let source_path = temp_dir.join("sample-source.ts");
    tokio::fs::write(&recording_path, b"mp4 recording data")
        .await
        .unwrap();
    tokio::fs::write(&source_path, b"ts source data")
        .await
        .unwrap();
    db::create_ingest(
        &pool,
        "ingest-1",
        "sample-source.ts",
        "stream-key-1",
        false,
        "",
        false,
        2,
    )
    .await
    .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/media", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let files = json["files"].as_array().unwrap();
    let recording = files
        .iter()
        .find(|file| file["name"].as_str() == Some("sample-recording.mp4"))
        .expect("recording-named files should be visible in the media library");
    assert_eq!(recording["kind"].as_str(), Some("recording"));
    let source = files
        .iter()
        .find(|file| file["name"].as_str() == Some("sample-source.ts"))
        .expect("non-recording files should be visible as source files");
    assert_eq!(source["kind"].as_str(), Some("source"));

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/media/sample-source.ts", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("video/mp2t")
    );
    assert_eq!(
        resp.headers()
            .get(header::ACCEPT_RANGES)
            .and_then(|value| value.to_str().ok()),
        Some("bytes")
    );
    assert_eq!(body_bytes(resp).await.as_ref(), b"ts source data");

    let resp = app
        .clone()
        .oneshot(auth_req("HEAD", "/media/sample-source.ts", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some("14")
    );
    assert_eq!(body_bytes(resp).await.len(), 0);

    let resp = app
        .clone()
        .oneshot(auth_req_with_header(
            "GET",
            "/media/sample-source.ts",
            &cookie,
            "Range",
            "bytes=3-8",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("bytes 3-8/14")
    );
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some("6")
    );
    assert_eq!(body_bytes(resp).await.as_ref(), b"source");

    let resp = app
        .clone()
        .oneshot(auth_req_with_header(
            "GET",
            "/media/sample-source.ts",
            &cookie,
            "Range",
            "bytes=-4",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body_bytes(resp).await.as_ref(), b"data");

    let resp = app
        .clone()
        .oneshot(auth_req_with_header(
            "GET",
            "/media/sample-source.ts",
            &cookie,
            "Range",
            "bytes=99-120",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("bytes */14")
    );

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/media/sample-source.ts",
            &cookie,
            Some(r#"{"newName":"renamed-source.ts"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let renamed = body_json(resp).await;
    assert_eq!(renamed["renamed"], true);
    assert_eq!(renamed["name"], "renamed-source.ts");
    assert_eq!(renamed["updatedIngests"], 1);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/media/sample-source.ts", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/media/renamed-source.ts", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_bytes(resp).await.as_ref(), b"ts source data");

    let ingests = db::list_ingests_for_filename(&pool, "renamed-source.ts")
        .await
        .unwrap();
    assert_eq!(ingests.len(), 1);
    assert_eq!(ingests[0].id, "ingest-1");

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/media/renamed-source.ts",
            &cookie,
            Some(r#"{"newName":"renamed-source.mp4"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    assert!(db::delete_ingest(&pool, "ingest-1").await.unwrap());

    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/media/renamed-source.ts",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["deleted"], true);

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/media/renamed-source.ts", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn media_library_upload_streams_one_validated_file() {
    let (app, cookie, temp_dir, _) = authenticated_app_with_temp_media().await;

    let response = app
        .clone()
        .oneshot(media_upload_req(&cookie, "source.mp4", b"uploaded media"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let uploaded = body_json(response).await;
    assert_eq!(uploaded["uploaded"], true);
    assert_eq!(uploaded["name"], "source.mp4");
    assert_eq!(uploaded["size"], 14);
    assert_eq!(
        std::fs::read(temp_dir.join("source.mp4")).unwrap(),
        b"uploaded media"
    );

    let duplicate = app
        .clone()
        .oneshot(media_upload_req(&cookie, "source.mp4", b"second copy"))
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let unsafe_name = app
        .oneshot(media_upload_req(&cookie, "../../escape.txt", b"not media"))
        .await
        .unwrap();
    assert_eq!(unsafe_name.status(), StatusCode::BAD_REQUEST);
    assert!(!temp_dir.parent().unwrap().join("escape.txt").exists());
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn media_library_uses_recording_metadata_for_file_rows() {
    let (app, cookie, temp_dir, pool) = authenticated_app_with_temp_media().await;
    let final_path = temp_dir.join("session-final.mp4");
    tokio::fs::write(&final_path, b"recording bytes")
        .await
        .unwrap();
    db::create_pipeline(
        &pool,
        "pipe-rec",
        "Recording Pipeline",
        "rec-key",
        None,
        None,
    )
    .await
    .unwrap();
    let recording_id = restream::domain::ids::RecordingId::from("rec-meta-1");
    db::create_recording(
        &pool,
        &recording_id,
        "pipe-rec",
        "2026-07-09T00:00:00Z",
        Some(temp_dir.join("session-temp.ts").to_string_lossy().as_ref()),
        Some("h264/aac"),
    )
    .await
    .unwrap();
    db::finalize_recording(
        &pool,
        &recording_id,
        "2026-07-09T00:01:00Z",
        final_path.to_string_lossy().as_ref(),
    )
    .await
    .unwrap();

    let resp = app
        .oneshot(auth_req("GET", "/api/v1/media", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let files = json["files"].as_array().unwrap();
    let recording = files
        .iter()
        .find(|file| file["name"].as_str() == Some("session-final.mp4"))
        .expect("recording metadata should attach to the final filename");

    assert_eq!(recording["kind"], "recording");
    assert_eq!(recording["recordingId"], "rec-meta-1");
    assert_eq!(recording["pipelineId"], "pipe-rec");
    assert_eq!(recording["recordingStatus"], "ready");
    assert_eq!(recording["recordingStartedAt"], "2026-07-09T00:00:00Z");
    assert_eq!(recording["recordingEndedAt"], "2026-07-09T00:01:00Z");
    assert_eq!(recording["recordingCodecSummary"], "h264/aac");
    let _ = std::fs::remove_dir_all(temp_dir);
}

#[tokio::test]
async fn media_library_groups_recording_conversion_artifacts_and_renames_companions() {
    let (app, cookie, temp_dir, _pool) = authenticated_app_with_temp_media().await;
    let recording_ts = temp_dir.join("recording_20260629T235959_demo.ts");
    let recording_mp4 = temp_dir.join("recording_20260629T235959_demo.mp4");
    let recording_state = temp_dir.join("recording_20260629T235959_demo.ts.conversion.json");
    tokio::fs::write(&recording_ts, b"ts bytes").await.unwrap();
    tokio::fs::write(&recording_mp4, b"mp4 bytes")
        .await
        .unwrap();
    tokio::fs::write(
        &recording_state,
        serde_json::to_vec(&restream::media::recording::RecordingConversionState {
            status: restream::media::recording::RecordingConversionStatus::Ready,
            updated_at: "2026-06-29T18:30:00Z".to_string(),
            error: None,
        })
        .unwrap(),
    )
    .await
    .unwrap();

    let resp = app
        .clone()
        .oneshot(auth_req("GET", "/api/v1/media", &cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let files = json["files"].as_array().unwrap();
    assert_eq!(
        files.len(),
        1,
        "recording ts/mp4 should collapse into one row"
    );
    let recording = &files[0];
    assert_eq!(recording["name"], "recording_20260629T235959_demo.ts");
    assert_eq!(recording["playName"], "recording_20260629T235959_demo.mp4");
    assert_eq!(recording["sourceName"], "recording_20260629T235959_demo.ts");
    assert_eq!(
        recording["convertedName"],
        "recording_20260629T235959_demo.mp4"
    );
    assert_eq!(recording["conversionStatus"], "ready");

    let resp = app
        .clone()
        .oneshot(auth_req(
            "PATCH",
            "/api/v1/media/recording_20260629T235959_demo.ts",
            &cookie,
            Some(r#"{"newName":"recording_20260629T235959_renamed.ts"}"#),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert!(
        temp_dir
            .join("recording_20260629T235959_renamed.ts")
            .exists()
    );
    assert!(
        temp_dir
            .join("recording_20260629T235959_renamed.mp4")
            .exists()
    );
    assert!(
        temp_dir
            .join("recording_20260629T235959_renamed.ts.conversion.json")
            .exists()
    );
    assert!(!recording_ts.exists());
    assert!(!recording_mp4.exists());
    assert!(!recording_state.exists());

    let resp = app
        .clone()
        .oneshot(auth_req(
            "DELETE",
            "/api/v1/media/recording_20260629T235959_renamed.ts",
            &cookie,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        !temp_dir
            .join("recording_20260629T235959_renamed.ts")
            .exists()
    );
    assert!(
        !temp_dir
            .join("recording_20260629T235959_renamed.mp4")
            .exists()
    );
    assert!(
        !temp_dir
            .join("recording_20260629T235959_renamed.ts.conversion.json")
            .exists()
    );

    let _ = std::fs::remove_dir_all(temp_dir);
}
