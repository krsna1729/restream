//! Optional agent execution plane.
//!
//! This module is compiled only with `agent-execution`, which depends on the
//! read/planning `agent-plane` feature. It owns operation state, approval
//! transitions, audit events, idempotency lookups, and redacted public views;
//! API handlers still perform the actual runtime mutations through core APIs.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use crate::agent_core::{ApprovalRequest, OperationCreateRequest, PlanRequest};
use crate::agent_plane::PlanResponse;

const MAX_AGENT_EXECUTION_RECORDS: usize = 1024;
const AUTHENTICATED_DASHBOARD_ACTOR: &str = "dashboard-admin";
const AUTHENTICATED_DASHBOARD_APPROVER: &str = "dashboard-session";
const AGENT_EXECUTION_TOOL_IDENTITY: &str = "agent-execution-api";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreCreateResult {
    pub operation: Value,
    pub reused: bool,
}

#[derive(Debug, Clone)]
pub struct OperationRecord {
    pub operation_id: String,
    pub idempotency_key: Option<String>,
    pub status: OperationStatus,
    pub request: OperationCreateRequest,
    pub plan: PlanResponse,
    pub plan_hash: String,
    pub idempotency_hash: Option<String>,
    pub approval: Option<ApprovalState>,
    pub approval_required: bool,
    pub created_at: String,
    pub updated_at: String,
    pub actor: String,
    pub agent_id: String,
    pub tool_identity: String,
    pub affected_objects: Value,
    pub warnings: Vec<String>,
    pub progress_snapshots: Vec<Value>,
    pub state_transitions: Vec<Value>,
    pub audit_log: Vec<Value>,
    pub execution_result: Option<Value>,
    pub verification_result: Option<Value>,
    pub pre_apply_alert_count: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationStatus {
    Invalid,
    AwaitingApproval,
    Approved,
    Applying,
    Applied,
    Verified,
    VerificationFailed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalState {
    pub approved_by: String,
    pub reason: Option<String>,
    pub approved_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStoreError {
    NotFound,
    IdempotencyConflict,
    Invalid,
    RequiresApproval,
    AlreadyApplying,
    AlreadyTerminal,
}

#[derive(Default)]
struct AgentExecutionState {
    records: HashMap<String, OperationRecord>,
    idempotency: HashMap<String, String>,
}

// records and idempotency share a single lock so idempotency-key dedup in
// `create()` is atomic end-to-end: two concurrent creates with the same key
// must not both observe "not present yet" and both insert a full record.
#[derive(Default)]
pub struct AgentExecutionStore {
    state: Mutex<AgentExecutionState>,
}

impl AgentExecutionStore {
    pub fn create(
        &self,
        request: OperationCreateRequest,
        plan: PlanResponse,
        pre_alert_count: usize,
    ) -> Result<StoreCreateResult, OperationStoreError> {
        let plan_request = request.plan_request();
        let plan_hash = plan_hash(&plan_request);
        let idempotency_hash = request
            .idempotency_key
            .as_ref()
            .map(|_| operation_idempotency_hash(&request, &plan_hash));

        let mut state = lock_or_recover(&self.state);
        if let Some(key) = request.idempotency_key.as_deref()
            && let Some(operation_id) = state.idempotency.get(key).cloned()
            && let Some(record) = state.records.get(&operation_id).cloned()
        {
            if record.idempotency_hash.as_ref() != idempotency_hash.as_ref() {
                return Err(OperationStoreError::IdempotencyConflict);
            }
            return Ok(StoreCreateResult {
                operation: public_record(&record),
                reused: true,
            });
        }

        let created_at = now();
        let operation_id = operation_id(&request, &created_at);
        let status = if plan.validation.valid {
            OperationStatus::AwaitingApproval
        } else {
            OperationStatus::Invalid
        };
        let affected_objects = affected_objects(&plan_request);
        let mut audit_log = vec![audit_event(
            "created",
            &created_at,
            "operation object created from agent plan",
            serde_json::json!({
                "planHash": plan_hash,
                "approvalRequired": true,
                "valid": plan.validation.valid,
                "incidentId": request.incident_id,
                "incidentLinks": request.incident_links,
            }),
        )];
        if status == OperationStatus::Invalid {
            audit_log.push(audit_event(
                "invalid",
                &created_at,
                "operation cannot be approved or applied until validation errors are fixed",
                serde_json::json!({"validation": plan.validation}),
            ));
        }
        let warnings = plan
            .validation
            .warnings
            .iter()
            .map(|issue| issue.message.clone())
            .collect();

        let record = OperationRecord {
            operation_id: operation_id.clone(),
            idempotency_key: request.idempotency_key.clone(),
            status,
            plan,
            plan_hash,
            idempotency_hash,
            approval: None,
            approval_required: true,
            created_at: created_at.clone(),
            updated_at: created_at,
            actor: AUTHENTICATED_DASHBOARD_ACTOR.to_string(),
            agent_id: AUTHENTICATED_DASHBOARD_ACTOR.to_string(),
            tool_identity: AGENT_EXECUTION_TOOL_IDENTITY.to_string(),
            affected_objects,
            warnings,
            progress_snapshots: Vec::new(),
            state_transitions: Vec::new(),
            audit_log,
            execution_result: None,
            verification_result: None,
            pre_apply_alert_count: Some(pre_alert_count),
            request,
        };

        if let Some(key) = record.idempotency_key.clone() {
            state.idempotency.insert(key, operation_id.clone());
        }
        state.records.insert(operation_id.clone(), record.clone());
        Self::enforce_record_limit(&mut state);

        Ok(StoreCreateResult {
            operation: public_record(&record),
            reused: false,
        })
    }

    pub fn get(&self, operation_id: &str) -> Option<OperationRecord> {
        lock_or_recover(&self.state)
            .records
            .get(operation_id)
            .cloned()
    }

    pub fn approve(
        &self,
        operation_id: &str,
        approval: ApprovalRequest,
    ) -> Result<OperationRecord, OperationStoreError> {
        let mut state = lock_or_recover(&self.state);
        let record = state
            .records
            .get_mut(operation_id)
            .ok_or(OperationStoreError::NotFound)?;
        match record.status {
            OperationStatus::Invalid => return Err(OperationStoreError::Invalid),
            OperationStatus::AwaitingApproval => {}
            OperationStatus::Applying => return Err(OperationStoreError::AlreadyApplying),
            OperationStatus::Applied
            | OperationStatus::Verified
            | OperationStatus::VerificationFailed
            | OperationStatus::Failed => return Err(OperationStoreError::AlreadyTerminal),
            OperationStatus::Approved => {}
        }

        let approved_at = now();
        record.status = OperationStatus::Approved;
        record.approval = Some(ApprovalState {
            approved_by: AUTHENTICATED_DASHBOARD_APPROVER.to_string(),
            reason: approval.reason,
            approved_at: approved_at.clone(),
        });
        record.updated_at = approved_at.clone();
        record.audit_log.push(audit_event(
            "approved",
            &approved_at,
            "operation approved for application",
            serde_json::json!({"approval": record.approval}),
        ));
        Ok(record.clone())
    }

    pub fn start_apply(&self, operation_id: &str) -> Result<OperationRecord, OperationStoreError> {
        let mut state = lock_or_recover(&self.state);
        let record = state
            .records
            .get_mut(operation_id)
            .ok_or(OperationStoreError::NotFound)?;
        match record.status {
            OperationStatus::Invalid => return Err(OperationStoreError::Invalid),
            OperationStatus::AwaitingApproval => return Err(OperationStoreError::RequiresApproval),
            OperationStatus::Applying => return Err(OperationStoreError::AlreadyApplying),
            OperationStatus::Applied
            | OperationStatus::Verified
            | OperationStatus::VerificationFailed
            | OperationStatus::Failed => return Err(OperationStoreError::AlreadyTerminal),
            OperationStatus::Approved => {}
        }

        let ts = now();
        record.status = OperationStatus::Applying;
        record.updated_at = ts.clone();
        record.audit_log.push(audit_event(
            "applyStarted",
            &ts,
            "operation application started",
            serde_json::json!({}),
        ));
        Ok(record.clone())
    }

    pub fn complete_apply(
        &self,
        operation_id: &str,
        state_transitions: Vec<Value>,
        progress_snapshots: Vec<Value>,
        execution_result: Value,
    ) -> Option<OperationRecord> {
        self.update(operation_id, |record, ts| {
            record.status = OperationStatus::Applied;
            record.state_transitions.extend(state_transitions);
            record.progress_snapshots.extend(progress_snapshots);
            record.execution_result = Some(execution_result);
            record.audit_log.push(audit_event(
                "applyCompleted",
                &ts,
                "operation application completed",
                serde_json::json!({"result": record.execution_result}),
            ));
        })
    }

    pub fn fail_apply(&self, operation_id: &str, error: String) -> Option<OperationRecord> {
        self.update(operation_id, |record, ts| {
            record.status = OperationStatus::Failed;
            record.execution_result = Some(serde_json::json!({
                "success": false,
                "error": error,
            }));
            record.audit_log.push(audit_event(
                "applyFailed",
                &ts,
                "operation application failed",
                serde_json::json!({"error": error}),
            ));
        })
    }

    pub fn complete_verify(
        &self,
        operation_id: &str,
        verification_result: Value,
    ) -> Option<OperationRecord> {
        self.update(operation_id, |record, ts| {
            record.status = if verification_result["success"].as_bool().unwrap_or(false) {
                OperationStatus::Verified
            } else {
                OperationStatus::VerificationFailed
            };
            record.verification_result = Some(verification_result);
            record.audit_log.push(audit_event(
                "verified",
                &ts,
                "operation post-change verification completed",
                serde_json::json!({"result": record.verification_result}),
            ));
        })
    }

    fn update(
        &self,
        operation_id: &str,
        f: impl FnOnce(&mut OperationRecord, String),
    ) -> Option<OperationRecord> {
        let mut state = lock_or_recover(&self.state);
        let record = state.records.get_mut(operation_id)?;
        let ts = now();
        f(record, ts.clone());
        record.updated_at = ts;
        Some(record.clone())
    }

    fn enforce_record_limit(state: &mut AgentExecutionState) {
        while state.records.len() > MAX_AGENT_EXECUTION_RECORDS {
            let Some(operation_id) = oldest_operation_id(&state.records) else {
                return;
            };
            if let Some(record) = state.records.remove(&operation_id)
                && let Some(key) = record.idempotency_key
            {
                state.idempotency.remove(&key);
            }
        }
    }
}

fn oldest_operation_id(records: &HashMap<String, OperationRecord>) -> Option<String> {
    records
        .values()
        .min_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.operation_id.cmp(&right.operation_id))
        })
        .map(|record| record.operation_id.clone())
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn public_record(record: &OperationRecord) -> Value {
    crate::agent_plane::redact_secrets(serde_json::json!({
        "operationId": record.operation_id,
        "idempotencyKey": record.idempotency_key,
        "status": record.status,
        "approvalRequired": record.approval_required,
        "approval": record.approval,
        "createdAt": record.created_at,
        "updatedAt": record.updated_at,
        "actor": record.actor,
        "agentId": record.agent_id,
        "toolIdentity": record.tool_identity,
        "incidentId": record.request.incident_id,
        "incidentLinks": record.request.incident_links,
        "intentSummary": record.request.intent,
        "proposedPlanHash": record.plan_hash,
        "request": record.request,
        "plan": record.plan,
        "affectedObjects": record.affected_objects,
        "warnings": record.warnings,
        "progressSnapshots": record.progress_snapshots,
        "stateTransitions": record.state_transitions,
        "auditLog": record.audit_log,
        "executionResult": record.execution_result,
        "verificationResult": record.verification_result,
        "preApplyAlertCount": record.pre_apply_alert_count,
    }))
}

fn affected_objects(request: &PlanRequest) -> Value {
    let mut pipelines = Vec::new();
    let mut outputs = Vec::new();
    for change in &request.proposed_changes {
        if let Some(pid) = change
            .pipeline_id
            .as_deref()
            .or(request.pipeline_id.as_deref())
            && !pipelines.iter().any(|existing: &String| existing == pid)
        {
            pipelines.push(pid.to_string());
        }
        if let Some(output_id) = &change.output_id
            && !outputs.iter().any(|existing| existing == output_id)
        {
            outputs.push(output_id.clone());
        }
    }
    pipelines.sort();
    outputs.sort();
    serde_json::json!({
        "pipelineIds": pipelines,
        "outputIds": outputs,
    })
}

fn plan_hash(request: &PlanRequest) -> String {
    let raw = serde_json::to_vec(request).unwrap_or_default();
    let digest = Sha256::digest(raw);
    format!("sha256:{}", hex_prefix(&digest, 32))
}

fn operation_idempotency_hash(request: &OperationCreateRequest, plan_hash: &str) -> String {
    let raw = serde_json::to_vec(&(request, plan_hash)).unwrap_or_default();
    let digest = Sha256::digest(raw);
    format!("sha256:{}", hex_prefix(&digest, 32))
}

fn operation_id(request: &OperationCreateRequest, created_at: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(created_at.as_bytes());
    hasher.update(serde_json::to_vec(request).unwrap_or_default());
    hasher.update(rand::random::<[u8; 16]>());
    let digest = hasher.finalize();
    format!("op_{}", hex_prefix(&digest, 16))
}

fn audit_event(kind: &str, at: &str, summary: &str, details: Value) -> Value {
    serde_json::json!({
        "kind": kind,
        "at": at,
        "summary": summary,
        "details": details,
    })
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .flat_map(|b| [b >> 4, b & 0x0f])
        .take(len)
        .map(|n| char::from_digit(n as u32, 16).unwrap_or('0'))
        .collect()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_core::types::ProposedChange;
    use crate::agent_plane::{GraphPreview, ImpactPreview, ValidationResult};

    fn test_request(idempotency_key: Option<String>) -> OperationCreateRequest {
        OperationCreateRequest {
            intent: "add output".to_string(),
            pipeline_id: Some("pipe".to_string()),
            proposed_changes: vec![ProposedChange {
                kind: "addOutput".to_string(),
                pipeline_id: Some("pipe".to_string()),
                output_id: Some("out".to_string()),
                name: Some("Output".to_string()),
                url: Some("rtmp://example.test/live/out".to_string()),
                monitoring_url: None,
                config: None,
                desired_state: Some("stopped".to_string()),
            }],
            idempotency_key,
            actor: Some("test".to_string()),
            agent_id: Some("agent".to_string()),
            tool_identity: Some("unit-test".to_string()),
            incident_id: None,
            incident_links: Vec::new(),
        }
    }

    fn test_plan() -> PlanResponse {
        PlanResponse {
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            plan_id: "plan-test".to_string(),
            status: "draft",
            intent: "add output".to_string(),
            execution_enabled: true,
            execution_note: "test",
            steps: Vec::new(),
            validation: ValidationResult {
                valid: true,
                errors: Vec::new(),
                warnings: Vec::new(),
            },
            graph_preview: GraphPreview {
                mode: "test",
                added_nodes: Vec::new(),
                removed_nodes: Vec::new(),
                changed_edges: Vec::new(),
                notes: Vec::new(),
            },
            impact: ImpactPreview {
                affected_pipelines: vec!["pipe".to_string()],
                affected_outputs: vec!["out".to_string()],
                shared_stage_candidates: Vec::new(),
                operator_summary: "test".to_string(),
                engineering_notes: Vec::new(),
            },
        }
    }

    fn test_record(
        operation_id: &str,
        idempotency_key: Option<String>,
        created_at: &str,
    ) -> OperationRecord {
        let request = test_request(idempotency_key.clone());
        let plan_hash = plan_hash(&request.plan_request());
        OperationRecord {
            operation_id: operation_id.to_string(),
            idempotency_key,
            status: OperationStatus::AwaitingApproval,
            idempotency_hash: request
                .idempotency_key
                .as_ref()
                .map(|_| operation_idempotency_hash(&request, &plan_hash)),
            plan_hash,
            request,
            plan: test_plan(),
            approval: None,
            approval_required: true,
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            actor: "test".to_string(),
            agent_id: "agent".to_string(),
            tool_identity: "unit-test".to_string(),
            affected_objects: serde_json::json!({"pipelineIds": ["pipe"], "outputIds": ["out"]}),
            warnings: Vec::new(),
            progress_snapshots: Vec::new(),
            state_transitions: Vec::new(),
            audit_log: Vec::new(),
            execution_result: None,
            verification_result: None,
            pre_apply_alert_count: Some(0),
        }
    }

    #[test]
    fn record_limit_evicts_oldest_and_removes_idempotency_key() {
        let store = AgentExecutionStore::default();
        {
            let mut state = lock_or_recover(&store.state);
            state.records.insert(
                "old".to_string(),
                test_record("old", Some("old-key".to_string()), "2026-01-01T00:00:00Z"),
            );
            for index in 0..MAX_AGENT_EXECUTION_RECORDS {
                let operation_id = format!("new-{index}");
                state.records.insert(
                    operation_id.clone(),
                    test_record(
                        &operation_id,
                        Some(format!("new-key-{index}")),
                        "2026-01-02T00:00:00Z",
                    ),
                );
            }
            state
                .idempotency
                .insert("old-key".to_string(), "old".to_string());
            for index in 0..MAX_AGENT_EXECUTION_RECORDS {
                state
                    .idempotency
                    .insert(format!("new-key-{index}"), format!("new-{index}"));
            }
        }

        {
            let mut state = lock_or_recover(&store.state);
            AgentExecutionStore::enforce_record_limit(&mut state);
        }

        assert!(store.get("old").is_none());
        assert_eq!(
            lock_or_recover(&store.state).records.len(),
            MAX_AGENT_EXECUTION_RECORDS
        );
        assert!(
            !lock_or_recover(&store.state)
                .idempotency
                .contains_key("old-key")
        );
    }

    #[test]
    fn poisoned_record_lock_does_not_panic_reads() {
        let store = AgentExecutionStore::default();
        lock_or_recover(&store.state).records.insert(
            "op".to_string(),
            test_record("op", Some("key".to_string()), "2026-01-01T00:00:00Z"),
        );

        let _ = std::panic::catch_unwind(|| {
            let _guard = store.state.lock().unwrap();
            panic!("poison records lock");
        });

        assert!(store.get("op").is_some());
    }

    #[test]
    fn idempotency_reuse_requires_matching_request_hash() {
        let store = AgentExecutionStore::default();
        let request = test_request(Some("same-key".to_string()));
        let first = store
            .create(request.clone(), test_plan(), 0)
            .expect("first create succeeds");
        assert!(!first.reused);

        let replay = store
            .create(request, test_plan(), 0)
            .expect("matching replay succeeds");
        assert!(replay.reused);

        let mut changed = test_request(Some("same-key".to_string()));
        changed.intent = "remove output".to_string();
        let err = store
            .create(changed, test_plan(), 0)
            .expect_err("different request with same key should conflict");

        assert_eq!(err, OperationStoreError::IdempotencyConflict);
    }

    #[test]
    fn create_and_approve_use_authenticated_dashboard_identity() {
        let store = AgentExecutionStore::default();
        let mut request = test_request(Some("identity-key".to_string()));
        request.actor = Some("spoofed-actor".to_string());
        request.agent_id = Some("spoofed-agent".to_string());
        request.tool_identity = Some("spoofed-tool".to_string());

        let created = store
            .create(request, test_plan(), 0)
            .expect("create succeeds");
        let operation_id = created.operation["operationId"].as_str().unwrap();
        let record = store.get(operation_id).expect("record stored");

        assert_eq!(record.actor, AUTHENTICATED_DASHBOARD_ACTOR);
        assert_eq!(record.agent_id, AUTHENTICATED_DASHBOARD_ACTOR);
        assert_eq!(record.tool_identity, AGENT_EXECUTION_TOOL_IDENTITY);

        let approved = store
            .approve(
                operation_id,
                ApprovalRequest {
                    approved_by: "spoofed-approver".to_string(),
                    reason: Some("approved from test".to_string()),
                },
            )
            .expect("approval succeeds");

        assert_eq!(
            approved.approval.unwrap().approved_by,
            AUTHENTICATED_DASHBOARD_APPROVER
        );
    }

    #[test]
    fn concurrent_create_with_same_idempotency_key_creates_exactly_one_record() {
        let store = std::sync::Arc::new(AgentExecutionStore::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = std::sync::Arc::clone(&store);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let request = test_request(Some("race-key".to_string()));
                    barrier.wait();
                    store
                        .create(request, test_plan(), 0)
                        .expect("create should not error for identical requests")
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread should not panic"))
            .collect();

        let operation_ids: std::collections::HashSet<_> = results
            .iter()
            .map(|result| {
                result.operation["operationId"]
                    .as_str()
                    .expect("operationId present")
                    .to_string()
            })
            .collect();
        assert_eq!(
            operation_ids.len(),
            1,
            "all concurrent creates with the same idempotency key must resolve to one operation"
        );

        let reused_count = results.iter().filter(|result| result.reused).count();
        assert_eq!(
            reused_count, 7,
            "exactly one create should win and the rest should be reported as reused"
        );

        assert_eq!(lock_or_recover(&store.state).records.len(), 1);
    }
}
