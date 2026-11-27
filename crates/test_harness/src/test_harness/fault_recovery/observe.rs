//! Pipeline setup and output/sink observation shared by the fault cells.

use super::super::*;

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
