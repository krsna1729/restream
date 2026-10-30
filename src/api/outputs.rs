//! Output HTTP handlers own request validation and response shaping for egress
//! configuration. The application services remain responsible for persistence
//! and state transitions once a request has been normalized at this boundary.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

use crate::api_view_models;

use restream_domain::domain::output_spec::{
    OutputConfig, OutputProtocolConfig, OutputUrlScheme, ProtocolCapabilities, RecirculationTarget,
};

use restream_domain::domain::state::DesiredOutputState;

use super::error::ApiError;
use super::state::{
    AppState, MAX_NAME_LEN, MAX_OUTPUT_CONFIG_LEN, MAX_URL_LEN, check_field_len,
    require_authenticated, to_hex,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct OutputPayload {
    pub name: String,
    pub url: String,
    pub config: OutputConfig,
    pub monitoring_url: Option<String>,
}

impl OutputPayload {
    pub fn stage_encoding_label(&self) -> String {
        self.config.stage_encoding_label()
    }
}

#[derive(Debug, Clone)]
struct ValidatedOutputPayload {
    output_config: OutputConfig,
    url: String,
    monitoring_url: Option<String>,
}

pub fn is_supported_output_url(url: &str) -> bool {
    OutputUrlScheme::from_url(url).is_supported_output()
}

pub const OUTPUT_URL_SCHEME_ERROR: &str = "Invalid URL scheme. Supported schemes are rtmp://, rtmps://, srt://, hls://, sink://, pipeline://, http://, https://, and moqt://";
pub const MONITORING_URL_SCHEME_ERROR: &str =
    "Invalid monitoring URL scheme. Supported schemes are http://, https://, and srt://";
pub const OUTPUT_URL_PARSE_ERROR: &str = "Output URL must be a valid absolute URL with a host";
pub const RTMP_OUTPUT_URL_ERROR: &str =
    "RTMP output URL must name an app and a stream key: rtmp://host[:port]/app/stream-key";
pub const MONITORING_URL_PARSE_ERROR: &str =
    "Monitoring URL must be a valid absolute URL with a host";
pub const CUSTOM_OUTPUT_ENCODING_ERROR: &str =
    "Custom output encoding is not available yet; choose source or a preset encoding";
pub const OUTPUT_PROTOCOL_CONFIG_ERROR: &str =
    "RTMP protocol settings require an RTMP or RTMPS output URL";
pub const PIPELINE_OUTPUT_CONFIG_ERROR: &str =
    "Pipeline recirculation outputs must use source video and passthrough audio";
const YOUTUBE_MONITORING_TIMEOUT: Duration = Duration::from_secs(5);
const YOUTUBE_MONITORING_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const YOUTUBE_MONITORING_MAX_BYTES: usize = 512 * 1024;
const YOUTUBE_WATCH_URL: &str = "https://www.youtube.com/watch";

fn normalize_supported_url(
    url: &str,
    supports: impl Fn(OutputUrlScheme) -> bool,
) -> Option<String> {
    let mut parsed = Url::parse(url.trim()).ok()?;
    let scheme = OutputUrlScheme::from_url(parsed.as_str());
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !supports(scheme) {
        return None;
    }
    parsed.set_host(Some(&host)).ok()?;
    Some(parsed.to_string())
}

/// Canonicalizes one output URL into the host-normalized form persisted by the
/// API and application layers.
pub fn normalize_output_url(url: &str) -> Option<String> {
    normalize_supported_url(url, OutputUrlScheme::is_supported_output)
}

/// Canonicalizes an optional monitoring URL, treating blank input as absent so
/// callers do not need to special-case empty form fields.
pub fn normalize_monitoring_url(url: Option<&str>) -> Result<Option<String>, &'static str> {
    let trimmed = url.unwrap_or_default().trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        normalize_supported_url(trimmed, OutputUrlScheme::supports_monitoring)
            .map(Some)
            .ok_or(MONITORING_URL_PARSE_ERROR)
    }
}

