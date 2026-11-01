use super::*;
use restream_agent_contract::{
    AgentBackend, AgentFuture, ApprovalRequest, InvestigationRequest, OperationCreateRequest,
    PlanRequest, VerifyRequest,
};
use restream_sync::lock::lock;
use std::io::ErrorKind;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, WriteHalf};

/// A backend that records which calls reached it, with one operation
/// awaiting approval (or already approved).
#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<&'static str>>,
    approval: Mutex<Option<ApprovalRequest>>,
    apply_takes: Duration,
    approved: bool,
}

impl Recording {
    fn record(&self, call: &'static str) -> AgentFuture<'_, Value> {
        lock(&self.calls).push(call);
        Box::pin(async move { Ok(json!({ "tool": call })) })
    }

    fn calls(&self) -> Vec<&'static str> {
        lock(&self.calls).clone()
    }
}

impl AgentBackend for Recording {
    fn capabilities(&self) -> AgentFuture<'_, Value> {
        self.record("capabilities")
    }

    fn context(&self) -> AgentFuture<'_, Value> {
        self.record("context")
    }

    fn investigate(&self, _request: InvestigationRequest) -> AgentFuture<'_, Value> {
        self.record("investigate")
    }

    fn plan(&self, _request: PlanRequest) -> AgentFuture<'_, Value> {
        self.record("plan")
    }

    fn validate(&self, _request: PlanRequest) -> AgentFuture<'_, Value> {
        self.record("validate")
    }

    fn graph_diff(&self, _request: PlanRequest) -> AgentFuture<'_, Value> {
        self.record("graph")
    }

    fn create_operation(&self, _request: OperationCreateRequest) -> AgentFuture<'_, Value> {
        self.record("create")
    }

    fn get_operation(&self, operation_id: &str) -> AgentFuture<'_, Value> {
        lock(&self.calls).push("get");
        let operation = json!({
            "operationId": operation_id,
            "status": if self.approved { "approved" } else { "awaitingApproval" },
            "intentSummary": "raise the ladder bitrate",
            "affectedObjects": { "pipelineIds": ["pipe-1"], "outputIds": ["out-1"] },
            "warnings": [],
        });
        Box::pin(async move { Ok(operation) })
    }

    fn approve_operation(
        &self,
        _operation_id: &str,
        request: ApprovalRequest,
    ) -> AgentFuture<'_, Value> {
        *lock(&self.approval) = Some(request);
        self.record("approve")
    }

    fn apply_operation(&self, _operation_id: &str) -> AgentFuture<'_, Value> {
        Box::pin(async move {
            tokio::time::sleep(self.apply_takes).await;
            lock(&self.calls).push("apply");
            Ok(json!({ "tool": "apply", "applied": true }))
        })
    }

    fn verify_operation(&self, _request: VerifyRequest) -> AgentFuture<'_, Value> {
        self.record("verify")
    }
}

/// Every agent tool reaches its backend call: none falls through to
/// "unknown MCP tool".
#[tokio::test]
async fn every_agent_tool_dispatches() {
    let handler = AgentToolHandler::new(Arc::new(Recording::default()));
    let input = json!({
        "intent": "probe",
        "operationId": "op-1",
        "approvedBy": "probe",
    });
    for tool in restream_agent_contract::AGENT_TOOLS {
        let result = handler
            .dispatch(tool.name, input.clone(), &CallContext::detached())
            .await;
        assert!(result.is_ok(), "{} did not dispatch: {result:?}", tool.name);
    }
}

#[test]
fn stateless_initialize_and_tools_list_answer_without_a_session() {
    let Inbound::Reply(initialized) = classify(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        None,
        Handshake::NotRequired,
    ) else {
        panic!("initialize must be answered");
    };
    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(initialized["result"]["serverInfo"]["name"], SERVER_NAME);

    let Inbound::Reply(tools) = classify(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        None,
        Handshake::NotRequired,
    ) else {
        panic!("tools/list must be answered");
    };
    assert!(!tools["result"]["tools"].as_array().unwrap().is_empty());
}

/// An MCP client on the far end of an in-memory stdio pipe.
struct StdioClient {
    to_server: WriteHalf<DuplexStream>,
    from_server: Lines<BufReader<DuplexStream>>,
    server: tokio::task::JoinHandle<Result<(), AgentError>>,
}

