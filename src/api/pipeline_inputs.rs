use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::domain::pipeline_input::{
    HeaderPair, PipelineInput, PipelineInputSource, PipelineInputSourceKind, PullAuth, PushAuth,
    VariantRule,
};

use super::error::ApiError;
use super::state::{
    AppState, MAX_NAME_LEN, MAX_URL_LEN, check_field_len, refresh_srt_ingest_policy_store,
    require_authenticated,
};

/// Longest bearer token, in bytes.
const MAX_AUTH_TOKEN_LEN: usize = 4096;
/// A signed-cookie triple is a few hundred bytes; this leaves room for a
/// whole session's cookies without letting the row grow without bound.
const MAX_AUTH_COOKIES_LEN: usize = 4096;
/// Longest basic-auth username, in bytes.
const MAX_AUTH_USERNAME_LEN: usize = 256;
/// Longest basic-auth password, in bytes.
const MAX_AUTH_PASSWORD_LEN: usize = 4096;
/// At most this many request headers in a `headers` auth entry.
const MAX_INPUT_HEADERS: usize = 8;
/// Longest request header name, in bytes.
const MAX_HEADER_NAME_LEN: usize = 64;
/// Longest request header value, in bytes.
const MAX_HEADER_VALUE_LEN: usize = 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInputPayload {
    label: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    variant_rule: Option<String>,
    #[serde(default)]
    auth: Option<AuthPayload>,
    #[serde(default)]
    push_auth: Option<AuthPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInputPayload {
    label: Option<String>,
    enabled: Option<bool>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    source_url: Option<String>,
    #[serde(default)]
    variant_rule: Option<String>,
    #[serde(default)]
    auth: Option<AuthPayload>,
    #[serde(default)]
    push_auth: Option<AuthPayload>,
}

/// The `auth` object on create/update: one tagged credential mechanism.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthPayload {
    kind: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    cookies: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    headers: Option<Vec<HeaderPair>>,
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

/// Parse the request `kind` string. Unknown values are rejected rather than
/// silently defaulting to a keyed input.
fn parse_source_kind(raw: &str) -> Result<PipelineInputSourceKind, String> {
    serde_json::from_value::<PipelineInputSourceKind>(serde_json::Value::String(raw.to_string()))
        .map_err(|_| format!("unknown input kind \"{raw}\": expected \"keyed\" or \"hlsPull\""))
}

/// Parse the request `variantRule` string. Unknown values are rejected.
fn parse_variant_rule(raw: &str) -> Result<VariantRule, String> {
    serde_json::from_value::<VariantRule>(serde_json::Value::String(raw.to_string())).map_err(|_| {
        format!(
            "unknown variant rule \"{raw}\": expected \"highestBandwidth\" or \"lowestBandwidth\""
        )
    })
}

/// A credential the puller sends as an HTTP header value. The puller builds a
/// `HeaderValue` from it, so a value that type refuses must be refused here:
/// a hand-rolled control-byte check lets bytes through (a lone `\x01`, a
/// `\x7f`, a non-ASCII byte) that the client then refuses on every attempt,
/// and the operator sees a pull that fails forever with nothing to fix.
fn invalid_header_value(kind: &str, value: &str) -> Option<String> {
    axum::http::HeaderValue::from_str(value)
        .err()
        .map(|error| format!("{kind} is not a valid HTTP header value: {error}"))
}

fn validate_headers(headers: &[HeaderPair]) -> Option<String> {
    if headers.len() > MAX_INPUT_HEADERS {
        return Some(format!(
            "at most {MAX_INPUT_HEADERS} request headers are allowed"
        ));
    }
    for header in headers {
        if header.name.len() > MAX_HEADER_NAME_LEN {
            return Some(format!(
                "header name exceeds maximum length of {MAX_HEADER_NAME_LEN} bytes"
            ));
        }
        if header.value.len() > MAX_HEADER_VALUE_LEN {
            return Some(format!(
                "header value exceeds maximum length of {MAX_HEADER_VALUE_LEN} bytes"
            ));
        }
        // Parse with the same types reqwest uses, so a value accepted here is
        // one the puller can send. A hand-rolled check lets bytes through
        // that the client then refuses on every request, with nothing shown
        // to the operator.
        let name = match axum::http::HeaderName::from_bytes(header.name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                return Some(format!(
                    "header name \"{}\" is not a valid HTTP header name",
                    header.name
                ));
            }
        };
        if let Err(error) = axum::http::HeaderValue::from_str(&header.value) {
            return Some(format!(
                "header value for \"{}\" is not a valid HTTP header value: {error}",
                header.name
            ));
        }
        // These belong to the connection the client opens, not to the input:
        // setting them breaks framing in ways that are hard to diagnose.
        if matches!(
            name,
            axum::http::header::HOST
                | axum::http::header::CONTENT_LENGTH
                | axum::http::header::TRANSFER_ENCODING
                | axum::http::header::CONNECTION
        ) {
            return Some(format!(
                "header \"{}\" belongs to the connection and cannot be set here",
                header.name
            ));
        }
    }
    None
}

