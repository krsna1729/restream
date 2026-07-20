//! Central routing table for the dashboard HTTP surface.
//!
//! The router intentionally keeps route registration near the transport layer
//! so API modules own handlers while this file documents the public boundary
//! between static assets, authenticated APIs, and HLS/media endpoints.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header},
    routing::{any, get, patch, post, put},
};
use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use super::agent::{
    agent_capabilities_handler, agent_context_handler, agent_graph_diff_preview_handler,
    agent_investigation_handler, agent_operation_apply_handler, agent_operation_approve_handler,
    agent_operation_create_handler, agent_operation_get_handler, agent_operation_verify_handler,
    agent_plan_handler, agent_plan_validate_handler, agent_verify_handler,
};
use super::alerts::aggregate_alerts_handler;
use super::auth::{
    audio_caps_handler, change_password_handler, dismiss_password_change_prompt_handler,
    login_post_handler, logout_handler, rate_limits_handler, rate_limits_reset_handler,
    stream_keys_handler,
};
use super::file_ingest::{
    custom_encoding_get, custom_encoding_put, pipeline_file_ingest_delete_handler,
    pipeline_file_ingest_get_handler, pipeline_file_ingest_put_handler,
};
use super::health::{healthz_get_handler, v1_engine_health_handler};
use super::hls::{
    hls_audio_init_handler, hls_audio_playlist_handler, hls_audio_segment_handler,
    hls_master_handler, hls_playlist_handler, hls_segment_handler, hls_video_init_handler,
    hls_video_playlist_handler, hls_video_segment_handler, input_hls_audio_init_handler,
    input_hls_audio_playlist_handler, input_hls_audio_segment_handler, input_hls_master_handler,
    input_hls_video_init_handler, input_hls_video_playlist_handler,
    input_hls_video_segment_handler,
};
use super::ingests::{
    ingests_delete_handler, ingests_get_handler, ingests_post_handler, ingests_start_handler,
    ingests_stop_handler, ingests_update_handler,
};
use super::logs::{logs_handler, logs_stream_handler};
use super::media_library::{
    MAX_MEDIA_UPLOAD_BYTES, media_analysis_handler, media_delete_handler, media_file_handler,
    media_list_handler, media_rename_handler, media_upload_handler, recording_start_handler,
    recording_stop_handler,
};
use super::outputs::{
    output_status_handler, outputs_create_handler, outputs_delete_handler, outputs_start_handler,
    outputs_stop_handler, outputs_update_handler, youtube_monitoring_status_handler,
};
use super::pipeline_inputs::{
    pipeline_input_delete_handler, pipeline_input_patch_handler, pipeline_input_promote_handler,
    pipeline_inputs_get_handler, pipeline_inputs_post_handler,
};
use super::pipeline_observability::{
    pipeline_alerts_handler, pipeline_diagnostics_context_handler, pipeline_graph_handler,
    v1_pipeline_summary_handler,
};
use super::pipelines::{
    pipeline_detail_handler, pipeline_probe_handler, pipelines_delete_handler,
    pipelines_get_handler, pipelines_post_handler, pipelines_update_handler,
};
use super::settings::{config_get_handler, config_patch_handler};
use super::state::AppState;
use super::static_assets::{
    api_not_found_handler, css_handler, login_get_handler, login_html_redirect_handler,
    logo_handler, settings_html_redirect_handler, spa_fallback_handler,
    status_html_redirect_handler,
};
use super::telemetry::{
    metrics_system_handler, pipeline_diagnostics_run_handler, status_get_handler,
    status_sbom_get_handler, v1_dashboard_runtime_handler, v1_engine_resource_map_handler,
    v1_engine_telemetry_handler, v1_events_handler, v1_overview_handler,
    v1_pipeline_telemetry_handler, v1_stage_telemetry_handler,
};

const DEFAULT_API_BODY_LIMIT_BYTES: usize = 4 * 1024 * 1024;

