//! Approving an operation: all its changes at once, or one change at a time.
//! Each approval acts as the session's member at its effective role (viewing
//! the dashboard as a lower role approves as that role); a role below the
//! change's minimum (`restream_agent::execution::change_role`) answers 403,
//! which the team middleware audits like any refused mutation.

use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};
use std::sync::Arc;

use super::super::state::AppState;
#[cfg(not(feature = "agent-execution"))]
use super::super::state::require_authenticated;
#[cfg(feature = "agent-execution")]
use super::super::team::{forbidden, session_principal};
#[cfg(not(feature = "agent-execution"))]
use super::agent_execution_unavailable;
#[cfg(feature = "agent-execution")]
use super::operations::operation_store_error;
#[cfg(feature = "agent-execution")]
use axum::response::Response;
#[cfg(feature = "agent-execution")]
use restream_agent::execution::{Approver, OperationRecord, OperationStoreError, public_record};

#[cfg(feature = "agent-execution")]
/// The body of `POST /api/v1/agent/operations/{id}/actions/{index}/approve`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionApprovalRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// The session's member, acting at its effective role.
#[cfg(feature = "agent-execution")]
async fn approver(state: &AppState, headers: &HeaderMap) -> Result<Approver, Response> {
    let (_, principal) = session_principal(state, headers).await?;
    Ok(Approver {
        role: principal.effective_role(),
        username: principal.username,
    })
}

#[cfg(feature = "agent-execution")]
fn approval_response(
    result: Result<OperationRecord, OperationStoreError>,
    approver: &Approver,
) -> Response {
    match result {
        Ok(record) => Json(public_record(&record)).into_response(),
        Err(OperationStoreError::RoleTooLow { needed }) => forbidden(needed, approver.role),
        Err(err) => operation_store_error(err),
    }
}

/// Approves every change at once: needs the most demanding change's role.
#[cfg(feature = "agent-execution")]
pub async fn agent_operation_approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
    Json(request): Json<restream_agent_contract::ApprovalRequest>,
) -> impl IntoResponse {
    let approver = match approver(&state, &headers).await {
        Ok(approver) => approver,
        Err(response) => return response,
    };
    let result = state
        .agent_execution
        .approve(&operation_id, &approver, request);
    approval_response(result, &approver)
}

/// Approves the change at `index`; the operation is approved with its last.
#[cfg(feature = "agent-execution")]
pub async fn agent_action_approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((operation_id, index)): Path<(String, usize)>,
    Json(request): Json<ActionApprovalRequest>,
) -> impl IntoResponse {
    let approver = match approver(&state, &headers).await {
        Ok(approver) => approver,
        Err(response) => return response,
    };
    let result =
        state
            .agent_execution
            .approve_action(&operation_id, index, &approver, request.reason);
    approval_response(result, &approver)
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_operation_approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_operation_id): Path<String>,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_action_approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_path): Path<(String, String)>,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}
