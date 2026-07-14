//! Synthetic HLS PUT upload sink and probe helpers for the test harness.

use super::*;

/// Result bundle returned by the synthetic HLS PUT upload probe.
pub(crate) struct HlsPutProbeResult {
    pub(crate) passed: bool,
    pub(crate) summary: Value,
    pub(crate) output_id: String,
}

pub(crate) async fn run_hls_put_probe(
    api: &RampApi,
    pipeline_id: &str,
    label: &str,
    put_port: u16,
) -> Result<HlsPutProbeResult, String> {
    let sink_dir = artifact_path(&format!("hls-put-probe-{label}"));
    let _ = std::fs::remove_dir_all(&sink_dir);
    std::fs::create_dir_all(&sink_dir).map_err(|e| e.to_string())?;

    let (sink_cancel, sink_handle) = start_hls_put_sink(put_port, sink_dir.clone()).await?;

    let put_url =
        format!("http://127.0.0.1:{put_port}/upload?cid=probe-{label}&copy=0&file=out.m3u8");
    let output_id = create_output(
        api,
        pipeline_id,
        &format!("hls-put-{label}"),
        &put_url,
        "source",
    )
    .await?;
    start_output(api, pipeline_id, &output_id).await?;

    let artifacts = wait_for_hls_put_artifacts(&sink_dir, Duration::from_secs(30)).await;
    let mut playlist_ok = false;
    let mut content_types_ok = false;
    let mut segment_ok = false;

    if let Ok(ref arts) = artifacts {
        playlist_ok = validate_hls_playlist(&arts.youtube_playlist, "probe").is_ok();

        if let Ok(requests) = read_hls_put_requests(&sink_dir) {
            let playlist_ct = request_seen(&requests, |r| {
                r["file"] == "out.m3u8" && r["contentType"] == "application/vnd.apple.mpegurl"
            });
            let segment_ct = request_seen(&requests, |r| {
                r["file"]
                    .as_str()
                    .is_some_and(|f| is_segment_file(f, "seg"))
                    && r["contentType"] == "video/mp2t"
            });
            content_types_ok = playlist_ct && segment_ct;
        }

        if let Ok(probe) = ffprobe(&arts.youtube_segment.to_string_lossy()).await {
            let has_video = probe["streams"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "video"));
            let has_audio = probe["streams"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "audio"));
            segment_ok = has_video && has_audio;
        }
    }

    let status = api.get_output_status(pipeline_id, &output_id).await.ok();
    let status_ok = status
        .as_ref()
        .is_some_and(|(status, _)| status.bytes_out > 0);
    let status_json = status.as_ref().map(|(_, json)| json.clone());

    let _ = api
        .post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
        ))
        .await;

    sink_cancel.cancel();
    let _ = sink_handle.await;

    let passed = playlist_ok && content_types_ok && segment_ok && status_ok;
    let summary = json!({
        "playlistValid": playlist_ok,
        "contentTypesCorrect": content_types_ok,
        "segmentDecodable": segment_ok,
        "artifactsFound": artifacts.is_ok(),
        "outputStatus": status_json,
    });

    if !passed {
        eprintln!(
            "[hls-put-probe:{label}] FAIL: playlist={playlist_ok} content_types={content_types_ok} segment={segment_ok} status={status_ok}"
        );
    } else {
        println!(
            "[hls-put-probe:{label}] ok: playlist={playlist_ok} content_types={content_types_ok} segment={segment_ok} status={status_ok}"
        );
    }

    Ok(HlsPutProbeResult {
        passed,
        summary,
        output_id,
    })
}

/// Test: SRT ingest -> HLS HTTP PUT upload for YouTube-style and path-style sinks.
/// Files written by the synthetic HLS PUT sink.
pub(crate) struct HlsPutArtifacts {
    youtube_playlist: PathBuf,
    youtube_segment: PathBuf,
}

/// Shared filesystem/request state for the synthetic HLS PUT sink.
struct HlsPutSinkState {
    root: PathBuf,
    requests_path: PathBuf,
    write_lock: Mutex<()>,
}

/// State for an HLS PUT sink that intentionally delays responses.
#[derive(Clone)]
struct HlsPutHangSinkState {
    cancel: CancellationToken,
    delay: Duration,
}

pub(crate) async fn start_hls_put_sink(
    port: u16,
    root: PathBuf,
) -> Result<(CancellationToken, tokio::task::JoinHandle<()>), String> {
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let state = Arc::new(HlsPutSinkState {
        requests_path: root.join("requests.jsonl"),
        root,
        write_lock: Mutex::new(()),
    });
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route("/{*path}", put(hls_put_sink_put))
        .layer(DefaultBodyLimit::disable())
        .with_state(state);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let cancel = CancellationToken::new();
    let server_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(server_cancel.cancelled_owned())
            .await
        {
            eprintln!("[hls-put-sink] server failed: {err}");
        }
    });
    Ok((cancel, handle))
}

