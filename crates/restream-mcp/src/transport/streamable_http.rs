//! Streamable HTTP: one POST per client message on `/mcp`. `initialize`
//! opens a session (`Mcp-Session-Id`); a tool call from a client that accepts
//! `text/event-stream` answers as an SSE stream carrying progress and the
//! server's requests (an approval prompt) before the result, and the client
//! POSTs its answers to those requests under the same session. Without a
//! session or SSE, a request is answered statelessly as plain JSON.

use super::session::{CALL_OUTBOX, CallContext, Session};
use super::{
    Handshake, Inbound, PROTOCOL_VERSION, TransportConfig, classify, error_response, run_tool_call,
};
use crate::handlers::AgentToolHandler;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::{ACCEPT, ALLOW, HeaderMap, HeaderName, HeaderValue, ORIGIN};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use restream_agent_contract::{AgentBackend, AgentError};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";

/// Open sessions kept per server. A client that never ends its session is
/// evicted (oldest first) rather than held forever.
const MAX_SESSIONS: usize = 256;

pub(super) struct HttpTransportState<B: AgentBackend> {
    handler: Arc<AgentToolHandler<B>>,
    allowed_origins: Vec<String>,
    sessions: Mutex<Sessions>,
}

#[derive(Default)]
struct Sessions {
    by_id: HashMap<String, Arc<Session>>,
    /// Session ids, oldest first.
    opened: VecDeque<String>,
}

impl Sessions {
    fn open(&mut self) -> (String, Arc<Session>) {
        while self.opened.len() >= MAX_SESSIONS {
            let Some(oldest) = self.opened.pop_front() else {
                break;
            };
            if let Some(session) = self.by_id.remove(&oldest) {
                session.close();
            }
        }
        let id = to_hex(&rand::random::<[u8; 16]>());
        let session = Arc::new(Session::default());
        self.by_id.insert(id.clone(), Arc::clone(&session));
        self.opened.push_back(id.clone());
        (id, session)
    }

    fn end(&mut self, id: &str) -> bool {
        let Some(session) = self.by_id.remove(id) else {
            return false;
        };
        self.opened.retain(|open| open != id);
        session.close();
        true
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl<B: AgentBackend> HttpTransportState<B> {
    pub(super) fn new(handler: Arc<AgentToolHandler<B>>, allowed_origins: Vec<String>) -> Self {
        Self {
            handler,
            allowed_origins,
            sessions: Mutex::default(),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, Sessions> {
        restream_sync::lock::lock(&self.sessions)
    }
}

pub(super) async fn run<B: AgentBackend + 'static>(
    config: TransportConfig,
    handler: Arc<AgentToolHandler<B>>,
) -> Result<(), AgentError> {
    let bind_addr = config.bind_addr;
    let app = router(Arc::new(HttpTransportState::new(
        handler,
        config.allowed_origins,
    )));
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|err| {
            AgentError::Transport(format!(
                "failed to bind MCP HTTP listener on {bind_addr}: {err}"
            ))
        })?;
    serve(listener, app).await
}

pub(super) fn router<B: AgentBackend + 'static>(state: Arc<HttpTransportState<B>>) -> Router {
    Router::new()
        .route(
            "/mcp",
            post(post_handler::<B>)
                .get(get_handler)
                .delete(delete_handler::<B>),
        )
        .route("/", post(post_handler::<B>))
        .with_state(state)
}

pub(super) async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
) -> Result<(), AgentError> {
    axum::serve(listener, app)
        .await
        .map_err(|err| AgentError::Transport(format!("MCP HTTP server failed: {err}")))
}

