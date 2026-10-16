//! HTTP surface of HLS push ingest: `PUT`/`POST /ingest/hls/{key}/{*path}`.
//!
//! The stream key is the credential: it is resolved before any body byte is
//! read. The handler only reads the bounded body and hands the object to the
//! key's session; it never demuxes.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;

use crate::media::hls_ingest::playlist::normalize_path;
use crate::media::hls_ingest::{
    AuthorizeError, BodyError, DispatchError, HlsIngest, ObjectKind, ObjectOutcome,
};

pub async fn hls_ingest_handler(
    State(ingest): State<Arc<HlsIngest>>,
    Path((stream_key, path)): Path<(String, String)>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
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
            "only .m3u8 playlists and MPEG-TS .ts segments are ingested",
        )
            .into_response();
    };
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let mut object = match ingest.object_body(kind, content_length) {
        Ok(object) => object,
        Err(error) => return body_error(error),
    };
    let Some(slot) = ingest.upload_slot(&pipeline.input_id).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut frames = body.into_data_stream();
    while let Some(frame) = frames.next().await {
        let Ok(frame) = frame else {
            return (StatusCode::BAD_REQUEST, "request body failed").into_response();
        };
        if let Err(error) = object.push(&frame) {
            return body_error(error);
        }
    }

    // The object is complete. Encoders that never read the response may
    // reset the connection now, which drops this handler; the object must
    // still reach its session, so the dispatch runs detached.
    let client_addr = client.map_or_else(|| "unknown".to_string(), |addr| addr.to_string());
    let dispatch = tokio::spawn(async move {
        let outcome = ingest
            .dispatch(&pipeline, &stream_key, &client_addr, kind, path, object)
            .await;
        drop(slot);
        outcome
    });
    match dispatch.await {
        Ok(Ok(ObjectOutcome::Stored | ObjectOutcome::Dropped)) => StatusCode::OK.into_response(),
        Ok(Ok(ObjectOutcome::Held)) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(ObjectOutcome::Malformed(error))) => (StatusCode::BAD_REQUEST, error).into_response(),
        Ok(Err(DispatchError::PublisherActive)) => (
            StatusCode::CONFLICT,
            "input already has an active publisher",
        )
            .into_response(),
        Ok(Err(DispatchError::Unavailable)) | Err(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

fn body_error(error: BodyError) -> Response {
    match error {
        BodyError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        BodyError::BufferFull => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, HeaderValue::from_static("1"))],
            "ingest buffer full",
        )
            .into_response(),
    }
}

#[cfg(test)]
#[path = "hls_ingest_tests.rs"]
mod tests;
