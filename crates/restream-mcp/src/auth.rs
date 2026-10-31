//! MCP transport auth/session context.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpAuthContext {
    pub session_cookie: Option<String>,
    pub bearer_token: Option<String>,
}
