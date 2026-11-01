//! Newline-delimited JSON-RPC over stdin/stdout. Tool calls run as their own
//! tasks so the read loop keeps routing the client's answers to requests a
//! running call made (an approval prompt) while that call waits.

use super::session::{CALL_OUTBOX, CallContext, Session};
use super::{Handshake, Inbound, classify, error_response, run_tool_call};
use crate::handlers::AgentToolHandler;
use restream_agent_contract::{AgentBackend, AgentError};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub(super) async fn run<B: AgentBackend + 'static>(
    handler: Arc<AgentToolHandler<B>>,
) -> Result<(), AgentError> {
    serve(handler, tokio::io::stdin(), tokio::io::stdout()).await
}

pub(super) async fn serve<B, R, W>(
    handler: Arc<AgentToolHandler<B>>,
    input: R,
    mut output: W,
) -> Result<(), AgentError>
where
    B: AgentBackend + 'static,
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut outbox) = mpsc::channel::<Value>(CALL_OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(message) = outbox.recv().await {
            let mut line = serde_json::to_vec(&message).map_err(std::io::Error::other)?;
            line.push(b'\n');
            output.write_all(&line).await?;
            output.flush().await?;
        }
        Ok::<_, std::io::Error>(())
    });

    let session = Arc::new(Session::default());
    let mut lines = BufReader::new(input).lines();
    let mut read_error = None;
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                read_error = Some(AgentError::Transport(format!(
                    "failed to read stdin: {err}"
                )));
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str(line) {
            Ok(message) => match classify(message, Some(&session), Handshake::Required) {
                Inbound::Reply(reply) => reply,
                Inbound::Nothing => continue,
                Inbound::ToolCall {
                    id,
                    name,
                    arguments,
                    progress_token,
                } => {
                    let handler = Arc::clone(&handler);
                    let ctx = CallContext::streaming(
                        Some(Arc::clone(&session)),
                        out.clone(),
                        progress_token,
                    );
                    let out = out.clone();
                    tokio::spawn(async move {
                        let response = run_tool_call(&handler, &ctx, id, &name, arguments).await;
                        let _ = out.send(response).await;
                    });
                    continue;
                }
            },
            Err(err) => error_response(
                Value::Null,
                -32700,
                format!("invalid JSON-RPC payload: {err}"),
                None,
            ),
        };
        if out.send(reply).await.is_err() {
            break;
        }
    }
    // No answer can arrive any more: a call waiting on one fails now. Calls
    // still running hold their own senders, so the writer drains their
    // results, then ends once the last one finishes.
    session.close();
    drop(out);
    writer
        .await
        .map_err(|err| AgentError::Transport(format!("stdout writer failed: {err}")))?
        .map_err(|err| AgentError::Transport(format!("failed to write stdout: {err}")))?;
    read_error.map_or(Ok(()), Err)
}