fn validate_source_url(url: &str) -> Option<String> {
    if url.len() > MAX_URL_LEN {
        return Some(format!(
            "sourceUrl exceeds maximum length of {MAX_URL_LEN} bytes"
        ));
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return Some("sourceUrl must be an absolute http or https URL".to_string());
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return Some("sourceUrl must be an absolute http or https URL".to_string());
    }
    // A userinfo component is a password in the clear: it would be stored in
    // the row and returned by every read of it. The `basic` credential choice
    // stores the same password write-only instead.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Some(
            "sourceUrl must not carry a userinfo component; use the `basic` \
             authentication choice for origin credentials"
                .to_string(),
        );
    }
    None
}

/// Build and validate the credential an input presents to its origin.
fn build_auth(payload: AuthPayload) -> Result<PullAuth, String> {
    let AuthPayload {
        kind,
        token,
        cookies,
        username,
        password,
        headers,
    } = payload;
    if token.is_some() && kind != "bearer"
        || cookies.is_some() && kind != "cookies"
        || (username.is_some() || password.is_some()) && kind != "basic"
        || headers.is_some() && kind != "headers"
    {
        return Err(format!(
            "auth kind \"{kind}\" carries a payload for a different mechanism"
        ));
    }
    match kind.as_str() {
        "none" => Ok(PullAuth::None),
        "bearer" => {
            let token = token.ok_or_else(|| "a bearer auth requires token".to_string())?;
            if token.is_empty() || token.len() > MAX_AUTH_TOKEN_LEN {
                return Err(format!(
                    "bearer token must be 1 to {MAX_AUTH_TOKEN_LEN} bytes"
                ));
            }
            if let Some(message) = invalid_header_value("bearer token", &token) {
                return Err(message);
            }
            Ok(PullAuth::Bearer { token })
        }
        "cookies" => {
            let cookies = cookies.ok_or_else(|| "a cookies auth requires cookies".to_string())?;
            if cookies.is_empty() || cookies.len() > MAX_AUTH_COOKIES_LEN {
                return Err(format!(
                    "cookie string must be 1 to {MAX_AUTH_COOKIES_LEN} bytes"
                ));
            }
            // One header line, sent as one `Cookie` header: the same type the
            // puller builds decides what a valid value is.
            if let Some(message) = invalid_header_value("cookie string", &cookies) {
                return Err(message);
            }
            Ok(PullAuth::Cookies { cookies })
        }
        "basic" => {
            let username = username.ok_or_else(|| "a basic auth requires username".to_string())?;
            let password = password.ok_or_else(|| "a basic auth requires password".to_string())?;
            if username.is_empty() || username.len() > MAX_AUTH_USERNAME_LEN {
                return Err(format!(
                    "basic username must be 1 to {MAX_AUTH_USERNAME_LEN} bytes"
                ));
            }
            if password.is_empty() || password.len() > MAX_AUTH_PASSWORD_LEN {
                return Err(format!(
                    "basic password must be 1 to {MAX_AUTH_PASSWORD_LEN} bytes"
                ));
            }
            // Basic is base64-encoded into an `Authorization` header, so any
            // UTF-8 survives — including control and non-ASCII bytes, which is
            // why a legitimate non-ASCII password must not be refused here. The
            // one rule is RFC 7617's: the first colon delimits, so a colon in
            // the username would be read as the start of the password.
            if username.contains(':') {
                return Err("basic username must not contain a colon".to_string());
            }
            Ok(PullAuth::Basic { username, password })
        }
        "headers" => {
            let headers = headers.ok_or_else(|| "a headers auth requires headers".to_string())?;
            if let Some(message) = validate_headers(&headers) {
                return Err(message);
            }
            Ok(PullAuth::Headers { headers })
        }
        other => Err(format!(
            "unknown auth kind \"{other}\": expected \"none\", \"bearer\", \"cookies\", \"basic\", or \"headers\""
        )),
    }
}

