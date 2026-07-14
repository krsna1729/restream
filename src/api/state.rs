//! Shared API transport-state helpers.
//!
//! This module wires together `AppState` and provides the small boundary
//! helpers that handlers reuse for authentication, cookie management, length
//! checks, and common runtime lookups.

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
    AgentService, AuthService, FileIngestService, HealthService, IngestService, LogService,
    MediaLibraryService, OutputService, PipelineService, SettingsService,
};
use crate::config::AppConfig;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::engine::MediaEngine;
use crate::media::security::{IngestSecurityService, RateLimitScope, RateLimitSnapshot};
use crate::media::srt::SrtIngestPolicyStore;

pub const MAX_NAME_LEN: usize = 256;
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_ENCODING_LEN: usize = 512;
pub const MAX_STREAM_KEY_LEN: usize = 256;
pub const MAX_FFMPEG_ARGS_LEN: usize = 4096;
pub const MAX_PASSWORD_LEN: usize = 1024;
pub const MIN_DASHBOARD_PASSWORD_LEN: usize = 12;

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

fn session_cookie_security_attr(secure: bool) -> &'static str {
    if secure { "; Secure" } else { "" }
}

pub struct PortConfig {
    pub rtmp: u16,
    pub srt: u16,
}

pub struct AppStateRuntimeConfig {
    pub ingest_disconnect_grace_ms: u64,
    pub ports: PortConfig,
    pub media_dir: String,
    pub db_path: String,
    pub srt_passphrase: Option<String>,
    pub srt_pbkeylen: i32,
    pub secure_session_cookies: bool,
}

impl Default for AppStateRuntimeConfig {
    fn default() -> Self {
        Self::from(&AppConfig::default())
    }
}

impl From<&AppConfig> for AppStateRuntimeConfig {
    fn from(config: &AppConfig) -> Self {
        Self {
            ingest_disconnect_grace_ms: config.tuning.ingest_disconnect_grace_ms,
            ports: PortConfig {
                rtmp: config.ports.rtmp,
                srt: config.ports.srt,
            },
            media_dir: config.media_dir.clone(),
            db_path: config.db_path.clone(),
            srt_passphrase: config.srt_passphrase.clone(),
            srt_pbkeylen: config.srt_pbkeylen,
            secure_session_cookies: config.secure_session_cookies,
        }
    }
}

pub struct AppState {
    pub db: SqlitePool,
    security: Arc<IngestSecurityService>,
    ingest_policy_store: Arc<SrtIngestPolicyStore>,
    sessions: Arc<TokioRwLock<HashSet<String>>>,
    pub engine: Arc<MediaEngine>,
    pub ingest_disconnect_grace_ms: u64,
    pub ports: PortConfig,
    pub media_dir: String,
    pub db_path: String,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
    pub pipeline_service: PipelineService,
    pub output_service: OutputService,
    pub ingest_service: IngestService,
    pub auth_service: AuthService,
    pub settings_service: SettingsService,
    pub health_service: HealthService,
    pub file_ingest_service: FileIngestService,
    pub media_library_service: MediaLibraryService,
    pub log_service: LogService,
    pub agent_service: AgentService,
    pub alert_tracker: alerts::AlertTracker,
    pub log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
    secure_session_cookies: bool,
    #[cfg(feature = "agent-execution")]
    pub agent_execution: Arc<crate::agent_execution::AgentExecutionStore>,
}

impl AppState {
    /// Wires together the shared API state and the default service wrappers
    /// built from the application's primary SQLite pool.
    pub fn new(
        db: SqlitePool,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
        sessions: Arc<TokioRwLock<HashSet<String>>>,
        engine: Arc<MediaEngine>,
        log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
        runtime: AppStateRuntimeConfig,
    ) -> Self {
        let pipeline_service = PipelineService::new(db.clone());
        let output_service = OutputService::new(db.clone());
        let ingest_service = IngestService::new(db.clone());
        let auth_service = AuthService::new(db.clone());
        let settings_service = SettingsService::new(db.clone());
        let health_service = HealthService::new(db.clone());
        let file_ingest_service = FileIngestService::new(db.clone(), pipeline_service.clone());
        let media_library_service =
            MediaLibraryService::new(db.clone(), pipeline_service.clone(), ingest_service.clone());
        let log_service = LogService::new(db.clone());
        let agent_service = AgentService::new(db.clone());

        Self {
            db,
            security,
            ingest_policy_store,
            sessions,
            engine,
            ingest_disconnect_grace_ms: runtime.ingest_disconnect_grace_ms,
            ports: runtime.ports,
            media_dir: runtime.media_dir,
            db_path: runtime.db_path,
            srt_passphrase: runtime.srt_passphrase,
            srt_pbkeylen: runtime.srt_pbkeylen,
            pipeline_service,
            output_service,
            ingest_service,
            auth_service,
            settings_service,
            health_service,
            file_ingest_service,
            media_library_service,
            log_service,
            agent_service,
            alert_tracker: alerts::AlertTracker::new(),
            log_broadcast,
            secure_session_cookies: runtime.secure_session_cookies,
            #[cfg(feature = "agent-execution")]
            agent_execution: Arc::new(crate::agent_execution::AgentExecutionStore::default()),
        }
    }

