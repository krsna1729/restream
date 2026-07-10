//! HTTP PUT uploader for remote HLS ingest targets.
//!
//! YouTube-style endpoints pass the target object name as a `file=` query
//! parameter. Other HLS PUT origins commonly use a playlist path and expect
//! segments beside it. This module supports both shapes.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::error;

use reqwest::{Client, Url};

use super::{HlsStore, HlsStoreSnapshot};
use crate::domain::stage::StageKey;
use crate::domain::state::EgressPhase;
use crate::media::engine::{EgressRegistration, MediaEngine};

const HLS_PLAYLIST_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
const HLS_SEGMENT_CONTENT_TYPE: &str = "video/mp2t";
const UPLOAD_INTERVAL: Duration = Duration::from_millis(500);
const HLS_UPLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const HLS_UPLOAD_RETRY_BACKOFF: Duration = Duration::from_secs(1);

pub struct HlsUploadStart {
    pub output_id: String,
    pub pipeline_id: String,
    pub target_url: String,
    pub terminal_stage_key: StageKey,
}

pub async fn start_hls_put_upload(
    start: HlsUploadStart,
    store: Arc<HlsStore>,
    engine: Arc<MediaEngine>,
    registration: EgressRegistration,
) {
    let HlsUploadStart {
        output_id,
        pipeline_id,
        target_url,
        terminal_stage_key,
    } = start;

    let terminal_matches = engine
        .with_active_egress(&output_id, |egress| {
            egress.attempt_id == registration.attempt_id
                && egress.terminal_stage_key.as_ref() == Some(&terminal_stage_key)
        })
        .await
        .unwrap_or(false);
    if !terminal_matches {
        engine
            .record_egress_error_if_current(
                &output_id,
                &registration,
                "hls_terminal_stage_mismatch",
                format!("expected terminal stage {terminal_stage_key}"),
            )
            .await;
        return;
    }

    engine
        .update_egress_phase_if_current(&output_id, &registration, EgressPhase::Uploading)
        .await;
    let playlist_url = match Url::parse(&target_url) {
        Ok(url) => url,
        Err(err) => {
            error!(output_id = %output_id, err = %err, "invalid HLS upload URL");
            engine
                .record_egress_error_if_current(
                    &output_id,
                    &registration,
                    "parse_url",
                    err.to_string(),
                )
                .await;
            return;
        }
    };
    if let Some(host) = playlist_url.host_str() {
        let port = playlist_url
            .port_or_known_default()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        engine
            .update_egress_target_addr_if_current(
                &output_id,
                &registration,
                format!("{host}:{port}"),
            )
            .await;
    }
    let client = Client::new();
    let mut uploaded_segments = HashSet::new();
    let mut retry_attempts = 0u32;

    loop {
        tokio::select! {
            _ = registration.cancel_token.cancelled() => return,
            _ = tokio::time::sleep(UPLOAD_INTERVAL) => {}
        }

        let Some(snapshot) = store.snapshot() else {
            continue;
        };
        prune_uploaded_segments(&mut uploaded_segments, &snapshot);

        let mut upload_failed = false;
        for segment in snapshot.segments {
            if uploaded_segments.contains(&segment.index) {
                continue;
            }
            let segment_name = format!("seg{}.ts", segment.index);
            let segment_url = derive_hls_upload_url(&playlist_url, &segment_name);
            let segment_len = segment.data.len() as u64;
            match put_bytes_with_timeout(
                &client,
                segment_url,
                HLS_SEGMENT_CONTENT_TYPE,
                segment.data,
                HLS_UPLOAD_REQUEST_TIMEOUT,
            )
            .await
            {
                Ok(()) => {
                    uploaded_segments.insert(segment.index);
                    engine
                        .record_egress_progress_if_current(&output_id, &registration, segment_len)
                        .await;
                }
                Err(err) => {
                    error!(
                        "[hls-upload] Segment upload failed output={} pipeline={} segment={}: {}",
                        output_id, pipeline_id, segment_name, err
                    );
                    engine
                        .record_egress_error_if_current(
                            &output_id,
                            &registration,
                            "upload_segment",
                            err,
                        )
                        .await;
                    retry_attempts = retry_attempts.saturating_add(1);
                    publish_upload_retry_state(&engine, &output_id, &registration, retry_attempts)
                        .await;
                    upload_failed = true;
                    break;
                }
            }
        }
        if upload_failed {
            if wait_for_upload_retry_backoff(&registration).await {
                return;
            }
            continue;
        }

        let playlist_bytes = snapshot.playlist.into_bytes();
        let playlist_len = playlist_bytes.len() as u64;
        if let Err(err) = put_bytes(
            &client,
            playlist_url.clone(),
            HLS_PLAYLIST_CONTENT_TYPE,
            playlist_bytes,
        )
        .await
        {
            error!(
                "[hls-upload] Playlist upload failed output={} pipeline={}: {}",
                output_id, pipeline_id, err
            );
            engine
                .record_egress_error_if_current(&output_id, &registration, "upload_playlist", err)
                .await;
            retry_attempts = retry_attempts.saturating_add(1);
            publish_upload_retry_state(&engine, &output_id, &registration, retry_attempts).await;
            if wait_for_upload_retry_backoff(&registration).await {
                return;
            }
        } else {
            retry_attempts = 0;
            engine.clear_egress_retry_state(&output_id).await;
            engine
                .record_egress_progress_if_current(&output_id, &registration, playlist_len)
                .await;
        }
    }
}

