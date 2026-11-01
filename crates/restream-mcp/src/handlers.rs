//! Thin MCP handlers that parse tool input and delegate to `AgentBackend`.

use crate::approval::{self, Approval};
use crate::inputs::{OperationApprovalInput, OperationIdInput};
use crate::transport::CallContext;
use restream_agent_contract::{
    AgentBackend, AgentError, InvestigationRequest, OperationCreateRequest, PlanRequest,
    VerifyRequest,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct AgentToolHandler<B: AgentBackend> {
    backend: Arc<B>,
}

impl<B: AgentBackend> AgentToolHandler<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    pub async fn dispatch(
        &self,
        tool_name: &str,
        input: Value,
        ctx: &CallContext,
    ) -> Result<Value, AgentError> {
        match tool_name {
            "get_agent_capabilities" => self.backend.capabilities().await,
            "get_agent_context" => self.backend.context().await,
            "investigate_pipeline_issue" => {
                let request: InvestigationRequest = serde_json::from_value(input)?;
                self.backend.investigate(request).await
            }
            "plan_pipeline_change" => {
                let request: PlanRequest = serde_json::from_value(input)?;
                self.backend.plan(request).await
            }
            "validate_change" => {
                let request: PlanRequest = serde_json::from_value(input)?;
                self.backend.validate(request).await
            }
            "preview_graph_diff" => {
                let request: PlanRequest = serde_json::from_value(input)?;
                self.backend.graph_diff(request).await
            }
            "create_agent_operation" => {
                let request: OperationCreateRequest = serde_json::from_value(input)?;
                self.backend.create_operation(request).await
            }
            "get_agent_operation" => {
                let request: OperationIdInput = serde_json::from_value(input)?;
                self.backend.get_operation(&request.operation_id).await
            }
            "approve_agent_operation" => {
                let request: OperationApprovalInput = serde_json::from_value(input)?;
                self.backend
                    .approve_operation(&request.operation_id, request.body())
                    .await
            }
            "apply_agent_operation" => {
                let request: OperationIdInput = serde_json::from_value(input)?;
                self.apply(&request.operation_id, ctx).await
            }
            "verify_agent_operation" => {
                let request: OperationIdInput = serde_json::from_value(input)?;
                ctx.with_progress(
                    "verifying",
                    self.backend.verify_operation(VerifyRequest {
                        operation_id: request.operation_id,
                    }),
                )
                .await
            }
            other => Err(AgentError::InvalidRequest(format!(
                "unknown MCP tool '{other}'"
            ))),
        }
    }

    /// Apply an operation. One still awaiting approval is first put to the
    /// client when it can be asked mid-call; without an explicit approval
    /// nothing is approved or applied.
    async fn apply(&self, operation_id: &str, ctx: &CallContext) -> Result<Value, AgentError> {
        if ctx.can_elicit() {
            let operation = self.backend.get_operation(operation_id).await?;
            if operation["status"] == "awaitingApproval" {
                match approval::ask(ctx, &operation).await {
                    Approval::Granted(body) => {
                        self.backend.approve_operation(operation_id, body).await?;
                    }
                    Approval::Withheld(outcome) => {
                        return Ok(json!({
                            "operationId": operation_id,
                            "applied": false,
                            "approval": outcome,
                        }));
                    }
                }
            }
        }
        ctx.with_progress("applying", self.backend.apply_operation(operation_id))
            .await
    }
}
