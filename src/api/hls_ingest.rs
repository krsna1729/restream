//! HTTP surface of HLS push ingest: `PUT`/`POST /ingest/hls/{key}/{*path}`.
//!
//! The stream key and optional per-input Basic credential are resolved before
//! any body byte is read, including DELETE requests. The handler only reads
//! the bounded body and hands the object to the key's session; it never demuxes.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt;

use crate::application::ingest::PipelineInputLookup;
use restream_domain::domain::pipeline_input::PushAuth;

/// Bound queued/running password KDFs as well as their memory consumption.
/// A permit lives in the blocking closure even if its HTTP request is dropped.
pub(super) static PUSH_AUTH_WORK: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const MAX_BASIC_BYTES: usize = 256 + 1 + 4096;
const MAX_BASIC_HEADER: usize = 6 + MAX_BASIC_BYTES.div_ceil(3) * 4;

#[derive(Clone)]
pub(super) struct HlsIngestState {
    pub ingest: Arc<HlsIngest>,
    pub inputs: Arc<dyn PipelineInputLookup>,
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"HLS push\", charset=\"UTF-8\""),
        )],
        "HLS push credentials required",
    )
        .into_response()
}

async fn require_push_auth(auth: PushAuth, headers: &HeaderMap) -> Result<(), Response> {
    let PushAuth::Basic {
        username,
        password_hash,
    } = auth
    else {
        return Ok(());
    };
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Err(unauthorized());
    };
    if values.next().is_some() || value.as_bytes().len() > MAX_BASIC_HEADER {
        return Err(unauthorized());
    }
    let Some((scheme, encoded)) = value.to_str().ok().and_then(|value| value.split_once(' '))
    else {
        return Err(unauthorized());
    };
    if !scheme.eq_ignore_ascii_case("basic") {
        return Err(unauthorized());
    }
    let decoded = STANDARD.decode(encoded).map_err(|_| unauthorized())?;
    if decoded.len() > MAX_BASIC_BYTES {
        return Err(unauthorized());
    }
    let Some((candidate_user, password)) = std::str::from_utf8(&decoded)
        .ok()
        .and_then(|value| value.split_once(':'))
    else {
        return Err(unauthorized());
    };
    if candidate_user != username || password.is_empty() || password.len() > 4096 {
        return Err(unauthorized());
    }
    let password = password.to_owned();
    let permit = PUSH_AUTH_WORK
        .try_acquire()
        .map_err(|_| retry_later("credential verification busy"))?;
    let valid = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        super::auth::verify_password(&password, &password_hash)
    })
    .await
    .unwrap_or(false);
    if valid { Ok(()) } else { Err(unauthorized()) }
}

use restream_engine::hls_ingest::playlist::normalize_path;
use restream_engine::hls_ingest::{
    AuthorizeError, BodyError, DispatchError, DispatchOrigin, HlsIngest, ObjectKind, ObjectOutcome,
};

