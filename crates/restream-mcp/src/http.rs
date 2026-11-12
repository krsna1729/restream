//! HTTP backend that talks to the existing `/api/v1/agent/*` routes.

use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue};
use restream_agent_contract::{
    AgentBackend, AgentError, AgentFuture, ApprovalRequest, InvestigationRequest,
    OperationCreateRequest, PlanRequest, VerifyRequest,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone)]
pub enum HttpAgentAuth {
    SessionCookie(String),
    BearerToken(String),
    None,
}

#[derive(Clone)]
pub struct HttpAgentBackend {
    client: reqwest::Client,
    base_url: String,
    auth: HttpAgentAuth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildIdentity {
    version: String,
    commit: String,
}

/// Bound on reaching the target.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound on a whole exchange, body included. The agent routes do database
/// work and return; none waits on media, so this only ends a stalled target.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn transport_error(error: reqwest::Error) -> AgentError {
    if error.is_timeout() {
        // A mutation that timed out may still have been applied: read the
        // operation back before retrying it.
        return AgentError::Transport(format!(
            "target did not answer in time; its outcome is unknown: {error}"
        ));
    }
    AgentError::Transport(error.to_string())
}

impl HttpAgentBackend {
    /// `Err` when the HTTP client cannot be built (its TLS backend fails to
    /// initialize).
    pub fn new(base_url: impl Into<String>, auth: HttpAgentAuth) -> Result<Self, AgentError> {
        Self::with_timeouts(base_url, auth, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
    }

    fn with_timeouts(
        base_url: impl Into<String>,
        auth: HttpAgentAuth,
        connect: Duration,
        request: Duration,
    ) -> Result<Self, AgentError> {
        let client = reqwest::Client::builder()
            .connect_timeout(connect)
            .timeout(request)
            .build()
            .map_err(|error| AgentError::Transport(format!("build the HTTP client: {error}")))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            auth,
        })
    }

    pub async fn verify_target_compatibility(&self) -> Result<(), AgentError> {
        let status = self.get_json("/api/v1/engine").await?;
        let remote = parse_build_identity(&status)?;
        let local = local_build_identity();
        if remote != local {
            return Err(AgentError::Transport(format!(
                "restream-mcp {} ({}) expects the same restream build, but target {} reports {} ({})",
                local.version, local.commit, self.base_url, remote.version, remote.commit
            )));
        }

        // Prove the target has the agent-plane surface enabled before we start
        // serving MCP traffic for it.
        self.capabilities().await.map(|_| ())
    }

    async fn get_json(&self, path: &str) -> Result<Value, AgentError> {
        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .headers(self.auth_headers()?)
            .send()
            .await
            .map_err(transport_error)?;
        Self::decode_response(response).await
    }

    async fn post_json<T: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<Value, AgentError> {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .headers(self.auth_headers()?)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(body)?)
            .send()
            .await
            .map_err(transport_error)?;
        Self::decode_response(response).await
    }

    fn auth_headers(&self) -> Result<HeaderMap, AgentError> {
        let mut headers = HeaderMap::new();
        match &self.auth {
            HttpAgentAuth::SessionCookie(cookie) => {
                headers.insert(
                    COOKIE,
                    HeaderValue::from_str(cookie).map_err(|err| {
                        AgentError::InvalidRequest(format!("invalid session cookie: {err}"))
                    })?,
                );
            }
            HttpAgentAuth::BearerToken(token) => {
                let mut value =
                    HeaderValue::from_str(&format!("Bearer {token}")).map_err(|err| {
                        AgentError::InvalidRequest(format!("invalid bearer token: {err}"))
                    })?;
                value.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
            HttpAgentAuth::None => {}
        }
        Ok(headers)
    }

    async fn decode_response(response: reqwest::Response) -> Result<Value, AgentError> {
        let status = response.status();
        let text = response.text().await.map_err(transport_error)?;
        let body =
            serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "raw": text }));
        if status.is_success() {
            Ok(body)
        } else {
            Err(AgentError::Upstream {
                status: status.as_u16(),
                body: body.to_string(),
            })
        }
    }
}

fn local_build_identity() -> BuildIdentity {
    BuildIdentity {
        version: env!("CARGO_PKG_VERSION").to_string(),
        commit: env!("GIT_COMMIT_HASH").to_string(),
    }
}

fn parse_build_identity(payload: &Value) -> Result<BuildIdentity, AgentError> {
    let restream = payload
        .get("restream")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AgentError::Transport(
                "target /api/v1/engine response is missing the 'restream' object".to_string(),
            )
        })?;

    let version = restream
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AgentError::Transport(
                "target /api/v1/engine response is missing 'restream.version'".to_string(),
            )
        })?;
    let commit = restream
        .get("commit")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AgentError::Transport(
                "target /api/v1/engine response is missing 'restream.commit'".to_string(),
            )
        })?;
    Ok(BuildIdentity {
        version: version.to_string(),
        commit: commit.to_string(),
    })
}

