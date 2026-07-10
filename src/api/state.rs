use axum::{
    Json,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::RwLock as TokioRwLock;
use tracing::warn;

use crate::alerts;
use crate::application::services::{
    AuthService, FileIngestService, HealthService, IngestService, LogService, MediaLibraryService,
    OutputService, PipelineService, RuntimeViewService, SettingsService,
};
use crate::media::engine::MediaEngine;
use crate::media::security::IngestSecurityService;
use crate::media::srt::SrtIngestPolicyStore;

pub const MAX_NAME_LEN: usize = 256;
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_ENCODING_LEN: usize = 512;
pub const MAX_STREAM_KEY_LEN: usize = 256;
pub const MAX_FFMPEG_ARGS_LEN: usize = 4096;
pub const MAX_PASSWORD_LEN: usize = 1024;

#[derive(Clone, Copy)]
pub struct EngineCpuSample {
    pub total_ticks: u64,
    pub restream_ticks: u64,
    pub external_ffmpeg_ticks: u64,
}

pub static ENGINE_CPU_SAMPLE: OnceLock<Mutex<Option<EngineCpuSample>>> = OnceLock::new();

pub const SESSION_COOKIE_NAME: &str = "session";
pub const SESSION_MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const PASSWORD_META_KEY: &str = "dashboardPasswordHash";
pub const BOOTSTRAP_PASSWORD_PROMPT_META_KEY: &str = "dashboardPasswordPrompt";
pub const DEFAULT_INGEST_HOST: &str = "localhost";

pub struct PortConfig {
    pub rtmp: u16,
    pub srt: u16,
}

pub struct AppState {
    pub db: SqlitePool,
    pub security: Arc<IngestSecurityService>,
    pub ingest_policy_store: Arc<SrtIngestPolicyStore>,
    pub sessions: Arc<TokioRwLock<HashSet<String>>>,
    pub engine: Arc<MediaEngine>,
    pub ingest_disconnect_grace_ms: u64,
    pub ports: PortConfig,
    pub media_dir: String,
    pub db_path: String,
    pub srt_passphrase: Option<String>,
    pub srt_pbkeylen: i32,
    pub pipeline_service: PipelineService,
    pub output_service: OutputService,
    pub ingest_service: IngestService,
    pub auth_service: AuthService,
    pub settings_service: SettingsService,
    pub health_service: HealthService,
    pub runtime_view_service: RuntimeViewService,
    pub file_ingest_service: FileIngestService,
    pub media_library_service: MediaLibraryService,
    pub log_service: LogService,
    pub alert_tracker: alerts::AlertTracker,
    pub log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
    #[cfg(feature = "agent-execution")]
    pub agent_execution: Arc<crate::agent_execution::AgentExecutionStore>,
}

impl AppState {
    pub async fn is_authenticated(&self, token: &str) -> bool {
        let token_hash = hash_session_token(token);
        let sessions = self.sessions.read().await;
        sessions.contains(&token_hash)
    }

    /// Construct an AppState with all default services wired, for testing.
    pub fn test_new(
        db: SqlitePool,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
        sessions: Arc<TokioRwLock<HashSet<String>>>,
        engine: Arc<MediaEngine>,
        log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
        media_dir: String,
    ) -> Self {
        let pipeline_service = PipelineService::new(db.clone());
        let output_service = OutputService::new(db.clone());
        let ingest_service = IngestService::new(db.clone());
        let auth_service = AuthService::new(db.clone());
        let settings_service = SettingsService::new(db.clone());
        let health_service = HealthService::new(db.clone());
        let runtime_view_service = RuntimeViewService::new();
        let file_ingest_service = FileIngestService::new(db.clone(), pipeline_service.clone());
        let media_library_service =
            MediaLibraryService::new(db.clone(), pipeline_service.clone(), ingest_service.clone());
        let log_service = LogService::new(db.clone());

        Self {
            db,
            security,
            ingest_policy_store,
            sessions,
            engine,
            ingest_disconnect_grace_ms: 5000,
            ports: PortConfig {
                rtmp: 1935,
                srt: 10080,
            },
            media_dir,
            db_path: "data.db".to_string(),
            srt_passphrase: None,
            srt_pbkeylen: 16,
            pipeline_service,
            output_service,
            ingest_service,
            auth_service,
            settings_service,
            health_service,
            runtime_view_service,
            file_ingest_service,
            media_library_service,
            log_service,
            alert_tracker: alerts::AlertTracker::new(),
            log_broadcast,
            #[cfg(feature = "agent-execution")]
            agent_execution: Arc::new(crate::agent_execution::AgentExecutionStore::default()),
        }
    }
}

pub fn check_field_len(field: &str, s: &str, max: usize) -> Option<Response> {
    if s.len() > max {
        Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("{} exceeds maximum length of {} bytes", field, max)
                })),
            )
                .into_response(),
        )
    } else {
        None
    }
}

pub fn get_session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for cookie in cookie_header.split(';') {
        let mut parts = cookie.trim().splitn(2, '=');
        let name = parts.next()?;
        if name == SESSION_COOKIE_NAME {
            return parts.next().map(|s| s.to_string());
        }
    }
    None
}

pub async fn request_is_authenticated(state: &AppState, headers: &HeaderMap) -> bool {
    if let Some(token) = get_session_token_from_headers(headers) {
        state.is_authenticated(&token).await
    } else {
        false
    }
}

pub async fn require_authenticated(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if request_is_authenticated(state, headers).await {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "Unauthorized").into_response())
    }
}

pub async fn require_hls_access(
    state: &AppState,
    headers: &HeaderMap,
    _uri: &axum::http::Uri,
) -> Option<Response> {
    require_authenticated(state, headers).await
}

pub fn make_session_cookie(token: &str, max_age: i64) -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age={}",
        SESSION_COOKIE_NAME, token, max_age
    )
}

pub fn clear_session_cookie() -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=0",
        SESSION_COOKIE_NAME, ""
    )
}

pub async fn refresh_srt_ingest_policy_store(state: &AppState) {
    if let Err(error) = state
        .settings_service
        .refresh_srt_ingest_policy_store(
            &state.ingest_policy_store,
            state.srt_passphrase.clone(),
            state.srt_pbkeylen,
        )
        .await
    {
        warn!(err = %error, "failed to refresh SRT ingest policy store");
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn hash_session_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    to_hex(&digest)
}

pub async fn recording_enabled_map(
    state: &AppState,
    pipeline_ids: &[String],
) -> std::collections::HashMap<String, bool> {
    state
        .settings_service
        .recording_enabled_map(pipeline_ids)
        .await
}
