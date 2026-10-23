//! Notification HTTP surface: channels (admin), the delivery log (admin),
//! and each member's own browser push subscriptions.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use super::error::ApiError;
use super::state::{AppState, get_session_token_from_headers, hash_session_token, to_hex};
use crate::application::notify::{
    AlertBrief, ChannelKind, EventKind, NotifyEvent, NotifySeverity, PushSubscription,
};
use crate::application::services::notify_service::ChannelDraft;

/// The session's member name, or the handler's 401.
async fn member(state: &AppState, headers: &HeaderMap) -> Result<String, Response> {
    let unauthorized = || (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    let token = get_session_token_from_headers(headers).ok_or_else(unauthorized)?;
    if !state.is_authenticated(&token).await {
        return Err(unauthorized());
    }
    match state
        .team_service
        .principal(&hash_session_token(&token))
        .await
    {
        Ok(Some(principal)) => Ok(principal.username),
        Ok(None) => Err(unauthorized()),
        Err(error) => Err(ApiError::from(error).into_response()),
    }
}

fn random_hex(bytes: usize) -> String {
    use rand::RngExt;
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill(&mut buffer[..]);
    to_hex(&buffer)
}

fn test_event(username: &str) -> NotifyEvent {
    NotifyEvent {
        kind: EventKind::Test,
        at_ms: chrono::Utc::now().timestamp_millis(),
        alert: AlertBrief {
            id: "test".to_string(),
            severity: NotifySeverity::Warning,
            title: "Test notification from restream".to_string(),
            cause: format!("{username} sent a test. If you can read this, this channel works."),
            recommended_action: "Nothing: this is only a test.".to_string(),
            pipeline_id: None,
            output_id: None,
        },
    }
}

fn result<T: serde::Serialize>(
    value: Result<T, crate::application::services::ServiceError>,
) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPayload {
    pub kind: ChannelKind,
    pub name: String,
    pub target: String,
    pub min_severity: NotifySeverity,
    #[serde(default)]
    pub on_resolved: bool,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl From<ChannelPayload> for ChannelDraft {
    fn from(payload: ChannelPayload) -> Self {
        Self {
            kind: payload.kind,
            name: payload.name,
            target: payload.target,
            min_severity: payload.min_severity,
            on_resolved: payload.on_resolved,
            enabled: payload.enabled,
        }
    }
}

/// `GET /api/v1/notifications/channels` (admin, by policy).
pub async fn list_channels_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    result(
        state
            .notify_service
            .channels()
            .await
            .map(|channels| serde_json::json!({ "channels": channels })),
    )
}

/// `POST /api/v1/notifications/channels` (admin): a webhook also gets a
/// signing secret, returned once here.
pub async fn create_channel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ChannelPayload>,
) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    let secret = format!("whsec_{}", random_hex(24));
    match state
        .notify_service
        .create_channel(
            format!("ch_{}", random_hex(8)),
            secret,
            payload.into(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
    {
        Ok(channel) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "channel": channel, "secret": channel.secret })),
        )
            .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `PUT /api/v1/notifications/channels/{id}` (admin).
pub async fn update_channel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<ChannelPayload>,
) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    result(
        state
            .notify_service
            .update_channel(&id, payload.into())
            .await,
    )
}

/// `DELETE /api/v1/notifications/channels/{id}` (admin).
pub async fn delete_channel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    result(
        state
            .notify_service
            .delete_channel(&id)
            .await
            .map(|()| serde_json::json!({ "deleted": true })),
    )
}

/// `POST /api/v1/notifications/channels/{id}/test` (admin): sends now and
/// answers with how it ended.
pub async fn test_channel_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let username = match member(&state, &headers).await {
        Ok(username) => username,
        Err(response) => return response,
    };
    result(
        state
            .notify_service
            .test_channel(&id, &test_event(&username))
            .await,
    )
}

/// `GET /api/v1/notifications/channels/{id}/secret` (admin): the webhook
/// signing secret, to configure the receiver.
pub async fn channel_secret_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    result(
        state
            .notify_service
            .channel_secret(&id)
            .await
            .map(|secret| serde_json::json!({ "secret": secret })),
    )
}

#[derive(Deserialize)]
pub struct LimitQuery {
    pub limit: Option<u32>,
}

/// `GET /api/v1/notifications/deliveries?limit` (admin): newest first.
pub async fn deliveries_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LimitQuery>,
) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    result(
        state
            .notify_service
            .deliveries(query.limit.unwrap_or(100))
            .await
            .map(|deliveries| serde_json::json!({ "deliveries": deliveries })),
    )
}

/// `GET /api/v1/notifications/push/key`: the VAPID public key a browser
/// subscribes with.
pub async fn push_key_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(response) = member(&state, &headers).await {
        return response;
    }
    result(
        state
            .notify_service
            .push_public_key()
            .await
            .map(|key| serde_json::json!({ "publicKey": key })),
    )
}

/// `GET /api/v1/notifications/push/subscriptions`: this member's browsers.
pub async fn my_subscriptions_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let username = match member(&state, &headers).await {
        Ok(username) => username,
        Err(response) => return response,
    };
    result(
        state
            .notify_service
            .subscriptions_of(&username)
            .await
            .map(|subscriptions| serde_json::json!({ "subscriptions": subscriptions })),
    )
}

#[derive(Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

/// A browser's `PushSubscription.toJSON()`, plus what it wants.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribePayload {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
    pub min_severity: NotifySeverity,
}

/// `POST /api/v1/notifications/push/subscriptions`: any member, for
/// themselves.
pub async fn subscribe_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<SubscribePayload>,
) -> Response {
    let username = match member(&state, &headers).await {
        Ok(username) => username,
        Err(response) => return response,
    };
    let subscription = PushSubscription {
        id: format!("ps_{}", random_hex(8)),
        username,
        endpoint: payload.endpoint,
        p256dh: payload.keys.p256dh,
        auth: payload.keys.auth,
        min_severity: payload.min_severity,
        created_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    match state.notify_service.subscribe(subscription).await {
        Ok(subscription) => (StatusCode::CREATED, Json(subscription)).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `DELETE /api/v1/notifications/push/subscriptions/{id}`: one's own.
pub async fn unsubscribe_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let username = match member(&state, &headers).await {
        Ok(username) => username,
        Err(response) => return response,
    };
    result(
        state
            .notify_service
            .unsubscribe(&username, &id)
            .await
            .map(|()| serde_json::json!({ "deleted": true })),
    )
}

/// `POST /api/v1/notifications/push/test`: a test push to this member's
/// browsers.
pub async fn test_push_handler(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let username = match member(&state, &headers).await {
        Ok(username) => username,
        Err(response) => return response,
    };
    result(
        state
            .notify_service
            .test_push(&username, &test_event(&username))
            .await
            .map(|deliveries| serde_json::json!({ "deliveries": deliveries })),
    )
}