impl AgentBackend for HttpAgentBackend {
    fn capabilities(&self) -> AgentFuture<'_, Value> {
        Box::pin(async move { self.get_json("/api/v1/agent/capabilities").await })
    }

    fn context(&self) -> AgentFuture<'_, Value> {
        Box::pin(async move { self.get_json("/api/v1/agent/context").await })
    }

    fn investigate(&self, request: InvestigationRequest) -> AgentFuture<'_, Value> {
        Box::pin(async move {
            self.post_json("/api/v1/agent/investigations", &request)
                .await
        })
    }

    fn plan(&self, request: PlanRequest) -> AgentFuture<'_, Value> {
        Box::pin(async move { self.post_json("/api/v1/agent/plans", &request).await })
    }

    fn validate(&self, request: PlanRequest) -> AgentFuture<'_, Value> {
        Box::pin(async move {
            self.post_json("/api/v1/agent/plans/validate", &request)
                .await
        })
    }

    fn graph_diff(&self, request: PlanRequest) -> AgentFuture<'_, Value> {
        Box::pin(async move {
            self.post_json("/api/v1/agent/graph-diff-preview", &request)
                .await
        })
    }

    fn create_operation(&self, request: OperationCreateRequest) -> AgentFuture<'_, Value> {
        Box::pin(async move { self.post_json("/api/v1/agent/operations", &request).await })
    }

    fn get_operation(&self, operation_id: &str) -> AgentFuture<'_, Value> {
        let operation_id = operation_id.to_string();
        Box::pin(async move {
            self.get_json(&format!("/api/v1/agent/operations/{operation_id}"))
                .await
        })
    }

    fn approve_operation(
        &self,
        operation_id: &str,
        request: ApprovalRequest,
    ) -> AgentFuture<'_, Value> {
        let operation_id = operation_id.to_string();
        Box::pin(async move {
            self.post_json(
                &format!("/api/v1/agent/operations/{operation_id}/approve"),
                &request,
            )
            .await
        })
    }

    fn apply_operation(&self, operation_id: &str) -> AgentFuture<'_, Value> {
        let operation_id = operation_id.to_string();
        Box::pin(async move {
            self.post_json(
                &format!("/api/v1/agent/operations/{operation_id}/apply"),
                &serde_json::json!({}),
            )
            .await
        })
    }

    fn verify_operation(&self, request: VerifyRequest) -> AgentFuture<'_, Value> {
        Box::pin(async move { self.post_json("/api/v1/agent/verify", &request).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_build_identity_reads_expected_fields() {
        let parsed = parse_build_identity(&json!({
            "restream": {
                "version": "0.1.0",
                "commit": "abc123"
            }
        }))
        .expect("parse should succeed");

        assert_eq!(
            parsed,
            BuildIdentity {
                version: "0.1.0".to_string(),
                commit: "abc123".to_string(),
            }
        );
    }

    #[test]
    fn this_builds_engine_status_matches_the_local_identity() {
        let (status, _) = restream::host::runtime_info::status_and_sbom(false);
        assert_eq!(
            parse_build_identity(&status).expect("engine status carries an identity"),
            local_build_identity()
        );
    }

    #[test]
    fn parse_build_identity_rejects_missing_fields() {
        let error = parse_build_identity(&json!({
            "restream": {
                "version": "0.1.0"
            }
        }))
        .expect_err("parse should fail");

        assert!(
            error.to_string().contains("missing 'restream.commit'"),
            "unexpected error: {error}"
        );
    }

    /// A target that accepts connections and never answers.
    async fn stalled_target() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        address
    }

    /// A stalled target ends the call at the request deadline, reported as
    /// an unknown outcome, instead of hanging the agent.
    #[tokio::test]
    async fn a_stalled_target_ends_at_the_request_deadline() {
        let address = stalled_target().await;
        let backend = HttpAgentBackend::with_timeouts(
            format!("http://{address}"),
            HttpAgentAuth::None,
            Duration::from_secs(1),
            Duration::from_millis(300),
        )
        .expect("client builds");
        let error = tokio::time::timeout(Duration::from_secs(5), backend.capabilities())
            .await
            .expect("the call ends at its deadline")
            .expect_err("a stalled target is an error");
        assert!(
            error.to_string().contains("outcome is unknown"),
            "unexpected error: {error}"
        );
    }
}
