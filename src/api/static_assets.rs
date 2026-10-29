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
// Jobs and builds without generated frontend output compile with no embedded
// assets (served from disk first in development); releases verify the
// assets exist in `cargo xtask release prepare-build-tree`.
#[allow_missing = true]
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

/// Where a nested dashboard path (`a/b`, `a/`) is sent: back to the shell's
/// own directory, by a *relative* `../` per level, with the query kept (the
/// workspace lives in it). The shell's asset URLs are relative so it can sit
/// under a proxy prefix this server never sees; served at a nested path they
/// would resolve under it and the page would stay blank, and so would the
/// relative login redirect. A relative target resolves against the browser's
/// full URL, prefix included. `None` for a top-level path or an asset.
fn nested_shell_redirect(path: &str, query: Option<&str>) -> Option<String> {
    let depth = path.matches('/').count();
    if depth == 0 || request_targets_embedded_asset(path) {
        return None;
    }
    let mut target = "../".repeat(depth);
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        target.push('?');
        target.push_str(query);
    }
    Some(target)
}

fn request_targets_embedded_asset(path: &str) -> bool {
    !path.is_empty() && path.contains('.')
}

fn request_targets_authenticated_html(path: &str) -> bool {
    path.ends_with(".html")
}

/// Serves one embedded or disk-backed dashboard asset and applies the module's
/// shared cache and ETag policy.
pub fn serve_embedded_with_headers(path: &str, headers: &HeaderMap) -> Response {
    let content_type = static_asset_content_type(path);

    #[cfg(debug_assertions)]
    {
        // Development builds prefer disk assets first so frontend edits do not
        // need a Rust rebuild before the dashboard shell reflects them.
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

// Centralize the static asset cache contract here so development disk reads,
// embedded assets, and conditional GET handling all emit the same headers.
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

/// HTML, scripts and styles revalidate on every load (the content-hash ETag
/// makes that a 304): module and stylesheet names carry no content hash, so
/// a cached copy could outlive an upgrade and mix two builds' modules in one
/// page. Other assets (images) keep an hour.
fn cache_control(path: &str) -> HeaderValue {
    if [".html", ".js", ".css"]
        .iter()
        .any(|extension| path.ends_with(extension))
    {
        HeaderValue::from_static("no-cache")
    } else {
        HeaderValue::from_static("public, max-age=3600")
    }
}

/// Serves the public login page, redirecting already-authenticated sessions
/// back into the dashboard shell instead.
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

/// Historical redirect for `/login` routes that should land on the login HTML
/// entrypoint.
pub async fn login_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("login")
}

/// Redirects the legacy settings HTML path to the control room's Settings.
pub async fn settings_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("./?w=settings")
}

/// Redirects the legacy status HTML path to the control room's System.
pub async fn status_html_redirect_handler() -> impl IntoResponse {
    Redirect::to("./?w=system")
}

/// Serves the embedded dashboard logo asset.
pub async fn logo_handler(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_with_headers("logo.png", &headers)
}

/// Serves the compiled dashboard stylesheet asset.
pub async fn css_handler(headers: HeaderMap) -> impl IntoResponse {
    serve_embedded_with_headers("output.css", &headers)
}

/// Serves embedded assets directly when requested by path, otherwise applies
/// the authenticated SPA-shell fallback for dashboard routes.
pub async fn spa_fallback_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(root) = nested_shell_redirect(path, uri.query()) {
        return Redirect::to(&root).into_response();
    }
    if request_targets_embedded_asset(path) {
        if request_targets_authenticated_html(path)
            && !request_is_authenticated(&state, &headers).await
        {
            return login_redirect_response();
        }
        return serve_embedded_with_headers(path, &headers).into_response();
    }
    if !request_is_authenticated(&state, &headers).await {
        return login_redirect_response();
    }
    serve_embedded_with_headers("index.html", &headers).into_response()
}

/// Shared JSON 404 response for unknown API routes, keeping API misses off the
/// HTML SPA fallback path.
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
    use super::{
        cache_control, nested_shell_redirect, request_targets_authenticated_html,
        request_targets_embedded_asset, static_asset_content_type,
    };
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
    fn unhashed_code_revalidates_and_images_cache_for_an_hour() {
        for path in ["index.html", "js/control-room/app.js", "output.css"] {
            assert_eq!(
                cache_control(path),
                HeaderValue::from_static("no-cache"),
                "{path}"
            );
        }
        assert_eq!(
            cache_control("logo.png"),
            HeaderValue::from_static("public, max-age=3600")
        );
    }

    #[test]
    fn embedded_asset_detection_matches_spa_fallback_policy() {
        assert!(request_targets_embedded_asset("output.css"));
        assert!(request_targets_embedded_asset("nested/app.js"));
        assert!(!request_targets_embedded_asset(""));
        assert!(!request_targets_embedded_asset("dashboard"));
    }

    #[test]
    fn authenticated_html_detection_only_matches_html_documents() {
        assert!(request_targets_authenticated_html("login.html"));
        assert!(request_targets_authenticated_html("nested/index.html"));
        assert!(!request_targets_authenticated_html("output.css"));
    }

    /// A nested dashboard path goes back to the shell's directory by a
    /// relative `../` per level, query kept; a top-level path and an asset
    /// path are served where they are.
    #[test]
    fn a_nested_dashboard_path_redirects_relatively_to_the_shell() {
        assert_eq!(
            nested_shell_redirect("a/b", Some("w=inspect")),
            Some("../?w=inspect".to_string())
        );
        assert_eq!(nested_shell_redirect("a/", None), Some("../".to_string()));
        assert_eq!(
            nested_shell_redirect("a/b/c", Some("")),
            Some("../../".to_string())
        );
        assert_eq!(nested_shell_redirect("old-path", Some("w=media")), None);
        assert_eq!(nested_shell_redirect("", None), None);
        assert_eq!(nested_shell_redirect("js/control-room/main.js", None), None);
    }
}