async fn post_handler<B: AgentBackend + 'static>(
    State(state): State<Arc<HttpTransportState<B>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_headers(&headers, &state.allowed_origins) {
        return *response;
    }

    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                error_response(
                    Value::Null,
                    -32700,
                    format!("invalid JSON-RPC payload: {err}"),
                    None,
                ),
            );
        }
    };

    let (session, opened) = if message.get("method").and_then(Value::as_str) == Some("initialize") {
        let (id, session) = state.sessions().open();
        (Some(session), Some(id))
    } else {
        match session_id(&headers) {
            None => (None, None),
            Some(id) => match state.sessions().by_id.get(id) {
                Some(session) => (Some(Arc::clone(session)), None),
                None => return plain_response(StatusCode::NOT_FOUND, "unknown MCP session"),
            },
        }
    };

    let mut response = match classify(message, session.as_deref(), Handshake::NotRequired) {
        Inbound::Reply(reply) => json_response(StatusCode::OK, reply),
        Inbound::Nothing => empty_response(StatusCode::ACCEPTED),
        Inbound::ToolCall {
            id,
            name,
            arguments,
            progress_token,
        } if accepts_event_stream(&headers) => {
            let (out, outbox) = mpsc::channel(CALL_OUTBOX);
            let handler = Arc::clone(&state.handler);
            let ctx = CallContext::streaming(session, out.clone(), progress_token);
            // The call runs to completion even if the client drops the
            // stream: an apply is not abandoned half way.
            tokio::spawn(async move {
                let response = run_tool_call(&handler, &ctx, id, &name, arguments).await;
                let _ = out.send(response).await;
            });
            event_stream(outbox)
        }
        Inbound::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            let ctx = CallContext::detached();
            json_response(
                StatusCode::OK,
                run_tool_call(&state.handler, &ctx, id, &name, arguments).await,
            )
        }
    };
    if let Some(id) = opened
        && let Ok(value) = HeaderValue::from_str(&id)
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static(MCP_SESSION_ID_HEADER), value);
    }
    response
}

/// Each message the call queues, as one SSE event; the stream ends once the
/// call has sent its result and dropped its senders.
fn event_stream(outbox: mpsc::Receiver<Value>) -> Response {
    let events = futures_util::stream::unfold(outbox, |mut outbox| async move {
        let message = outbox.recv().await?;
        Some((
            Ok::<_, Infallible>(Event::default().data(message.to_string())),
            outbox,
        ))
    });
    with_protocol_version(Sse::new(events).into_response())
}

async fn get_handler() -> Response {
    let mut response = plain_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "This MCP endpoint streams server messages in POST responses only.",
    );
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static("POST, DELETE"));
    response
}

async fn delete_handler<B: AgentBackend>(
    State(state): State<Arc<HttpTransportState<B>>>,
    headers: HeaderMap,
) -> Response {
    let Some(id) = session_id(&headers) else {
        return plain_response(StatusCode::BAD_REQUEST, "missing Mcp-Session-Id");
    };
    if state.sessions().end(id) {
        empty_response(StatusCode::NO_CONTENT)
    } else {
        plain_response(StatusCode::NOT_FOUND, "unknown MCP session")
    }
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(HeaderName::from_static(MCP_SESSION_ID_HEADER))
        .and_then(|value| value.to_str().ok())
}

fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.contains("text/event-stream"))
}

fn validate_headers(headers: &HeaderMap, allowed_origins: &[String]) -> Result<(), Box<Response>> {
    if let Some(origin) = headers.get(ORIGIN) {
        let Ok(origin_str) = origin.to_str() else {
            return Err(Box::new(plain_response(
                StatusCode::FORBIDDEN,
                "invalid Origin header",
            )));
        };
        if !origin_is_allowed(origin_str, allowed_origins) {
            return Err(Box::new(plain_response(
                StatusCode::FORBIDDEN,
                "Origin not allowed",
            )));
        }
    }

    if let Some(version) = headers.get(HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER)) {
        match version.to_str() {
            Ok(value) if value == PROTOCOL_VERSION => {}
            Ok(_) => {
                return Err(Box::new(plain_response(
                    StatusCode::BAD_REQUEST,
                    "unsupported MCP protocol version",
                )));
            }
            Err(_) => {
                return Err(Box::new(plain_response(
                    StatusCode::BAD_REQUEST,
                    "invalid MCP protocol version header",
                )));
            }
        }
    }

    Ok(())
}

fn origin_is_allowed(origin: &str, allowed_origins: &[String]) -> bool {
    if !allowed_origins.is_empty() {
        return allowed_origins.iter().any(|allowed| allowed == origin);
    }

    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host == "localhost" {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn with_protocol_version(mut response: Response) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER),
        HeaderValue::from_static(PROTOCOL_VERSION),
    );
    response
}

fn json_response(status: StatusCode, payload: Value) -> Response {
    with_protocol_version((status, Json(payload)).into_response())
}

fn empty_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    with_protocol_version(response)
}

fn plain_response(status: StatusCode, message: &str) -> Response {
    let mut response = Response::new(Body::from(message.to_string()));
    *response.status_mut() = status;
    response
}