// These path lists document which transport surfaces are intentionally public
// versus session-gated. They are also reused by tests and external tooling that
// want to reason about route exposure without rebuilding the router tree.
pub const PUBLIC_ROUTE_PATHS: &[&str] = &[
    "/login",
    "/login.html",
    "/settings.html",
    "/status.html",
    "/logo.png",
    "/output.css",
    "/api/v1/auth/login",
    "/api/{*path}",
    "/healthz",
    "/metrics/system",
];

// Authenticated routes include both dashboard JSON APIs and media/HLS entry
// points that should only resolve after the request has passed session checks.
pub const AUTHENTICATED_ROUTE_PATHS: &[&str] = &[
    "/api/v1/auth/logout",
    "/api/v1/auth/change-password",
    "/api/v1/auth/dismiss-password-change",
    "/api/v1/security/rate-limits",
    "/api/v1/security/rate-limits/reset",
    "/api/v1/audio-caps",
    "/api/v1/settings",
    "/api/v1/stream-keys",
    "/api/v1/dashboard/runtime",
    "/api/v1/engine/resource-map",
    "/api/v1/monitoring/youtube-status",
    "/api/v1/pipelines",
    "/api/v1/pipelines/{id}",
    "/api/v1/pipelines/{pipeline_id}/inputs",
    "/api/v1/pipelines/{pipeline_id}/inputs/{input_id}",
    "/api/v1/pipelines/{pipeline_id}/inputs/{input_id}/promote",
    "/api/v1/pipelines/{pipeline_id}/file-ingest",
    "/api/v1/pipelines/{pipeline_id}/outputs",
    "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}",
    "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/start",
    "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop",
    "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status",
    "/api/v1/pipelines/{pipeline_id}/probe",
    "/api/v1/pipelines/{pipeline_id}/graph",
    "/api/v1/pipelines/{pipeline_id}/alerts",
    "/api/v1/logs",
    "/api/v1/logs/stream",
    "/api/v1/alerts",
    "/api/v1/events",
    "/api/v1/overview",
    "/api/v1/engine/telemetry",
    "/api/v1/agent/capabilities",
    "/api/v1/agent/context",
    "/api/v1/agent/investigations",
    "/api/v1/agent/plans",
    "/api/v1/agent/plans/validate",
    "/api/v1/agent/graph-diff-preview",
    "/api/v1/agent/operations",
    "/api/v1/agent/operations/{operation_id}",
    "/api/v1/agent/operations/{operation_id}/approve",
    "/api/v1/agent/operations/{operation_id}/apply",
    "/api/v1/agent/operations/{operation_id}/verify",
    "/api/v1/agent/verify",
    "/api/v1/pipelines/{pipeline_id}/telemetry",
    "/api/v1/stages/{stage_key}/telemetry",
    "/api/v1/pipelines/{pipeline_id}/summary",
    "/api/v1/pipelines/{pipeline_id}/diagnostics/context",
    "/api/v1/pipelines/{pipeline_id}/diagnostics/run",
    "/api/v1/pipelines/{pipeline_id}/recording/start",
    "/api/v1/pipelines/{pipeline_id}/recording/stop",
    "/api/v1/encodings/custom",
    "/api/v1/ingests",
    "/api/v1/ingests/{id}",
    "/api/v1/ingests/{id}/start",
    "/api/v1/ingests/{id}/stop",
    "/api/v1/engine",
    "/api/v1/engine/sbom",
    "/api/v1/engine/health",
    "/api/v1/media",
    "/api/v1/media/upload",
    "/api/v1/media/{filename}/analysis",
    "/api/v1/media/{filename}",
    "/media/{filename}",
    "/hls/{pipeline_id}",
    "/hls/{pipeline_id}/master.m3u8",
    "/hls/{pipeline_id}/index.m3u8",
    "/hls/{pipeline_id}/video/index.m3u8",
    "/hls/{pipeline_id}/video/init.mp4",
    "/hls/{pipeline_id}/video/{segment}",
    "/hls/{pipeline_id}/audio/{track_index}/index.m3u8",
    "/hls/{pipeline_id}/audio/{track_index}/init.mp4",
    "/hls/{pipeline_id}/audio/{track_index}/{segment}",
    "/hls/{pipeline_id}/{segment}",
    "/hls/inputs/{input_id}/master.m3u8",
    "/hls/inputs/{input_id}/video/index.m3u8",
    "/hls/inputs/{input_id}/video/init.mp4",
    "/hls/inputs/{input_id}/video/{segment}",
    "/hls/inputs/{input_id}/audio/{track_index}/index.m3u8",
    "/hls/inputs/{input_id}/audio/{track_index}/init.mp4",
    "/hls/inputs/{input_id}/audio/{track_index}/{segment}",
];

