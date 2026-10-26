//! Share links over HTTP: admins make and revoke them per pipeline; anyone
//! with a live link opens `/watch/:token`, a bare player that can be
//! embedded on any site.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use super::error::ApiError;
use super::state::{AppState, require_authenticated, to_hex};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `GET /api/v1/pipelines/{pipeline_id}/share-links` (admin, by policy).
pub async fn list_share_links_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state.share_links.for_pipeline(&pipeline_id).await {
        Ok(links) => Json(serde_json::json!({ "links": links })).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateShareLink {
    pub label: String,
    /// `None`: until revoked.
    pub lifetime_hours: Option<i64>,
}

/// `POST /api/v1/pipelines/{pipeline_id}/share-links` (admin, by policy).
pub async fn create_share_link_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<CreateShareLink>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Err(error) = state.file_ingest_service.get_pipeline(&pipeline_id).await {
        return ApiError::from(error).into_response();
    }
    use rand::RngExt;
    let mut id = [0u8; 8];
    let mut token = [0u8; 24];
    rand::rng().fill(&mut id);
    rand::rng().fill(&mut token);
    match state
        .share_links
        .create(
            format!("sl_{}", to_hex(&id)),
            to_hex(&token),
            &pipeline_id,
            &payload.label,
            payload.lifetime_hours,
            now_ms(),
        )
        .await
    {
        Ok(link) => (StatusCode::CREATED, Json(link)).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `DELETE /api/v1/share-links/{id}` (admin, by policy): the link stops
/// working at once, including for players already watching.
pub async fn revoke_share_link_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state.share_links.revoke(&id).await {
        Ok(()) => Json(serde_json::json!({ "deleted": true })).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Any page may frame the player: `frame-ancestors *` takes precedence over
/// the server-wide `X-Frame-Options: SAMEORIGIN` in current browsers.
fn embeddable(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors *"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /watch/{token}`: the public player for a live share link.
pub async fn watch_page_handler(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Response {
    let Some(link) = state.share_links.resolve(&token, now_ms()).await else {
        return embeddable((StatusCode::NOT_FOUND, Html(WATCH_GONE.to_string())).into_response());
    };
    state.share_links.count_view(&link.id).await;
    let title = state
        .file_ingest_service
        .get_pipeline(&link.pipeline_id)
        .await
        .map(|pipeline| pipeline.name)
        .unwrap_or_else(|_| link.label.clone());
    // Relative to /watch/: works under any proxy prefix.
    let source = format!("../hls/{}/master.m3u8?t={}", link.pipeline_id, link.token);
    let page = WATCH_PAGE
        .replace("{{title}}", &escape_html(&title))
        .replace("{{label}}", &escape_html(&link.label))
        .replace("{{source}}", &escape_html(&source))
        .replace("{{token}}", &escape_html(&link.token));
    embeddable(Html(page).into_response())
}

const WATCH_GONE: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Link unavailable</title>
<style>html,body{height:100%;margin:0;background:#0b0f14;color:#e6edf3;font:16px system-ui,sans-serif;display:grid;place-items:center;text-align:center}p{color:#9aa7b4}</style></head>
<body><main><h1>This link is no longer available</h1><p>It expired or was revoked. Ask whoever shared it for a new one.</p></main></body></html>"#;

const WATCH_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>{{title}} · live</title>
<style>
  :root { color-scheme: dark; }
  html, body { height: 100%; margin: 0; background: #000; color: #e6edf3; font: 15px system-ui, sans-serif; }
  .stage { position: fixed; inset: 0; display: grid; place-items: center; }
  video { width: 100%; height: 100%; background: #000; object-fit: contain; }
  .bar { position: fixed; left: 12px; top: 12px; display: flex; gap: 8px; align-items: center; pointer-events: none; }
  .live { background: #d1242f; color: #fff; font-weight: 700; font-size: 12px; letter-spacing: .06em; padding: 3px 8px; border-radius: 4px; }
  .live.off { background: #4b5563; }
  .title { background: rgba(0,0,0,.55); padding: 3px 8px; border-radius: 4px; max-width: 60vw; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .note { position: fixed; inset: auto 0 18px 0; text-align: center; color: #c9d1d9; text-shadow: 0 1px 2px #000; }
</style>
</head>
<body>
<div class="stage"><video id="player" controls playsinline muted autoplay aria-label="{{title}}, live"></video></div>
<div class="bar"><span id="live" class="live off">OFFLINE</span><span class="title">{{title}}</span></div>
<p id="note" class="note" role="status">Connecting…</p>
<script type="module">
// The dashboard's own build of hls.js (Vite emits it under js/vendor).
import Hls from "../js/vendor/hls.js/dist/hls.js";
(function () {
  var video = document.getElementById("player");
  var live = document.getElementById("live");
  var note = document.getElementById("note");
  var source = "{{source}}";
  var token = "{{token}}";
  function onAir(on, message) {
    live.textContent = on ? "LIVE" : "OFFLINE";
    live.className = on ? "live" : "live off";
    note.textContent = message || "";
  }
  video.addEventListener("playing", function () { onAir(true, ""); });
  if (Hls.isSupported()) {
    var start = function () {
      var hls = new Hls({
        liveSyncDurationCount: 3,
        // Every playlist and segment carries the link's token.
        xhrSetup: function (xhr, url) {
          if (url.indexOf("t=") === -1) xhr.open("GET", url + (url.indexOf("?") === -1 ? "?" : "&") + "t=" + token, true);
        },
      });
      hls.on(Hls.Events.ERROR, function (_event, data) {
        if (!data.fatal) return;
        hls.destroy();
        onAir(false, "The stream is not live right now. Retrying…");
        setTimeout(start, 5000);
      });
      hls.loadSource(source);
      hls.attachMedia(video);
    };
    start();
  } else if (video.canPlayType("application/vnd.apple.mpegurl")) {
    video.src = source;
  } else {
    onAir(false, "This browser cannot play the stream.");
  }
})();
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn page_values_are_escaped() {
        assert_eq!(
            escape_html(r#"<b>"x"&'y'"#),
            "&lt;b&gt;&quot;x&quot;&amp;&#39;y&#39;"
        );
    }
}
