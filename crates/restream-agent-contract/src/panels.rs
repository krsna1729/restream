//! Small live panels a host may render beside a tool's result (MCP Apps,
//! extension `io.modelcontextprotocol/ui`). Each panel declares the fields it
//! reads from the tool's structured result: the sidecar renders exactly
//! these, and the product's contract tests check its results carry them
//! ([`Panel::mismatches`]).

use serde::Serialize;
use serde_json::Value;

/// The MCP Apps extension identifier a host advertises in its capabilities.
pub const UI_EXTENSION: &str = "io.modelcontextprotocol/ui";
/// The content type of a panel resource.
pub const PANEL_MIME_TYPE: &str = "text/html;profile=mcp-app";

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub uri: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [Field],
    /// A button that calls a tool with values from the result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<PanelAction>,
    /// The tool (without arguments) the panel calls to refresh itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<&'static str>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub label: &'static str,
    /// JSON pointer into the tool's structured result.
    pub pointer: &'static str,
    pub kind: FieldKind,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FieldKind {
    Text,
    Number,
    Bool,
    /// An array of strings.
    TextList,
    /// An array of objects, one row each; columns point into each row and
    /// hold a scalar or null.
    Rows {
        columns: &'static [Column],
    },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub label: &'static str,
    pub pointer: &'static str,
}

/// Shown while the string at `when_pointer` equals `when_equals`; calls
/// `tool` with `{ id_argument: <string at id_pointer>, ...fixed }`.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelAction {
    pub label: &'static str,
    pub tool: &'static str,
    pub when_pointer: &'static str,
    pub when_equals: &'static str,
    pub id_pointer: &'static str,
    pub id_argument: &'static str,
    /// Constant arguments, as `(name, value)`.
    pub fixed: &'static [(&'static str, &'static str)],
}

impl Panel {
    /// Every field (and action input) `payload` lacks or holds with the
    /// wrong type, as `pointer: problem`. Empty when the panel can render
    /// the payload in full.
    pub fn mismatches(&self, payload: &Value) -> Vec<String> {
        let mut found = Vec::new();
        for field in self.fields {
            if let Err(problem) = check(payload.pointer(field.pointer), &field.kind) {
                found.push(format!("{}: {problem}", field.pointer));
            }
        }
        if let Some(action) = &self.action {
            for pointer in [action.when_pointer, action.id_pointer] {
                if let Err(problem) = check(payload.pointer(pointer), &FieldKind::Text) {
                    found.push(format!("{pointer}: {problem}"));
                }
            }
        }
        found
    }
}

fn check(value: Option<&Value>, kind: &FieldKind) -> Result<(), String> {
    let Some(value) = value else {
        return Err("missing".into());
    };
    let fits = match kind {
        FieldKind::Text => value.is_string(),
        FieldKind::Number => value.is_number(),
        FieldKind::Bool => value.is_boolean(),
        FieldKind::TextList => value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string)),
        FieldKind::Rows { columns } => {
            let Some(rows) = value.as_array() else {
                return Err(format!("expected rows, got {value}"));
            };
            for (index, row) in rows.iter().enumerate() {
                for column in *columns {
                    match row.pointer(column.pointer) {
                        None => return Err(format!("row {index} lacks {}", column.pointer)),
                        Some(Value::Array(_) | Value::Object(_)) => {
                            return Err(format!("row {index} {} is not a scalar", column.pointer));
                        }
                        Some(_) => {}
                    }
                }
            }
            true
        }
    };
    if fits {
        Ok(())
    } else {
        Err(format!("expected {kind:?}, got {value}"))
    }
}

/// An agent operation's plan as a reviewable diff, with an Approve button
/// that calls `approve_agent_operation` (the same approval path as the
/// tool) while the operation awaits approval.
pub const OPERATION_PANEL: Panel = Panel {
    uri: "ui://restream/operation-plan",
    name: "operation-plan",
    title: "Change plan",
    description: "An agent operation's proposed changes, impact and validation, with Approve.",
    fields: &[
        Field {
            label: "Operation",
            pointer: "/operationId",
            kind: FieldKind::Text,
        },
        Field {
            label: "Status",
            pointer: "/status",
            kind: FieldKind::Text,
        },
        Field {
            label: "Intent",
            pointer: "/intentSummary",
            kind: FieldKind::Text,
        },
        Field {
            label: "Summary",
            pointer: "/plan/impact/operatorSummary",
            kind: FieldKind::Text,
        },
        Field {
            label: "Valid",
            pointer: "/plan/validation/valid",
            kind: FieldKind::Bool,
        },
        Field {
            label: "Changes",
            pointer: "/request/proposedChanges",
            kind: FieldKind::Rows {
                columns: &[
                    Column {
                        label: "Kind",
                        pointer: "/kind",
                    },
                    Column {
                        label: "Pipeline",
                        pointer: "/pipelineId",
                    },
                    Column {
                        label: "Output",
                        pointer: "/outputId",
                    },
                    Column {
                        label: "Name",
                        pointer: "/name",
                    },
                    Column {
                        label: "Desired state",
                        pointer: "/desiredState",
                    },
                ],
            },
        },
        Field {
            label: "Validation errors",
            pointer: "/plan/validation/errors",
            kind: FieldKind::Rows {
                columns: &[
                    Column {
                        label: "Code",
                        pointer: "/code",
                    },
                    Column {
                        label: "Message",
                        pointer: "/message",
                    },
                ],
            },
        },
        Field {
            label: "Pipelines",
            pointer: "/affectedObjects/pipelineIds",
            kind: FieldKind::TextList,
        },
        Field {
            label: "Outputs",
            pointer: "/affectedObjects/outputIds",
            kind: FieldKind::TextList,
        },
        Field {
            label: "Warnings",
            pointer: "/warnings",
            kind: FieldKind::TextList,
        },
        Field {
            label: "Graph notes",
            pointer: "/plan/graphPreview/notes",
            kind: FieldKind::TextList,
        },
    ],
    action: Some(PanelAction {
        label: "Approve",
        tool: "approve_agent_operation",
        when_pointer: "/status",
        when_equals: "awaitingApproval",
        id_pointer: "/operationId",
        id_argument: "operationId",
        fixed: &[("approvedBy", "mcp-app")],
    }),
    refresh: None,
};

