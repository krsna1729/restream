use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use rust_embed::RustEmbed;
use std::sync::Arc;

use super::state::{AppState, get_session_token_from_headers, request_is_authenticated};

#[derive(RustEmbed)]
#[folder = "public/"]
pub struct EmbeddedAssets;

pub fn serve_embedded(path: &str) -> Response {
    let content_type = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    };

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
            return (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data).into_response();
        }
    }

    match EmbeddedAssets::get(path) {
        Some(file) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            file.data.to_vec(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn login_get_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = get_session_token_from_headers(&headers)
        && state.is_authenticated(&token).await
    {
        return Redirect::to("/").into_response();
    }
    serve_embedded("login.html").into_response()
}

pub async fn login_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("/login")
}

pub async fn settings_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("/?mode=settings")
}

pub async fn status_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("/?mode=status")
}

pub async fn logo_handler() -> impl IntoResponse {
    serve_embedded("logo.png")
}

pub async fn css_handler() -> impl IntoResponse {
    serve_embedded("output.css")
}

pub async fn spa_fallback_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let path = uri.path().trim_start_matches('/');
    if !path.is_empty() && path.contains('.') {
        if path.ends_with(".html") && !request_is_authenticated(&state, &headers).await {
            return Redirect::to("/login").into_response();
        }
        return serve_embedded(path).into_response();
    }
    if !request_is_authenticated(&state, &headers).await {
        return Redirect::to("/login").into_response();
    }
    serve_embedded("index.html").into_response()
}
