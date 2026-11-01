//! The agent tools: the one list that capability discovery (`restream-agent`)
//! and the MCP catalog and dispatch (`restream-mcp`) both follow, so neither
//! advertises a tool the other cannot serve.

use crate::panels::{OPERATION_PANEL, PIPELINE_HEALTH_PANEL, Panel};

/// What a tool does, as capability discovery groups it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentToolKind {
    Read,
    Planning,
    /// Compiled in only with the `agent-execution` feature.
    Execution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTool {
    pub name: &'static str,
    pub kind: AgentToolKind,
    /// The panel a host may render the tool's result in.
    pub panel: Option<&'static Panel>,
}

pub const AGENT_TOOLS: &[AgentTool] = &[
    AgentTool {
        name: "get_agent_capabilities",
        kind: AgentToolKind::Read,
        panel: None,
    },
    AgentTool {
        name: "get_agent_context",
        kind: AgentToolKind::Read,
        panel: Some(&PIPELINE_HEALTH_PANEL),
    },
    AgentTool {
        name: "investigate_pipeline_issue",
        kind: AgentToolKind::Read,
        panel: None,
    },
    AgentTool {
        name: "plan_pipeline_change",
        kind: AgentToolKind::Planning,
        panel: None,
    },
    AgentTool {
        name: "validate_change",
        kind: AgentToolKind::Planning,
        panel: None,
    },
    AgentTool {
        name: "preview_graph_diff",
        kind: AgentToolKind::Planning,
        panel: None,
    },
    AgentTool {
        name: "create_agent_operation",
        kind: AgentToolKind::Execution,
        panel: Some(&OPERATION_PANEL),
    },
    AgentTool {
        name: "get_agent_operation",
        kind: AgentToolKind::Execution,
        panel: Some(&OPERATION_PANEL),
    },
    AgentTool {
        name: "approve_agent_operation",
        kind: AgentToolKind::Execution,
        panel: None,
    },
    AgentTool {
        name: "apply_agent_operation",
        kind: AgentToolKind::Execution,
        panel: None,
    },
    AgentTool {
        name: "verify_agent_operation",
        kind: AgentToolKind::Execution,
        panel: None,
    },
];

/// The compiled-in tool names of one kind, in catalog order.
pub fn agent_tool_names(kind: AgentToolKind) -> Vec<&'static str> {
    if kind == AgentToolKind::Execution && !cfg!(feature = "agent-execution") {
        return Vec::new();
    }
    AGENT_TOOLS
        .iter()
        .filter(|tool| tool.kind == kind)
        .map(|tool| tool.name)
        .collect()
}

/// The panel a host may render `tool`'s result in.
pub fn tool_panel(tool: &str) -> Option<&'static Panel> {
    AGENT_TOOLS
        .iter()
        .find(|candidate| candidate.name == tool)
        .and_then(|candidate| candidate.panel)
}
