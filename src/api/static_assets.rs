//! Static asset handlers serve the embedded dashboard shell and gate access to
//! authenticated HTML entrypoints. This module keeps cache and redirect policy
//! close to the transport layer so asset delivery rules stay easy to audit.

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Redirect, Response},
};
use bytes::Bytes;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use super::state::{AppState, get_session_token_from_headers, request_is_authenticated, to_hex};

#[derive(RustEmbed)]
#[folder = "public/"]
pub struct EmbeddedAssets;

fn static_asset_content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn login_redirect_response() -> Response {
    Redirect::to("login").into_response()
}

pub fn serve_embedded(path: &str) -> Response {
    serve_embedded_with_headers(path, &HeaderMap::new())
}

pub fn serve_embedded_with_headers(path: &str, headers: &HeaderMap) -> Response {
    let content_type = static_asset_content_type(path);

    #[cfg(debug_assertions)]
    {
        let public_root = match std::fs::canonicalize("public") {
            Ok(p) => p,
            Err(_) => std::path::PathBuf::new(),
        };
        if let Ok(candidate) = std::fs::canonicalize(format!("public/{}", path))
            && candidate.starts_with(&public_root)
            && let Ok(data) = std::fs::read(&candidate)
        {
            return static_asset_response(path, content_type, bytes::Bytes::from(data), headers);
        }
    }

    match EmbeddedAssets::get(path) {
        Some(file) => {
            let data = match file.data {
                std::borrow::Cow::Borrowed(data) => Bytes::from_static(data),
                std::borrow::Cow::Owned(data) => Bytes::from(data),
            };
            static_asset_response(path, content_type, data, headers)
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn static_asset_response(
    path: &str,
    content_type: &'static str,
    data: Bytes,
    headers: &HeaderMap,
) -> Response {
    let etag = asset_etag(path, &data);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, header_value(&etag)),
                (header::CACHE_CONTROL, cache_control(path)),
            ],
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::ETAG, header_value(&etag)),
            (header::CACHE_CONTROL, cache_control(path)),
        ],
        data,
    )
        .into_response()
}

fn asset_etag(path: &str, data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hasher.update([0]);
    hasher.update(data);
    format!("\"{}\"", to_hex(&hasher.finalize()))
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("generated static asset header is valid")
}

fn cache_control(path: &str) -> HeaderValue {
    if path.ends_with(".html") {
        HeaderValue::from_static("no-cache")
    } else {
        HeaderValue::from_static("public, max-age=3600")
    }
}

pub async fn login_get_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    // The login page is public, but authenticated sessions should land in the
    // app shell instead of seeing the sign-in screen again.
    if let Some(token) = get_session_token_from_headers(&headers)
        && state.is_authenticated(&token).await
    {
        return Redirect::to("./").into_response();
    }
    serve_embedded_with_headers("login.html", &headers).into_response()
}

pub async fn login_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("login")
}

pub async fn settings_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("./?mode=settings")
}

pub async fn status_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("./?mode=status")
}

pub async fn logo_handler(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_with_headers("logo.png", &headers)
}

pub async fn css_handler(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_with_headers("output.css", &headers)
}

pub async fn spa_fallback_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let path = uri.path().trim_start_matches('/');
    if !path.is_empty() && path.contains('.') {
        if path.ends_with(".html") && !request_is_authenticated(&state, &headers).await {
            return login_redirect_response();
        }
        return serve_embedded_with_headers(path, &headers).into_response();
    }
    if !request_is_authenticated(&state, &headers).await {
        return login_redirect_response();
    }
    serve_embedded_with_headers("index.html", &headers).into_response()
}

pub async fn api_not_found_handler(uri: Uri) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        axum::Json(serde_json::json!({
            "error": "API route not found",
            "path": uri.path(),
            "status": 404,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::{cache_control, static_asset_content_type};
    use axum::http::HeaderValue;

    #[test]
    fn static_asset_content_type_matches_known_extensions() {
        assert_eq!(
            static_asset_content_type("index.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(static_asset_content_type("output.css"), "text/css");
        assert_eq!(
            static_asset_content_type("asset.bin"),
            "application/octet-stream"
        );
    }

    #[test]
    fn html_assets_disable_long_term_cache() {
        assert_eq!(
            cache_control("index.html"),
            HeaderValue::from_static("no-cache")
        );
        assert_eq!(
            cache_control("output.css"),
            HeaderValue::from_static("public, max-age=3600")
        );
    }
}
