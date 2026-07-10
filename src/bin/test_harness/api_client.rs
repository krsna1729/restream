//! Typed API client and DTOs for the integration harness.

use super::*;

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiOutputMetrics {
    #[serde(default)]
    pub(crate) bytes_out: u64,
    #[serde(default)]
    pub(crate) packets_out: u64,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiBlockedByStage {
    #[serde(default)]
    pub(crate) stage: Option<String>,
    #[serde(default)]
    pub(crate) phase: Option<String>,
    #[serde(default)]
    pub(crate) backend: Option<String>,
    #[serde(default)]
    pub(crate) capacity_wait_ms: Option<u64>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiOutputStatus {
    #[serde(default)]
    pub(crate) output_id: String,
    #[serde(default)]
    pub(crate) output_name: Option<String>,
    #[serde(default)]
    pub(crate) pipeline_id: Option<String>,
    #[serde(default)]
    pub(crate) protocol: Option<String>,
    pub(crate) status: String,
    pub(crate) raw_status: String,
    pub(crate) phase: String,
    #[serde(default)]
    pub(crate) encoding: Option<String>,
    #[serde(default)]
    pub(crate) target_url: Option<String>,
    #[serde(default)]
    pub(crate) target_addr: Option<String>,
    #[serde(default)]
    pub(crate) uptime_secs: Option<f64>,
    #[serde(default)]
    pub(crate) bytes_out: u64,
    #[serde(default)]
    pub(crate) total_size: u64,
    #[serde(default)]
    pub(crate) metrics: ApiOutputMetrics,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
    #[serde(default)]
    pub(crate) last_error_at: Option<String>,
    #[serde(default)]
    pub(crate) last_progress_at: Option<String>,
    #[serde(default)]
    pub(crate) last_progress_age_ms: Option<u64>,
    #[serde(default)]
    pub(crate) terminal_stage: Option<String>,
    #[serde(default)]
    pub(crate) blocked_by: Option<ApiBlockedByStage>,
    #[serde(default)]
    pub(crate) recent_failure_count: u64,
    #[serde(default)]
    pub(crate) flapping: bool,
    #[serde(default)]
    pub(crate) retrying: bool,
    #[serde(default)]
    pub(crate) retry_attempts: Option<u64>,
    #[serde(default)]
    pub(crate) retry_backoff_ms: Option<u64>,
    #[serde(default)]
    pub(crate) next_retry_at: Option<String>,
    #[serde(default)]
    pub(crate) retry_remaining_ms: Option<u64>,
    #[serde(default)]
    pub(crate) failure_phase: Option<String>,
    #[serde(default)]
    pub(crate) quality: Value,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
}

impl ApiOutputStatus {
    pub(crate) fn from_value(output_id: &str, value: &Value) -> Result<Self, String> {
        let mut status: Self = serde_json::from_value(value.clone()).map_err(|e| {
            format!("output status for {output_id} does not match harness API schema: {e}")
        })?;
        if status.output_id.is_empty() {
            status.output_id = output_id.to_string();
        }
        if status.status.trim().is_empty()
            || status.raw_status.trim().is_empty()
            || status.phase.trim().is_empty()
        {
            return Err(format!(
                "output status for {output_id} is missing required status/rawStatus/phase"
            ));
        }
        status.touch_schema_fields_for_audit();
        Ok(status)
    }

    fn touch_schema_fields_for_audit(&self) {
        let _ = (
            &self.pipeline_id,
            &self.protocol,
            &self.target_addr,
            &self.uptime_secs,
            &self.last_error_at,
            &self.last_progress_at,
            &self.recent_failure_count,
            &self.flapping,
            &self.retry_attempts,
            &self.retry_backoff_ms,
            &self.next_retry_at,
            &self.retry_remaining_ms,
            &self.quality,
        );
    }

    pub(crate) fn has_progress(&self) -> bool {
        self.bytes_out > 0 || self.metrics.bytes_out > 0 || self.metrics.packets_out > 0
    }

    pub(crate) fn failure_phase_is_empty(&self) -> bool {
        self.failure_phase
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    }

    pub(crate) fn last_error_is_empty(&self) -> bool {
        self.last_error
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    }
}

/// Small authenticated HTTP client wrapper for the local restream API.
pub(crate) struct RampApi {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) cookie: Option<String>,
}

impl RampApi {
    pub(crate) fn new(http_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: format!("http://127.0.0.1:{http_port}"),
            cookie: None,
        }
    }

    pub(crate) async fn login(&mut self) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/api/v1/auth/login", self.base_url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(r#"{"password":"admin"}"#)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("login failed with HTTP {}", response.status()));
        }
        self.cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_string);
        if self.cookie.is_none() {
            return Err("login response did not include a session cookie".to_string());
        }
        Ok(())
    }

    pub(crate) async fn get_json(&self, path: &str) -> Result<Value, String> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    pub(crate) async fn get_json_or_not_found(&self, path: &str) -> Result<Option<Value>, String> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        if bytes.is_empty() {
            return Ok(Some(Value::Null));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn get_output_status(
        &self,
        pipeline_id: &str,
        output_id: &str,
    ) -> Result<(ApiOutputStatus, Value), String> {
        let value = self
            .get_json(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
            ))
            .await?;
        let status = ApiOutputStatus::from_value(output_id, &value)?;
        Ok((status, value))
    }

    pub(crate) async fn get_output_status_or_not_found(
        &self,
        pipeline_id: &str,
        output_id: &str,
    ) -> Result<Option<(ApiOutputStatus, Value)>, String> {
        let Some(value) = self
            .get_json_or_not_found(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/status"
            ))
            .await?
        else {
            return Ok(None);
        };
        let status = ApiOutputStatus::from_value(output_id, &value)?;
        Ok(Some((status, value)))
    }

    pub(crate) async fn get_text_response(
        &self,
        path: &str,
    ) -> Result<(reqwest::StatusCode, String), String> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status();
        let body = response.text().await.map_err(|e| e.to_string())?;
        Ok((status, body))
    }

    pub(crate) async fn post_json(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    pub(crate) async fn post_empty(&self, path: &str) -> Result<Value, String> {
        self.post_json(path, json!({})).await
    }

    pub(crate) async fn post_null(&self, path: &str) -> Result<Value, String> {
        self.post_json(path, Value::Null).await
    }

    pub(crate) async fn patch_json(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .patch(format!("{}{}", self.base_url, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    pub(crate) async fn put_json(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut request = self
            .client
            .put(format!("{}{}", self.base_url, path))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string());
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }

    pub(crate) async fn delete_json(&self, path: &str) -> Result<Value, String> {
        let mut request = self.client.delete(format!("{}{}", self.base_url, path));
        if let Some(cookie) = &self.cookie {
            request = request.header(reqwest::header::COOKIE, cookie);
        }
        json_response(request).await
    }
}

async fn json_response(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "HTTP {status}: {}",
            String::from_utf8_lossy(&bytes)
        ));
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
