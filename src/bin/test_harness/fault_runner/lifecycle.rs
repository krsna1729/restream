use super::super::*;

pub(crate) async fn run_publisher_disconnect_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    timeout: Duration,
    case: &PublisherDisconnectCase,
) -> Result<Value, String> {
    let pid = create_pipeline(api, &case.pipeline).await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &case.protocol.publish_url(ports, &case.pipeline),
        case.protocol.ffmpeg_format(),
        case.protocol.map_all_streams(),
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    println!("[fault] {} publisher live", case.log_label);

    stop_child(&mut pub_child).await;
    let started = Instant::now();
    let off_result = wait_for_api_input_off(api, &pid, timeout).await;
    let elapsed = started.elapsed();
    let off_health = api.get_json("/api/v1/engine/health").await.ok();
    let off_input = health_input_snapshot(off_health.as_ref(), &pid);
    let assert_disconnect_fields = matches!(case.protocol, HarnessPublisherProtocol::Rtmp);
    let disconnect_fields_ok = !assert_disconnect_fields
        || (off_input["lastSessionProtocol"] == "rtmp"
            && off_input["lastDisconnectAt"].is_string()
            && off_input["lastDisconnectReason"] == "publisher disconnected"
            && off_input["lastFailurePhase"] == "disconnect"
            && off_input["recentDisconnectError"] == false);
    let passed = off_result.is_ok() && disconnect_fields_ok;
    println!(
        "[fault] {} publisher disconnect: {} ({:.1}s)",
        case.log_label,
        if passed { "PASS" } else { "FAIL" },
        elapsed.as_secs_f64()
    );

    let mut result = json!({
        "test": case.test_name,
        "passed": passed,
        "elapsedMs": elapsed.as_millis(),
        "error": off_result.err(),
        "disconnectFieldsOk": disconnect_fields_ok,
    });
    if assert_disconnect_fields {
        result["inputSnapshot"] = off_input;
    }
    Ok(result)
}

async fn configure_file_ingest_case(
    api: &RampApi,
    pipeline_id: &str,
    stream_key: &str,
    fixture: &Path,
) -> Result<String, String> {
    let fixture_name = fixture.file_name().unwrap().to_string_lossy().to_string();
    let media_root = harness_media_root();
    std::fs::create_dir_all(&media_root).map_err(|e| e.to_string())?;
    let media_dest = media_root.join(&fixture_name);
    if !media_dest.exists() {
        std::fs::copy(fixture, &media_dest).map_err(|e| e.to_string())?;
    }

    api.put_json(
        &format!("/api/v1/pipelines/{pipeline_id}/file-ingest"),
        json!({"filename": fixture_name, "loop": false}),
    )
    .await?;

    let ingest_list = api.get_json("/api/v1/ingests").await?;
    ingest_list
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|ingest| ingest["streamKey"].as_str() == Some(stream_key))
        })
        .and_then(|ingest| ingest["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("file ingest not found in list for {stream_key}"))
}

fn harness_media_root() -> PathBuf {
    PathBuf::from(
        std::env::var("RESTREAM_MEDIA_DIR")
            .unwrap_or_else(|_| restream::config::DEFAULT_MEDIA_DIR.into()),
    )
}

fn recording_file_exists(media_root: &Path, pipeline_name: &str) -> bool {
    std::fs::read_dir(media_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            path.extension()
                .is_some_and(|ext| ext == "ts" || ext == "mp4")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(pipeline_name))
        })
}

async fn wait_for_recording_file(media_root: &Path, pipeline_name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if recording_file_exists(media_root, pipeline_name) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    recording_file_exists(media_root, pipeline_name)
}

