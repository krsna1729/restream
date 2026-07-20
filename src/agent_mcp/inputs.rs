use crate::agent_core::ApprovalRequest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationIdInput {
    pub(crate) operation_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationApprovalInput {
    pub(crate) operation_id: String,
    pub(crate) approved_by: String,
    pub(crate) reason: Option<String>,
}

impl OperationApprovalInput {
    pub(crate) fn body(&self) -> ApprovalRequest {
        ApprovalRequest {
            approved_by: self.approved_by.clone(),
            reason: self.reason.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_input_preserves_mcp_wire_fields_when_projecting_http_body() {
        let input: OperationApprovalInput = serde_json::from_value(serde_json::json!({
            "operationId": "operation-1",
            "approvedBy": "operator",
            "reason": "reviewed"
        }))
        .expect("MCP approval input should deserialize");

        let body = input.body();

        assert_eq!(input.operation_id, "operation-1");
        assert_eq!(body.approved_by, "operator");
        assert_eq!(body.reason.as_deref(), Some("reviewed"));
    }
}
