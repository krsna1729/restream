//! HTTP PUT uploader for remote HLS ingest targets, on Tokio with Reqwest.
//!
//! What to send and how to react is [`super::upload_policy`]; this module is
//! the transport and the status reporting.
//!
//! YouTube-style endpoints pass the target object name as a `file=` query
//! parameter. Other HLS PUT origins commonly use a playlist path and expect
//! segments beside it. This module supports both shapes.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

use reqwest::{Client, Url};

use super::HlsStore;
use super::upload_policy::{
    Next, ResultEffect, Stopped, UploadOutcome, UploadPolicy, UploadRequest, UploadTarget, backoff,
};
use crate::domain::stage::StageKey;
use crate::domain::state::EgressPhase;
use crate::media::engine::{EgressRegistration, MediaEngine};

const HLS_UPLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// The end-of-stream playlist after a stop gets this long, once.
const HLS_UPLOAD_END_TIMEOUT: Duration = Duration::from_secs(1);

/// `<manufacturer> / <model> / <version>`: YouTube asks for this form and
/// Akamai requires a User-Agent on every request.
const HLS_UPLOAD_USER_AGENT: &str = concat!("Restream / restream / ", env!("CARGO_PKG_VERSION"));

/// One client for every uploader: one connection pool and one TLS
/// configuration, instead of a pool and TLS state per output.
static HLS_UPLOAD_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent(HLS_UPLOAD_USER_AGENT)
        .build()
        .unwrap_or_else(|_| Client::new())
});

pub struct HlsUploadStart {
    pub output_id: String,
    pub pipeline_id: String,
    pub target_url: String,
    pub terminal_stage_key: StageKey,
}

/// A segment-name prefix unique to this output attempt, also across process
/// restarts (YouTube and Akamai require segment names never to repeat).
pub(crate) fn upload_session_token() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    format!("r{millis:x}")
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
            warn!(output_id = %output_id, err = %err, "invalid HLS upload URL");
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
    let mut published = store.subscribe();
    drop(store);
    // Once every store handle is gone no segment can follow, but uploads
    // already taken (and their retries) still finish.
    let mut store_open = true;
    let mut policy = UploadPolicy::new(upload_session_token());
    let report = Report {
        engine: &engine,
        output_id: &output_id,
        pipeline_id: &pipeline_id,
        registration: &registration,
    };

    let mut dropped = 0;
    // Why the last request failed, so a drop reports its cause.
    let mut last_failure: Option<String> = None;
    loop {
        let next = policy.next(Instant::now());
        if policy.dropped_segments() > dropped {
            dropped = policy.dropped_segments();
            report.dropped(dropped, last_failure.as_deref()).await;
        }
        match next {
            Next::Put(request) => {
                let url = object_url(&playlist_url, &request);
                let ending = matches!(request.target, UploadTarget::Playlist { end: true, .. });
                let timeout = if ending {
                    HLS_UPLOAD_END_TIMEOUT
                } else {
                    HLS_UPLOAD_REQUEST_TIMEOUT
                };
                let send = send_upload(&HLS_UPLOAD_CLIENT, url, &request, timeout);
                let result = if ending {
                    send.await
                } else {
                    tokio::select! {
                        result = send => result,
                        _ = registration.cancel_token.cancelled() => {
                            // Abandon the request; the stop sends the end
                            // playlist next.
                            policy.on_result(UploadOutcome::Transport, Instant::now());
                            policy.finish();
                            continue;
                        }
                    }
                };
                let (outcome, failure) = match result {
                    Ok(status) if status.is_success() => {
                        (UploadOutcome::Status(status.as_u16()), None)
                    }
                    Ok(status) => (
                        UploadOutcome::Status(status.as_u16()),
                        Some(format!(
                            "PUT {} returned HTTP {status}",
                            object_name(&request)
                        )),
                    ),
                    Err(error) => (UploadOutcome::Transport, Some(error)),
                };
                if failure.is_some() {
                    last_failure.clone_from(&failure);
                }
                if let Some(effect) = policy.on_result(outcome, Instant::now()) {
                    report.result(&request, effect, failure).await;
                }
            }
            Next::Wait(until) => {
                if !store_open && until.is_none() {
                    return;
                }
                let backoff = async {
                    match until {
                        Some(until) => tokio::time::sleep_until(until.into()).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    _ = registration.cancel_token.cancelled(), if !policy.is_finishing() => {
                        policy.finish();
                    }
                    changed = published.changed(), if store_open => {
                        if changed.is_err() {
                            store_open = false;
                        } else if let Some(snapshot) = published.borrow_and_update().clone() {
                            policy.on_publish(&snapshot);
                        }
                    }
                    _ = backoff => {}
                }
            }
            Next::Stop(Stopped::Ended) => return,
            Next::Stop(Stopped::Rejected { status }) => {
                warn!(
                    output_id = %output_id,
                    pipeline_id = %pipeline_id,
                    status,
                    "HLS ingest rejected the upload; not retrying"
                );
                return;
            }
        }
    }
}