pub(super) async fn hls_ingest_handler(
    State(state): State<HlsIngestState>,
    Path((stream_key, path)): Path<(String, String)>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ingest = state.ingest;
    let stores = match method {
        Method::PUT | Method::POST => true,
        Method::DELETE => false,
        _ => {
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, HeaderValue::from_static("PUT, POST, DELETE"))],
            )
                .into_response();
        }
    };
    let client = connect_info.map(|Extension(ConnectInfo(addr))| addr);
    let client_ip = client.map_or_else(|| "unknown".to_string(), |addr| addr.ip().to_string());
    let pipeline = match ingest.authorize(&stream_key, &client_ip).await {
        Ok(pipeline) => pipeline,
        Err(AuthorizeError::Forbidden) => {
            return (StatusCode::FORBIDDEN, "invalid stream key").into_response();
        }
        Err(AuthorizeError::LookupFailed(_)) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "stream key lookup failed").into_response();
        }
    };
    // Reuse the persisted input lookup rather than a credential cache: updates
    // and clears apply to the very next object, even in an existing session.
    let auth = match state.inputs.get_by_stream_key(&stream_key).await {
        Ok(Some(input))
            if input.id == pipeline.input_id
                && input.pipeline_id == pipeline.id
                && input.enabled
                && input.source_kind
                    == restream_domain::domain::pipeline_input::PipelineInputSourceKind::Keyed =>
        {
            input.push_auth
        }
        Ok(_) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Err(response) = require_push_auth(auth, &headers).await {
        return response;
    }
    if !stores {
        // FFmpeg's `delete_segments` removes old segments; nothing is kept
        // after publishing, so there is nothing to delete.
        return StatusCode::OK.into_response();
    }
    let Some(path) = normalize_path(&path) else {
        return (StatusCode::BAD_REQUEST, "invalid object path").into_response();
    };
    let Some(kind) = ObjectKind::of(&path) else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "only .m3u8 playlists, MPEG-TS .ts segments and fMP4 .mp4/.m4s objects are ingested",
        )
            .into_response();
    };
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let idle_floor = ingest.limits().idle_floor;
    let Ok(Some(slot)) =
        tokio::time::timeout(idle_floor, ingest.upload_slot(&pipeline.input_id)).await
    else {
        return (
            StatusCode::REQUEST_TIMEOUT,
            "upload slots for this stream key stayed busy",
        )
            .into_response();
    };
    // A stalled or trickling upload must not hold its slot and budget: each
    // frame has the idle floor to arrive, the whole body (including waits
    // for the key's buffer share) its deadline, and everything claimed is
    // released on expiry.
    let body_deadline = tokio::time::Instant::now() + ingest.body_deadline(&pipeline.input_id);
    let object = tokio::time::timeout_at(
        body_deadline,
        ingest.object_body(kind, content_length, &pipeline.input_id),
    )
    .await;
    let mut object = match object {
        Ok(Ok(object)) => object,
        Ok(Err(error)) => return body_error(error),
        Err(_) => return body_too_slow(),
    };
    let mut frames = body.into_data_stream();
    loop {
        let frame_deadline = body_deadline.min(tokio::time::Instant::now() + idle_floor);
        let frame = match tokio::time::timeout_at(frame_deadline, frames.next()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) => return body_too_slow(),
        };
        let Ok(frame) = frame else {
            return (StatusCode::BAD_REQUEST, "request body failed").into_response();
        };
        match tokio::time::timeout_at(body_deadline, object.push(&frame)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return body_error(error),
            Err(_) => return body_too_slow(),
        }
    }

    // The object is complete. Encoders that never read the response may
    // reset the connection now, which drops this handler; the object must
    // still reach its session, so the dispatch runs detached. The address is
    // the full peer, as the ingest protocols record it; the session start
    // re-authorizes from it and needs the bare IP, which it derives.
    let client_addr = client.map_or_else(|| "unknown".to_string(), |addr| addr.to_string());
    let dispatch = tokio::spawn(async move {
        let outcome = ingest
            .dispatch(
                &pipeline,
                &stream_key,
                &client_addr,
                DispatchOrigin::Push,
                kind,
                path,
                object,
            )
            .await;
        drop(slot);
        outcome
    });
    match dispatch.await {
        Ok(Ok(ObjectOutcome::Stored | ObjectOutcome::Dropped)) => StatusCode::OK.into_response(),
        Ok(Ok(ObjectOutcome::Held)) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(ObjectOutcome::Malformed(error))) => (StatusCode::BAD_REQUEST, error).into_response(),
        Ok(Ok(ObjectOutcome::Unsupported(error))) => {
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, error).into_response()
        }
        Ok(Ok(ObjectOutcome::TooLarge(error))) => {
            (StatusCode::PAYLOAD_TOO_LARGE, error).into_response()
        }
        Ok(Err(DispatchError::PublisherActive)) => (
            StatusCode::CONFLICT,
            "input already has an active publisher",
        )
            .into_response(),
        Ok(Err(DispatchError::AuthorizationChanged)) => (
            StatusCode::FORBIDDEN,
            "the stream key no longer authorizes this input",
        )
            .into_response(),
        Ok(Ok(ObjectOutcome::Retry)) => retry_later("ingest session ended"),
        Ok(Err(DispatchError::Unavailable)) | Err(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

fn body_error(error: BodyError) -> Response {
    match error {
        BodyError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        BodyError::BufferFull => retry_later("ingest buffer full"),
    }
}

fn body_too_slow() -> Response {
    (
        StatusCode::REQUEST_TIMEOUT,
        "request body stalled or too slow",
    )
        .into_response()
}

fn retry_later(reason: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, HeaderValue::from_static("1"))],
        reason,
    )
        .into_response()
}

#[cfg(test)]
mod tests;
