use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header},
    routing::{get, post, patch, delete, put},
};
use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

use super::state::AppState;
use super::auth::{
    login_post_handler, logout_handler, change_password_handler, audio_caps_handler,
    stream_keys_handler,
};
use super::static_assets::{
    login_get_handler, login_html_redirect_handler, settings_html_redirect_handler,
    status_html_redirect_handler, logo_handler, css_handler, spa_fallback_handler,
};
use super::settings::{config_get_handler, config_patch_handler};
use super::telemetry::{
    v1_dashboard_runtime_handler, v1_events_handler, v1_overview_handler,
    v1_engine_telemetry_handler, v1_pipeline_telemetry_handler, v1_stage_telemetry_handler,
    pipeline_diagnostics_sse_handler, status_get_handler, status_sbom_get_handler,
    v1_engine_health_handler, healthz_get_handler, metrics_system_handler,
};
use super::outputs::{
    youtube_monitoring_status_handler, outputs_create_handler, outputs_update_handler,
    outputs_delete_handler, outputs_start_handler, outputs_stop_handler, output_status_handler,
};
use super::pipelines::{
    pipelines_get_handler, pipelines_post_handler, pipeline_detail_handler,
    pipelines_update_handler, pipelines_delete_handler, pipeline_probe_handler,
    pipeline_graph_handler, pipeline_alerts_handler, v1_pipeline_summary_handler,
};
use super::file_ingest::{
    pipeline_file_ingest_get_handler, pipeline_file_ingest_put_handler,
    pipeline_file_ingest_delete_handler, custom_encoding_get, custom_encoding_put,
};
use super::logs::{logs_handler, logs_stream_handler};
use super::alerts::aggregate_alerts_handler;
use super::agent::{
    agent_capabilities_handler, agent_context_handler, agent_investigation_handler,
    agent_plan_handler, agent_plan_validate_handler, agent_graph_diff_preview_handler,
    agent_operation_create_handler, agent_operation_get_handler, agent_operation_approve_handler,
    agent_operation_apply_handler, agent_operation_verify_handler, agent_verify_handler,
};
use super::ingests::{
    ingests_get_handler, ingests_post_handler, ingests_update_handler,
    ingests_delete_handler, ingests_start_handler, ingests_stop_handler,
};
use super::media_library::{
    recording_start_handler, recording_stop_handler, media_list_handler,
    media_analysis_handler, media_rename_handler, media_delete_handler, media_file_handler,
};
use super::hls::{
    hls_playlist_handler, hls_master_handler, hls_video_playlist_handler,
    hls_video_init_handler, hls_video_segment_handler, hls_audio_playlist_handler,
    hls_audio_init_handler, hls_audio_segment_handler, hls_segment_handler,
};

pub fn create_router(state: Arc<AppState>) -> Router {
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
            "/api/v1/pipelines/:id",
            get(pipeline_detail_handler)
                .patch(pipelines_update_handler)
                .delete(pipelines_delete_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/file-ingest",
            get(pipeline_file_ingest_get_handler)
                .put(pipeline_file_ingest_put_handler)
                .delete(pipeline_file_ingest_delete_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/outputs",
            post(outputs_create_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/outputs/:output_id",
            patch(outputs_update_handler).delete(outputs_delete_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/outputs/:output_id/start",
            post(outputs_start_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/outputs/:output_id/stop",
            post(outputs_stop_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/outputs/:output_id/status",
            get(output_status_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/probe",
            get(pipeline_probe_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/graph",
            get(pipeline_graph_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/alerts",
            get(pipeline_alerts_handler),
        )
        .route("/api/v1/logs", get(logs_handler))
        .route("/api/v1/logs/stream", get(logs_stream_handler))
        .route("/api/v1/alerts", get(aggregate_alerts_handler))
        .route("/api/v1/events", get(v1_events_handler))
        .route("/api/v1/overview", get(v1_overview_handler))
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
            "/api/v1/agent/operations/:operation_id",
            get(agent_operation_get_handler),
        )
        .route(
            "/api/v1/agent/operations/:operation_id/approve",
            post(agent_operation_approve_handler),
        )
        .route(
            "/api/v1/agent/operations/:operation_id/apply",
            post(agent_operation_apply_handler),
        )
        .route(
            "/api/v1/agent/operations/:operation_id/verify",
            post(agent_operation_verify_handler),
        )
        .route("/api/v1/agent/verify", post(agent_verify_handler))
        .route(
            "/api/v1/pipelines/:pipeline_id/telemetry",
            get(v1_pipeline_telemetry_handler),
        )
        .route(
            "/api/v1/stages/:stage_key/telemetry",
            get(v1_stage_telemetry_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/summary",
            get(v1_pipeline_summary_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/diagnostics",
            get(pipeline_diagnostics_sse_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/recording/start",
            post(recording_start_handler),
        )
        .route(
            "/api/v1/pipelines/:pipeline_id/recording/stop",
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
            "/api/v1/ingests/:id",
            put(ingests_update_handler).delete(ingests_delete_handler),
        )
        .route("/api/v1/ingests/:id/start", post(ingests_start_handler))
        .route("/api/v1/ingests/:id/stop", post(ingests_stop_handler))
        .route("/api/v1/engine", get(status_get_handler))
        .route("/api/v1/engine/sbom", get(status_sbom_get_handler))
        .route("/api/v1/engine/health", get(v1_engine_health_handler))
        .route("/api/v1/media", get(media_list_handler))
        .route(
            "/api/v1/media/:filename/analysis",
            get(media_analysis_handler),
        )
        .route(
            "/api/v1/media/:filename",
            patch(media_rename_handler).delete(media_delete_handler),
        )
        .route("/media/:filename", get(media_file_handler))
        .route("/healthz", get(healthz_get_handler))
        .route("/metrics/system", get(metrics_system_handler))
        .fallback(get(spa_fallback_handler))
        .layer(CompressionLayer::new())
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("SAMEORIGIN"),
        ))
        .merge(
            Router::new()
                .route("/hls/:pipeline_id", get(hls_playlist_handler))
                .route("/hls/:pipeline_id/master.m3u8", get(hls_master_handler))
                .route("/hls/:pipeline_id/index.m3u8", get(hls_playlist_handler))
                .route(
                    "/hls/:pipeline_id/video/index.m3u8",
                    get(hls_video_playlist_handler),
                )
                .route(
                    "/hls/:pipeline_id/video/init.mp4",
                    get(hls_video_init_handler),
                )
                .route(
                    "/hls/:pipeline_id/video/:segment",
                    get(hls_video_segment_handler),
                )
                .route(
                    "/hls/:pipeline_id/audio/:track_index/index.m3u8",
                    get(hls_audio_playlist_handler),
                )
                .route(
                    "/hls/:pipeline_id/audio/:track_index/init.mp4",
                    get(hls_audio_init_handler),
                )
                .route(
                    "/hls/:pipeline_id/audio/:track_index/:segment",
                    get(hls_audio_segment_handler),
                )
                .route("/hls/:pipeline_id/:segment", get(hls_segment_handler))
                .layer(
                    CorsLayer::new()
                        .allow_origin(AllowOrigin::any())
                        .allow_methods([axum::http::Method::GET, axum::http::Method::OPTIONS])
                        .allow_headers([header::CONTENT_TYPE, header::RANGE]),
                )
                .with_state(state.clone()),
        )
        .with_state(state)
}