pub(crate) async fn start_hls_put_hang_sink(
    port: u16,
    delay: Duration,
) -> Result<(CancellationToken, tokio::task::JoinHandle<()>), String> {
    let cancel = CancellationToken::new();
    let state = HlsPutHangSinkState {
        cancel: cancel.clone(),
        delay,
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .route(
            "/{*path}",
            put(
                |State(state): State<HlsPutHangSinkState>,
                 OriginalUri(_uri): OriginalUri,
                 _headers: HeaderMap,
                 _body: Bytes| async move {
                    tokio::select! {
                        _ = state.cancel.cancelled() => StatusCode::SERVICE_UNAVAILABLE,
                        _ = tokio::time::sleep(state.delay) => StatusCode::NO_CONTENT,
                    }
                },
            ),
        )
        .layer(DefaultBodyLimit::disable())
        .with_state(state);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let server_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app)
            .with_graceful_shutdown(server_cancel.cancelled_owned())
            .await
        {
            eprintln!("[hls-put-hang-sink] server failed: {err}");
        }
    });
    Ok((cancel, handle))
}

async fn hls_put_sink_put(
    State(state): State<Arc<HlsPutSinkState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let name =
        hls_put_sink_file_name(uri.path(), uri.query()).unwrap_or_else(|| "index.m3u8".to_string());
    let name = name.replace('\\', "/").trim_start_matches('/').to_string();
    if name.is_empty() || name.split('/').any(|part| part == "..") {
        return StatusCode::BAD_REQUEST;
    }

    let target = state.root.join(&name);
    if let Some(parent) = target.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "[hls-put-sink] failed to create {}: {err}",
            parent.display()
        );
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    if let Err(err) = std::fs::write(&target, &body) {
        eprintln!("[hls-put-sink] failed to write {}: {err}", target.display());
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE.as_str())
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let record = json!({
        "path": uri.to_string(),
        "file": name,
        "contentType": content_type,
        "bytes": body.len(),
    });
    let _guard = state.write_lock.lock().unwrap_or_else(|e| e.into_inner());
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.requests_path)
    {
        Ok(mut file) => {
            if let Err(err) = writeln!(file, "{record}") {
                eprintln!(
                    "[hls-put-sink] failed to append {}: {err}",
                    state.requests_path.display()
                );
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        }
        Err(err) => {
            eprintln!(
                "[hls-put-sink] failed to open {}: {err}",
                state.requests_path.display()
            );
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }
    StatusCode::NO_CONTENT
}

fn hls_put_sink_file_name(path: &str, query: Option<&str>) -> Option<String> {
    query
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == "file").then(|| value.to_string())
            })
        })
        .or_else(|| {
            let trimmed = path.trim_start_matches('/');
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
}

pub(crate) async fn wait_for_hls_put_artifacts(
    sink_dir: &Path,
    timeout: Duration,
) -> Result<HlsPutArtifacts, String> {
    let deadline = Instant::now() + timeout;
    let youtube_playlist = sink_dir.join("out.m3u8");
    loop {
        let youtube_segment = first_segment_in(sink_dir);
        if youtube_playlist.is_file()
            && file_nonempty(&youtube_playlist)
            && let Some(youtube_segment) = youtube_segment
        {
            return Ok(HlsPutArtifacts {
                youtube_playlist,
                youtube_segment,
            });
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for HLS PUT playlist/segment artifacts in {}",
                sink_dir.display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn first_segment_in(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_segment_file(name, "seg"))
                && file_nonempty(path)
        })
        .collect();
    entries.sort();
    entries.into_iter().next()
}

fn file_nonempty(path: &Path) -> bool {
    path.metadata().map(|meta| meta.len() > 0).unwrap_or(false)
}

fn validate_hls_playlist(path: &Path, label: &str) -> Result<(), String> {
    let playlist = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if !playlist.contains("#EXTM3U") {
        return Err(format!("{label} HLS PUT playlist missing EXTM3U header"));
    }
    if !playlist.contains(".ts") {
        return Err(format!(
            "{label} HLS PUT playlist missing segment reference"
        ));
    }
    Ok(())
}

pub(crate) fn read_hls_put_requests(sink_dir: &Path) -> Result<Vec<Value>, String> {
    let path = sink_dir.join("requests.jsonl");
    let body = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    body.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|e| e.to_string()))
        .collect()
}

pub(crate) fn request_seen(requests: &[Value], predicate: impl Fn(&Value) -> bool) -> bool {
    requests.iter().any(predicate)
}

pub(crate) fn is_segment_file(file: &str, prefix: &str) -> bool {
    file.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(".ts"))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}