pub fn is_supported_monitoring_url(url: &str) -> bool {
    OutputUrlScheme::from_url(url).supports_monitoring()
}

#[derive(Deserialize)]
pub struct YoutubeMonitoringStatusQuery {
    pub url: String,
}

#[derive(Serialize)]
pub struct YoutubeMonitoringStatusResponse {
    pub canonical_watch_url: String,
    pub live_now: bool,
    pub live_content: bool,
    pub upcoming: bool,
    pub title: Option<String>,
}

/// Accepts common YouTube watch/share/live URL shapes and returns a fixed
/// YouTube request target plus the canonical watch URL for API responses.
fn youtube_watch_target(url: &str) -> Option<(String, String)> {
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    if host != "youtu.be" && host != "youtube.com" && !host.ends_with(".youtube.com") {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    let path_parts = parsed
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    let video_id = if host == "youtu.be" {
        path_parts
            .first()
            .copied()
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    } else {
        parsed
            .query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                if matches!(
                    path_parts.first().copied(),
                    Some("live" | "embed" | "shorts")
                ) {
                    path_parts.get(1).map(|value| (*value).to_string())
                } else {
                    None
                }
            })
    }?;
    let mut canonical = Url::parse(YOUTUBE_WATCH_URL).ok()?;
    canonical.query_pairs_mut().append_pair("v", &video_id);
    Some((video_id, canonical.into()))
}

/// Canonicalizes a supported YouTube watch, share, live, embed, or Shorts URL.
pub fn normalize_youtube_watch_url(url: &str) -> Option<String> {
    youtube_watch_target(url).map(|(_, canonical)| canonical)
}

/// Small HTML substring probe used for the YouTube monitoring response shape.
pub fn youtube_watch_page_contains_flag(html: &str, flag: &str) -> bool {
    html.contains(flag)
}

/// Extracts the HTML `<title>` content and applies only the minimal entity
/// decoding needed by the monitoring UI.
pub fn extract_html_title(html: &str) -> Option<String> {
    let start = html.find("<title>")?;
    let rest = &html[start + "<title>".len()..];
    let end = rest.find("</title>")?;
    let title = rest[..end].trim();
    (!title.is_empty()).then(|| {
        title
            .replace("&amp;", "&")
            .replace("&#39;", "'")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
    })
}

