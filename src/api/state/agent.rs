//! Agent-plane operations: output catalog reads, gated output mutations,
//! operation-store transitions, and reconciler wakeups.

use std::sync::Arc;

use super::AppState;

#[cfg(feature = "agent-execution")]
use restream_agent::execution::{
    ApplyBaseline, OperationRecord, OperationStoreError, StoreCreateResult,
};

impl AppState {
    /// The shared reconciler request handle used by runtime tasks.
    pub(crate) fn reconcile_handle(&self) -> Arc<crate::application::topology::ReconcileRequests> {
        Arc::clone(&self.reconcile)
    }

    /// Wakes the reconciler after a persisted mutation.
    pub(crate) fn wake_reconciler(&self) {
        self.reconcile.wake();
    }

    /// Consumes due reconciler requests for timing tests.
    #[doc(hidden)]
    pub fn take_due_reconcile_requests(&self, now: std::time::Instant) -> bool {
        self.reconcile.take_due(now)
    }

    /// Tells the reconciler that one pipeline's selected input changed.
    pub(crate) fn input_selection_changed(&self, pipeline_id: &str) {
        self.reconcile.input_selection_changed(pipeline_id);
    }

    /// Loads the pipeline and output catalog for agent planning.
    pub async fn agent_output_catalog(
        &self,
    ) -> crate::application::services::agent_service::AgentPipelineOutputCatalog {
        self.agent_service.load_pipeline_output_catalog().await
    }

    /// Loads the pipeline and output catalog, surfacing store failures.
    #[cfg(feature = "agent-execution")]
    pub(crate) async fn try_agent_output_catalog(
        &self,
    ) -> Result<crate::application::services::agent_service::AgentPipelineOutputCatalog, String>
    {
        self.agent_service.try_load_pipeline_output_catalog().await
    }
}

#[cfg(feature = "agent-execution")]
impl AppState {
    /// Loads one output for an agent mutation.
    pub(crate) async fn load_output_for_mutation(
        &self,
        pipeline_id: &str,
        output_id: &str,
    ) -> Result<restream_domain::models::Output, String> {
        self.agent_service
            .load_output_for_mutation(pipeline_id, output_id)
            .await
    }

    /// Applies one validated agent output mutation.
    pub(crate) async fn apply_agent_output_mutation(
        &self,
        pipeline_id: &str,
        mutation: crate::application::services::agent_service::AgentOutputMutation,
    ) -> Result<crate::application::services::agent_service::AgentOutputMutationOutcome, String>
    {
        self.agent_service
            .apply_output_mutation(&self.reconcile, pipeline_id, mutation)
            .await
    }

    /// Stores a newly created, not-yet-applied agent operation.
    pub(crate) fn create_agent_operation(
        &self,
        request: restream_agent_contract::OperationCreateRequest,
        plan: restream_agent::plane::PlanResponse,
    ) -> Result<StoreCreateResult, OperationStoreError> {
        self.agent_execution.create(request, plan)
    }

    /// Fetches one agent operation by id.
    pub(crate) fn agent_operation(&self, operation_id: &str) -> Option<OperationRecord> {
        self.agent_execution.get(operation_id)
    }

    /// Marks one agent operation as applying.
    pub(crate) fn start_agent_apply(
        &self,
        operation_id: &str,
        baseline: ApplyBaseline,
    ) -> Result<OperationRecord, OperationStoreError> {
        self.agent_execution.start_apply(operation_id, baseline)
    }

    /// Marks one agent operation as applied.
    pub(crate) fn complete_agent_apply(
        &self,
        operation_id: &str,
        state_transitions: Vec<serde_json::Value>,
        progress_snapshots: Vec<serde_json::Value>,
        execution_result: serde_json::Value,
    ) -> Option<OperationRecord> {
        self.agent_execution.complete_apply(
            operation_id,
            state_transitions,
            progress_snapshots,
            execution_result,
        )
    }

    /// Marks one agent operation as failed.
    pub(crate) fn fail_agent_apply(
        &self,
        operation_id: &str,
        error: String,
    ) -> Option<OperationRecord> {
        self.agent_execution.fail_apply(operation_id, error)
    }

    /// Approves one agent operation.
    pub(crate) fn approve_agent_operation(
        &self,
        operation_id: &str,
        approver: &restream_agent::execution::Approver,
        approval: restream_agent_contract::ApprovalRequest,
    ) -> Result<OperationRecord, OperationStoreError> {
        self.agent_execution
            .approve(operation_id, approver, approval)
    }

    /// Approves one change of an agent operation.
    pub(crate) fn approve_agent_action(
        &self,
        operation_id: &str,
        index: usize,
        approver: &restream_agent::execution::Approver,
        reason: Option<String>,
    ) -> Result<OperationRecord, OperationStoreError> {
        self.agent_execution
            .approve_action(operation_id, index, approver, reason)
    }

    /// Stores one operation's verification result.
    pub(crate) fn complete_agent_verify(
        &self,
        operation_id: &str,
        verification_result: serde_json::Value,
    ) -> Result<OperationRecord, OperationStoreError> {
        self.agent_execution
            .complete_verify(operation_id, verification_result)
    }
}
