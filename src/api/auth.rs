use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{info, warn};

use super::state::{
    AppState, BOOTSTRAP_PASSWORD_PROMPT_META_KEY, MAX_PASSWORD_LEN, MIN_DASHBOARD_PASSWORD_LEN,
    SESSION_MAX_AGE_SECONDS, check_field_len, clear_session_cookie, get_session_token_from_headers,
    hash_session_token, make_session_cookie, to_hex,
};
use crate::application::services::AuthService;
use crate::media::security::RateLimitScope;

#[derive(Deserialize)]
pub struct LoginPayload {
    pub password: Option<String>,
}

#[derive(Deserialize)]
pub struct ChangePasswordPayload {
    pub current_password: Option<String>,
    pub new_password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitResetPayload {
    pub scope: Option<String>,
    pub ip: Option<String>,
}

const BOOTSTRAP_PROMPT_PENDING: &str = "pending";
const BOOTSTRAP_PROMPT_DISMISSED: &str = "dismissed";

fn select_initial_admin_password(env_password: Option<String>) -> (String, bool) {
    match env_password {
        Some(value) if !value.is_empty() => (value, false),
        _ => (generate_bootstrap_password(), true),
    }
}

fn generate_bootstrap_password() -> String {
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    to_hex(&bytes)
}

fn write_bootstrap_password_file(path: &Path, password: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        writeln!(file, "{password}")?;
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, format!("{password}\n"))?;
    }

    Ok(())
}

pub fn hash_password(password: &str) -> String {
    use rand::RngExt;
    use scrypt::Params;

    let mut salt_bytes = [0u8; 16];
    rand::rng().fill(&mut salt_bytes);
    let salt = to_hex(&salt_bytes);

    let mut hash_bytes = [0u8; 32];
    let params = Params::new(14, 8, 1, 32).unwrap();
    scrypt::scrypt(
        password.as_bytes(),
        salt.as_bytes(),
        &params,
        &mut hash_bytes,
    )
    .unwrap();
    let hash = to_hex(&hash_bytes);
    format!("{}:{}", salt, hash)
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    use scrypt::Params;

    let parts: Vec<&str> = stored.split(':').collect();
    if parts.len() != 2 {
        return false;
    }
    let salt = parts[0];
    let stored_hash = parts[1];

    let mut new_hash = [0u8; 32];
    let params = Params::new(14, 8, 1, 32).unwrap();
    if scrypt::scrypt(password.as_bytes(), salt.as_bytes(), &params, &mut new_hash).is_err() {
        return false;
    }
    let hex_hash = to_hex(&new_hash);
    constant_time_eq(hex_hash.as_bytes(), stored_hash.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn validate_new_dashboard_password(password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("New password cannot be empty".to_string());
    }
    if password.len() < MIN_DASHBOARD_PASSWORD_LEN {
        return Err(format!(
            "New password must be at least {MIN_DASHBOARD_PASSWORD_LEN} characters"
        ));
    }
    Ok(())
}

pub async fn initialize_auth(
    db_pool: &sqlx::SqlitePool,
    sessions_set: &TokioRwLock<std::collections::HashSet<String>>,
) {
    initialize_auth_with_bootstrap_file(db_pool, sessions_set, None, None).await;
}

pub async fn initialize_auth_for_test(
    db_pool: &sqlx::SqlitePool,
    sessions_set: &TokioRwLock<std::collections::HashSet<String>>,
    password: &str,
) {
    let auth_service = AuthService::new(db_pool.clone());
    auth_service
        .set_password_hash(&hash_password(password))
        .await
        .expect("test auth password should persist");
    let _ = auth_service
        .set_meta(
            BOOTSTRAP_PASSWORD_PROMPT_META_KEY,
            BOOTSTRAP_PROMPT_DISMISSED,
        )
        .await;
    initialize_auth(db_pool, sessions_set).await;
}

pub async fn initialize_auth_with_bootstrap_file(
    db_pool: &sqlx::SqlitePool,
    sessions_set: &TokioRwLock<std::collections::HashSet<String>>,
    bootstrap_password_file: Option<&Path>,
    initial_admin_password: Option<&str>,
) {
    let auth_service = AuthService::new(db_pool.clone());
    if matches!(auth_service.get_password_hash().await, Ok(None)) {
        let (password, generated) =
            select_initial_admin_password(initial_admin_password.map(str::to_string));
        let admin_hash = hash_password(&password);
        if let Err(error) = auth_service.ensure_password_hash(&admin_hash).await {
            panic!("failed to initialize dashboard password: {error}");
        }
        if generated {
            if let Some(path) = bootstrap_password_file {
                write_bootstrap_password_file(path, &password)
                    .unwrap_or_else(|error| panic!("failed to write bootstrap password: {error}"));
                info!(
                    path = %path.display(),
                    "generated initial dashboard password; read it from this local file"
                );
            } else {
                info!(
                    password = %password,
                    "generated initial dashboard password"
                );
            }
            let _ = auth_service
                .set_meta(BOOTSTRAP_PASSWORD_PROMPT_META_KEY, BOOTSTRAP_PROMPT_PENDING)
                .await;
        } else {
            let _ = auth_service
                .set_meta(
                    BOOTSTRAP_PASSWORD_PROMPT_META_KEY,
                    BOOTSTRAP_PROMPT_DISMISSED,
                )
                .await;
        }
    }

    let _ = auth_service
        .prune_expired_sessions(30 * 24 * 60 * 60 * 1000)
        .await;
    match auth_service.list_sessions().await {
        Ok(tokens) => {
            let mut sessions = sessions_set.write().await;
            for token in tokens {
                sessions.insert(token);
            }
        }
        Err(e) => {
            warn!(err = %e, "Failed to load active sessions from SQLite");
        }
    }
}

pub async fn login_post_handler(
    State(state): State<Arc<AppState>>,
    connect_info: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    Json(payload): Json<LoginPayload>,
) -> impl IntoResponse {
    let client_ip = connect_info
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    if let Some(ban_remaining) =
        state.login_ban_remaining(RateLimitScope::DashboardLogin, &client_ip)
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": format!("Too many failed attempts. Try again in {} seconds.",
                                 ban_remaining.as_secs())
            })),
        )
            .into_response();
    }

    let password = payload.password.unwrap_or_default();
    if let Some(r) = check_field_len("password", &password, MAX_PASSWORD_LEN) {
        return r;
    }
    let stored_hash = match state.auth_service.get_password_hash().await {
        Ok(Some(hash)) => hash,
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Incorrect password"})),
            )
                .into_response();
        }
    };

    let verified = tokio::task::spawn_blocking(move || verify_password(&password, &stored_hash))
        .await
        .unwrap_or(false);

    if !verified {
        state.record_security_failure(RateLimitScope::DashboardLogin, &client_ip);
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Incorrect password"})),
        )
            .into_response();
    }
    use rand::RngExt;
    let mut token_bytes = [0u8; 32];
    rand::rng().fill(&mut token_bytes);
    let token = to_hex(&token_bytes);
    let token_hash = hash_session_token(&token);

    let ts = chrono::Utc::now().timestamp_millis();
    if state
        .auth_service
        .create_session(&token_hash, ts)
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create session",
        )
            .into_response();
    }

    state.add_session_hash(token_hash).await;

    let cookie = make_session_cookie(
        &token,
        SESSION_MAX_AGE_SECONDS,
        state.secure_session_cookies(),
    );
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"ok": true})),
    )
        .into_response()
}