/// Desired against actual state for every pipeline, and the open alerts;
/// refreshes itself through `get_agent_context`.
pub const PIPELINE_HEALTH_PANEL: Panel = Panel {
    uri: "ui://restream/pipeline-health",
    name: "pipeline-health",
    title: "Pipeline health",
    description: "Output convergence per pipeline and the open alerts, refreshed live.",
    fields: &[
        Field {
            label: "Generated",
            pointer: "/generatedAt",
            kind: FieldKind::Text,
        },
        Field {
            label: "Pipelines",
            pointer: "/desiredVsActual/summary/pipelines",
            kind: FieldKind::Number,
        },
        Field {
            label: "Outputs",
            pointer: "/desiredVsActual/summary/outputs",
            kind: FieldKind::Number,
        },
        Field {
            label: "Converged",
            pointer: "/desiredVsActual/summary/convergedOutputs",
            kind: FieldKind::Number,
        },
        Field {
            label: "Pending",
            pointer: "/desiredVsActual/summary/pendingOutputs",
            kind: FieldKind::Number,
        },
        Field {
            label: "Drifted",
            pointer: "/desiredVsActual/summary/driftedOutputs",
            kind: FieldKind::Number,
        },
        Field {
            label: "Retries exhausted",
            pointer: "/desiredVsActual/summary/retriesExhaustedOutputs",
            kind: FieldKind::Number,
        },
        Field {
            label: "Pipelines",
            pointer: "/desiredVsActual/pipelines",
            kind: FieldKind::Rows {
                columns: &[
                    Column {
                        label: "Pipeline",
                        pointer: "/name",
                    },
                    Column {
                        label: "Input",
                        pointer: "/input/actualStatus",
                    },
                    Column {
                        label: "Recording",
                        pointer: "/recording/reason",
                    },
                ],
            },
        },
        Field {
            label: "Alerts",
            pointer: "/runtime/alerts",
            kind: FieldKind::Rows {
                columns: &[
                    Column {
                        label: "Severity",
                        pointer: "/severity",
                    },
                    Column {
                        label: "Alert",
                        pointer: "/title",
                    },
                    Column {
                        label: "Action",
                        pointer: "/recommendedAction",
                    },
                ],
            },
        },
    ],
    action: None,
    refresh: Some("get_agent_context"),
};

/// Every panel, in resource-list order.
pub const PANELS: &[&Panel] = &[&OPERATION_PANEL, &PIPELINE_HEALTH_PANEL];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operation() -> Value {
        json!({
            "operationId": "op-1",
            "status": "awaitingApproval",
            "intentSummary": "add a backup",
            "plan": {
                "impact": { "operatorSummary": "one output" },
                "validation": { "valid": true, "errors": [] },
                "graphPreview": { "notes": [] },
            },
            "request": { "proposedChanges": [{
                "kind": "addOutput", "pipelineId": "p", "outputId": null,
                "name": "backup", "desiredState": null,
            }] },
            "affectedObjects": { "pipelineIds": ["p"], "outputIds": [] },
            "warnings": [],
        })
    }

    #[test]
    fn a_complete_payload_has_no_mismatch() {
        assert_eq!(
            OPERATION_PANEL.mismatches(&operation()),
            Vec::<String>::new()
        );
    }

    /// Each way a producer can drift from a panel is named by its pointer.
    #[test]
    fn missing_fields_wrong_types_and_bad_rows_are_named() {
        let mut payload = operation();
        payload.as_object_mut().unwrap().remove("warnings");
        payload["plan"]["validation"]["valid"] = json!("yes");
        payload["request"]["proposedChanges"][0]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        payload["affectedObjects"]["outputIds"] = json!([1]);
        payload["operationId"] = json!(7);

        let found = OPERATION_PANEL.mismatches(&payload);
        for pointer in [
            "/warnings: missing",
            "/plan/validation/valid: expected Bool",
            "/request/proposedChanges: row 0 lacks /kind",
            "/affectedObjects/outputIds: expected TextList",
            "/operationId: expected Text",
        ] {
            assert!(
                found.iter().any(|problem| problem.starts_with(pointer)),
                "{pointer} not reported in {found:?}"
            );
        }
    }

    #[test]
    fn a_row_column_holding_an_object_is_a_mismatch() {
        let mut payload = operation();
        payload["request"]["proposedChanges"][0]["name"] = json!({"nested": true});
        assert_eq!(
            OPERATION_PANEL.mismatches(&payload),
            ["/request/proposedChanges: row 0 /name is not a scalar"]
        );
    }

    #[test]
    fn panel_uris_are_ui_resources_and_unique() {
        let mut uris: Vec<_> = PANELS.iter().map(|panel| panel.uri).collect();
        assert!(uris.iter().all(|uri| uri.starts_with("ui://")));
        uris.sort_unstable();
        uris.dedup();
        assert_eq!(uris.len(), PANELS.len());
    }
}