/// Push passwords use the dashboard's salted scrypt storage, never plaintext.
/// The shared bounded blocking lane keeps expensive KDF work off CONTROL.
async fn build_push_auth(payload: AuthPayload) -> Result<PushAuth, Response> {
    if !matches!(payload.kind.as_str(), "none" | "basic") {
        return Err(bad_request("pushAuth kind must be none or basic"));
    }
    match build_auth(payload).map_err(bad_request)? {
        PullAuth::None => Ok(PushAuth::None),
        PullAuth::Basic { username, password } => {
            let permit = super::hls_ingest::PUSH_AUTH_WORK
                .try_acquire()
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
            let password_hash = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                super::auth::hash_password(&password)
            })
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
            Ok(PushAuth::Basic {
                username,
                password_hash,
            })
        }
        _ => Err(bad_request("pushAuth kind must be none or basic")),
    }
}

/// Assemble and validate the source half of an input mutation.
fn build_source(
    kind: PipelineInputSourceKind,
    url: Option<String>,
    variant_rule: VariantRule,
    auth: PullAuth,
    push_auth: PushAuth,
) -> Result<PipelineInputSource, String> {
    match kind {
        PipelineInputSourceKind::Keyed => {
            if url.is_some() {
                return Err("a keyed input cannot carry sourceUrl".to_string());
            }
            if auth != PullAuth::None {
                return Err("a keyed input cannot carry auth".to_string());
            }
            Ok(PipelineInputSource {
                push_auth,
                ..PipelineInputSource::keyed()
            })
        }
        PipelineInputSourceKind::HlsPull => {
            if push_auth != PushAuth::None {
                return Err("a hlsPull input cannot carry pushAuth".to_string());
            }
            let url = url.ok_or_else(|| "a hlsPull input requires sourceUrl".to_string())?;
            if let Some(message) = validate_source_url(&url) {
                return Err(message);
            }
            Ok(PipelineInputSource {
                kind,
                url: Some(url),
                variant_rule,
                auth,
                push_auth,
            })
        }
    }
}