/// Shapes one YouTube monitoring response from the fetched watch-page HTML.
pub fn parse_youtube_monitoring_status(
    canonical_watch_url: String,
    html: &str,
) -> YoutubeMonitoringStatusResponse {
    YoutubeMonitoringStatusResponse {
        canonical_watch_url,
        live_now: youtube_watch_page_contains_flag(html, "\"isLiveNow\":true"),
        live_content: youtube_watch_page_contains_flag(html, "\"isLiveContent\":true"),
        upcoming: youtube_watch_page_contains_flag(html, "\"isUpcoming\":true"),
        title: extract_html_title(html).map(|title| title.replace(" - YouTube", "")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YoutubeMonitoringFetchError {
    BuildClient,
    Request,
    Status,
    Body,
    TooLarge,
}

// Monitoring fetches use short timeouts and a bounded response size because
// they are advisory dashboard checks, not stream-critical control paths.
fn youtube_monitoring_client() -> Result<reqwest::Client, YoutubeMonitoringFetchError> {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36")
        .connect_timeout(YOUTUBE_MONITORING_CONNECT_TIMEOUT)
        .timeout(YOUTUBE_MONITORING_TIMEOUT)
        .read_timeout(YOUTUBE_MONITORING_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if url.scheme() == "https"
                && url.host_str().is_some_and(|host| {
                    host == "youtube.com" || host.ends_with(".youtube.com")
                })
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|_| YoutubeMonitoringFetchError::BuildClient)
}

async fn fetch_youtube_monitoring_text(
    client: &reqwest::Client,
    video_id: &str,
) -> Result<String, YoutubeMonitoringFetchError> {
    let response = client
        .get(YOUTUBE_WATCH_URL)
        .query(&[("v", video_id)])
        .send()
        .await
        .map_err(|_| YoutubeMonitoringFetchError::Request)?;
    read_limited_response(response, YOUTUBE_MONITORING_MAX_BYTES).await
}

#[cfg(test)]
async fn fetch_limited_text(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<String, YoutubeMonitoringFetchError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| YoutubeMonitoringFetchError::Request)?;
    read_limited_response(response, max_bytes).await
}

async fn read_limited_response(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<String, YoutubeMonitoringFetchError> {
    let response = response
        .error_for_status()
        .map_err(|_| YoutubeMonitoringFetchError::Status)?;
    if !response.status().is_success() {
        return Err(YoutubeMonitoringFetchError::Status);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| YoutubeMonitoringFetchError::Body)?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(YoutubeMonitoringFetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| YoutubeMonitoringFetchError::Body)
}

// Transport-specific fetch failures stay mapped at the API boundary so the
// dashboard receives stable status codes and user-facing error strings.
fn youtube_fetch_error_response(error: YoutubeMonitoringFetchError) -> axum::response::Response {
    match error {
        YoutubeMonitoringFetchError::BuildClient => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        YoutubeMonitoringFetchError::Status => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": "YouTube metadata request returned an unsuccessful status"})),
        )
            .into_response(),
        YoutubeMonitoringFetchError::TooLarge => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": "YouTube metadata response was too large"})),
        )
            .into_response(),
        YoutubeMonitoringFetchError::Request | YoutubeMonitoringFetchError::Body => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": "Failed to fetch YouTube metadata"})),
        )
            .into_response(),
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

fn output_response_body(
    message: &'static str,
    output: &crate::application::models::Output,
) -> serde_json::Value {
    serde_json::json!({
        "message": message,
        "output": api_view_models::output_response_json(output)
    })
}

fn output_state_response(
    message: &'static str,
    desired_state: &'static str,
    output: &crate::application::models::Output,
) -> Response {
    Json(serde_json::json!({
        "message": message,
        "desiredState": desired_state,
        "output": api_view_models::output_response_json(output)
    }))
    .into_response()
}

type ValidationResponse = Box<Response>;

fn validate_output_payload_fields(
    payload: &OutputPayload,
) -> Result<OutputConfig, ValidationResponse> {
    if let Some(response) = check_field_len("name", &payload.name, MAX_NAME_LEN) {
        return Err(Box::new(response));
    }
    if let Some(response) = check_field_len("url", &payload.url, MAX_URL_LEN) {
        return Err(Box::new(response));
    }

    let output_config = payload.config.clone();
    let output_config_json = match serde_json::to_string(&output_config) {
        Ok(value) => value,
        Err(err) => {
            return Err(Box::new(bad_request(format!(
                "Invalid output config: {err}"
            ))));
        }
    };
    if let Some(response) = check_field_len("config", &output_config_json, MAX_OUTPUT_CONFIG_LEN) {
        return Err(Box::new(response));
    }
    if let Some(monitoring_url) = payload.monitoring_url.as_deref()
        && let Some(response) = check_field_len("monitoring_url", monitoring_url, MAX_URL_LEN)
    {
        return Err(Box::new(response));
    }
    if output_config.is_custom_output() {
        return Err(Box::new(bad_request(CUSTOM_OUTPUT_ENCODING_ERROR)));
    }

    Ok(output_config)
}

/// What the egress leaf for `url`'s scheme can actually use, checked for
/// every output mutation (HTTP and agent execution) so a URL is refused
/// here rather than failing on every connect attempt. The message is the
/// 400 body.
pub(super) fn validate_output_url_for_config(
    url: &str,
    output_config: &OutputConfig,
) -> Result<(), String> {
    let scheme = OutputUrlScheme::from_url(url);
    // The leaf connects only to `app/stream-key`.
    if scheme.is_rtmp_family() && restream_rtmp::parse_rtmp_url(url).is_none() {
        return Err(RTMP_OUTPUT_URL_ERROR.to_string());
    }
    if scheme == OutputUrlScheme::Moqt {
        restream_moq::url::parse_moq_url(url).map_err(|error| error.message().to_string())?;
    }
    if scheme == OutputUrlScheme::Srt {
        restream_srt::egress_connect::validate_srt_egress_url(url)?;
    }
    if matches!(output_config.protocol, OutputProtocolConfig::Rtmp { .. })
        && !scheme.is_rtmp_family()
    {
        return Err(OUTPUT_PROTOCOL_CONFIG_ERROR.to_string());
    }
    output_config
        .validate_capabilities(ProtocolCapabilities::from_output(url, output_config))
        .map_err(|error| error.message().to_string())
}

fn validate_output_transport(
    payload: &OutputPayload,
    output_config: OutputConfig,
) -> Result<ValidatedOutputPayload, ValidationResponse> {
    // Normalize once at the API boundary so downstream services only receive
    // absolute URLs in a canonical host/scheme form.
    let Some(url) = normalize_output_url(&payload.url) else {
        return Err(Box::new(bad_request(OUTPUT_URL_PARSE_ERROR)));
    };
    validate_output_url_for_config(&url, &output_config)
        .map_err(|error| Box::new(bad_request(error)))?;
    let monitoring_url = normalize_monitoring_url(payload.monitoring_url.as_deref())
        .map_err(|error| Box::new(bad_request(error)))?;

    Ok(ValidatedOutputPayload {
        output_config,
        url,
        monitoring_url,
    })
}

// Validate and canonicalize output mutations at the HTTP boundary so services
// only ever see normalized absolute URLs and supported config choices.
fn validate_output_payload(
    payload: &OutputPayload,
) -> Result<ValidatedOutputPayload, ValidationResponse> {
    let output_config = validate_output_payload_fields(payload)?;
    validate_output_transport(payload, output_config)
}

async fn validate_output_payload_for_pipeline(
    state: &AppState,
    source_pipeline_id: &str,
    payload: &OutputPayload,
) -> Result<ValidatedOutputPayload, ValidationResponse> {
    if matches!(
        OutputUrlScheme::from_url(&payload.url),
        OutputUrlScheme::Pipeline
    ) {
        let output_config = validate_output_payload_fields(payload)?;
        validate_pipeline_recirculation_config(&output_config)?;
        validate_reserved_recirculation_candidate(state, source_pipeline_id, &payload.url).await?;
        return validate_output_transport(payload, output_config);
    }

    validate_output_payload(payload)
}

fn validate_pipeline_recirculation_config(
    output_config: &OutputConfig,
) -> Result<(), ValidationResponse> {
    if output_config.is_source_passthrough() {
        Ok(())
    } else {
        Err(Box::new(bad_request(PIPELINE_OUTPUT_CONFIG_ERROR)))
    }
}

async fn validate_reserved_recirculation_candidate(
    state: &AppState,
    source_pipeline_id: &str,
    url: &str,
) -> Result<(), ValidationResponse> {
    let target =
        RecirculationTarget::parse(url).map_err(|error| Box::new(bad_request(error.message())))?;
    state
        .recirculation_service
        .validate_output_candidate(source_pipeline_id, &target)
        .await
        .map_err(|error| Box::new(ApiError::from(error).into_response()))
}

/// Fetches and summarizes lightweight YouTube watch-page metadata for output
/// monitoring setup flows.
pub async fn youtube_monitoring_status_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<YoutubeMonitoringStatusQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Some(response) = check_field_len("url", &query.url, MAX_URL_LEN) {
        return response;
    }
    let (video_id, canonical_watch_url) = match youtube_watch_target(query.url.trim()) {
        Some(target) => target,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Expected a YouTube monitoring URL"})),
            )
                .into_response();
        }
    };

    let client = match youtube_monitoring_client() {
        Ok(client) => client,
        Err(error) => return youtube_fetch_error_response(error),
    };
    let html = match fetch_youtube_monitoring_text(&client, &video_id).await {
        Ok(html) => html,
        Err(error) => return youtube_fetch_error_response(error),
    };
    Json(parse_youtube_monitoring_status(canonical_watch_url, &html)).into_response()
}

