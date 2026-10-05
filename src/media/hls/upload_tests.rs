use super::*;
use axum::Router;
use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::put;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::domain::stage::{StageKey, StageKind};

/// `uri` with the per-attempt session token (`r<hex>-`) of segment names
/// replaced by `S-`, so tests can name segments.
fn normalized(uri: &str) -> String {
    let mut out = String::with_capacity(uri.len());
    let mut rest = uri;
    while let Some(at) = rest.find(['/', '=']) {
        out.push_str(&rest[..=at]);
        rest = &rest[at + 1..];
        let token = rest
            .strip_prefix('r')
            .map(|tail| tail.chars().take_while(char::is_ascii_hexdigit).count());
        if let Some(digits) = token.filter(|digits| *digits > 0)
            && rest[1 + digits..].starts_with('-')
        {
            out.push('S');
            rest = &rest[1 + digits..];
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn session_tokens_are_valid_segment_name_characters() {
    let token = upload_session_token();
    assert!(token.starts_with('r') && token.len() > 1);
    assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(normalized(&format!("/live/{token}-7.ts")), "/live/S-7.ts");
    assert_eq!(
        normalized(&format!("/u?cid=a&file={token}-0.ts")),
        "/u?cid=a&file=S-0.ts"
    );
    assert_eq!(normalized("/live/out.m3u8"), "/live/out.m3u8");
}

fn planned_hls_key(pipeline_id: &str) -> StageKey {
    StageKey::new(pipeline_id, StageKind::hls_segmenter(StageKind::source()))
}

#[test]
fn derives_segment_url_from_file_query() {
    let playlist =
        Url::parse("https://a.upload.youtube.com/http_upload_hls?cid=abc&copy=0&file=out.m3u8")
            .unwrap();
    let segment = derive_hls_upload_url(&playlist, "seg42.ts");
    assert_eq!(
        segment.as_str(),
        "https://a.upload.youtube.com/http_upload_hls?cid=abc&copy=0&file=seg42.ts"
    );
}

#[test]
fn derives_segment_url_from_playlist_path() {
    let playlist = Url::parse("https://example.com/live/out.m3u8").unwrap();
    let segment = derive_hls_upload_url(&playlist, "seg42.ts");
    assert_eq!(segment.as_str(), "https://example.com/live/seg42.ts");
}

#[test]
fn derives_segment_url_from_directory_path() {
    let playlist = Url::parse("https://example.com/live/channel/").unwrap();
    let segment = derive_hls_upload_url(&playlist, "seg42.ts");
    assert_eq!(
        segment.as_str(),
        "https://example.com/live/channel/seg42.ts"
    );
}

#[test]
fn preserves_signed_query_for_path_style_uploads() {
    let playlist = Url::parse("https://example.com/live/out.m3u8?hdnea=token&policy=abc").unwrap();
    let segment = derive_hls_upload_url(&playlist, "seg42.ts");
    assert_eq!(
        segment.as_str(),
        "https://example.com/live/seg42.ts?hdnea=token&policy=abc"
    );
}

#[tokio::test]
async fn uploads_segments_and_playlist_to_put_sink() {
    let seen = Arc::new(Mutex::new(Vec::<(String, String, Vec<u8>)>::new()));
    let seen_for_handler = seen.clone();
    let agents = Arc::new(Mutex::new(HashSet::<String>::new()));
    let agents_for_handler = agents.clone();
    let app = Router::new().route(
        "/{*path}",
        put(move |uri: OriginalUri, headers: HeaderMap, body: Bytes| {
            let seen = seen_for_handler.clone();
            let agents = agents_for_handler.clone();
            async move {
                let header = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string()
                };
                let content_type = header(reqwest::header::CONTENT_TYPE.as_str());
                agents
                    .lock()
                    .unwrap()
                    .insert(header(reqwest::header::USER_AGENT.as_str()));
                seen.lock().unwrap().push((
                    normalized(&uri.0.to_string()),
                    content_type,
                    body.to_vec(),
                ));
                StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let engine = Arc::new(MediaEngine::new());
    let terminal_stage_key = planned_hls_key("pipe1");
    let registration = engine
        .register_egress_attempt(
            "out1",
            "pipe1",
            &format!("http://{addr}/upload?cid=abc&file=out.m3u8"),
            Some(terminal_stage_key.clone()),
        )
        .await;
    let uploader = tokio::spawn(start_hls_put_upload(
        HlsUploadStart {
            output_id: "out1".to_string(),
            pipeline_id: "pipe1".to_string(),
            target_url: format!("http://{addr}/upload?cid=abc&file=out.m3u8"),
            terminal_stage_key,
        },
        store,
        engine,
        registration.clone(),
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if seen.lock().unwrap().len() >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for PUT uploads"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    registration.cancel_token.cancel();
    let _ = uploader.await;

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(|(uri, content_type, body)| {
            uri == "/upload?cid=abc&file=S-0.ts"
                && content_type == super::super::upload_policy::HLS_SEGMENT_CONTENT_TYPE
                && body == b"segment-0"
        }),
        "segment PUT not observed: {seen:?}"
    );
    assert!(
        seen.iter().any(|(uri, content_type, body)| {
            uri == "/upload?cid=abc&file=out.m3u8"
                && content_type == super::super::upload_policy::HLS_PLAYLIST_CONTENT_TYPE
                && body.starts_with(b"#EXTM3U")
        }),
        "playlist PUT not observed: {seen:?}"
    );
    assert_eq!(
        *agents.lock().unwrap(),
        HashSet::from([HLS_UPLOAD_USER_AGENT.to_string()]),
        "every request carries the encoder User-Agent"
    );
}

/// Uploads follow publishes: the playlist goes out once per new segment,
/// not on a timer (the uploader used to re-PUT an unchanged playlist
/// every 500 ms per output), and a new segment goes out when published.
#[tokio::test]
async fn uploads_follow_publishes_instead_of_a_timer() {
    let seen = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let seen_for_handler = seen.clone();
    let app = Router::new().route(
        "/{*path}",
        put(move |uri: OriginalUri| {
            let seen = seen_for_handler.clone();
            async move {
                *seen
                    .lock()
                    .unwrap()
                    .entry(normalized(&uri.0.to_string()))
                    .or_default() += 1;
                StatusCode::NO_CONTENT
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let count = |uri: &str| seen.lock().unwrap().get(uri).copied().unwrap_or(0);
    let wait_for = |uri: &'static str, target: usize| {
        let seen = seen.clone();
        async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while seen.lock().unwrap().get(uri).copied().unwrap_or(0) < target {
                assert!(tokio::time::Instant::now() < deadline, "no PUT of {uri}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };

    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let engine = Arc::new(MediaEngine::new());
    let terminal_stage_key = planned_hls_key("pipe1");
    let target_url = format!("http://{addr}/live/out.m3u8");
    let registration = engine
        .register_egress_attempt(
            "out1",
            "pipe1",
            &target_url,
            Some(terminal_stage_key.clone()),
        )
        .await;
    let uploader = tokio::spawn(start_hls_put_upload(
        HlsUploadStart {
            output_id: "out1".to_string(),
            pipeline_id: "pipe1".to_string(),
            target_url,
            terminal_stage_key,
        },
        store.clone(),
        engine,
        registration.clone(),
    ));

    wait_for("/live/out.m3u8", 1).await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(count("/live/out.m3u8"), 1, "unchanged playlist re-sent");

    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-1"));
    wait_for("/live/S-1.ts", 1).await;
    wait_for("/live/out.m3u8", 2).await;
    assert_eq!(count("/live/S-0.ts"), 1, "a segment is sent once");

    registration.cancel_token.cancel();
    let _ = uploader.await;
}

#[tokio::test]
async fn put_bytes_times_out_against_hung_sink() {
    let app = Router::new().route(
        "/{*path}",
        put(|| async {
            tokio::time::sleep(Duration::from_millis(250)).await;
            StatusCode::NO_CONTENT
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::new();
    let request = UploadRequest {
        target: UploadTarget::Playlist {
            last_sequence: None,
            end: false,
        },
        file_name: String::new(),
        content_type: super::super::upload_policy::HLS_PLAYLIST_CONTENT_TYPE,
        body: Bytes::from_static(b"#EXTM3U"),
        fresh_connection: false,
    };
    let result = send_upload(
        &client,
        Url::parse(&format!("http://{addr}/upload?file=out.m3u8")).unwrap(),
        &request,
        Duration::from_millis(50),
    )
    .await;

    let err = result.expect_err("hung sink should time out");
    assert!(
        err.to_ascii_lowercase().contains("timed out"),
        "expected timeout error, got: {err}"
    );
}

#[tokio::test]
async fn uploader_retries_after_transient_upload_error() {
    let seen = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let seen_for_handler = seen.clone();
    let app = Router::new().route(
        "/{*path}",
        put(move |uri: OriginalUri| {
            let seen = seen_for_handler.clone();
            async move {
                let uri = normalized(&uri.0.to_string());
                let mut seen = seen.lock().unwrap();
                let count = seen.entry(uri.clone()).or_default();
                *count += 1;
                if uri.ends_with("file=S-0.ts") && *count == 1 {
                    StatusCode::BAD_GATEWAY
                } else {
                    StatusCode::NO_CONTENT
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let engine = Arc::new(MediaEngine::new());
    let terminal_stage_key = planned_hls_key("pipe1");
    let registration = engine
        .register_egress_attempt(
            "out1",
            "pipe1",
            &format!("http://{addr}/upload?cid=abc&file=out.m3u8"),
            Some(terminal_stage_key.clone()),
        )
        .await;

    let engine_for_uploader = engine.clone();
    let uploader = tokio::spawn(start_hls_put_upload(
        HlsUploadStart {
            output_id: "out1".to_string(),
            pipeline_id: "pipe1".to_string(),
            target_url: format!("http://{addr}/upload?cid=abc&file=out.m3u8"),
            terminal_stage_key,
        },
        store,
        engine_for_uploader,
        registration.clone(),
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut saw_retry_state = false;
    loop {
        saw_retry_state |= engine.egress_retry_state("out1").await.is_some();
        let (segment_attempts, playlist_attempts) = {
            let seen = seen.lock().unwrap();
            (
                seen.get("/upload?cid=abc&file=S-0.ts")
                    .copied()
                    .unwrap_or(0),
                seen.get("/upload?cid=abc&file=out.m3u8")
                    .copied()
                    .unwrap_or(0),
            )
        };
        if segment_attempts >= 2 && playlist_attempts >= 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for retried PUT upload"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw_retry_state,
        "transient upload failure should publish retry state"
    );
    assert!(
        engine.egress_retry_state("out1").await.is_none(),
        "retry state should clear after upload recovery"
    );
    registration.cancel_token.cancel();
    let _ = uploader.await;
}

/// A sink that records every PUT (normalized URI, body) and answers with
/// `status`.
async fn recording_sink(
    status: StatusCode,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, Vec<u8>)>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_handler = seen.clone();
    let app = Router::new().route(
        "/{*path}",
        put(move |uri: OriginalUri, body: Bytes| {
            let seen = seen_for_handler.clone();
            async move {
                seen.lock()
                    .unwrap()
                    .push((normalized(&uri.0.to_string()), body.to_vec()));
                status
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, seen)
}

async fn spawn_uploader(
    target_url: String,
    store: Arc<HlsStore>,
) -> (EgressRegistration, tokio::task::JoinHandle<()>) {
    let engine = Arc::new(MediaEngine::new());
    let terminal_stage_key = planned_hls_key("pipe1");
    let registration = engine
        .register_egress_attempt(
            "out1",
            "pipe1",
            &target_url,
            Some(terminal_stage_key.clone()),
        )
        .await;
    let uploader = tokio::spawn(start_hls_put_upload(
        HlsUploadStart {
            output_id: "out1".to_string(),
            pipeline_id: "pipe1".to_string(),
            target_url,
            terminal_stage_key,
        },
        store,
        engine,
        registration.clone(),
    ));
    (registration, uploader)
}

/// Stopping an output sends a last playlist ending in EXT-X-ENDLIST
/// (Akamai marks a finished live stream this way), then the task ends.
#[tokio::test]
async fn stopping_sends_a_final_playlist_with_endlist() {
    let (addr, seen) = recording_sink(StatusCode::OK).await;
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let (registration, uploader) =
        spawn_uploader(format!("http://{addr}/live/out.m3u8"), store.clone()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while seen.lock().unwrap().len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "no first upload");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    registration.cancel_token.cancel();
    tokio::time::timeout(Duration::from_secs(3), uploader)
        .await
        .expect("the uploader ends after a stop")
        .unwrap();

    let seen = seen.lock().unwrap();
    let (uri, body) = seen.last().unwrap();
    assert_eq!(uri, "/live/out.m3u8");
    let playlist = std::str::from_utf8(body).unwrap();
    assert!(
        playlist.contains("-0.ts\n") && playlist.ends_with("#EXT-X-ENDLIST\n"),
        "{playlist}"
    );
}

/// A 401 (YouTube: the cid expired) ends the uploader without retrying.
#[tokio::test]
async fn a_rejected_upload_ends_the_uploader_without_retrying() {
    let (addr, seen) = recording_sink(StatusCode::UNAUTHORIZED).await;
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let (_registration, uploader) =
        spawn_uploader(format!("http://{addr}/live/out.m3u8"), store.clone()).await;

    tokio::time::timeout(Duration::from_secs(3), uploader)
        .await
        .expect("a rejected uploader stops by itself")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "no retry after a 401");
}

/// A segment dropped after failing for its window keeps its failure
/// visible: the recorded failure is still an `upload_segment` failure
/// and names the cause, not just the drop (the hung-sink fault case
/// checks that operators see the timeout).
#[tokio::test]
async fn a_dropped_segment_keeps_its_failure_cause_visible() {
    let (addr, _seen) = recording_sink(StatusCode::SERVICE_UNAVAILABLE).await;
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let engine = Arc::new(MediaEngine::new());
    let terminal_stage_key = planned_hls_key("pipe1");
    let target_url = format!("http://{addr}/live/out.m3u8");
    let registration = engine
        .register_egress_attempt(
            "out1",
            "pipe1",
            &target_url,
            Some(terminal_stage_key.clone()),
        )
        .await;
    let uploader = tokio::spawn(start_hls_put_upload(
        HlsUploadStart {
            output_id: "out1".to_string(),
            pipeline_id: "pipe1".to_string(),
            target_url,
            terminal_stage_key,
        },
        store.clone(),
        engine.clone(),
        registration.clone(),
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let (phase, error) = loop {
        let observed = engine
            .with_active_egress("out1", |egress| {
                (
                    crate::sync::lock(&egress.failure_phase).clone(),
                    crate::sync::lock(&egress.last_error).clone(),
                )
            })
            .await
            .unwrap_or_default();
        if observed
            .1
            .as_deref()
            .is_some_and(|error| error.contains("dropped"))
        {
            break observed;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no drop recorded: {observed:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(phase.as_deref(), Some("upload_segment"));
    let error = error.unwrap();
    assert!(error.contains("503"), "the cause is lost: {error}");

    registration.cancel_token.cancel();
    let _ = uploader.await;
}

#[tokio::test]
async fn uploader_rejects_terminal_stage_mismatch() {
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let engine = Arc::new(MediaEngine::new());
    let registered_key = planned_hls_key("pipe1");
    let registration = engine
        .register_egress_attempt(
            "out1",
            "pipe1",
            "http://127.0.0.1:9/upload?file=out.m3u8",
            Some(registered_key),
        )
        .await;

    start_hls_put_upload(
        HlsUploadStart {
            output_id: "out1".to_string(),
            pipeline_id: "pipe1".to_string(),
            target_url: "http://127.0.0.1:9/upload?file=out.m3u8".to_string(),
            terminal_stage_key: StageKey::new(
                "pipe1",
                StageKind::hls_segmenter(StageKind::video_preset("720p")),
            ),
        },
        store,
        engine.clone(),
        registration,
    )
    .await;

    let error = engine
        .with_active_egress("out1", |egress| {
            crate::sync::lock(&egress.last_error).clone()
        })
        .await
        .flatten()
        .expect("terminal mismatch should record an egress error");
    assert!(
        error.contains("expected terminal stage"),
        "unexpected mismatch error: {error}"
    );
}

/// An origin on a std thread that answers 503 to the first request and 200
/// after, recording the connection each request arrived on.
/// `(connection number, request target)` per request, in arrival order.
type SeenRequests = Arc<Mutex<Vec<(usize, String)>>>;

fn connection_recording_origin() -> (std::net::SocketAddr, SeenRequests) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<(usize, String)>::new()));
    let seen_for_accept = seen.clone();
    std::thread::spawn(move || {
        for (connection, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { return };
            let seen = seen_for_accept.clone();
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                loop {
                    let head_end = loop {
                        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            break end + 4;
                        }
                        let mut chunk = [0u8; 4096];
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                    let target = head.split_whitespace().nth(1).unwrap_or("").to_string();
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_string)
                        })
                        .map_or(0, |value| value.trim().parse().unwrap());
                    while buffer.len() < head_end + length {
                        let mut chunk = [0u8; 4096];
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                        }
                    }
                    buffer.drain(..head_end + length);
                    let nth = {
                        let mut seen = crate::sync::lock(&seen);
                        seen.push((connection, target));
                        seen.len() - 1
                    };
                    let status = if nth == 0 { "503 Busy" } else { "200 OK" };
                    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                    if stream.write_all(response.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, seen)
}

/// After a retryable failure the retry goes out on a new connection (and
/// so a new DNS lookup), as Akamai asks, not on the keep-alive connection
/// of the failed node.
#[tokio::test]
async fn a_retry_uses_a_new_connection() {
    let (addr, seen) = connection_recording_origin();
    let store = Arc::new(HlsStore::new());
    store.push_segment(1.2, bytes::Bytes::from_static(b"segment-0"));
    let (registration, uploader) =
        spawn_uploader(format!("http://{addr}/live/out.m3u8"), store.clone()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while crate::sync::lock(&seen).len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "no retry");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = crate::sync::lock(&seen).clone();
    assert_eq!(seen[0].1, seen[1].1, "the same segment is retried");
    assert_ne!(seen[0].0, seen[1].0, "the retry opened a new connection");

    registration.cancel_token.cancel();
    let _ = uploader.await;
}

/// Session tokens are unique even when many outputs start at once.
#[test]
fn session_tokens_do_not_repeat_within_a_burst() {
    let tokens: HashSet<String> = (0..10_000).map(|_| upload_session_token()).collect();
    assert_eq!(tokens.len(), 10_000);
}