pub(crate) async fn run_ingest_lifecycle_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    case: &IngestLifecycleCase,
) -> Result<Value, String> {
    let pid = create_pipeline(api, &case.pipeline).await?;
    let file_eof_restart = matches!(case.file_completion, Some(FileIngestCompletion::EofRestart));
    let (mut publisher, mut file_ingest_id): (Option<Child>, Option<String>) = (None, None);

    match case.kind {
        IngestLifecycleKind::FileIngest => {
            let ingest_id =
                configure_file_ingest_case(api, &pid, &case.pipeline, fixture_h264).await?;
            api.post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
                .await?;
            file_ingest_id = Some(ingest_id);
        }
        IngestLifecycleKind::HlsPreview | IngestLifecycleKind::Recording => {
            publisher = Some(
                spawn_publisher(
                    fixture_h264,
                    &format!("rtmp://127.0.0.1:{}/live/{}", ports.rtmp, case.pipeline),
                    "flv",
                    false,
                )
                .await?,
            );
        }
    }
    wait_for_api_input_live(api, &pid, Duration::from_secs(30)).await?;

    let (mut hls_playlist_status, mut hls_playlist_ok, mut hls_playlist_error) = (None, None, None);
    let active_result = match case.kind {
        IngestLifecycleKind::FileIngest => match case.file_completion {
            Some(FileIngestCompletion::EofRestart) => Some(
                wait_for_pipeline_file_ingest_running_state(
                    api,
                    &pid,
                    true,
                    Duration::from_secs(10),
                )
                .await,
            ),
            Some(FileIngestCompletion::Stop) => {
                println!("[fault] File ingest live");
                None
            }
            None => return Err(format!("{} missing fileCompletion", case.test_name)),
        },
        IngestLifecycleKind::HlsPreview => {
            match wait_for_hls_playlist_ready(api, &pid, Duration::from_secs(15)).await {
                Ok((status, body)) => {
                    hls_playlist_status = Some(status);
                    hls_playlist_ok = Some(body.contains("#EXTM3U"));
                }
                Err(error) => {
                    hls_playlist_status = Some(reqwest::StatusCode::NOT_FOUND);
                    hls_playlist_ok = Some(false);
                    hls_playlist_error = Some(error);
                }
            }
            Some(wait_for_api_hls_preview_state(api, &pid, true, Duration::from_secs(10)).await)
        }
        IngestLifecycleKind::Recording => {
            api.post_empty(&format!("/api/v1/pipelines/{pid}/recording/start"))
                .await?;
            Some(wait_for_api_recording_state(api, &pid, true, Duration::from_secs(10)).await)
        }
    };

    match case.kind {
        IngestLifecycleKind::FileIngest => {
            if matches!(case.file_completion, Some(FileIngestCompletion::Stop)) {
                let ingest_id = file_ingest_id.as_ref().ok_or("file ingest id missing")?;
                api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
                    .await?;
            }
        }
        IngestLifecycleKind::HlsPreview | IngestLifecycleKind::Recording => {
            if matches!(case.kind, IngestLifecycleKind::Recording) {
                tokio::time::sleep(Duration::from_secs(6)).await;
            }
            if let Some(child) = publisher.as_mut() {
                stop_child(child).await;
            }
        }
    }

    let started = Instant::now();
    let off_result =
        wait_for_api_input_off(api, &pid, Duration::from_secs(case.input_off_timeout_secs)).await;
    let inactive_result = match case.kind {
        IngestLifecycleKind::FileIngest if file_eof_restart => Some(
            wait_for_pipeline_file_ingest_running_state(api, &pid, false, Duration::from_secs(10))
                .await,
        ),
        IngestLifecycleKind::HlsPreview => {
            Some(wait_for_api_hls_preview_state(api, &pid, false, Duration::from_secs(15)).await)
        }
        IngestLifecycleKind::Recording => {
            Some(wait_for_api_recording_state(api, &pid, false, Duration::from_secs(10)).await)
        }
        IngestLifecycleKind::FileIngest => None,
    };

    let active_ok = active_result.as_ref().is_none_or(Result::is_ok);
    let inactive_ok = inactive_result.as_ref().is_none_or(Result::is_ok);
    let restart_result = if file_eof_restart {
        if off_result.is_ok() && inactive_ok {
            let ingest_id = file_ingest_id.as_ref().ok_or("file ingest id missing")?;
            match api
                .post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
                .await
            {
                Ok(_) => {
                    if let Err(error) =
                        wait_for_api_input_live(api, &pid, Duration::from_secs(30)).await
                    {
                        Err(error)
                    } else {
                        api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
                            .await
                            .map(|_| ())
                    }
                }
                Err(error) => Err(error),
            }
        } else {
            Err("skipped restart because EOF cleanup did not complete".to_string())
        }
    } else {
        Ok(())
    };
    let feature_result = match case.kind {
        IngestLifecycleKind::FileIngest => json!({}),
        IngestLifecycleKind::HlsPreview => {
            let mut final_status = reqwest::StatusCode::OK;
            let mut playlist_gone = false;
            let shutdown_deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < shutdown_deadline {
                let (status, _) = api
                    .get_text_response(&format!("/hls/{pid}/master.m3u8"))
                    .await?;
                final_status = status;
                if status == reqwest::StatusCode::NOT_FOUND {
                    playlist_gone = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            json!({"finalPlaylistStatus": final_status.as_u16(), "finalPlaylistGone": playlist_gone})
        }
        IngestLifecycleKind::Recording => {
            // A completed recording may already have been remuxed from .ts to
            // .mp4 (recording.rs deletes the source .ts on successful remux
            // unless retention is enabled), so either extension counts as found.
            let media_root = harness_media_root();
            let recording_file_found = wait_for_recording_file(&media_root, &case.pipeline).await;
            let state = inactive_result
                .as_ref()
                .and_then(|result| result.as_ref().ok());
            json!({
                "recordingEnabled": state.and_then(|state| state["enabled"].as_bool()).unwrap_or(false),
                "recordingActive": state.and_then(|state| state["active"].as_bool()).unwrap_or(true),
                "recordingFileFound": recording_file_found,
            })
        }
    };
    let elapsed = started.elapsed();
    let feature_ok = match case.kind {
        IngestLifecycleKind::FileIngest => true,
        IngestLifecycleKind::HlsPreview => {
            hls_playlist_ok == Some(true) && feature_result["finalPlaylistGone"] == true
        }
        IngestLifecycleKind::Recording => {
            feature_result["recordingEnabled"] == true
                && feature_result["recordingActive"] == false
                && feature_result["recordingFileFound"] == true
        }
    };
    let passed =
        active_ok && off_result.is_ok() && inactive_ok && restart_result.is_ok() && feature_ok;
    println!(
        "[fault] {}: {} ({:.1}s)",
        case.test_name,
        if passed { "PASS" } else { "FAIL" },
        elapsed.as_secs_f64()
    );

    let mut result = json!({
        "test": case.test_name,
        "passed": passed,
        "elapsedMs": elapsed.as_millis(),
    });
    if file_eof_restart {
        result["runningError"] = json!(active_result.and_then(Result::err));
        result["inputOffError"] = json!(off_result.err());
        result["stoppedError"] = json!(inactive_result.and_then(Result::err));
        result["restartError"] = json!(restart_result.err());
    } else if matches!(case.kind, IngestLifecycleKind::FileIngest) {
        result["error"] = json!(off_result.err());
    } else {
        result["inputOffError"] = json!(off_result.err());
        if matches!(case.kind, IngestLifecycleKind::HlsPreview) {
            result["playlistStatus"] = json!(hls_playlist_status.map(|status| status.as_u16()));
            result["playlistOk"] = json!(hls_playlist_ok);
            result["playlistError"] = json!(hls_playlist_error);
            result["hlsPreviewActiveError"] = json!(active_result.and_then(Result::err));
            result["hlsPreviewInactiveError"] = json!(inactive_result.and_then(Result::err));
        } else {
            result["recordingActiveError"] = json!(active_result.and_then(Result::err));
            result["recordingInactiveError"] = json!(inactive_result.and_then(Result::err));
        }
        if let Some(extra) = feature_result.as_object() {
            for (key, value) in extra {
                result[key] = value.clone();
            }
        }
    }
    Ok(result)
}
