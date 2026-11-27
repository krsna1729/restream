//! Pipeline setup and output/sink observation shared by the fault cells.

use super::*;

pub(crate) async fn create_pipeline_with_stream_key(
    api: &RampApi,
    name: &str,
    stream_key: &str,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("missing pipeline id for {name}"))
}

pub(crate) async fn create_pipeline(api: &RampApi, name: &str) -> Result<String, String> {
    create_pipeline_with_stream_key(api, name, name).await
}

pub(crate) async fn delete_pipeline_v1(api: &RampApi, pipeline_id: &str) -> Result<(), String> {
    let delete_url = format!("{}/api/v1/pipelines/{pipeline_id}", api.base_url);
    let mut request = api.client.delete(&delete_url);
    if let Some(cookie) = &api.cookie {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("delete pipeline {pipeline_id}: {e}"))?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "delete pipeline {pipeline_id}: unexpected status {}",
            response.status()
        ))
    }
}

pub(crate) const RECOVERY_WARM_VIDEO_MIN: u64 = 10;

pub(crate) async fn wait_for_sink_video_above(
    metrics: &GeneralizedSinkMetrics,
    threshold: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if metrics.video_count.load(Ordering::Relaxed) > threshold {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

pub(crate) fn health_input_snapshot(health: Option<&Value>, pipeline_id: &str) -> Value {
    health
        .map(|health| health["pipelines"][pipeline_id]["input"].clone())
        .unwrap_or(Value::Null)
}

pub(crate) fn disconnect_grace_remaining_bounded(input: &Value) -> bool {
    input["disconnectGraceRemainingMs"]
        .as_u64()
        .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
}

pub(crate) fn input_disconnect_cleared(input: &Value) -> bool {
    input["status"] == "on"
        && input["probeStatus"] == "ready"
        && input["lastSessionProtocol"].is_null()
        && input["lastDisconnectReason"].is_null()
        && input["lastFailurePhase"].is_null()
        && input["recentDisconnectError"] == false
}

/// Final output state checked by recovery/fault cells after perturbation.
pub(crate) struct FinalOutputObservation {
    pub(crate) status: Option<Value>,
    pub(crate) health: Value,
    pub(crate) running: bool,
    pub(crate) retrying: bool,
    pub(crate) error_cleared: bool,
    pub(crate) recent_failure_count: u64,
    pub(crate) flapping: bool,
    pub(crate) health_recent_failure_count: u64,
    pub(crate) health_flapping: bool,
}

pub(crate) async fn observe_final_output(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
) -> FinalOutputObservation {
    let status = api.get_output_status(pipeline_id, output_id).await.ok();
    let status_json = status.as_ref().map(|(_, json)| json.clone());
    let health = api.get_json("/api/v1/engine/health").await.ok();
    let output_health = health
        .as_ref()
        .map(|health| health["pipelines"][pipeline_id]["outputs"][output_id].clone())
        .unwrap_or(Value::Null);

    FinalOutputObservation {
        running: status.as_ref().map(|(status, _)| status.status.as_str()) == Some("running"),
        retrying: status.as_ref().is_some_and(|(status, _)| status.retrying),
        error_cleared: status
            .as_ref()
            .is_some_and(|(status, _)| status.last_error_is_empty()),
        recent_failure_count: status
            .as_ref()
            .map(|(status, _)| status.recent_failure_count)
            .unwrap_or(0),
        flapping: status.as_ref().is_some_and(|(status, _)| status.flapping),
        health_recent_failure_count: output_health["recentFailureCount"].as_u64().unwrap_or(0),
        health_flapping: output_health["flapping"].as_bool().unwrap_or(false),
        status: status_json,
        health: output_health,
    }
}

async fn output_running_without_retry(api: &RampApi, pipeline_id: &str, output_id: &str) -> bool {
    api.get_output_status(pipeline_id, output_id)
        .await
        .ok()
        .is_some_and(|(status, _)| status.status == "running" && !status.retrying)
}

pub(crate) async fn wait_for_output_running(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if output_running_without_retry(api, pipeline_id, output_id).await {
            return true;
        }
    }
    false
}

pub(crate) async fn wait_for_output_running_and_sink_video_above(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    metrics: &GeneralizedSinkMetrics,
    threshold: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let video_progressed = metrics.video_count.load(Ordering::Relaxed) > threshold;
        if video_progressed && output_running_without_retry(api, pipeline_id, output_id).await {
            return true;
        }
    }
    false
}

