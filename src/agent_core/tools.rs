//! The agent tools: the one list that capability discovery (`agent_plane`)
//! and the MCP catalog and dispatch (`agent_mcp`) both follow, so neither
//! advertises a tool the other cannot serve.

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
}

pub const AGENT_TOOLS: &[AgentTool] = &[
    AgentTool {
        name: "get_agent_capabilities",
        kind: AgentToolKind::Read,
    },
    AgentTool {
        name: "get_agent_context",
        kind: AgentToolKind::Read,
    },
    AgentTool {
        name: "investigate_pipeline_issue",
        kind: AgentToolKind::Read,
    },
    AgentTool {
        name: "plan_pipeline_change",
        kind: AgentToolKind::Planning,
    },
    AgentTool {
        name: "validate_change",
        kind: AgentToolKind::Planning,
    },
    AgentTool {
        name: "preview_graph_diff",
        kind: AgentToolKind::Planning,
    },
    AgentTool {
        name: "create_agent_operation",
        kind: AgentToolKind::Execution,
    },
    AgentTool {
        name: "get_agent_operation",
        kind: AgentToolKind::Execution,
    },
    AgentTool {
        name: "approve_agent_operation",
        kind: AgentToolKind::Execution,
    },
    AgentTool {
        name: "apply_agent_operation",
        kind: AgentToolKind::Execution,
    },
    AgentTool {
        name: "verify_agent_operation",
        kind: AgentToolKind::Execution,
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
