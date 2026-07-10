//! Shared harness output-progress gates.

use super::*;

pub(crate) async fn wait_for_outputs_progress(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<(), String> {
    wait_for_outputs_progress_with_env(api, pipeline_id, output_ids, timeout, None).await
}

pub(crate) async fn wait_for_outputs_progress_with_env(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
    mixed_env: Option<&MixedEnv>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = api.get_json("/api/v1/engine/health").await?;
        let mut progressed = 0usize;
        let mut stalled = Vec::new();
        for output_id in output_ids {
            let entry = &health["pipelines"][pipeline_id]["outputs"][output_id];
            let status = match ApiOutputStatus::from_value(output_id, entry) {
                Ok(status) => status,
                Err(error) if entry.is_null() => {
                    let cell = mixed_env
                        .and_then(|env| env.output_cell_label(output_id))
                        .unwrap_or_else(|| "unregistered-cell".to_string());
                    stalled.push(format!(
                        "{cell}\n  outputId={output_id}\n  healthRow=missing\n  lastError={error}"
                    ));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if status.has_progress() {
                progressed += 1;
            } else {
                let blocked_by = status.blocked_by.as_ref();
                let wait_ms = blocked_by
                    .and_then(|blocked| blocked.capacity_wait_ms)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string());
                let cell = mixed_env
                    .and_then(|env| env.output_cell_label(output_id))
                    .unwrap_or_else(|| "unregistered-cell".to_string());
                stalled.push(format!(
                    "{}\n  outputName={} outputId={} encoding={} url={}\n  phase={}\n  terminalStage={}\n  blockedBy={}\n  blockedByPhase={}\n  backend={} waitMs={}\n  lastError={}",
                    cell,
                    status.output_name.as_deref().unwrap_or("unknown"),
                    status.output_id,
                    status.encoding.as_deref().unwrap_or("unknown"),
                    status.target_url.as_deref().unwrap_or("unknown"),
                    status.phase,
                    status.terminal_stage.as_deref().unwrap_or("none"),
                    blocked_by
                        .and_then(|blocked| blocked.stage.as_deref())
                        .unwrap_or("none"),
                    blocked_by
                        .and_then(|blocked| blocked.phase.as_deref())
                        .unwrap_or("none"),
                    blocked_by
                        .and_then(|blocked| blocked.backend.as_deref())
                        .unwrap_or("none"),
                    wait_ms,
                    status.last_error.as_deref().unwrap_or("")
                ));
            }
        }
        if progressed == output_ids.len() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "outputs did not make progress for pipeline {pipeline_id} within {:?}: {progressed}/{}; stalled={}",
                timeout,
                output_ids.len(),
                stalled.join(", ")
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