/// Registers the authenticated HLS transport surface separately from the main
/// JSON API so streaming routes stay easy to audit as one group.
fn create_hls_router() -> Router<Arc<AppState>> {
    // HLS endpoints are grouped separately because they expose the streaming
    // surface and are easiest to scan when kept together.
    Router::new()
        .route(
            "/hls/inputs/{input_id}/master.m3u8",
            get(input_hls_master_handler),
        )
        .route(
            "/hls/inputs/{input_id}/video/index.m3u8",
            get(input_hls_video_playlist_handler),
        )
        .route(
            "/hls/inputs/{input_id}/video/init.mp4",
            get(input_hls_video_init_handler),
        )
        .route(
            "/hls/inputs/{input_id}/video/{segment}",
            get(input_hls_video_segment_handler),
        )
        .route(
            "/hls/inputs/{input_id}/audio/{track_index}/index.m3u8",
            get(input_hls_audio_playlist_handler),
        )
        .route(
            "/hls/inputs/{input_id}/audio/{track_index}/init.mp4",
            get(input_hls_audio_init_handler),
        )
        .route(
            "/hls/inputs/{input_id}/audio/{track_index}/{segment}",
            get(input_hls_audio_segment_handler),
        )
        .route("/hls/{pipeline_id}", get(hls_playlist_handler))
        .route("/hls/{pipeline_id}/master.m3u8", get(hls_master_handler))
        .route("/hls/{pipeline_id}/index.m3u8", get(hls_playlist_handler))
        .route(
            "/hls/{pipeline_id}/video/index.m3u8",
            get(hls_video_playlist_handler),
        )
        .route(
            "/hls/{pipeline_id}/video/init.mp4",
            get(hls_video_init_handler),
        )
        .route(
            "/hls/{pipeline_id}/video/{segment}",
            get(hls_video_segment_handler),
        )
        .route(
            "/hls/{pipeline_id}/audio/{track_index}/index.m3u8",
            get(hls_audio_playlist_handler),
        )
        .route(
            "/hls/{pipeline_id}/audio/{track_index}/init.mp4",
            get(hls_audio_init_handler),
        )
        .route(
            "/hls/{pipeline_id}/audio/{track_index}/{segment}",
            get(hls_audio_segment_handler),
        )
        .route("/hls/{pipeline_id}/{segment}", get(hls_segment_handler))
}