async fn input_json(state: &AppState, input: &PipelineInput, host: &str) -> serde_json::Value {
    let runtime = state
        .engine
        .ingests
        .sessions
        .read()
        .await
        .get(&input.id)
        .cloned();
    let runtime = runtime
        .as_ref()
        .map(|ingest| {
            let metadata = ingest.metadata();
            serde_json::json!({
                "connected": true,
                "forwardingState": ingest.gate.state().as_str(),
                "protocol": ingest.protocol,
                "uptimeSeconds": ingest.start_time.elapsed().as_secs_f64(),
                "bytesReceived": ingest.bytes_received.load(std::sync::atomic::Ordering::Relaxed),
                "remoteAddr": metadata.remote_addr,
                "video": metadata.video,
                "audio": metadata.audio,
                "quality": metadata.quality,
            })
        })
        .unwrap_or_else(|| {
            serde_json::json!({
                "connected": false,
                "forwardingState": null,
                "protocol": null,
                "uptimeSeconds": null,
                "bytesReceived": 0,
                "remoteAddr": null,
                "video": null,
                "audio": null,
                "quality": null,
            })
        });
    let endpoints = crate::application::runtime_state::IngestEndpoints {
        host: host.to_string(),
        ports: state.ports.server_ports(),
    };
    serde_json::json!({
        "id": input.id,
        "pipelineId": input.pipeline_id,
        "label": input.label,
        "streamKey": input.stream_key,
        "role": input.role.as_str(),
        "enabled": input.enabled,
        "selected": input.selected,
        "kind": input.source_kind,
        "sourceUrl": input.source_url,
        "variantRule": input.variant_rule,
        // Auth is write-only: the variant and (for basic) the username are
        // safe to echo, but no token, password or header value ever is.
        "authKind": input.source_auth.kind_str(),
        "authUsername": input.source_auth.username(),
        "pushAuthKind": input.push_auth.kind_str(),
        "pushAuthUsername": input.push_auth.username(),
        "ingestUrls": endpoints.urls(input),
        "previewUrl": crate::application::runtime_state::inputs::input_preview_url(&input.id),
        "runtime": runtime,
    })
}

fn validate_label(label: &str) -> Option<Response> {
    if let Some(response) = check_field_len("label", label, MAX_NAME_LEN) {
        return Some(response);
    }
    if label.trim().is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Input label cannot be empty"})),
            )
                .into_response(),
        );
    }
    None
}

pub async fn pipeline_inputs_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let inputs = match state.pipeline_input_service.list(&pipeline_id).await {
        Ok(inputs) => inputs,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let host = state.pipeline_service.get_ingest_host().await;
    let selected_input_id = inputs
        .iter()
        .find(|input| input.selected)
        .map(|input| input.id.clone());
    let mut values = Vec::with_capacity(inputs.len());
    for input in &inputs {
        values.push(input_json(&state, input, &host).await);
    }
    Json(serde_json::json!({
        "inputs": values,
        "selectedInputId": selected_input_id,
    }))
    .into_response()
}

pub async fn pipeline_inputs_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<CreateInputPayload>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Some(response) = validate_label(&payload.label) {
        return response;
    }
    let kind = match payload.kind.as_deref() {
        None => PipelineInputSourceKind::Keyed,
        Some(raw) => match parse_source_kind(raw) {
            Ok(kind) => kind,
            Err(message) => return bad_request(message),
        },
    };
    let variant_rule = match payload.variant_rule.as_deref() {
        None => VariantRule::default(),
        Some(raw) => match parse_variant_rule(raw) {
            Ok(rule) => rule,
            Err(message) => return bad_request(message),
        },
    };
    let auth = match payload.auth {
        Some(auth) => match build_auth(auth) {
            Ok(auth) => auth,
            Err(message) => return bad_request(message),
        },
        None => PullAuth::None,
    };
    let push_auth = match payload.push_auth {
        Some(auth) => match build_push_auth(auth).await {
            Ok(auth) => auth,
            Err(response) => return response,
        },
        None => PushAuth::None,
    };
    let source = match build_source(kind, payload.source_url, variant_rule, auth, push_auth) {
        Ok(source) => source,
        Err(message) => return bad_request(message),
    };
    let input = match state
        .pipeline_input_service
        .create(&pipeline_id, payload.label.trim(), source)
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    refresh_srt_ingest_policy_store(&state).await;
    let host = state.pipeline_service.get_ingest_host().await;
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "input": input_json(&state, &input, &host).await
        })),
    )
        .into_response()
}

