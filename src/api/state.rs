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
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tracing::warn;

use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::recirculation::RecirculationService;
use crate::application::services::{
    AgentService, AuthService, FileIngestService, IngestService, MediaLibraryService,
    PipelineService, SettingsService,
};
use crate::config::AppConfig;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_engine::engine::MediaEngine;
use restream_engine::security::{IngestSecurityService, RateLimitScope, RateLimitSnapshot};
use restream_engine::srt::SrtIngestPolicyStore;
use restream_health::alerts;

use super::AppServices;

pub const MAX_NAME_LEN: usize = 256;
pub const MAX_URL_LEN: usize = 2048;
pub const MAX_OUTPUT_CONFIG_LEN: usize = 512;
pub const MAX_STREAM_KEY_LEN: usize = 256;
pub const MAX_PASSWORD_LEN: usize = 1024;
pub const MIN_DASHBOARD_PASSWORD_LEN: usize = 12;

pub const SESSION_COOKIE_NAME: &str = "session";
pub const SESSION_MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Applies the API-level default and lower bound for live-optimized GOP sizing
/// before the value reaches the service layer.
pub(crate) fn sanitize_target_gop_seconds(value: Option<u32>) -> u32 {
    value
        .unwrap_or(restream_domain::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS)
        .max(1)
}
pub const BOOTSTRAP_PASSWORD_PROMPT_META_KEY: &str = "dashboardPasswordPrompt";
pub const DEFAULT_INGEST_HOST: &str = "localhost";

fn session_cookie_security_attr(secure: bool) -> &'static str {
    if secure { "; Secure" } else { "" }
}

pub struct PortConfig {
    pub http: u16,
    pub rtmp: u16,
    pub srt: u16,
    /// The MoQ ingest listener's UDP port; `None` while MoQ ingest is off.
    pub moq: Option<u16>,
}

impl PortConfig {
    pub fn server_ports(&self) -> crate::config::ServerPorts {
        crate::config::ServerPorts {
            http: self.http,
            rtmp: self.rtmp,
            srt: self.srt,
            moq: self.moq,
        }
    }
}

pub struct AppStateRuntimeConfig {
    pub ingest_disconnect_grace_ms: u64,
    pub ports: PortConfig,
    pub media_dir: String,
    pub db_path: String,
    pub srt_passphrase: Option<String>,
    pub srt_pbkeylen: i32,
    pub secure_session_cookies: bool,
    /// See [`AppConfig::public_url`].
    pub public_url: Option<String>,
    /// See [`AppConfig::push_subject`].
    pub push_subject: String,
    /// Hex SHA-256 of the MoQ listener's leaf certificate, read once at
    /// startup as the listener reads it: viewer grants carry it so a browser
    /// can pin a short-lived self-signed certificate.
    pub moq_certificate_sha256: Option<String>,
    /// The process configuration, for readers of settings the engine does
    /// not own (tuning, Tokio runtime, capacity calibration).
    pub config: Arc<AppConfig>,
}

impl Default for AppStateRuntimeConfig {
    fn default() -> Self {
        Self::from(Arc::new(AppConfig::default()))
    }
}

impl From<Arc<AppConfig>> for AppStateRuntimeConfig {
    fn from(config: Arc<AppConfig>) -> Self {
        Self {
            ingest_disconnect_grace_ms: config.tuning.ingest_disconnect_grace_ms,
            ports: PortConfig {
                http: config.ports.http,
                rtmp: config.ports.rtmp,
                srt: config.ports.srt,
                moq: config.ports.moq,
            },
            media_dir: config.media_dir.clone(),
            db_path: config.db_path.clone(),
            srt_passphrase: config.srt_passphrase.clone(),
            srt_pbkeylen: config.srt_pbkeylen,
            secure_session_cookies: config.secure_session_cookies,
            public_url: config.public_url.clone(),
            push_subject: config.push_subject.clone(),
            moq_certificate_sha256: config
                .engine
                .moq_ingest
                .config()
                .and_then(|moq| std::fs::read(&moq.tls_cert_path).ok())
                .and_then(|pem| restream_moq::tls::leaf_sha256_hex(&pem)),
            config,
        }
    }
}

// Bound work that outlives its HTTP caller, not tenant accounting.
const API_MUTATION_PERMITS: usize = 64;

