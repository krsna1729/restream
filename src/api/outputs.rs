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
use crate::application::services::ApiError;

use crate::domain::output_spec::{OutputConfig, OutputProtocolConfig, OutputUrlScheme};

use crate::domain::state::DesiredOutputState;

use super::state::{
    AppState, MAX_NAME_LEN, MAX_OUTPUT_CONFIG_LEN, MAX_URL_LEN, check_field_len,
    require_authenticated, to_hex,
};

#[derive(Deserialize)]
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

pub const OUTPUT_URL_SCHEME_ERROR: &str = "Invalid URL scheme. Supported schemes are rtmp://, rtmps://, srt://, hls://, http://, and https://";
pub const MONITORING_URL_SCHEME_ERROR: &str =
    "Invalid monitoring URL scheme. Supported schemes are http://, https://, and srt://";
pub const OUTPUT_URL_PARSE_ERROR: &str = "Output URL must be a valid absolute URL with a host";
pub const MONITORING_URL_PARSE_ERROR: &str =
    "Monitoring URL must be a valid absolute URL with a host";
pub const CUSTOM_OUTPUT_ENCODING_ERROR: &str =
    "Custom output encoding is not available yet; choose source or a preset encoding";
pub const OUTPUT_PROTOCOL_CONFIG_ERROR: &str =
    "RTMP protocol settings require an RTMP or RTMPS output URL";
const YOUTUBE_MONITORING_TIMEOUT: Duration = Duration::from_secs(5);
const YOUTUBE_MONITORING_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const YOUTUBE_MONITORING_MAX_BYTES: usize = 512 * 1024;

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

/// Accepts the common YouTube watch/share/live URL shapes and converts them to
/// one canonical watch URL for monitoring fetches.
pub fn normalize_youtube_watch_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();
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
    } else if host.ends_with("youtube.com") {
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
    } else {
        None
    }?;
    Some(format!("https://www.youtube.com/watch?v={video_id}"))
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
        .build()
        .map_err(|_| YoutubeMonitoringFetchError::BuildClient)
}

async fn fetch_limited_text(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<String, YoutubeMonitoringFetchError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| YoutubeMonitoringFetchError::Request)?
        .error_for_status()
        .map_err(|_| YoutubeMonitoringFetchError::Status)?;
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

// Validate and canonicalize output mutations at the HTTP boundary so services
// only ever see normalized absolute URLs and supported config choices.
fn validate_output_payload(
    payload: &OutputPayload,
) -> Result<ValidatedOutputPayload, ValidationResponse> {
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

    // Normalize once at the API boundary so downstream services only receive
    // absolute URLs in a canonical host/scheme form.
    let Some(url) = normalize_output_url(&payload.url) else {
        return Err(Box::new(bad_request(OUTPUT_URL_PARSE_ERROR)));
    };
    if matches!(output_config.protocol, OutputProtocolConfig::Rtmp { .. })
        && !OutputUrlScheme::from_url(&url).is_rtmp_family()
    {
        return Err(Box::new(bad_request(OUTPUT_PROTOCOL_CONFIG_ERROR)));
    }
    let monitoring_url = normalize_monitoring_url(payload.monitoring_url.as_deref())
        .map_err(|error| Box::new(bad_request(error)))?;

    Ok(ValidatedOutputPayload {
        output_config,
        url,
        monitoring_url,
    })
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
    let canonical_watch_url = match normalize_youtube_watch_url(query.url.trim()) {
        Some(url) => url,
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
    let html = match fetch_limited_text(&client, &canonical_watch_url, YOUTUBE_MONITORING_MAX_BYTES)
        .await
    {
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

    let validated = match validate_output_payload(&payload) {
        Ok(validated) => validated,
        Err(response) => return Ok(*response),
    };

    let id = format!("output_{}", to_hex(&rand::random::<[u8; 8]>()));

    let output = state
        .output_service
        .create_output(
            &id,
            &pipeline_id,
            &payload.name,
            &validated.url,
            validated.monitoring_url.as_deref(),
            DesiredOutputState::Stopped.as_str(),
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

    let validated = match validate_output_payload(&payload) {
        Ok(validated) => validated,
        Err(response) => return Ok(*response),
    };
    let existing = state
        .output_service
        .get_by_id(&pipeline_id, &output_id)
        .await?;
    if existing.desired_state == DesiredOutputState::Running
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

    let updated = state
        .output_service
        .update_output(
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

/// Deletes one output and clears any runtime egress registration that may still
/// be associated with its id.
pub async fn outputs_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, output_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let deleted = state
        .output_service
        .delete_output(&pipeline_id, &output_id)
        .await?;
    if !deleted {
        return Ok((StatusCode::NOT_FOUND, "Output not found").into_response());
    }
    state.engine.unregister_egress(&output_id).await;
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

    let output = state
        .output_service
        .request_start(&pipeline_id, &output_id)
        .await?;
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

    let output = state
        .output_service
        .request_stop(&pipeline_id, &output_id)
        .await?;
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
    use crate::domain::audio_routing::AudioRouting;
    use crate::domain::output_spec::{OutputVideoConfig, RtmpOutputMode};
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
        let validated = validate_output_payload(&test_output_payload("RTMP://EXAMPLE.COM/live"))
            .expect("valid outputs should normalize");

        assert_eq!(validated.url, "rtmp://example.com/live");
    }

    #[test]
    fn validate_output_payload_rejects_custom_output_configs() {
        let payload = OutputPayload {
            config: OutputConfig {
                video: OutputVideoConfig::Custom,
                audio: AudioRouting::Passthrough,
                ..OutputConfig::default()
            },
            ..test_output_payload("rtmp://example.com/live")
        };

        let response =
            validate_output_payload(&payload).expect_err("custom outputs should be rejected");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
    fn output_state_response_uses_ok_status() {
        let output = crate::application::models::Output {
            id: "output-1".to_string(),
            pipeline_id: "pipe-1".to_string(),
            name: "Primary Output".to_string(),
            url: "rtmp://example.com/live/stream".to_string(),
            desired_state: DesiredOutputState::Stopped,
            config: OutputConfig::source(),
            monitoring_url: None,
        };

        let response = output_state_response("Output started", "running", &output);

        assert_eq!(response.status(), StatusCode::OK);
    }
}