    // Session hashes are mirrored in-memory so request auth can stay cheap
    // while SQLite remains the durable source of truth.
    pub async fn add_session_hash(&self, token_hash: String) {
        self.sessions.write().await.insert(token_hash);
    }

    pub async fn remove_session_hash(&self, token_hash: &str) {
        self.sessions.write().await.remove(token_hash);
    }

    pub async fn clear_session_hashes(&self) {
        self.sessions.write().await.clear();
    }

    pub async fn retain_only_session_hash(&self, token_hash: &str) {
        self.sessions
            .write()
            .await
            .retain(|token| token == token_hash);
    }

    pub const fn secure_session_cookies(&self) -> bool {
        self.secure_session_cookies
    }

    pub fn set_secure_session_cookies_for_test(&mut self, secure: bool) {
        self.secure_session_cookies = secure;
    }

    pub fn ingest_security_config(&self) -> IngestSecurityConfig {
        self.security.get_config()
    }

    pub fn update_ingest_security_config(&self, config: IngestSecurityConfig) {
        self.security.update_config(config);
    }

    pub fn record_security_failure(&self, scope: RateLimitScope, ip: &str) {
        self.security.record_failure_for(scope, ip);
    }

    pub fn login_ban_remaining(
        &self,
        scope: RateLimitScope,
        ip: &str,
    ) -> Option<std::time::Duration> {
        self.security.is_ip_banned_for(scope, ip)
    }

    pub fn reset_security_failures(
        &self,
        scope: Option<RateLimitScope>,
        ip: Option<&str>,
    ) -> usize {
        self.security.reset(scope, ip)
    }

    pub fn security_failure_snapshots(&self) -> Vec<RateLimitSnapshot> {
        self.security.snapshots()
    }

    pub async fn settings_snapshot(
        &self,
    ) -> crate::application::services::ApiResult<crate::application::settings::SettingsSnapshot>
    {
        self.settings_service
            .load_snapshot(&self.security, self.engine.backend_policy())
            .await
    }

    pub async fn refresh_srt_ingest_policy_store(
        &self,
    ) -> crate::application::services::ApiResult<()> {
        self.settings_service
            .refresh_srt_ingest_policy_store(
                &self.ingest_policy_store,
                self.srt_passphrase.clone(),
                self.srt_pbkeylen,
            )
            .await
    }

    pub async fn agent_context_catalog(
        &self,
    ) -> crate::application::services::agent_service::AgentContextCatalog {
        self.agent_service
            .load_context_catalog(&self.security)
            .await
    }

    /// Checks the cookie token against the in-memory session cache and then
    /// re-validates it against persisted session state and expiry.
    pub async fn is_authenticated(&self, token: &str) -> bool {
        let token_hash = hash_session_token(token);
        {
            let sessions = self.sessions.read().await;
            if !sessions.contains(&token_hash) {
                return false;
            }
        }

        // The in-memory session set is only a fast cache; SQLite remains the
        // source of truth for expiry and cross-process/session-store recovery.
        let created_at = match self.auth_service.get_session_created_at(&token_hash).await {
            Ok(Some(created_at)) => created_at,
            Ok(None) => {
                self.sessions.write().await.remove(&token_hash);
                return false;
            }
            Err(error) => {
                warn!(err = %error, "failed to validate session against SQLite");
                self.sessions.write().await.remove(&token_hash);
                return false;
            }
        };

        let now = chrono::Utc::now().timestamp_millis();
        let max_age_ms = SESSION_MAX_AGE_SECONDS * 1000;
        if now.saturating_sub(created_at) > max_age_ms {
            self.sessions.write().await.remove(&token_hash);
            if let Err(error) = self.auth_service.delete_session(&token_hash).await {
                warn!(err = %error, "failed to delete expired session from SQLite");
            }
            return false;
        }

        true
    }