pub struct AppState {
    security: Arc<IngestSecurityService>,
    ingest_policy_store: Arc<SrtIngestPolicyStore>,
    sessions: Arc<TokioRwLock<HashSet<String>>>,
    mutation_admission: Arc<tokio::sync::Semaphore>,
    pub engine: Arc<MediaEngine>,
    /// Main-pool reads are deadline bounded; writes run to their outcome.
    pub db: crate::db::DeadlineDb,
    pub ingest_disconnect_grace_ms: u64,
    pub ports: PortConfig,
    /// See [`AppStateRuntimeConfig::moq_certificate_sha256`].
    pub moq_certificate_sha256: Option<String>,
    /// See [`AppStateRuntimeConfig::config`].
    pub config: Arc<AppConfig>,
    pub media_dir: String,
    pub db_path: String,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
    pub pipeline_service: PipelineService,
    pub pipeline_input_service: PipelineInputService,
    recirculation_service: RecirculationService,
    ingest_service: IngestService,
    pub auth_service: AuthService,
    pub team_service: crate::application::services::TeamService,
    pub notify_service: crate::application::services::NotifyService,
    pub settings_service: SettingsService,
    pub file_ingest_service: FileIngestService,
    pub media_library_service: MediaLibraryService,
    pub agent_service: AgentService,
    pub alert_tracker: alerts::AlertTracker,
    /// Mutation handlers persist, then ask the reconciler to converge here.
    pub reconcile: Arc<crate::application::topology::ReconcileRequests>,
    /// The revisioned runtime state behind `/api/v1/state`.
    pub runtime_state: Arc<crate::application::runtime_state::hub::StateHub>,
    /// The fleet's sampled signals behind `/api/v1/fleet/history`.
    fleet_history: Arc<crate::application::fleet_history::FleetHistory>,
    /// Each pipeline's recorded health behind
    /// `/api/v1/pipelines/:id/flight-recorder`.
    flight_recorder: Arc<crate::application::flight_recorder::FlightRecorder>,
    /// Public watch links behind `/watch/:token` and token HLS reads.
    share_links: Arc<crate::application::share::ShareLinks>,
    log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
    secure_session_cookies: bool,
    #[cfg(feature = "agent-execution")]
    pub agent_execution: Arc<restream_agent::execution::AgentExecutionStore>,
}