/// Creates one stopped output after validating and normalizing its transport
/// URLs at the API boundary.
pub async fn outputs_create_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<OutputPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let validated = match validate_output_payload_for_pipeline(&state, &pipeline_id, &payload).await
    {
        Ok(validated) => validated,
        Err(response) => return Ok(*response),
    };

    let id = format!("output_{}", to_hex(&rand::random::<[u8; 8]>()));

    let output = crate::application::outputs::create_output(
        &state.db,
        &id,
        &pipeline_id,
        &payload.name,
        &validated.url,
        validated.monitoring_url.as_deref(),
        DesiredOutputState::Stopped,
        &validated.output_config,
    )
    .await?;

    // Creation is the one output mutation that returns a 201 transport status;
    // the steady-state mutations below keep the same JSON envelope with 200s.
    Ok((
        StatusCode::CREATED,
        Json(output_response_body("Output created", &output)),
    )
        .into_response())
}

pub async fn outputs_update_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
    Json(payload): Json<OutputPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let validated = match validate_output_payload_for_pipeline(&state, &pipeline_id, &payload).await
    {
        Ok(validated) => validated,
        Err(response) => return Ok(*response),
    };
    let existing =
        crate::application::outputs::get_by_id(&state.db, &pipeline_id, &output_id).await?;
    // An output whose retries are exhausted is not running: its URL can be
    // fixed before the operator starts it again.
    if existing.desired_state == DesiredOutputState::Running
        && !existing.retries_exhausted
        && (existing.url != validated.url || existing.config != validated.output_config)
    {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "Cannot change output transport URL or config while the output is running"
            })),
        )
            .into_response());
    }

    let updated = crate::application::outputs::update_output(
        &state.db,
        &pipeline_id,
        &output_id,
        &payload.name,
        &validated.url,
        validated.monitoring_url.as_deref(),
        &validated.output_config,
    )
    .await?;

    Ok(Json(output_response_body("Output updated", &updated)).into_response())
}

