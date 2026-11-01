//! One client connection's protocol state, and what a tool call may send to
//! its client while it runs: progress notifications, and requests whose
//! answers the transport routes back (elicitation).

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use restream_sync::lock::lock;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

/// How often a running tool call reports progress. Clients reset their
/// request timeout on progress, so a long apply is not abandoned.
pub(crate) const PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// Messages a tool call may queue for its client before it waits.
pub(super) const CALL_OUTBOX: usize = 32;

#[derive(Default)]
pub(crate) struct Session {
    /// The client declared `capabilities.elicitation` at initialize.
    elicitation: AtomicBool,
    initialize_seen: AtomicBool,
    initialized: AtomicBool,
    next_request: AtomicU64,
    /// Requests this server sent, by id, awaiting the client's answer.
    pending: Mutex<Waiters>,
}

type Answer = Result<Value, Value>;

#[derive(Default)]
struct Waiters {
    /// The connection ended: no answer can arrive any more.
    closed: bool,
    by_id: HashMap<String, oneshot::Sender<Answer>>,
}

impl Session {
    pub(super) fn on_initialize(&self, params: &Value) {
        let elicitation = params
            .pointer("/capabilities/elicitation")
            .is_some_and(Value::is_object);
        self.elicitation.store(elicitation, Ordering::Relaxed);
        self.initialize_seen.store(true, Ordering::Relaxed);
    }

    pub(super) fn on_initialized(&self) {
        if self.initialize_seen.load(Ordering::Relaxed) {
            self.initialized.store(true, Ordering::Relaxed);
        }
    }

    /// `(initialize seen, initialized)`, for transports that require the
    /// handshake before normal operations.
    pub(super) fn handshake(&self) -> (bool, bool) {
        (
            self.initialize_seen.load(Ordering::Relaxed),
            self.initialized.load(Ordering::Relaxed),
        )
    }

    /// Hand a client's response to the request awaiting it. `false` when no
    /// request of this session has that id (a stale or foreign answer).
    pub(super) fn route_response(&self, message: &Value) -> bool {
        let Some(id) = message.get("id").and_then(Value::as_str) else {
            return false;
        };
        let Some(waiter) = lock(&self.pending).by_id.remove(id) else {
            return false;
        };
        let answer = match message.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        // The caller may have stopped waiting (its call ended): nothing to do.
        let _ = waiter.send(answer);
        true
    }

    /// The connection ended: every waiting request fails as disconnected,
    /// and so does any request made later.
    pub(super) fn close(&self) {
        let mut pending = lock(&self.pending);
        pending.closed = true;
        pending.by_id.clear();
    }

    fn register(&self) -> (String, oneshot::Receiver<Answer>) {
        let id = format!(
            "restream-{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        );
        let (sender, receiver) = oneshot::channel();
        let mut pending = lock(&self.pending);
        // Closed: the sender drops here, so the waiter sees a disconnect.
        if !pending.closed {
            pending.by_id.insert(id.clone(), sender);
        }
        (id, receiver)
    }

    fn forget(&self, id: &str) {
        lock(&self.pending).by_id.remove(id);
    }
}

/// Why a request to the client produced no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientRequestError {
    /// This call cannot reach its client mid-call (a plain JSON reply), or
    /// the client did not declare the capability.
    Unsupported,
    /// The client answered with a JSON-RPC error.
    Rejected(Value),
    /// The connection ended before the answer.
    Disconnected,
}

/// What one tool call may send its client while it runs.
pub struct CallContext {
    session: Option<Arc<Session>>,
    out: Option<mpsc::Sender<Value>>,
    progress_token: Option<Value>,
}

impl CallContext {
    /// A call that cannot reach its client before it returns.
    pub fn detached() -> Self {
        Self {
            session: None,
            out: None,
            progress_token: None,
        }
    }

    pub(super) fn streaming(
        session: Option<Arc<Session>>,
        out: mpsc::Sender<Value>,
        progress_token: Option<Value>,
    ) -> Self {
        Self {
            session,
            out: Some(out),
            progress_token,
        }
    }

    /// Whether the client can be asked for input during this call.
    pub fn can_elicit(&self) -> bool {
        self.out.is_some()
            && self
                .session
                .as_ref()
                .is_some_and(|session| session.elicitation.load(Ordering::Relaxed))
    }

    /// Report progress if the client asked for it. Best effort: a client
    /// that does not drain its stream misses a heartbeat, not the result.
    pub fn progress(&self, progress: u64, message: &str) {
        let (Some(out), Some(token)) = (&self.out, &self.progress_token) else {
            return;
        };
        let _ = out.try_send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {
                "progressToken": token,
                "progress": progress,
                "message": message,
            }
        }));
    }

    /// Run `work`, reporting its elapsed seconds every [`PROGRESS_EVERY`].
    pub async fn with_progress<T>(&self, label: &str, work: impl Future<Output = T>) -> T {
        if self.progress_token.is_none() || self.out.is_none() {
            return work.await;
        }
        let started = tokio::time::Instant::now();
        let mut ticks = tokio::time::interval(PROGRESS_EVERY);
        let mut work = std::pin::pin!(work);
        loop {
            tokio::select! {
                result = &mut work => return result,
                _ = ticks.tick() => {
                    let elapsed = started.elapsed().as_secs();
                    self.progress(elapsed, &format!("{label}: {elapsed} s"));
                }
            }
        }
    }

    /// Send `method` to the client and wait for its answer.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ClientRequestError> {
        let (Some(session), Some(out)) = (&self.session, &self.out) else {
            return Err(ClientRequestError::Unsupported);
        };
        let (id, answer) = session.register();
        // However this call ends (answered, disconnected, or dropped), the
        // request leaves the session's table.
        let _registered = Registered { session, id: &id };
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if out.send(message).await.is_err() {
            return Err(ClientRequestError::Disconnected);
        }
        match answer.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(ClientRequestError::Rejected(error)),
            Err(_) => Err(ClientRequestError::Disconnected),
        }
    }
}

struct Registered<'a> {
    session: &'a Session,
    id: &'a str,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.session.forget(self.id);
    }
}