impl AppState {
    /// Wires the complete API composition, sharing its database executor with
    /// the services so every main-pool writer uses the same permit budget.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        services: AppServices,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
        sessions: Arc<TokioRwLock<HashSet<String>>>,
        engine: Arc<MediaEngine>,
        log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
        runtime: AppStateRuntimeConfig,
    ) -> Self {
        let AppServices {
            db,
            pipeline_service,
            pipeline_input_service,
            recirculation_service,
            ingest_service,
            auth_service,
            team_service,
            notify_service,
            settings_service,
            file_ingest_service,
            media_library_service,
            agent_service,
        } = services;
        notify_service.configure(
            crate::application::services::notify_service::NotifyContext {
                public_url: runtime.public_url.clone(),
                push_subject: runtime.push_subject.clone(),
            },
        );

        let share_links = Arc::new(crate::application::share::ShareLinks::new(db.clone()));
        let runtime_state = Arc::new(crate::application::runtime_state::hub::StateHub::new(
            runtime.ports.server_ports(),
            runtime.config.tuning.output_max_retries,
        ));
        Self {
            security,
            ingest_policy_store,
            sessions,
            mutation_admission: Arc::new(tokio::sync::Semaphore::new(API_MUTATION_PERMITS)),
            engine,
            db,
            ingest_disconnect_grace_ms: runtime.ingest_disconnect_grace_ms,
            ports: runtime.ports,
            moq_certificate_sha256: runtime.moq_certificate_sha256,
            config: runtime.config,
            media_dir: runtime.media_dir,
            db_path: runtime.db_path,
            srt_passphrase: runtime.srt_passphrase,
            srt_pbkeylen: runtime.srt_pbkeylen,
            pipeline_service,
            pipeline_input_service,
            recirculation_service,
            ingest_service,
            auth_service,
            team_service,
            notify_service,
            settings_service,
            file_ingest_service,
            media_library_service,
            agent_service,
            alert_tracker: alerts::AlertTracker::new(),
            reconcile: Arc::new(crate::application::topology::ReconcileRequests::new()),
            runtime_state,
            fleet_history: Arc::new(crate::application::fleet_history::FleetHistory::new()),
            flight_recorder: Arc::new(crate::application::flight_recorder::FlightRecorder::new()),
            share_links,
            log_broadcast,
            secure_session_cookies: runtime.secure_session_cookies,
            #[cfg(feature = "agent-execution")]
            agent_execution: Arc::new(restream_agent::execution::AgentExecutionStore::default()),
        }
    }

    /// Subscribe this request to the live log stream.
    pub(crate) fn subscribe_logs(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::logging::LogBroadcast> {
        self.log_broadcast.subscribe()
    }

    /// The fleet history samples in `[since_ms, until_ms]`.
    pub(crate) fn fleet_history_window(
        &self,
        since_ms: u64,
        until_ms: u64,
    ) -> crate::application::fleet_history::FleetHistoryWindow {
        self.fleet_history.window(since_ms, until_ms)
    }

    /// The shared history handle used by the fleet sampler task.
    pub(crate) fn fleet_history_sampler_handle(
        &self,
    ) -> Arc<crate::application::fleet_history::FleetHistory> {
        Arc::clone(&self.fleet_history)
    }

    /// The pipeline's flight trace over `[since_ms, until_ms]`.
    pub(crate) async fn flight_recorder_trace(
        &self,
        pipeline_id: &str,
        since_ms: u64,
        until_ms: u64,
        max_points: usize,
    ) -> Option<crate::application::flight_recorder::FlightTrace> {
        crate::application::flight_recorder::trace_window(
            &self.flight_recorder,
            &self.db,
            pipeline_id,
            since_ms,
            until_ms,
            max_points,
        )
        .await
    }

    /// Destination anomalies the alert evaluator reads.
    pub(crate) fn flight_recorder_anomalies(
        &self,
    ) -> Vec<restream_health::destination_baselines::Anomaly> {
        self.flight_recorder.baselines.anomalies()
    }

    /// The shared recorder handle used by the flight-recorder task.
    pub(crate) fn flight_recorder_sampler_handle(
        &self,
    ) -> Arc<crate::application::flight_recorder::FlightRecorder> {
        Arc::clone(&self.flight_recorder)
    }

    /// Lists the persisted file ingests.
    pub(crate) async fn list_ingests(
        &self,
    ) -> crate::application::services::ServiceResult<Vec<restream_domain::models::Ingest>> {
        self.ingest_service.list_ingests().await
    }

    /// Creates one persisted file ingest.
    pub(crate) async fn create_ingest(
        &self,
        id: &str,
        payload: &crate::api::NormalizedIngestPayload,
    ) -> crate::application::services::ServiceResult<restream_domain::models::Ingest> {
        self.ingest_service
            .create_ingest(
                id,
                &payload.filename,
                &payload.stream_key,
                payload.loop_flag,
                &payload.start_time,
                payload.live_optimized,
                payload.target_gop_seconds,
            )
            .await
    }

    /// Updates one persisted file ingest.
    pub(crate) async fn update_ingest(
        &self,
        id: &str,
        payload: &crate::api::NormalizedIngestPayload,
    ) -> crate::application::services::ServiceResult<restream_domain::models::Ingest> {
        self.ingest_service
            .update_ingest(
                id,
                &payload.filename,
                &payload.stream_key,
                payload.loop_flag,
                &payload.start_time,
                payload.live_optimized,
                payload.target_gop_seconds,
            )
            .await
    }

    /// Validates a recirculation target before an output is reserved.
    pub(crate) async fn validate_recirculation_candidate(
        &self,
        source_pipeline_id: &str,
        target: &restream_domain::domain::output_spec::RecirculationTarget,
    ) -> crate::application::services::ServiceResult<()> {
        self.recirculation_service
            .validate_output_candidate(source_pipeline_id, target)
            .await
    }

    /// Lists the watch links of one pipeline.
    pub(crate) async fn share_links_for_pipeline(
        &self,
        pipeline_id: &str,
    ) -> crate::application::services::ServiceResult<Vec<crate::application::share::ShareLink>>
    {
        self.share_links.for_pipeline(pipeline_id).await
    }

    /// Creates one watch link and returns it.
    pub(crate) async fn create_share_link(
        &self,
        id: String,
        token: String,
        pipeline_id: &str,
        label: &str,
        lifetime_hours: Option<i64>,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<crate::application::share::ShareLink> {
        self.share_links
            .create(id, token, pipeline_id, label, lifetime_hours, now_ms)
            .await
    }

    /// Revokes one watch link.
    pub(crate) async fn revoke_share_link(
        &self,
        id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.share_links.revoke(id).await
    }

    /// Resolves a live watch credential.
    pub(crate) async fn resolve_share_link(
        &self,
        token: &str,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<Option<crate::application::share::ShareLink>>
    {
        self.share_links.resolve(token, now_ms).await
    }

    /// Records one best-effort watch view.
    pub(crate) fn count_share_link_view(
        &self,
        id: String,
    ) -> Option<tokio::task::JoinHandle<crate::application::services::ServiceResult<()>>> {
        self.share_links.count_view(id)
    }

    /// Drops every watch link of a deleted pipeline.
    pub(crate) async fn forget_pipeline_share_links(&self, pipeline_id: &str) {
        self.share_links.forget_pipeline(pipeline_id).await;
    }

    /// Returns whether a share credential admits one HLS pipeline.
    pub(crate) async fn share_link_admits(
        &self,
        token: &str,
        pipeline_id: &str,
        now_ms: i64,
    ) -> crate::application::services::ServiceResult<bool> {
        self.share_links.admits(token, pipeline_id, now_ms).await
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
    ) -> crate::application::services::ServiceResult<crate::application::settings::SettingsSnapshot>
    {
        self.settings_service.load_snapshot(&self.security).await
    }

    pub async fn refresh_srt_ingest_policy_store(
        &self,
    ) -> crate::application::services::ServiceResult<()> {
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
    pub async fn validate_session(
        &self,
        token: &str,
    ) -> crate::application::services::ServiceResult<bool> {
        let token_hash = hash_session_token(token);
        {
            let sessions = self.sessions.read().await;
            if !sessions.contains(&token_hash) {
                return Ok(false);
            }
        }

        // The in-memory session set is only a fast cache; SQLite remains the
        // source of truth for expiry and cross-process/session-store recovery.
        let created_at = match self.auth_service.get_session_created_at(&token_hash).await {
            Ok(Some(created_at)) => created_at,
            Ok(None) => {
                self.sessions.write().await.remove(&token_hash);
                return Ok(false);
            }
            Err(error) => {
                warn!(err = %error, "failed to validate session against SQLite");
                return Err(error);
            }
        };

        let now = jiff::Timestamp::now().as_millisecond();
        let max_age_ms = SESSION_MAX_AGE_SECONDS * 1000;
        if now.saturating_sub(created_at) > max_age_ms {
            self.sessions.write().await.remove(&token_hash);
            // Housekeeping prunes expired rows; an auth read must not wait for
            // a cleanup write before rejecting a known-expired credential.
            return Ok(false);
        }

        Ok(true)
    }

    /// Admit before detaching. Rejected work has never polled the handler;
    /// admitted work owns its slot through reconciliation and auditing.
    pub(in crate::api) async fn complete_mutation<Fut>(
        self: Arc<Self>,
        op: impl FnOnce(Arc<Self>) -> Fut + Send + 'static,
    ) -> Response
    where
        Fut: std::future::Future<Output = Response> + Send + 'static,
    {
        let Ok(permit) = self.mutation_admission.clone().try_acquire_owned() else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": "Too many pending mutations" })),
            )
                .into_response();
        };
        match tokio::spawn(async move {
            let _permit = permit;
            op(self).await
        })
        .await
        {
            Ok(response) => response,
            Err(error) => {
                warn!(err = %error, "API mutation task failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "Request failed" })),
                )
                    .into_response()
            }
        }
    }

    /// Construct an AppState with all default services wired, for testing.
    ///
    /// The service graph supplies the same executor its adapters use.
    pub fn test_new(
        services: AppServices,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
        sessions: Arc<TokioRwLock<HashSet<String>>>,
        engine: Arc<MediaEngine>,
        log_broadcast: tokio::sync::broadcast::Sender<crate::logging::LogBroadcast>,
    ) -> Self {
        Self::new(
            services,
            security,
            ingest_policy_store,
            sessions,
            engine,
            log_broadcast,
            AppStateRuntimeConfig::default(),
        )
    }

    /// Construct an AppState with default services and an isolated media directory.
    #[allow(clippy::too_many_arguments)]
    pub fn test_new_with_media_dir(
        services: AppServices,
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
            services,
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

pub fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "Unauthorized" })),
    )
        .into_response()
}

