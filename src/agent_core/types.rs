//! Shared request types used by HTTP handlers, MCP handlers, and backends.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvestigationRequest {
    pub workflow: Option<String>,
    pub pipeline_id: Option<String>,
    pub output_id: Option<String>,
    #[serde(default = "default_event_limit")]
    pub event_limit: usize,
}

const fn default_event_limit() -> usize {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRequest {
    pub intent: String,
    pub pipeline_id: Option<String>,
    #[serde(default)]
    pub proposed_changes: Vec<ProposedChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedChange {
    pub kind: String,
    pub pipeline_id: Option<String>,
    pub output_id: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    pub monitoring_url: Option<String>,
    pub config: Option<crate::domain::output_spec::OutputConfig>,
    pub desired_state: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationCreateRequest {
    pub intent: String,
    pub pipeline_id: Option<String>,
    #[serde(default)]
    pub proposed_changes: Vec<ProposedChange>,
    pub idempotency_key: Option<String>,
    pub actor: Option<String>,
    pub agent_id: Option<String>,
    pub tool_identity: Option<String>,
    pub incident_id: Option<String>,
    #[serde(default)]
    pub incident_links: Vec<String>,
}

impl OperationCreateRequest {
    pub fn plan_request(&self) -> PlanRequest {
        PlanRequest {
            intent: self.intent.clone(),
            pipeline_id: self.pipeline_id.clone(),
            proposed_changes: self.proposed_changes.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub approved_by: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    pub operation_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_create_request_projects_shared_plan_request() {
        let request = OperationCreateRequest {
            intent: "add output".to_string(),
            pipeline_id: Some("pipe".to_string()),
            proposed_changes: vec![ProposedChange {
                kind: "add_output".to_string(),
                pipeline_id: Some("pipe".to_string()),
                output_id: None,
                name: Some("out".to_string()),
                url: Some("rtmp://example/live/out".to_string()),
                monitoring_url: None,
                config: None,
                desired_state: None,
            }],
            idempotency_key: Some("idem".to_string()),
            actor: None,
            agent_id: None,
            tool_identity: None,
            incident_id: None,
            incident_links: Vec::new(),
        };

        let plan = request.plan_request();

        assert_eq!(plan.intent, "add output");
        assert_eq!(plan.pipeline_id.as_deref(), Some("pipe"));
        assert_eq!(plan.proposed_changes[0].kind, "add_output");
    }
}
