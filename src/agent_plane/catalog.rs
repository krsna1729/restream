use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub generated_at: String,
    pub feature: &'static str,
    pub version: u32,
    pub compiled_in: bool,
    pub execution_enabled: bool,
    pub read_tools: Vec<&'static str>,
    pub planning_tools: Vec<&'static str>,
    pub execution_tools: Vec<&'static str>,
    pub routes: Value,
    pub schemas: Value,
    pub redaction: Value,
    pub notes: Vec<&'static str>,
}

pub fn capabilities() -> AgentCapabilities {
    let execution_enabled = cfg!(feature = "agent-execution");
    AgentCapabilities {
        generated_at: now(),
        feature: "agent-plane",
        version: 1,
        compiled_in: true,
        execution_enabled,
        read_tools: vec![
            "get_agent_context",
            "investigate_pipeline_issue",
            "trace_output_path",
            "find_first_unhealthy_node",
            "explain_degradation",
            "estimate_change_impact",
            "inspect_resource_map",
            "inspect_desired_vs_actual",
            "inspect_diagnostics_summary",
        ],
        planning_tools: vec![
            "plan_pipeline_change",
            "validate_change",
            "preview_graph_diff",
            "estimate_change_impact",
        ],
        execution_tools: if execution_enabled {
            vec![
                "create_agent_operation",
                "get_agent_operation",
                "approve_agent_operation",
                "apply_agent_operation",
                "verify_agent_operation",
            ]
        } else {
            Vec::new()
        },
        routes: route_catalog(),
        schemas: schema_catalog(),
        redaction: redaction_policy(),
        notes: if execution_enabled {
            vec![
                "Phase 6 execution is compiled in.",
                "Operations are approval-gated and emit audit/verification records.",
                "Core operator routes are intentionally omitted from the agent catalog because their responses are not guaranteed to be redacted.",
            ]
        } else {
            vec![
                "Phase 4 is read/planning only.",
                "Phase 6 execution is intentionally not compiled into this feature.",
                "Core operator routes are intentionally omitted from the agent catalog because their responses are not guaranteed to be redacted.",
            ]
        },
    }
}

pub fn redaction_policy() -> Value {
    serde_json::json!({
        "policy": "agentContextV1",
        "streamKeys": "raw stream keys are replaced with stable SHA-256 fingerprints",
        "urls": "raw URLs are replaced with scheme, host, and stable SHA-256 fingerprints",
        "credentials": "credential-bearing fields are recursively redacted when represented as streamKey, stream_key, targetUrl, or url",
        "recursiveFields": ["streamKey", "stream_key", "targetUrl", "url"],
        "fingerprint": {
            "algorithm": "sha256",
            "encoding": "hex",
            "prefixChars": 16
        }
    })
}

pub fn route_catalog() -> Value {
    serde_json::json!([
        {"tool": "get_agent_capabilities", "method": "GET", "path": "/api/v1/agent/capabilities", "auth": "session", "feature": "agent-plane", "mutates": false, "responseSchema": "AgentCapabilities"},
        {"tool": "get_agent_context", "method": "GET", "path": "/api/v1/agent/context", "auth": "session", "feature": "agent-plane", "mutates": false, "responseSchema": "AgentContextV1"},
        {"tool": "investigate_pipeline_issue", "method": "POST", "path": "/api/v1/agent/investigations", "auth": "session", "feature": "agent-plane", "mutates": false, "requestSchema": "InvestigationRequest", "responseSchema": "InvestigationResponse"},
        {"tool": "plan_pipeline_change", "method": "POST", "path": "/api/v1/agent/plans", "auth": "session", "feature": "agent-plane", "mutates": false, "requestSchema": "PlanRequest", "responseSchema": "PlanResponse"},
        {"tool": "validate_change", "method": "POST", "path": "/api/v1/agent/plans/validate", "auth": "session", "feature": "agent-plane", "mutates": false, "requestSchema": "PlanRequest", "responseSchema": "ValidationResult"},
        {"tool": "preview_graph_diff", "method": "POST", "path": "/api/v1/agent/graph-diff-preview", "auth": "session", "feature": "agent-plane", "mutates": false, "requestSchema": "PlanRequest", "responseSchema": "GraphDiffPreview"},
        {"tool": "create_agent_operation", "method": "POST", "path": "/api/v1/agent/operations", "auth": "session", "feature": "agent-execution", "compiledIn": cfg!(feature = "agent-execution"), "mutates": true, "requestSchema": "OperationCreateRequest", "responseSchema": "OperationRecord"},
        {"tool": "get_agent_operation", "method": "GET", "path": "/api/v1/agent/operations/:operation_id", "auth": "session", "feature": "agent-execution", "compiledIn": cfg!(feature = "agent-execution"), "mutates": false, "responseSchema": "OperationRecord"},
        {"tool": "approve_agent_operation", "method": "POST", "path": "/api/v1/agent/operations/:operation_id/approve", "auth": "session", "feature": "agent-execution", "compiledIn": cfg!(feature = "agent-execution"), "mutates": true, "requestSchema": "ApprovalRequest", "responseSchema": "OperationRecord"},
        {"tool": "apply_agent_operation", "method": "POST", "path": "/api/v1/agent/operations/:operation_id/apply", "auth": "session", "feature": "agent-execution", "compiledIn": cfg!(feature = "agent-execution"), "mutates": true, "responseSchema": "OperationRecord"},
        {"tool": "verify_agent_operation", "method": "POST", "path": "/api/v1/agent/operations/:operation_id/verify", "auth": "session", "feature": "agent-execution", "compiledIn": cfg!(feature = "agent-execution"), "mutates": true, "responseSchema": "OperationRecord"},
        {"tool": "verify_agent_operation_by_body", "method": "POST", "path": "/api/v1/agent/verify", "auth": "session", "feature": "agent-execution", "compiledIn": cfg!(feature = "agent-execution"), "mutates": true, "requestSchema": "VerifyRequest", "responseSchema": "OperationRecord"}
    ])
}

