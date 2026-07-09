use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tracing::warn;

use super::state::{
    AppState, MAX_PASSWORD_LEN, SESSION_MAX_AGE_SECONDS, STREAM_KEYS, check_field_len,
    clear_session_cookie, get_session_token_from_headers, hash_session_token, make_session_cookie,
    to_hex,
};
use crate::application::services::AuthService;

#[derive(Deserialize)]
pub struct LoginPayload {
    pub password: Option<String>,
}

#[derive(Deserialize)]
pub struct ChangePasswordPayload {
    pub current_password: Option<String>,
    pub new_password: Option<String>,
}

pub fn hash_password(password: &str) -> String {
    use rand::RngCore;
    use scrypt::Params;

    let mut salt_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt_bytes);
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
    hex_hash == stored_hash
}

pub async fn initialize_auth(
    db_pool: &sqlx::SqlitePool,
    sessions_set: &TokioRwLock<std::collections::HashSet<String>>,
) {
    let auth_service = AuthService::new(db_pool.clone());
    let admin_hash = hash_password("admin");
    let _ = auth_service.ensure_password_hash(&admin_hash).await;

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
        .unwrap_or_else(|| "127.0.0.1".to_string());

    if let Some(ban_remaining) = state.security.is_ip_banned(&client_ip) {
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
        state.security.record_failure(&client_ip);
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Incorrect password"})),
        )
            .into_response();
    }
    state.security.record_success(&client_ip);

    use rand::RngCore;
    let mut token_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut token_bytes);
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

    state.sessions.write().await.insert(token_hash);

    let cookie = make_session_cookie(&token, SESSION_MAX_AGE_SECONDS);
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
        state.sessions.write().await.remove(&token_hash);
        if let Err(e) = state.auth_service.delete_session(&token_hash).await {
            warn!(err = %e, "failed to delete session from DB");
        }
    }
    let cookie = clear_session_cookie();
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
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let current_password = payload.current_password.unwrap_or_default();
    let new_password = payload.new_password.unwrap_or_default();
    if let Some(r) = check_field_len("current_password", &current_password, MAX_PASSWORD_LEN) {
        return r;
    }
    if let Some(r) = check_field_len("new_password", &new_password, MAX_PASSWORD_LEN) {
        return r;
    }

    if new_password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "New password cannot be empty"})),
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

    Json(serde_json::json!({"ok": true})).into_response()
}

pub async fn audio_caps_handler() -> impl IntoResponse {
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
    let mut keys = Vec::new();
    for &(key, label) in STREAM_KEYS {
        keys.push(serde_json::json!({
            "key": key,
            "label": label,
            "ingestUrls": {
                "rtmp": format!("rtmp://{}:{}/live/{}", host, state.ports.rtmp, key),
                "srt": format!("srt://{}:{}?streamid=publish:live/{}", host, state.ports.srt, key)
            }
        }));
    }
    Json(keys).into_response()
}