/// Builds the complete dashboard router before transport-wide layers are
/// applied, keeping route registration close to the public boundary list above.
fn create_app_router() -> Router<Arc<AppState>> {
    // Keep the full route table in one place so the public dashboard surface is
    // easy to audit, even though individual handlers stay in smaller modules.
    Router::new()
        .route("/login", get(login_get_handler))
        .route("/login.html", get(login_html_redirect_handler))
        .route("/settings.html", get(settings_html_redirect_handler))
        .route("/status.html", get(status_html_redirect_handler))
        .route("/logo.png", get(logo_handler))
        .route("/output.css", get(css_handler))
        .route("/api/v1/auth/login", post(login_post_handler))
        .route("/api/v1/auth/logout", post(logout_handler))
        .route(
            "/api/v1/auth/change-password",
            post(change_password_handler),
        )
        .route(
            "/api/v1/auth/dismiss-password-change",
            post(dismiss_password_change_prompt_handler),
        )
        .route("/api/v1/security/rate-limits", get(rate_limits_handler))
        .route(
            "/api/v1/security/rate-limits/reset",
            post(rate_limits_reset_handler),
        )
        .route("/api/v1/audio-caps", get(audio_caps_handler))
        .route(
            "/api/v1/settings",
            get(config_get_handler).patch(config_patch_handler),
        )
        .route("/api/v1/stream-keys", get(stream_keys_handler))
        .route(
            "/api/v1/dashboard/runtime",
            get(v1_dashboard_runtime_handler),
        )
        .route(
            "/api/v1/monitoring/youtube-status",
            get(youtube_monitoring_status_handler),
        )
        .route(
            "/api/v1/pipelines",
            get(pipelines_get_handler).post(pipelines_post_handler),
        )
        .route(
            "/api/v1/pipelines/{id}",
            get(pipeline_detail_handler)
                .patch(pipelines_update_handler)
                .delete(pipelines_delete_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/inputs",
            get(pipeline_inputs_get_handler).post(pipeline_inputs_post_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/inputs/{input_id}",
            patch(pipeline_input_patch_handler).delete(pipeline_input_delete_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/inputs/{input_id}/promote",
            post(pipeline_input_promote_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/file-ingest",
            get(pipeline_file_ingest_get_handler)
                .put(pipeline_file_ingest_put_handler)
                .delete(pipeline_file_ingest_delete_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/outputs",
            post(outputs_create_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}",
            patch(outputs_update_handler).delete(outputs_delete_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/start",
            post(outputs_start_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop",
            post(outputs_stop_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status",
            get(output_status_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/probe",
            get(pipeline_probe_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/graph",
            get(pipeline_graph_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/alerts",
            get(pipeline_alerts_handler),
        )
        .route("/api/v1/logs", get(logs_handler))
        .route("/api/v1/logs/stream", get(logs_stream_handler))
        .route("/api/v1/alerts", get(aggregate_alerts_handler))
        .route("/api/v1/events", get(v1_events_handler))
        .route("/api/v1/overview", get(v1_overview_handler))
        .route(
            "/api/v1/engine/resource-map",
            get(v1_engine_resource_map_handler),
        )
        .route("/api/v1/engine/telemetry", get(v1_engine_telemetry_handler))
        .route(
            "/api/v1/agent/capabilities",
            get(agent_capabilities_handler),
        )
        .route("/api/v1/agent/context", get(agent_context_handler))
        .route(
            "/api/v1/agent/investigations",
            post(agent_investigation_handler),
        )
        .route("/api/v1/agent/plans", post(agent_plan_handler))
        .route(
            "/api/v1/agent/plans/validate",
            post(agent_plan_validate_handler),
        )
        .route(
            "/api/v1/agent/graph-diff-preview",
            post(agent_graph_diff_preview_handler),
        )
        .route(
            "/api/v1/agent/operations",
            post(agent_operation_create_handler),
        )
        .route(
            "/api/v1/agent/operations/{operation_id}",
            get(agent_operation_get_handler),
        )
        .route(
            "/api/v1/agent/operations/{operation_id}/approve",
            post(agent_operation_approve_handler),
        )
        .route(
            "/api/v1/agent/operations/{operation_id}/apply",
            post(agent_operation_apply_handler),
        )
        .route(
            "/api/v1/agent/operations/{operation_id}/verify",
            post(agent_operation_verify_handler),
        )
        .route("/api/v1/agent/verify", post(agent_verify_handler))
        .route(
            "/api/v1/pipelines/{pipeline_id}/telemetry",
            get(v1_pipeline_telemetry_handler),
        )
        .route(
            "/api/v1/stages/{stage_key}/telemetry",
            get(v1_stage_telemetry_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/summary",
            get(v1_pipeline_summary_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/diagnostics/context",
            get(pipeline_diagnostics_context_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/diagnostics/run",
            post(pipeline_diagnostics_run_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/recording/start",
            post(recording_start_handler),
        )
        .route(
            "/api/v1/pipelines/{pipeline_id}/recording/stop",
            post(recording_stop_handler),
        )
        .route(
            "/api/v1/encodings/custom",
            get(custom_encoding_get).put(custom_encoding_put),
        )
        .route(
            "/api/v1/ingests",
            get(ingests_get_handler).post(ingests_post_handler),
        )
        .route(
            "/api/v1/ingests/{id}",
            put(ingests_update_handler).delete(ingests_delete_handler),
        )
        .route("/api/v1/ingests/{id}/start", post(ingests_start_handler))
        .route("/api/v1/ingests/{id}/stop", post(ingests_stop_handler))
        .route("/api/v1/engine", get(status_get_handler))
        .route("/api/v1/engine/sbom", get(status_sbom_get_handler))
        .route("/api/v1/engine/health", get(v1_engine_health_handler))
        .route(
            "/api/v1/media/upload",
            post(media_upload_handler).layer(DefaultBodyLimit::max(MAX_MEDIA_UPLOAD_BYTES)),
        )
        .route("/api/v1/media", get(media_list_handler))
        .route(
            "/api/v1/media/{filename}/analysis",
            get(media_analysis_handler),
        )
        .route(
            "/api/v1/media/{filename}",
            patch(media_rename_handler).delete(media_delete_handler),
        )
        .route(
            "/media/{filename}",
            get(media_file_handler).head(media_file_handler),
        )
        .route("/healthz", get(healthz_get_handler))
        .route("/metrics/system", get(metrics_system_handler))
        .merge(create_hls_router())
        .route("/api/{*path}", any(api_not_found_handler))
        .fallback(get(spa_fallback_handler))
}

// These layers define transport-wide policy that should apply equally to JSON,
// static assets, and streaming endpoints unless a route opts out explicitly.
/// Applies baseline transport policy shared by dashboard APIs, static assets,
/// and media endpoints unless a specific route opts out.
fn apply_standard_layers(router: Router<Arc<AppState>>) -> Router<Arc<AppState>> {
    router
        .layer(CompressionLayer::new())
        .layer(DefaultBodyLimit::max(DEFAULT_API_BODY_LIMIT_BYTES))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("SAMEORIGIN"),
        ))
}

/// Constructs the externally exposed application router with shared transport
/// layers and the concrete application state attached.
pub fn create_router(state: Arc<AppState>) -> Router {
    apply_standard_layers(create_app_router()).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::{AUTHENTICATED_ROUTE_PATHS, PUBLIC_ROUTE_PATHS};
    use std::collections::HashSet;

    #[test]
    fn public_and_authenticated_route_lists_do_not_overlap() {
        let public_routes = PUBLIC_ROUTE_PATHS.iter().copied().collect::<HashSet<_>>();

        assert!(
            AUTHENTICATED_ROUTE_PATHS
                .iter()
                .all(|path| !public_routes.contains(path))
        );
    }

    #[test]
    fn route_lists_cover_key_dashboard_boundaries() {
        assert!(PUBLIC_ROUTE_PATHS.contains(&"/api/v1/auth/login"));
        assert!(PUBLIC_ROUTE_PATHS.contains(&"/metrics/system"));
        assert!(AUTHENTICATED_ROUTE_PATHS.contains(&"/api/v1/settings"));
        assert!(AUTHENTICATED_ROUTE_PATHS.contains(&"/hls/{pipeline_id}/master.m3u8"));
    }
}