pub fn schema_catalog() -> Value {
    serde_json::json!({
        "InvestigationRequest": {
            "type": "object",
            "fields": {
                "workflow": {"type": "string", "optional": true},
                "pipelineId": {"type": "string", "optional": true},
                "outputId": {"type": "string", "optional": true},
                "eventLimit": {"type": "integer", "default": 100, "max": 1000}
            }
        },
        "PlanRequest": {
            "type": "object",
            "required": ["intent"],
            "fields": {
                "intent": {"type": "string"},
                "pipelineId": {"type": "string", "optional": true},
                "proposedChanges": {"type": "array", "items": "ProposedChange", "default": []}
            }
        },
        "ProposedChange": {
            "type": "object",
            "required": ["kind"],
            "fields": {
                "kind": {"type": "string", "enum": ["addOutput", "updateOutput", "removeOutput", "startOutput", "stopOutput"]},
                "pipelineId": {"type": "string", "optional": true},
                "outputId": {"type": "string", "optional": true},
                "name": {"type": "string", "optional": true},
                "url": {"type": "string", "optional": true, "redacted": true},
                "monitoringUrl": {"type": "string", "optional": true, "redacted": true},
                "config": {"type": "object", "optional": true},
                "desiredState": {"type": "string", "optional": true, "enum": ["running", "stopped"]}
            }
        },
        "OperationCreateRequest": {
            "type": "object",
            "required": ["intent"],
            "fields": {
                "intent": {"type": "string"},
                "pipelineId": {"type": "string", "optional": true},
                "proposedChanges": {"type": "array", "items": "ProposedChange", "default": []},
                "idempotencyKey": {"type": "string", "optional": true},
                "actor": {"type": "string", "optional": true},
                "agentId": {"type": "string", "optional": true},
                "toolIdentity": {"type": "string", "optional": true},
                "incidentId": {"type": "string", "optional": true},
                "incidentLinks": {"type": "array", "items": "string", "default": []}
            }
        },
        "ApprovalRequest": {
            "type": "object",
            "required": ["approvedBy"],
            "fields": {
                "approvedBy": {"type": "string"},
                "reason": {"type": "string", "optional": true}
            }
        },
        "VerifyRequest": {
            "type": "object",
            "required": ["operationId"],
            "fields": {
                "operationId": {"type": "string"}
            }
        },
        "OperationRecord": {
            "type": "object",
            "sections": ["operationId", "status", "approval", "request", "plan", "affectedObjects", "stateTransitions", "auditLog", "executionResult", "verificationResult"]
        },
        "AgentContextV1": {
            "type": "object",
            "sections": ["api", "engine", "features", "configuration", "state", "runtime", "desiredVsActual", "diagnostics", "dependencies", "media", "storage", "redaction"]
        }
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