fn prune_uploaded_segments(uploaded_segments: &mut HashSet<u64>, snapshot: &HlsStoreSnapshot) {
    uploaded_segments.retain(|index| {
        snapshot
            .segments
            .iter()
            .any(|segment| segment.index == *index)
    });
}

async fn wait_for_upload_retry_backoff(registration: &EgressRegistration) -> bool {
    tokio::select! {
        _ = registration.cancel_token.cancelled() => true,
        _ = tokio::time::sleep(HLS_UPLOAD_RETRY_BACKOFF) => false,
    }
}

async fn publish_upload_retry_state(
    engine: &MediaEngine,
    output_id: &str,
    registration: &EgressRegistration,
    attempts: u32,
) {
    let backoff_ms = HLS_UPLOAD_RETRY_BACKOFF
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    engine
        .update_egress_retry_state_if_current(
            output_id,
            registration,
            attempts,
            backoff_ms,
            backoff_ms,
        )
        .await;
}

async fn put_bytes<B>(
    client: &Client,
    url: Url,
    content_type: &'static str,
    body: B,
) -> Result<(), String>
where
    B: Into<reqwest::Body>,
{
    put_bytes_with_timeout(client, url, content_type, body, HLS_UPLOAD_REQUEST_TIMEOUT).await
}

async fn put_bytes_with_timeout<B>(
    client: &Client,
    url: Url,
    content_type: &'static str,
    body: B,
    timeout: Duration,
) -> Result<(), String>
where
    B: Into<reqwest::Body>,
{
    let status = client
        .put(url.clone())
        .timeout(timeout)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                format!("PUT {url} timed out after {} ms", timeout.as_millis())
            } else {
                err.to_string()
            }
        })?
        .status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("PUT {} returned {}", url, status))
    }
}

