//! Approval asked of the MCP client in the middle of an apply (elicitation):
//! the prompt names what the change touches, and only an explicit approval
//! lets the apply go ahead.

use crate::transport::{CallContext, ClientRequestError};
use restream_agent_contract::ApprovalRequest;
use serde_json::{Value, json};

/// The label recorded as who the request says approved. The product audits
/// the authenticated principal separately.
const ELICITED_APPROVER: &str = "mcp-elicitation";

/// The client's answer to an approval prompt.
#[derive(Debug)]
pub(crate) enum Approval {
    Granted(ApprovalRequest),
    /// No approval: why, as reported in the tool result.
    Withheld(&'static str),
}

/// Ask the client to approve `operation` (a public operation record).
pub(crate) async fn ask(ctx: &CallContext, operation: &Value) -> Approval {
    let answer = ctx
        .request(
            "elicitation/create",
            json!({
                "message": prompt(operation),
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "approve": {
                            "type": "boolean",
                            "title": "Approve this change",
                            "description": "Apply the change now.",
                        },
                        "reason": {
                            "type": "string",
                            "title": "Reason",
                            "description": "Recorded in the operation's audit log.",
                        },
                    },
                    "required": ["approve"],
                },
            }),
        )
        .await;
    match answer {
        Ok(result) => decide(&result),
        Err(ClientRequestError::Unsupported) => Approval::Withheld("unsupported"),
        Err(ClientRequestError::Rejected(_)) => Approval::Withheld("rejected"),
        Err(ClientRequestError::Disconnected) => Approval::Withheld("disconnected"),
    }
}

fn decide(result: &Value) -> Approval {
    match result.get("action").and_then(Value::as_str) {
        Some("accept") => {}
        Some("decline") => return Approval::Withheld("declined"),
        Some("cancel") => return Approval::Withheld("cancelled"),
        _ => return Approval::Withheld("invalid"),
    }
    let content = result.get("content");
    if content.and_then(|content| content.get("approve")) != Some(&Value::Bool(true)) {
        return Approval::Withheld("declined");
    }
    Approval::Granted(ApprovalRequest {
        approved_by: ELICITED_APPROVER.to_string(),
        reason: content
            .and_then(|content| content.get("reason"))
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
            .map(str::to_owned),
    })
}

fn prompt(operation: &Value) -> String {
    let id = operation["operationId"].as_str().unwrap_or("?");
    let intent = operation["intentSummary"].as_str().unwrap_or("(no intent)");
    let mut prompt = format!("Approve operation {id}: {intent}");
    for (key, label) in [("pipelineIds", "Pipelines"), ("outputIds", "Outputs")] {
        let names = joined(&operation["affectedObjects"][key]);
        if !names.is_empty() {
            prompt.push_str(&format!("\n{label}: {names}"));
        }
    }
    let warnings = joined(&operation["warnings"]);
    if !warnings.is_empty() {
        prompt.push_str(&format!("\nWarnings: {warnings}"));
    }
    prompt
}

/// A JSON list of strings, comma-joined.
fn joined(list: &Value) -> String {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}