impl StdioClient {
    async fn start(backend: Arc<Recording>, elicitation: bool) -> Self {
        let (client_in, server_in) = tokio::io::duplex(64 * 1024);
        let (server_out, client_out) = tokio::io::duplex(64 * 1024);
        let (_, to_server) = tokio::io::split(client_in);
        let handler = Arc::new(AgentToolHandler::new(backend));
        let server = tokio::spawn(stdio::serve(handler, server_in, server_out));
        let mut client = Self {
            to_server,
            from_server: BufReader::new(client_out).lines(),
            server,
        };
        let capabilities = if elicitation {
            json!({ "elicitation": {} })
        } else {
            json!({})
        };
        client
            .send(json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": { "capabilities": capabilities },
            }))
            .await;
        assert_eq!(client.recv().await["id"], 0);
        client
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        client
    }

    async fn send(&mut self, message: Value) {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.to_server.write_all(&line).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = self
            .from_server
            .next_line()
            .await
            .unwrap()
            .expect("server closed");
        serde_json::from_str(&line).unwrap()
    }

    async fn apply(&mut self, meta: Value) {
        self.send(json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {
                "name": "apply_agent_operation",
                "arguments": { "operationId": "op-1" },
                "_meta": meta,
            },
        }))
        .await;
    }
}

fn structured(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

/// The client approves mid-call: the prompt names what the change touches,
/// and the backend sees the approval (with the client's reason) before the
/// apply.
#[tokio::test]
async fn an_elicited_approval_approves_then_applies() {
    let backend = Arc::new(Recording::default());
    let mut client = StdioClient::start(Arc::clone(&backend), true).await;
    client.apply(json!({})).await;

    let prompt = client.recv().await;
    assert_eq!(prompt["method"], "elicitation/create");
    let message = prompt["params"]["message"].as_str().unwrap();
    for named in ["op-1", "raise the ladder bitrate", "pipe-1", "out-1"] {
        assert!(message.contains(named), "prompt omits {named}: {message}");
    }
    assert_eq!(
        prompt["params"]["requestedSchema"]["required"],
        json!(["approve"])
    );
    client
        .send(json!({
            "jsonrpc": "2.0", "id": prompt["id"],
            "result": { "action": "accept", "content": { "approve": true, "reason": "reviewed" } },
        }))
        .await;

    let result = client.recv().await;
    assert_eq!(result["id"], 7);
    assert_eq!(structured(&result)["applied"], true);
    assert_eq!(backend.calls(), ["get", "approve", "apply"]);
    let approval = lock(&backend.approval).take().unwrap();
    assert_eq!(approval.reason.as_deref(), Some("reviewed"));
}

/// Anything short of an explicit approval leaves the operation untouched:
/// nothing is approved or applied, and the result says why.
#[tokio::test]
async fn a_withheld_approval_changes_nothing() {
    let answers = [
        (json!({"result": {"action": "decline"}}), "declined"),
        (json!({"result": {"action": "cancel"}}), "cancelled"),
        (
            json!({"result": {"action": "accept", "content": {"approve": false}}}),
            "declined",
        ),
        (
            json!({"error": {"code": -32600, "message": "no"}}),
            "rejected",
        ),
    ];
    for (answer, outcome) in answers {
        let backend = Arc::new(Recording::default());
        let mut client = StdioClient::start(Arc::clone(&backend), true).await;
        client.apply(json!({})).await;
        let prompt = client.recv().await;
        let mut reply = answer.clone();
        reply["jsonrpc"] = json!("2.0");
        reply["id"] = prompt["id"].clone();
        client.send(reply).await;

        let result = client.recv().await;
        assert_eq!(structured(&result)["applied"], false, "{answer}");
        assert_eq!(structured(&result)["approval"], outcome, "{answer}");
        assert_eq!(backend.calls(), ["get"], "{answer}");
    }
}

/// A client that cannot be asked mid-call keeps the explicit
/// approve-then-apply flow: the apply goes straight to the backend, which
/// enforces approval itself.
#[tokio::test]
async fn without_elicitation_apply_goes_straight_to_the_backend() {
    let backend = Arc::new(Recording::default());
    let mut client = StdioClient::start(Arc::clone(&backend), false).await;
    client.apply(json!({})).await;
    let result = client.recv().await;
    assert_eq!(result["id"], 7);
    assert_eq!(backend.calls(), ["apply"]);
}

/// An operation approved beforehand applies without a prompt.
#[tokio::test]
async fn an_approved_operation_applies_without_asking() {
    let backend = Arc::new(Recording {
        approved: true,
        ..Recording::default()
    });
    let mut client = StdioClient::start(Arc::clone(&backend), true).await;
    client.apply(json!({})).await;
    let result = client.recv().await;
    assert_eq!(result["id"], 7);
    assert_eq!(backend.calls(), ["get", "apply"]);
}

/// The client goes away while the server waits for its answer: the call
/// ends unapproved and the server exits instead of waiting forever. (The
/// paused clock turns a hang into an immediate timeout.)
#[tokio::test(start_paused = true)]
async fn a_client_gone_mid_prompt_ends_the_call_unapproved() {
    let backend = Arc::new(Recording::default());
    let mut client = StdioClient::start(Arc::clone(&backend), true).await;
    client.apply(json!({})).await;
    assert_eq!(client.recv().await["method"], "elicitation/create");
    client.to_server.shutdown().await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .expect("the waiting call must end once stdin closes");
    assert_eq!(structured(&result)["approval"], "disconnected");
    tokio::time::timeout(Duration::from_secs(5), client.server)
        .await
        .expect("server must exit once stdin closes")
        .unwrap()
        .unwrap();
    assert_eq!(backend.calls(), ["get"]);
}

/// A long apply reports progress under the client's token until it ends.
#[tokio::test(start_paused = true)]
async fn a_long_apply_reports_progress_until_it_ends() {
    let backend = Arc::new(Recording {
        apply_takes: Duration::from_secs(12),
        ..Recording::default()
    });
    let mut client = StdioClient::start(Arc::clone(&backend), false).await;
    client.apply(json!({ "progressToken": "apply-1" })).await;

    let mut progress = Vec::new();
    let result = loop {
        let message = client.recv().await;
        if message["method"] == "notifications/progress" {
            assert_eq!(message["params"]["progressToken"], "apply-1");
            progress.push(message["params"]["progress"].as_u64().unwrap());
        } else {
            break message;
        }
    };
    assert_eq!(progress, [0, 5, 10]);
    assert_eq!(structured(&result)["applied"], true);
}

async fn bind_loopback() -> Option<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => Some(listener),
        // Some restricted runners deny loopback binds entirely.
        Err(error) if error.kind() == ErrorKind::PermissionDenied => None,
        Err(error) => panic!("failed to bind loopback listener: {error}"),
    }
}