/// Deletes one output; the reconciler then stops its egress.
pub async fn outputs_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let deleted =
        crate::application::outputs::delete_output(&state.db, &pipeline_id, &output_id).await?;
    if !deleted {
        return Ok((StatusCode::NOT_FOUND, "Output not found").into_response());
    }
    state.reconcile.wake();
    Ok(Json(serde_json::json!({"message": "Output deleted"})).into_response())
}

/// Requests the running desired state for one output and returns the standard
/// output-state response envelope.
pub async fn outputs_start_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let output =
        crate::application::outputs::request_start(&state.db, &pipeline_id, &output_id).await?;
    Ok(output_state_response("Output started", "running", &output))
}

/// Requests the stopped desired state for one output and returns the standard
/// output-state response envelope.
pub async fn outputs_stop_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let output =
        crate::application::outputs::request_stop(&state.db, &pipeline_id, &output_id).await?;
    Ok(output_state_response("Output stopped", "stopped", &output))
}

/// Reads the runtime status view for one output without exposing broader
/// pipeline or engine internals.
pub async fn output_status_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((_pipeline_id, output_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    match crate::api_runtime_views::output_status(&state.engine, &output_id).await {
        Some(status) => Json(status).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "output not active"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_domain::domain::audio_routing::AudioRouting;
    use restream_domain::domain::output_spec::{
        OutputVideoCodec, OutputVideoConfig, RtmpOutputMode,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_output_payload(url: &str) -> OutputPayload {
        OutputPayload {
            name: "Primary Output".to_string(),
            url: url.to_string(),
            config: OutputConfig::source(),
            monitoring_url: None,
        }
    }

    #[test]
    fn youtube_watch_target_restricts_hosts_and_encodes_video_ids() {
        for url in [
            "ftp://youtube.com/watch?v=abcdefghijk",
            "https://notyoutube.com/watch?v=abcdefghijk",
            "https://youtube.com.attacker.example/watch?v=abcdefghijk",
            "https://user:password@youtube.com/watch?v=abcdefghijk",
        ] {
            assert!(
                normalize_youtube_watch_url(url).is_none(),
                "invalid YouTube URL was accepted"
            );
        }

        let youtube_url = "https://www.youtube.com/watch?v=abc%26host%3D127.0.0.1";
        let (video_id, canonical) = youtube_watch_target(youtube_url).unwrap();
        assert_eq!(video_id, "abc&host=127.0.0.1");
        assert_eq!(
            canonical,
            "https://www.youtube.com/watch?v=abc%26host%3D127.0.0.1"
        );
    }

    #[tokio::test]
    async fn youtube_fetch_does_not_follow_redirects_to_other_hosts() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let target_hit = Arc::new(AtomicBool::new(false));
        let target_hit_task = target_hit.clone();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = target.accept().await {
                target_hit_task.store(true, Ordering::Relaxed);
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });

        let redirect = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_addr = redirect.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = redirect.accept().await {
                let mut request = [0u8; 1024];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let client = youtube_monitoring_client().unwrap();
        let error = fetch_limited_text(
            &client,
            &format!("http://{redirect_addr}/watch"),
            YOUTUBE_MONITORING_MAX_BYTES,
        )
        .await
        .unwrap_err();

        assert_eq!(error, YoutubeMonitoringFetchError::Status);
        assert!(!target_hit.load(Ordering::Relaxed));
    }

    async fn serve_once(status: &str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status = status.to_string();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });
        format!("http://{addr}/watch")
    }

    #[tokio::test]
    async fn youtube_fetch_rejects_unsuccessful_status() {
        let url = serve_once("503 Service Unavailable", b"retry later".to_vec()).await;
        let client = youtube_monitoring_client().unwrap();

        let error = fetch_limited_text(&client, &url, YOUTUBE_MONITORING_MAX_BYTES)
            .await
            .unwrap_err();

        assert_eq!(error, YoutubeMonitoringFetchError::Status);
    }

    #[tokio::test]
    async fn youtube_fetch_rejects_oversized_body() {
        let url = serve_once("200 OK", vec![b'a'; YOUTUBE_MONITORING_MAX_BYTES + 1]).await;
        let client = youtube_monitoring_client().unwrap();

        let error = fetch_limited_text(&client, &url, YOUTUBE_MONITORING_MAX_BYTES)
            .await
            .unwrap_err();

        assert_eq!(error, YoutubeMonitoringFetchError::TooLarge);
    }

    #[test]
    fn validate_output_payload_normalizes_urls() {
        let validated =
            validate_output_payload(&test_output_payload("RTMP://EXAMPLE.COM/live/key"))
                .expect("valid outputs should normalize");

        assert_eq!(validated.url, "rtmp://example.com/live/key");
    }

    #[test]
    fn validate_output_payload_rejects_rtmp_url_without_stream_key() {
        for url in ["rtmp://example.com/live", "rtmps://example.com/live/"] {
            let response = validate_output_payload(&test_output_payload(url))
                .expect_err("the leaf cannot connect without a stream key");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{url}");
        }
        validate_output_payload(&test_output_payload(
            "rtmp://b.rtmp.youtube.com/live2?backup=1/stream-key",
        ))
        .expect("YouTube backup ingest carries its key after the query");
    }

    #[test]
    fn validate_output_payload_rejects_custom_output_configs() {
        let payload = OutputPayload {
            config: OutputConfig {
                video: OutputVideoConfig::Custom,
                audio: AudioRouting::Passthrough,
                ..OutputConfig::default()
            },
            ..test_output_payload("rtmp://example.com/live/key")
        };

        let response =
            validate_output_payload(&payload).expect_err("custom outputs should be rejected");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn output_payload_rejects_legacy_encoding_field() {
        let value = serde_json::json!({
            "name": "Primary Output",
            "url": "rtmp://example.com/live/key",
            "encoding": "720p",
            "config": {
                "video": {"mode": "source"},
                "audio": {"mode": "all"}
            }
        });

        let parsed = serde_json::from_value::<OutputPayload>(value);

        assert!(parsed.is_err());
    }

    #[test]
    fn validate_output_payload_rejects_h265_for_legacy_rtmp() {
        let payload = OutputPayload {
            config: OutputConfig::preset("720p").with_video_codec(OutputVideoCodec::Hevc),
            ..test_output_payload("rtmp://example.com/live/key")
        };

        let response =
            validate_output_payload(&payload).expect_err("legacy RTMP cannot carry H.265");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_output_payload_allows_h265_for_enhanced_rtmp() {
        let payload = OutputPayload {
            config: OutputConfig::preset("720p")
                .with_video_codec(OutputVideoCodec::Hevc)
                .with_rtmp_mode(RtmpOutputMode::Enhanced),
            ..test_output_payload("rtmp://example.com/live/key")
        };

        let validated =
            validate_output_payload(&payload).expect("enhanced RTMP should allow H.265");

        assert_eq!(validated.output_config, payload.config);
    }

    #[test]
    fn validate_output_payload_rejects_rtmp_protocol_settings_for_srt_url() {
        let payload = OutputPayload {
            config: OutputConfig::source().with_rtmp_mode(RtmpOutputMode::Enhanced),
            ..test_output_payload("srt://example.com:9000")
        };

        let response = validate_output_payload(&payload)
            .expect_err("RTMP settings should be rejected for SRT outputs");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_output_payload_rejects_srt_latency_the_egress_cannot_honor() {
        validate_output_payload(&test_output_payload(
            "srt://example.com:9000?streamid=k&latency=200",
        ))
        .expect("200 ms is an SRT latency the egress applies");
        for url in [
            "srt://example.com:9000?latency=200000",
            "srt://example.com:9000?latency=fast",
        ] {
            let response = validate_output_payload(&test_output_payload(url))
                .expect_err("the SRT leaf cannot apply this latency");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{url}");
        }
    }

    #[test]
    fn validate_output_payload_accepts_pipeline_recirculation_urls() {
        let validated = validate_output_payload(&test_output_payload("pipeline://pipe-b/input-1"))
            .expect("pipeline recirculation is a supported output URL");

        assert_eq!(validated.url, "pipeline://pipe-b/input-1");
    }

    #[test]
    fn validate_output_payload_accepts_moqt_relay_urls_with_hevc() {
        let payload = OutputPayload {
            config: OutputConfig::source().with_video_codec(OutputVideoCodec::Hevc),
            ..test_output_payload("MOQT://Relay.Example:8893/live?ca=trust%2Fca.pem")
        };
        let validated = validate_output_payload(&payload).expect("moqt is an output scheme");

        assert_eq!(
            validated.url,
            "moqt://relay.example:8893/live?ca=trust%2Fca.pem"
        );
    }

    #[test]
    fn validate_output_payload_rejects_moqt_urls_the_leaf_cannot_use() {
        for url in [
            "moqt://relay.example/live?broadcast=",
            "moqt://relay.example/live?insecure=maybe",
            "moqt://user:secret@relay.example/live",
        ] {
            let response = validate_output_payload(&test_output_payload(url))
                .expect_err("the MoQ leaf cannot publish to this URL");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{url}");
        }
        let payload = OutputPayload {
            config: OutputConfig::source().with_rtmp_mode(RtmpOutputMode::Enhanced),
            ..test_output_payload("moqt://relay.example/live")
        };
        assert_eq!(
            validate_output_payload(&payload)
                .expect_err("RTMP settings do not apply to MoQ")
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn output_state_response_uses_ok_status() {
        let output = crate::application::models::Output {
            id: "output-1".to_string(),
            pipeline_id: "pipe-1".to_string(),
            name: "Primary Output".to_string(),
            url: "rtmp://example.com/live/stream".to_string(),
            desired_state: DesiredOutputState::Stopped,
            retries_exhausted: false,
            config: OutputConfig::source(),
            monitoring_url: None,
        };

        let response = output_state_response("Output started", "running", &output);

        assert_eq!(response.status(), StatusCode::OK);
    }
}