pub(crate) async fn wait_for_outputs_live_and_progressing(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut stabilized = Vec::new();
    let mut attempts = 0u32;
    let mut latest = Value::Null;

    while Instant::now() < deadline {
        attempts = attempts.saturating_add(1);
        let health = api.get_json("/api/v1/engine/health").await?;
        let mut snapshots = Vec::with_capacity(output_ids.len());
        let mut all_live = true;

        for output_id in output_ids {
            let output = health["pipelines"][pipeline_id]["outputs"][output_id].clone();
            let status = ApiOutputStatus::from_value(output_id, &output)?;
            let healthy = status.status == "running"
                && matches!(
                    status.phase.as_str(),
                    "sending" | "uploading" | "discarding"
                )
                && status.raw_status == "running"
                && status.bytes_out > 0
                && status.total_size > 0
                && !status.retrying
                && status.failure_phase_is_empty()
                && status.last_error_is_empty()
                && status.last_progress_age_ms.is_some_and(|age| age <= 5_000);
            if !healthy {
                all_live = false;
            }
            snapshots.push(json!({
                "outputId": status.output_id,
                "status": status.status,
                "phase": status.phase,
                "rawStatus": status.raw_status,
                "bytesOut": status.bytes_out,
                "totalSize": status.total_size,
                "lastProgressAgeMs": status.last_progress_age_ms,
                "retrying": status.retrying,
                "failurePhase": status.failure_phase,
                "lastError": status.last_error,
                "healthy": healthy,
            }));
        }

        latest = json!({
            "attempt": attempts,
            "outputs": snapshots,
        });

        if all_live {
            stabilized.push(latest.clone());
            if stabilized.len() >= 2 {
                return Ok(json!({
                    "attempts": attempts,
                    "stabilizedSamples": stabilized,
                }));
            }
        } else {
            stabilized.clear();
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(format!(
        "{} output(s) for pipeline {pipeline_id} did not stay live/progressing within {}s; latest={latest}",
        output_ids.len(),
        timeout.as_secs()
    ))
}

/// Output retry state observed from both the public status endpoint and engine health.
pub(crate) struct OutputRetryObservation {
    pub(crate) status_visible: bool,
    pub(crate) health_visible: bool,
    pub(crate) has_error: bool,
    pub(crate) cleaned_up: bool,
    pub(crate) phase: String,
    pub(crate) failure_phase: String,
    pub(crate) last_error: String,
    pub(crate) attempts: Option<u64>,
    pub(crate) backoff_ms: Option<u64>,
}

impl Default for OutputRetryObservation {
    fn default() -> Self {
        Self {
            status_visible: false,
            health_visible: false,
            has_error: false,
            cleaned_up: false,
            phase: String::from("unknown"),
            failure_phase: String::from("unknown"),
            last_error: String::new(),
            attempts: None,
            backoff_ms: None,
        }
    }
}

pub(crate) async fn wait_for_output_retry_observation(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> OutputRetryObservation {
    let deadline = Instant::now() + timeout;
    let mut observation = OutputRetryObservation::default();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok((status, _)) = api.get_output_status(pipeline_id, output_id).await {
            observation.status_visible = status.status == "retrying" && status.retrying;
            observation.phase = status.phase;
            observation.failure_phase = status
                .failure_phase
                .unwrap_or_else(|| "unknown".to_string());
            observation.last_error = status.last_error.unwrap_or_default();
            observation.has_error = !observation.last_error.is_empty();
            if observation.status_visible {
                observation.attempts = status.retry_attempts;
                observation.backoff_ms = status.retry_backoff_ms;
            }
        }
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let output = &health["pipelines"][pipeline_id]["outputs"][output_id];
            observation.health_visible = output["status"].as_str() == Some("retrying")
                && output["retrying"].as_bool() == Some(true);
        }
        if observation.status_visible && observation.health_visible && observation.has_error {
            break;
        }
    }
    observation
}

pub(crate) async fn wait_for_output_retry_or_cleanup_observation(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> OutputRetryObservation {
    let deadline = Instant::now() + timeout;
    let mut observation = OutputRetryObservation::default();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match api.get_output_status(pipeline_id, output_id).await {
            Err(_) => {
                observation.cleaned_up = true;
                observation.phase = "cleaned-up".to_string();
                break;
            }
            Ok((status, _)) => {
                observation.status_visible = status.status == "retrying";
                observation.phase = status.phase;
                observation.last_error = status.last_error.unwrap_or_default();
                observation.has_error = !observation.last_error.is_empty();
                if observation.status_visible {
                    observation.attempts = status.retry_attempts;
                    observation.backoff_ms = status.retry_backoff_ms;
                }
            }
        }
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            observation.health_visible =
                health["pipelines"][pipeline_id]["outputs"][output_id]["status"].as_str()
                    == Some("retrying");
        }
        if observation.status_visible && observation.has_error {
            break;
        }
    }
    observation
}

pub(crate) fn output_retry_or_cleanup_phase_ok(observation: &OutputRetryObservation) -> bool {
    observation.cleaned_up || (observation.status_visible && observation.has_error)
}

pub(crate) struct HarnessInput {
    pub(crate) id: String,
    pub(crate) stream_key: String,
}

pub(crate) async fn create_backup_input(
    api: &RampApi,
    pipeline_id: &str,
) -> Result<HarnessInput, String> {
    let response = api
        .post_json(
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            json!({"label": "Warm standby"}),
        )
        .await?;
    let input = &response["input"];
    Ok(HarnessInput {
        id: input["id"]
            .as_str()
            .ok_or_else(|| "created backup input is missing id".to_string())?
            .to_string(),
        stream_key: input["streamKey"]
            .as_str()
            .ok_or_else(|| "created backup input is missing stream key".to_string())?
            .to_string(),
    })
}