/// Status reporting for one uploader.
struct Report<'a> {
    engine: &'a MediaEngine,
    output_id: &'a str,
    pipeline_id: &'a str,
    registration: &'a EgressRegistration,
}

impl Report<'_> {
    /// A segment failed for its whole retry window. The failure stays
    /// recorded as an `upload_segment` failure with its cause (a timeout, a
    /// status, a reset); the drop only adds to the message.
    async fn dropped(&self, total: u64, cause: Option<&str>) {
        let cause = cause.unwrap_or("upload failed");
        warn!(
            output_id = %self.output_id,
            pipeline_id = %self.pipeline_id,
            total,
            cause,
            "HLS segment still failing after its duration; dropped"
        );
        self.engine
            .record_egress_error_if_current(
                self.output_id,
                self.registration,
                "upload_segment",
                format!("{cause}; segment dropped after failing for its duration ({total} so far)"),
            )
            .await;
    }

    async fn result(&self, request: &UploadRequest, effect: ResultEffect, failure: Option<String>) {
        let kind = match request.target {
            UploadTarget::Segment { .. } => "upload_segment",
            UploadTarget::Playlist { .. } => "upload_playlist",
        };
        match effect {
            ResultEffect::Acknowledged { bytes } => {
                self.engine.clear_egress_retry_state(self.output_id).await;
                self.engine
                    .record_egress_progress_if_current(self.output_id, self.registration, bytes)
                    .await;
            }
            ResultEffect::WillRetry { attempts } => {
                let error = failure.unwrap_or_else(|| "upload failed".to_string());
                warn!(
                    output_id = %self.output_id,
                    pipeline_id = %self.pipeline_id,
                    object = %request.file_name,
                    attempts,
                    error = %error,
                    "HLS upload failed; retrying"
                );
                self.engine
                    .record_egress_error_if_current(self.output_id, self.registration, kind, error)
                    .await;
                let backoff_ms = u64::try_from(backoff(attempts).as_millis()).unwrap_or(u64::MAX);
                self.engine
                    .update_egress_retry_state_if_current(
                        self.output_id,
                        self.registration,
                        attempts,
                        backoff_ms,
                        backoff_ms,
                    )
                    .await;
            }
            ResultEffect::EndNotDelivered => {
                tracing::info!(
                    output_id = %self.output_id,
                    error = failure.as_deref().unwrap_or("upload failed"),
                    "HLS end playlist not delivered"
                );
            }
            ResultEffect::Rejected { status } => {
                self.engine
                    .record_egress_error_if_current(
                        self.output_id,
                        self.registration,
                        kind,
                        format!(
                            "HLS ingest rejected {} with HTTP {status}",
                            request.file_name
                        ),
                    )
                    .await;
            }
        }
    }
}

fn object_name(request: &UploadRequest) -> &str {
    match request.target {
        UploadTarget::Playlist { .. } => "playlist",
        UploadTarget::Segment { .. } => &request.file_name,
    }
}

fn object_url(playlist_url: &Url, request: &UploadRequest) -> Url {
    match request.target {
        UploadTarget::Playlist { .. } => playlist_url.clone(),
        UploadTarget::Segment { .. } => derive_hls_upload_url(playlist_url, &request.file_name),
    }
}

/// PUT one object; the status, or why none was received.
async fn send_upload(
    client: &Client,
    url: Url,
    request: &UploadRequest,
    timeout: Duration,
) -> Result<reqwest::StatusCode, String> {
    client
        .put(url.clone())
        .timeout(timeout)
        .header(reqwest::header::CONTENT_TYPE, request.content_type)
        .body(request.body.clone())
        .send()
        .await
        .map(|response| response.status())
        .map_err(|err| {
            if err.is_timeout() {
                format!("PUT {url} timed out after {} ms", timeout.as_millis())
            } else {
                err.to_string()
            }
        })
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
#[path = "upload_tests.rs"]
mod tests;