pub async fn logout_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        let token_hash = hash_session_token(&token);
        state.remove_session_hash(&token_hash).await;
        if let Err(e) = state.auth_service.delete_session(&token_hash).await {
            warn!(err = %e, "failed to delete session from DB");
        }
    }
    let cookie = clear_session_cookie(state.secure_session_cookies());
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"ok": true})),
    )
}

pub async fn change_password_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ChangePasswordPayload>,
) -> impl IntoResponse {
    let token = match get_session_token_from_headers(&headers) {
        Some(token) if state.is_authenticated(&token).await => token,
        _ => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };
    let token_hash = hash_session_token(&token);

    let current_password = payload.current_password.unwrap_or_default();
    let new_password = payload.new_password.unwrap_or_default();
    if let Some(r) = check_field_len("current_password", &current_password, MAX_PASSWORD_LEN) {
        return r;
    }
    if let Some(r) = check_field_len("new_password", &new_password, MAX_PASSWORD_LEN) {
        return r;
    }

    if let Err(error) = validate_new_dashboard_password(&new_password) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": error})),
        )
            .into_response();
    }

    let stored_hash = match state.auth_service.get_password_hash().await {
        Ok(Some(hash)) => hash,
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "Current password is incorrect"})),
            )
                .into_response();
        }
    };

    let verified =
        tokio::task::spawn_blocking(move || verify_password(&current_password, &stored_hash))
            .await
            .unwrap_or(false);

    if !verified {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Current password is incorrect"})),
        )
            .into_response();
    }

    let new_hash = tokio::task::spawn_blocking(move || hash_password(&new_password))
        .await
        .unwrap_or_default();
    if new_hash.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to hash new password",
        )
            .into_response();
    }
    if state
        .auth_service
        .set_password_hash(&new_hash)
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to update password",
        )
            .into_response();
    }
    if state
        .auth_service
        .delete_sessions_except(&token_hash)
        .await
        .is_err()
    {
        state.clear_session_hashes().await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    state.retain_only_session_hash(&token_hash).await;
    let _ = state
        .auth_service
        .set_meta(
            BOOTSTRAP_PASSWORD_PROMPT_META_KEY,
            BOOTSTRAP_PROMPT_DISMISSED,
        )
        .await;

    Json(serde_json::json!({"ok": true})).into_response()
}