async fn serve_http(backend: Arc<Recording>) -> Option<(String, tokio::task::JoinHandle<()>)> {
    let listener = bind_loopback().await?;
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = streamable_http::router(Arc::new(streamable_http::HttpTransportState::new(
        Arc::new(AgentToolHandler::new(backend)),
        Vec::new(),
    )));
    let server = tokio::spawn(async move {
        streamable_http::serve(listener, app)
            .await
            .expect("server should run");
    });
    Some((url, server))
}

/// POST one JSON-RPC message, optionally under a session and accepting SSE.
async fn post(url: &str, session: Option<&str>, sse: bool, message: Value) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&message).unwrap());
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    if sse {
        request = request.header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        );
    }
    request.send().await.expect("request should succeed")
}

#[tokio::test]
async fn http_transport_answers_live_loopback_requests() {
    let Some((url, server)) = serve_http(Arc::new(Recording::default())).await else {
        return;
    };
    let response = post(
        &url,
        None,
        false,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert!(!payload["result"]["tools"].as_array().unwrap().is_empty());
    server.abort();
}

/// The JSON-RPC messages of one SSE response, in order.
struct SseReader {
    response: reqwest::Response,
    buffered: String,
}

impl SseReader {
    async fn next(&mut self) -> Value {
        loop {
            if let Some(end) = self.buffered.find("\n\n") {
                let event: String = self.buffered.drain(..end + 2).collect();
                let data: String = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .collect();
                return serde_json::from_str(data.trim()).unwrap();
            }
            let chunk = self
                .response
                .chunk()
                .await
                .unwrap()
                .expect("stream ended early");
            self.buffered.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }
}

/// Over streamable HTTP the approval prompt arrives on the tool call's SSE
/// stream and the client's answer, POSTed under its session, reaches it.
#[tokio::test]
async fn http_elicited_approval_routes_the_answer_by_session() {
    let backend = Arc::new(Recording::default());
    let Some((url, server)) = serve_http(Arc::clone(&backend)).await else {
        return;
    };
    let initialized = post(
        &url,
        None,
        false,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "capabilities": { "elicitation": {} } },
        }),
    )
    .await;
    let session = initialized.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();

    let call = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "apply_agent_operation", "arguments": { "operationId": "op-1" } },
    });
    let mut events = SseReader {
        response: post(&url, Some(&session), true, call).await,
        buffered: String::new(),
    };

    let prompt = events.next().await;
    assert_eq!(prompt["method"], "elicitation/create");
    let answer = json!({
        "jsonrpc": "2.0", "id": prompt["id"],
        "result": { "action": "accept", "content": { "approve": true } },
    });
    let answered = post(&url, Some(&session), false, answer).await;
    assert_eq!(answered.status(), reqwest::StatusCode::ACCEPTED);

    let result = events.next().await;
    assert_eq!(result["id"], 7);
    assert_eq!(structured(&result)["applied"], true);
    assert_eq!(backend.calls(), ["get", "approve", "apply"]);

    let unknown = post(
        &url,
        Some("not-a-session"),
        false,
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"}),
    )
    .await;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    server.abort();
}