pub async fn pipeline_input_patch_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, input_id)): Path<(String, String)>,
    Json(payload): Json<UpdateInputPayload>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Some(label) = payload.label.as_deref()
        && let Some(response) = validate_label(label)
    {
        return response;
    }
    let current = match state
        .pipeline_input_service
        .get(&pipeline_id, &input_id)
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let label = payload
        .label
        .as_deref()
        .map(str::trim)
        .unwrap_or(&current.label);
    let kind = match payload.kind.as_deref() {
        None => current.source_kind,
        Some(raw) => match parse_source_kind(raw) {
            Ok(kind) => kind,
            Err(message) => return bad_request(message),
        },
    };
    let variant_rule = match payload.variant_rule.as_deref() {
        None => current.variant_rule,
        Some(raw) => match parse_variant_rule(raw) {
            Ok(rule) => rule,
            Err(message) => return bad_request(message),
        },
    };
    // Auth is write-only. An absent `auth` keeps the stored credential so a
    // PATCH that does not mention auth never clears it; `{"kind":"none"}`
    // clears it and any other variant replaces it wholesale.
    let auth = match payload.auth {
        Some(auth) => match build_auth(auth) {
            Ok(auth) => auth,
            Err(message) => return bad_request(message),
        },
        None => current.source_auth.clone(),
    };
    let url = match kind {
        // A keyed input has no URL: passing one is rejected, not ignored.
        PipelineInputSourceKind::Keyed => payload.source_url,
        PipelineInputSourceKind::HlsPull => {
            payload.source_url.or_else(|| current.source_url.clone())
        }
    };
    let push_auth = match payload.push_auth {
        Some(auth) => match build_push_auth(auth).await {
            Ok(auth) => auth,
            Err(response) => return response,
        },
        None => current.push_auth.clone(),
    };
    let source = match build_source(kind, url, variant_rule, auth, push_auth) {
        Ok(source) => source,
        Err(message) => return bad_request(message),
    };
    let input = match state
        .pipeline_input_service
        .update(
            &pipeline_id,
            &input_id,
            label,
            payload.enabled.unwrap_or(current.enabled),
            source,
        )
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if !input.enabled {
        state.engine.cancel_pipeline_input(&input.id).await;
    }
    // A changed source kind hands the input's session to a different
    // publisher: a keyed input's session belongs to a live push, and a pull
    // input's to its pull task. Left alone, the new kind is refused as
    // `PublisherActive` and backs off until the old one leaves on its own —
    // which, for a pusher that never stops, is never.
    if kind != current.source_kind {
        state.engine.cancel_pipeline_input(&input.id).await;
    }
    refresh_srt_ingest_policy_store(&state).await;
    let host = state.pipeline_service.get_ingest_host().await;
    Json(serde_json::json!({
        "input": input_json(&state, &input, &host).await
    }))
    .into_response()
}

pub async fn pipeline_input_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, input_id)): Path<(String, String)>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state
        .pipeline_input_service
        .delete(&pipeline_id, &input_id)
        .await
    {
        Ok(true) => {
            state.engine.cancel_pipeline_input(&input_id).await;
            refresh_srt_ingest_policy_store(&state).await;
            Json(serde_json::json!({"deleted": true})).into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

pub async fn pipeline_input_promote_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, input_id)): Path<(String, String)>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let input = match state
        .pipeline_input_service
        .promote(&pipeline_id, &input_id)
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    // The reconciler switches the forwarded input (Q-039); report whether the
    // target has a live session to switch to.
    state.reconcile.input_selection_changed(&pipeline_id);
    let connected = state
        .engine
        .input_has_session(&pipeline_id, &input_id)
        .await;
    let host = state.pipeline_service.get_ingest_host().await;
    Json(serde_json::json!({
        "input": input_json(&state, &input, &host).await,
        "connected": connected,
    }))
    .into_response()
}