pub fn auth_unavailable_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "Authentication temporarily unavailable" })),
    )
        .into_response()
}

/// A storage failure is not an invalid credential and must not evict it.
pub async fn require_authenticated_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, Response> {
    let token = get_session_token_from_headers(headers).ok_or_else(unauthorized_response)?;
    match state.validate_session(&token).await {
        Ok(true) => Ok(token),
        Ok(false) => Err(unauthorized_response()),
        Err(_) => Err(auth_unavailable_response()),
    }
}

pub async fn require_authenticated(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    require_authenticated_token(state, headers).await.err()
}

/// HLS reads need a dashboard session, or a live share link's token (`?t=`)
/// for the pipeline in the path.
pub async fn require_hls_access(
    state: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
) -> Option<Response> {
    let session_error = match require_authenticated_token(state, headers).await {
        Ok(_) => return None,
        Err(response) => response,
    };
    // An independent share credential can still admit its own pipeline.
    if let (Some(pipeline_id), Some(token)) = (
        crate::application::share::hls_pipeline_of(uri.path()),
        crate::application::share::token_of(uri.query()),
    ) {
        match state
            .share_link_admits(token, pipeline_id, jiff::Timestamp::now().as_millisecond())
            .await
        {
            Ok(true) => return None,
            Ok(false) => {}
            Err(_) => return Some(auth_unavailable_response()),
        }
    }
    Some(session_error)
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

/// Best-effort startup/runtime refresh for the in-memory SRT ingest policy store.
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
/// runtime views can merge desired recording state with live snapshots; an
/// unreadable flag keeps the pipeline's current recording state.
pub async fn recording_enabled_map(
    state: &AppState,
    pipeline_ids: &[String],
) -> std::collections::HashMap<String, bool> {
    state
        .settings_service
        .recording_enabled_map(&state.engine, pipeline_ids)
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

#[cfg(test)]
mod mutation_admission_tests {
    use super::*;
    use tower::ServiceExt;

    #[tokio::test]
    async fn full_mutation_admission_rejects_without_a_later_write_and_recovers() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let hash = hash_session_token("admission-token");
        crate::db::create_session(&pool, &hash, jiff::Timestamp::now().as_millisecond())
            .await
            .unwrap();
        let mut state = AppState::test_new(
            crate::infrastructure::service_wiring::ControlPlane::on_pool(&pool).services,
            Arc::new(IngestSecurityService::new(
                restream_domain::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG,
            )),
            Arc::new(SrtIngestPolicyStore::new(
                restream_domain::domain::srt_ingest::SrtGlobalIngestConfig::default(),
                &[],
            )),
            Arc::new(TokioRwLock::new(HashSet::from([hash.clone()]))),
            Arc::new(MediaEngine::new()),
            tokio::sync::broadcast::channel(8).0,
        );
        state.mutation_admission = Arc::new(tokio::sync::Semaphore::new(1));
        let held = state
            .mutation_admission
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let state = Arc::new(state);
        let app = super::super::create_router(state.clone());
        let request = || {
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/pipelines")
                .header(header::COOKIE, "session=admission-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    r#"{"name":"admission","inputSource":"obs"}"#,
                ))
                .unwrap()
        };
        let rejected = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(crate::db::list_pipelines(&pool).await.unwrap().is_empty());
        let principal = state.team_service.principal(&hash).await.unwrap().unwrap();
        let principal = state
            .team_service
            .set_view_as(
                &hash,
                &principal,
                Some(restream_domain::domain::role::Role::Viewer),
            )
            .await
            .unwrap();
        let denied = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            app.clone().oneshot(request()),
        )
        .await
        .expect("denied-role auditing must also respect workflow admission")
        .unwrap();
        assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(state.team_service.audit(20, None).await.unwrap().is_empty());
        drop(held);
        // Draining admission proves rejection did not enqueue a hidden owner.
        let drained = state
            .mutation_admission
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        assert!(crate::db::list_pipelines(&pool).await.unwrap().is_empty());
        assert!(state.team_service.audit(20, None).await.unwrap().is_empty());
        drop(drained);
        state
            .team_service
            .set_view_as(&hash, &principal, None)
            .await
            .unwrap();
        let accepted = app.oneshot(request()).await.unwrap();
        assert!(accepted.status().is_success());
        assert_eq!(
            crate::db::list_pipelines(&pool)
                .await
                .unwrap()
                .into_iter()
                .map(|pipeline| pipeline.name)
                .collect::<Vec<_>>(),
            ["admission"]
        );
    }
}