pub async fn dismiss_password_change_prompt_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    if state
        .auth_service
        .set_meta(
            BOOTSTRAP_PASSWORD_PROMPT_META_KEY,
            BOOTSTRAP_PROMPT_DISMISSED,
        )
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

pub async fn rate_limits_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let attempts = state
        .security_failure_snapshots()
        .into_iter()
        .map(|snapshot| {
            serde_json::json!({
                "scope": snapshot.scope,
                "ip": snapshot.ip,
                "failureCount": snapshot.failure_count,
                "banned": snapshot.banned,
                "banRemainingMs": snapshot.ban_remaining_ms,
            })
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "attempts": attempts })).into_response()
}

pub async fn rate_limits_reset_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<RateLimitResetPayload>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let scope = match payload.scope.as_deref() {
        Some(scope) => match RateLimitScope::from_key(scope) {
            Some(scope) => Some(scope),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "Invalid rate limit scope"})),
                )
                    .into_response();
            }
        },
        None => None,
    };
    let removed = state.reset_security_failures(scope, payload.ip.as_deref());
    Json(serde_json::json!({ "ok": true, "removed": removed })).into_response()
}

pub async fn audio_caps_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    Json(serde_json::json!({
        "caps": {
            "facebook:hls": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "facebook:rtmp": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "facebook:rtmps": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "facebook:srt": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "generic:hls": {"codecs": ["aac", "ac3", "eac3"], "maxChannels": null, "maxTracks": null},
            "generic:rtmp": {"codecs": ["aac", "mp3"], "maxChannels": 6, "maxTracks": 1},
            "generic:rtmps": {"codecs": ["aac", "mp3"], "maxChannels": 6, "maxTracks": 1},
            "generic:srt": {"codecs": "any", "maxChannels": null, "maxTracks": null},
            "vdocipher:hls": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "vdocipher:rtmp": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "vdocipher:rtmps": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "vdocipher:srt": {"codecs": ["aac"], "maxChannels": 2, "maxTracks": 1},
            "youtube:hls": {"codecs": ["aac", "ac3", "eac3"], "maxChannels": 6, "maxTracks": 1},
            "youtube:rtmp": {"codecs": ["aac", "mp3"], "maxChannels": 2, "maxTracks": 1},
            "youtube:rtmps": {"codecs": ["aac", "mp3"], "maxChannels": 2, "maxTracks": 1},
            "youtube:srt": {"codecs": ["aac", "mp3"], "maxChannels": 2, "maxTracks": 1}
        },
        "platformLabels": {
            "facebook": "Facebook Live",
            "generic": "Generic",
            "vdocipher": "VdoCipher",
            "youtube": "YouTube"
        }
    }))
    .into_response()
}

pub async fn stream_keys_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let host = state.pipeline_service.get_ingest_host().await;
    match state.pipeline_service.list_pipelines().await {
        Ok(pipelines) => {
            let keys = pipelines
                .into_iter()
                .map(|pipeline| {
                    let key = pipeline.stream_key;
                    serde_json::json!({
                        "key": key.clone(),
                        "label": pipeline.name,
                        "ingestUrls": {
                            "rtmp": format!("rtmp://{}:{}/live/{}", host, state.ports.rtmp, key),
                            "srt": format!("srt://{}:{}?streamid=publish:{}", host, state.ports.srt, key)
                        }
                    })
                })
                .collect::<Vec<_>>();
            Json(keys).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        constant_time_eq, select_initial_admin_password, validate_new_dashboard_password,
        write_bootstrap_password_file,
    };

    #[test]
    fn initial_admin_password_prefers_non_empty_env_value() {
        let (password, generated) = select_initial_admin_password(Some("dev-secret".to_string()));

        assert_eq!(password, "dev-secret");
        assert!(!generated);
    }

    #[test]
    fn initial_admin_password_generates_high_entropy_hex_without_env_value() {
        let (password, generated) = select_initial_admin_password(None);

        assert!(generated);
        assert_eq!(password.len(), 64);
        assert!(password.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    #[cfg(unix)]
    fn generated_bootstrap_password_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "restream-bootstrap-password-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        write_bootstrap_password_file(&path, "secret").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);

        assert_eq!(contents, "secret\n");
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn password_hash_compare_is_length_aware() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"same-but-longer"));
        assert!(!constant_time_eq(b"same", b"diff"));
    }

    #[test]
    fn dashboard_password_policy_rejects_short_replacements() {
        assert_eq!(
            validate_new_dashboard_password("short").unwrap_err(),
            "New password must be at least 12 characters"
        );
        assert!(validate_new_dashboard_password("long-enough1").is_ok());
    }
}