    /// Construct an AppState with all default services wired, for testing.
    pub fn test_new(
        db: SqlitePool,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
        sessions: Arc<TokioRwLock<HashSet<String>>>,
        engine: Arc<MediaEngine>,
        log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
    ) -> Self {
        Self::new(
            db,
            security,
            ingest_policy_store,
            sessions,
            engine,
            log_broadcast,
            AppStateRuntimeConfig::default(),
        )
    }

    /// Construct an AppState with default services and an isolated media directory.
    pub fn test_new_with_media_dir(
        db: SqlitePool,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
        sessions: Arc<TokioRwLock<HashSet<String>>>,
        engine: Arc<MediaEngine>,
        log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
        media_dir: String,
    ) -> Self {
        let runtime = AppStateRuntimeConfig {
            media_dir,
            ..AppStateRuntimeConfig::default()
        };
        Self::new(
            db,
            security,
            ingest_policy_store,
            sessions,
            engine,
            log_broadcast,
            runtime,
        )
    }
}

/// Shared length guard for request fields that should fail fast at the HTTP
/// boundary before services or stores see oversized input.
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

/// Extracts the dashboard session token from the raw Cookie header, if present.
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

/// Convenience predicate for handlers that only need an auth yes/no answer.
pub async fn request_is_authenticated(state: &AppState, headers: &HeaderMap) -> bool {
    if let Some(token) = get_session_token_from_headers(headers) {
        state.is_authenticated(&token).await
    } else {
        false
    }
}

/// Returns an HTTP response when the caller is not authenticated, letting
/// handlers keep their transport contract local and linear.
pub async fn require_authenticated(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if request_is_authenticated(state, headers).await {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "Unauthorized").into_response())
    }
}

/// HLS currently shares the same session-based access policy as the rest of
/// the authenticated API surface.
pub async fn require_hls_access(
    state: &AppState,
    headers: &HeaderMap,
    _uri: &axum::http::Uri,
) -> Option<Response> {
    require_authenticated(state, headers).await
}

/// Builds one dashboard session cookie with the repository's shared security
/// attributes so login/logout flows stay consistent.
pub fn make_session_cookie(token: &str, max_age: i64, secure: bool) -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age={}",
        SESSION_COOKIE_NAME, token, max_age
    ) + session_cookie_security_attr(secure)
}

/// Clears the dashboard session cookie using the same attributes as creation.
pub fn clear_session_cookie(secure: bool) -> String {
    format!(
        "{}={}; HttpOnly; Path=/; SameSite=Strict; Max-Age=0",
        SESSION_COOKIE_NAME, ""
    ) + session_cookie_security_attr(secure)
}

/// Best-effort startup/runtime refresh for the in-memory libsrt policy store.
pub async fn refresh_srt_ingest_policy_store(state: &AppState) {
    if let Err(error) = state.refresh_srt_ingest_policy_store().await {
        warn!(err = %error, "failed to refresh SRT ingest policy store");
    }
}

/// Small hex encoder used by auth/session helpers that expose hash values as
/// lowercase hex strings.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Hashes a raw session token before it is stored or looked up in persistence.
pub fn hash_session_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    to_hex(&digest)
}

/// Loads the persisted recording-enabled flags for the requested pipelines so
/// runtime views can merge desired recording state with live snapshots.
pub async fn recording_enabled_map(
    state: &AppState,
    pipeline_ids: &[String],
) -> std::collections::HashMap<String, bool> {
    state
        .settings_service
        .recording_enabled_map(pipeline_ids)
        .await
}

#[cfg(test)]
mod runtime_config_tests {
    use super::*;

    #[test]
    fn defaults_are_derived_from_app_config() {
        let app_config = AppConfig::default();
        let runtime = AppStateRuntimeConfig::default();

        assert_eq!(runtime.media_dir, app_config.media_dir);
        assert_eq!(runtime.db_path, app_config.db_path);
        assert_eq!(runtime.ports.rtmp, app_config.ports.rtmp);
        assert_eq!(runtime.ports.srt, app_config.ports.srt);
        assert_eq!(
            runtime.ingest_disconnect_grace_ms,
            app_config.tuning.ingest_disconnect_grace_ms
        );
    }

    #[test]
    fn session_cookie_security_attr_matches_flag() {
        assert_eq!(session_cookie_security_attr(true), "; Secure");
        assert_eq!(session_cookie_security_attr(false), "");
    }

    #[test]
    fn make_and_clear_session_cookie_share_security_policy() {
        let secure_cookie = make_session_cookie("token", 60, true);
        let cleared_cookie = clear_session_cookie(true);

        assert!(secure_cookie.contains("; Secure"));
        assert!(cleared_cookie.contains("; Secure"));
        assert!(cleared_cookie.contains("Max-Age=0"));
    }
}
