//! MCP transport adapters for the agent-facing control surface.
//! This module owns the protocol boundary for stdio and streamable-HTTP MCP
//! sessions: request framing, JSON-RPC flow, initialization, and
//! transport-specific origin/header policy. A tool call may talk to its
//! client while it runs ([`CallContext`]): progress notifications, and
//! requests such as an approval prompt, whose answers the transport routes
//! back. Tool execution stays in handlers and backends.

mod session;
mod stdio;
mod streamable_http;

use crate::handlers::AgentToolHandler;
use crate::panels;
use crate::tools::available_tool_catalog;
use restream_agent_contract::panels::UI_EXTENSION;
use restream_agent_contract::{AgentBackend, AgentError, tool_panel};
use serde_json::{Value, json};
use std::sync::Arc;

use session::Session;
pub use session::{CallContext, ClientRequestError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMode {
    StreamableHttp,
    Stdio,
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub mode: TransportMode,
    pub bind_addr: String,
    pub allowed_origins: Vec<String>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            mode: TransportMode::StreamableHttp,
            bind_addr: "127.0.0.1:4040".to_string(),
            allowed_origins: Vec::new(),
        }
    }
}

pub async fn run_server<B>(config: TransportConfig, backend: Arc<B>) -> Result<(), AgentError>
where
    B: AgentBackend + 'static,
{
    let handler = Arc::new(AgentToolHandler::new(backend));
    match config.mode {
        TransportMode::Stdio => stdio::run(handler).await,
        TransportMode::StreamableHttp => streamable_http::run(config, handler).await,
    }
}

const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "restream-mcp";

/// What one inbound message asks of its transport.
enum Inbound {
    /// Send this message back.
    Reply(Value),
    /// Run a tool and send its result back under `id`.
    ToolCall {
        id: Value,
        name: String,
        arguments: Value,
        progress_token: Option<Value>,
    },
    /// Nothing to send: a notification, or a client's answer to one of our
    /// requests (handed to its waiter).
    Nothing,
}

/// Whether normal operations need the initialize handshake first: stdio
/// sessions do; a stateless HTTP request has no handshake to wait for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Handshake {
    Required,
    NotRequired,
}

fn classify(message: Value, session: Option<&Session>, handshake: Handshake) -> Inbound {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    let Some(method) = method else {
        if message.get("result").is_some() || message.get("error").is_some() {
            if let Some(session) = session {
                session.route_response(&message);
            }
            return Inbound::Nothing;
        }
        return match id {
            Some(id) => Inbound::Reply(error_response(id, -32600, "missing method".into(), None)),
            None => Inbound::Nothing,
        };
    };

    if method == "initialize" {
        if let Some(session) = session {
            session.on_initialize(&params);
        }
        return Inbound::Reply(initialize_response(id.unwrap_or(Value::Null)));
    }
    if method == "notifications/initialized" {
        if let Some(session) = session {
            session.on_initialized();
        }
        return Inbound::Nothing;
    }
    // Notifications never get a reply (including unknown ones).
    let Some(id) = id else {
        return Inbound::Nothing;
    };
    match method {
        "ping" => Inbound::Reply(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
        "tools/list" | "tools/call" | "resources/list" | "resources/read" => {
            if handshake == Handshake::Required
                && let Some(refusal) = handshake_refusal(&id, session)
            {
                return Inbound::Reply(refusal);
            }
            match method {
                "tools/list" => {
                    let panels = session.is_some_and(Session::renders_panels);
                    return Inbound::Reply(tools_list_response(id, panels));
                }
                "resources/list" => return Inbound::Reply(result_response(id, panels::list())),
                "resources/read" => {
                    let uri = params
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    return Inbound::Reply(match panels::read(uri) {
                        Some(contents) => result_response(id, contents),
                        None => error_response(
                            id,
                            -32002,
                            "Resource not found".into(),
                            Some(json!({ "uri": uri })),
                        ),
                    });
                }
                _ => {}
            }
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Inbound::Reply(error_response(
                    id,
                    -32602,
                    "tools/call requires a string 'name'".into(),
                    None,
                ));
            };
            Inbound::ToolCall {
                id,
                name: name.to_owned(),
                arguments: normalize_tool_arguments(
                    params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                ),
                progress_token: params.pointer("/_meta/progressToken").cloned(),
            }
        }
        other => Inbound::Reply(error_response(
            id,
            -32601,
            format!("unknown method '{other}'"),
            None,
        )),
    }
}

fn handshake_refusal(id: &Value, session: Option<&Session>) -> Option<Value> {
    let (initialize_seen, initialized) = session.map_or((false, false), Session::handshake);
    let message = if !initialize_seen {
        "server has not completed initialize negotiation"
    } else if !initialized {
        "client must send notifications/initialized before normal operations"
    } else {
        return None;
    };
    Some(error_response(id.clone(), -32002, message.into(), None))
}

async fn run_tool_call<B: AgentBackend>(
    handler: &AgentToolHandler<B>,
    ctx: &CallContext,
    id: Value,
    name: &str,
    arguments: Value,
) -> Value {
    tool_call_response(id, handler.dispatch(name, arguments, ctx).await)
}

fn initialize_response(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {
                    "listChanged": false
                },
                "resources": {},
                "extensions": {
                    UI_EXTENSION: {}
                }
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Use plan/approve/apply/verify workflows for mutations. Execution tools are only available when compiled in."
        }
    })
}

fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: String, data: Option<Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message,
    });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error
    })
}

fn normalize_tool_arguments(arguments: Value) -> Value {
    if arguments.is_object() {
        arguments
    } else {
        json!({})
    }
}

fn tool_result(result: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": result,
        "isError": is_error
    })
}

/// The tool list; for a client that renders panels, each tool with one names
/// it in `_meta.ui.resourceUri`.
fn tools_list_response(id: Value, panels: bool) -> Value {
    let tools = available_tool_catalog()
        .into_iter()
        .map(|tool| {
            let mut listed = json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            });
            if panels && let Some(panel) = tool_panel(tool.name) {
                listed["_meta"] = json!({ "ui": { "resourceUri": panel.uri } });
            }
            listed
        })
        .collect::<Vec<_>>();
    result_response(id, json!({ "tools": tools }))
}

fn tool_call_response(id: Value, result: Result<Value, AgentError>) -> Value {
    match result {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": tool_result(result, false)
        }),
        Err(err) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": tool_result(json!({
                "error": err.to_string(),
            }), true)
        }),
    }
}

#[cfg(test)]
mod tests;
