//! Team HTTP surface: who the session is, viewing the dashboard as a lower
//! role, members CRUD, the audit log, and the one layer that enforces the
//! route-to-role policy and audits every change.

use axum::{
    Json,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use tracing::warn;

use super::auth::hash_password;
use super::error::ApiError;
use super::state::{
    AppState, MAX_PASSWORD_LEN, MIN_DASHBOARD_PASSWORD_LEN, check_field_len,
    get_session_token_from_headers, hash_session_token, to_hex,
};
use crate::application::team::{AuditEntry, Principal, Role, UserRecord, required_role};

/// The session's principal, resolved from its cookie.
async fn session_principal(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(String, Principal), Response> {
    let unauthorized = || (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    let token = get_session_token_from_headers(headers).ok_or_else(unauthorized)?;
    if !state.is_authenticated(&token).await {
        return Err(unauthorized());
    }
    let token_hash = hash_session_token(&token);
    match state.team_service.principal(&token_hash).await {
        Ok(Some(principal)) => Ok((token_hash, principal)),
        Ok(None) => Err(unauthorized()),
        Err(error) => Err(ApiError::from(error).into_response()),
    }
}

fn forbidden(needed: Role, acting: Role) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": format!(
                "This needs the {} role; this session acts as {}.",
                needed.as_str(),
                acting.as_str()
            ),
            "requiredRole": needed,
            "effectiveRole": acting,
        })),
    )
        .into_response()
}

fn is_read(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Records one change; a failed write is logged, never fails the request.
pub(super) async fn audit(
    state: &AppState,
    principal: &Principal,
    method: &str,
    path: &str,
    status: u16,
) {
    let entry = AuditEntry {
        id: 0,
        at_ms: chrono::Utc::now().timestamp_millis(),
        username: principal.username.clone(),
        role: principal.effective_role(),
        method: method.to_string(),
        path: path.to_string(),
        status,
    };
    if let Err(error) = state.team_service.record(&entry).await {
        warn!(err = %error, "failed to append audit entry");
    }
}

/// Enforces [`required_role`] on every authenticated API request and audits
/// each change. A request without a valid session passes through: its
/// handler answers 401, so the authentication contract stays where it is.
/// Reads any session may make skip the principal lookup entirely.
pub async fn authorize_and_audit(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if !path.starts_with("/api/v1/") || path == "/api/v1/auth/login" {
        return next.run(request).await;
    }
    let method = request.method().clone();
    let needed = required_role(method.as_str(), &path);
    if needed == Role::Viewer && is_read(&method) {
        return next.run(request).await;
    }
    let Ok((_, principal)) = session_principal(&state, request.headers()).await else {
        return next.run(request).await;
    };
    let acting = principal.effective_role();
    if acting < needed {
        if !is_read(&method) {
            audit(&state, &principal, method.as_str(), &path, 403).await;
        }
        return forbidden(needed, acting);
    }
    let response = next.run(request).await;
    // Logout is recorded too: it is a change to who holds access.
    if !is_read(&method) {
        audit(
            &state,
            &principal,
            method.as_str(),
            &path,
            response.status().as_u16(),
        )
        .await;
    }
    response
}

/// `GET /api/v1/auth/me`: who this session is and the role it acts as.
pub async fn me_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match session_principal(&state, &headers).await {
        Ok((_, principal)) => principal_json(&principal),
        Err(response) => response,
    }
}

fn principal_json(principal: &Principal) -> Response {
    Json(serde_json::json!({
        "userId": principal.user_id,
        "username": principal.username,
        "role": principal.role,
        "viewAs": principal.view_as,
        "effectiveRole": principal.effective_role(),
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewAsPayload {
    pub role: Option<Role>,
}

/// `POST /api/v1/auth/view-as`: act as a lower role (enforced, so the view
/// is what that role sees and can do), or `null` to return to one's own.
pub async fn view_as_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ViewAsPayload>,
) -> Response {
    let (token_hash, principal) = match session_principal(&state, &headers).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    match state
        .team_service
        .set_view_as(&token_hash, &principal, payload.role)
        .await
    {
        Ok(principal) => principal_json(&principal),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserPayload {
    pub username: String,
    pub password: String,
    pub role: Role,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUserPayload {
    pub role: Option<Role>,
    pub disabled: Option<bool>,
    pub password: Option<String>,
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response()
}

async fn hashed(password: String) -> Result<String, Response> {
    if let Some(response) = check_field_len("password", &password, MAX_PASSWORD_LEN) {
        return Err(response);
    }
    if password.len() < MIN_DASHBOARD_PASSWORD_LEN {
        return Err(bad_request(format!(
            "A password needs at least {MIN_DASHBOARD_PASSWORD_LEN} characters."
        )));
    }
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .unwrap_or_default();
    if hash.is_empty() {
        return Err(ApiError::internal("failed to hash password").into_response());
    }
    Ok(hash)
}

/// `GET /api/v1/users` (admin, by policy).
pub async fn list_users_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = session_principal(&state, &headers).await {
        return response;
    }
    match state.team_service.list_users().await {
        Ok(users) => Json(serde_json::json!({ "users": users })).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `POST /api/v1/users` (admin, by policy).
pub async fn create_user_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CreateUserPayload>,
) -> Response {
    if let Err(response) = session_principal(&state, &headers).await {
        return response;
    }
    let hash = match hashed(payload.password).await {
        Ok(hash) => hash,
        Err(response) => return response,
    };
    use rand::RngExt;
    let mut id = [0u8; 8];
    rand::rng().fill(&mut id);
    let user = UserRecord {
        id: format!("u_{}", to_hex(&id)),
        username: payload.username.trim().to_string(),
        role: payload.role,
        created_at_ms: chrono::Utc::now().timestamp_millis(),
        disabled: false,
        last_active_ms: None,
    };
    match state.team_service.create_user(user, &hash).await {
        Ok(user) => (StatusCode::CREATED, Json(user)).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `PATCH /api/v1/users/{id}` (admin, by policy).
pub async fn update_user_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<UpdateUserPayload>,
) -> Response {
    if let Err(response) = session_principal(&state, &headers).await {
        return response;
    }
    let hash = match payload.password {
        Some(password) => match hashed(password).await {
            Ok(hash) => Some(hash),
            Err(response) => return response,
        },
        None => None,
    };
    match state
        .team_service
        .update_user(&id, payload.role, payload.disabled, hash.as_deref())
        .await
    {
        Ok(user) => Json(user).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `DELETE /api/v1/users/{id}` (admin, by policy).
pub async fn delete_user_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = session_principal(&state, &headers).await {
        return response;
    }
    match state.team_service.delete_user(&id).await {
        Ok(()) => Json(serde_json::json!({ "deleted": true })).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<u32>,
    pub before: Option<i64>,
}

/// `GET /api/v1/audit?limit&before` (admin, by policy): newest first.
pub async fn audit_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    if let Err(response) = session_principal(&state, &headers).await {
        return response;
    }
    match state
        .team_service
        .audit(query.limit.unwrap_or(100), query.before)
        .await
    {
        Ok(entries) => Json(serde_json::json!({ "entries": entries })).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