pub(crate) fn derive_hls_upload_url(playlist_url: &Url, file_name: &str) -> Url {
    let mut url = playlist_url.clone();
    let original_pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    if original_pairs.iter().any(|(key, _)| key == "file") {
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in original_pairs {
                if key == "file" {
                    pairs.append_pair(&key, file_name);
                } else {
                    pairs.append_pair(&key, &value);
                }
            }
        }
        return url;
    }

    let path = url.path();
    let new_path = if path.ends_with('/') {
        format!("{path}{file_name}")
    } else if path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'))
    {
        let prefix = path
            .rsplit_once('/')
            .map(|(prefix, _)| prefix)
            .unwrap_or("");
        if prefix.is_empty() {
            format!("/{file_name}")
        } else {
            format!("{prefix}/{file_name}")
        }
    } else {
        format!("{}/{}", path.trim_end_matches('/'), file_name)
    };
    url.set_path(&new_path);
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::OriginalUri;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::put;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::hls::HlsSegmentSnapshot;

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
        let playlist =
            Url::parse("https://example.com/live/out.m3u8?hdnea=token&policy=abc").unwrap();
        let segment = derive_hls_upload_url(&playlist, "seg42.ts");
        assert_eq!(
            segment.as_str(),
            "https://example.com/live/seg42.ts?hdnea=token&policy=abc"
        );
    }

    #[test]
    fn uploaded_segment_tracking_is_pruned_to_current_snapshot() {
        let mut uploaded_segments = HashSet::from([0, 1, 2, 3]);
        let snapshot = HlsStoreSnapshot {
            playlist: "#EXTM3U\n".to_string(),
            segments: vec![
                HlsSegmentSnapshot {
                    index: 2,
                    data: Bytes::new(),
                },
                HlsSegmentSnapshot {
                    index: 3,
                    data: Bytes::new(),
                },
                HlsSegmentSnapshot {
                    index: 4,
                    data: Bytes::new(),
                },
            ],
        };

        prune_uploaded_segments(&mut uploaded_segments, &snapshot);

        assert_eq!(uploaded_segments, HashSet::from([2, 3]));
    }

    #[tokio::test]
    async fn uploads_segments_and_playlist_to_put_sink() {
        let seen = Arc::new(Mutex::new(Vec::<(String, String, Vec<u8>)>::new()));
        let seen_for_handler = seen.clone();
        let app = Router::new().route(
            "/*path",
            put(move |uri: OriginalUri, headers: HeaderMap, body: Bytes| {
                let seen = seen_for_handler.clone();
                async move {
                    let content_type = headers
                        .get(reqwest::header::CONTENT_TYPE.as_str())
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    seen.lock()
                        .unwrap()
                        .push((uri.0.to_string(), content_type, body.to_vec()));
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
                uri == "/upload?cid=abc&file=seg0.ts"
                    && content_type == HLS_SEGMENT_CONTENT_TYPE
                    && body == b"segment-0"
            }),
            "segment PUT not observed: {seen:?}"
        );
        assert!(
            seen.iter().any(|(uri, content_type, body)| {
                uri == "/upload?cid=abc&file=out.m3u8"
                    && content_type == HLS_PLAYLIST_CONTENT_TYPE
                    && body.starts_with(b"#EXTM3U")
            }),
            "playlist PUT not observed: {seen:?}"
        );
    }

    #[tokio::test]
    async fn put_bytes_times_out_against_hung_sink() {
        let app = Router::new().route(
            "/*path",
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
        let result = put_bytes_with_timeout(
            &client,
            Url::parse(&format!("http://{addr}/upload?file=out.m3u8")).unwrap(),
            HLS_PLAYLIST_CONTENT_TYPE,
            Bytes::from_static(b"#EXTM3U"),
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
            "/*path",
            put(move |uri: OriginalUri| {
                let seen = seen_for_handler.clone();
                async move {
                    let uri = uri.0.to_string();
                    let mut seen = seen.lock().unwrap();
                    let count = seen.entry(uri.clone()).or_default();
                    *count += 1;
                    if uri.ends_with("file=seg0.ts") && *count == 1 {
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
                    seen.get("/upload?cid=abc&file=seg0.ts")
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
                egress
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone()
            })
            .await
            .flatten()
            .expect("terminal mismatch should record an egress error");
        assert!(
            error.contains("expected terminal stage"),
            "unexpected mismatch error: {error}"
        );
    }
}
