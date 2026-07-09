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
            let bytes_out = entry["bytesOut"].as_u64().unwrap_or(0);
            let metrics_bytes = entry["metrics"]["bytesOut"].as_u64().unwrap_or(0);
            let packets_out = entry["metrics"]["packetsOut"].as_u64().unwrap_or(0);
            if bytes_out > 0 || metrics_bytes > 0 || packets_out > 0 {
                progressed += 1;
            } else {
                let name = entry["outputName"].as_str().unwrap_or("unknown");
                let encoding = entry["encoding"].as_str().unwrap_or("unknown");
                let url = entry["targetUrl"].as_str().unwrap_or("unknown");
                let phase = entry["phase"].as_str().unwrap_or("unknown");
                let terminal_stage = entry["terminalStage"].as_str().unwrap_or("none");
                let blocked_by_stage = entry["blockedBy"]["stage"].as_str().unwrap_or("none");
                let blocked_by_phase = entry["blockedBy"]["phase"].as_str().unwrap_or("none");
                let backend = entry["blockedBy"]["backend"].as_str().unwrap_or("none");
                let wait_ms = entry["blockedBy"]["capacityWaitMs"]
                    .as_u64()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string());
                let last_error = entry["lastError"].as_str().unwrap_or("");
                let cell = mixed_env
                    .and_then(|env| env.output_cell_label(output_id))
                    .unwrap_or_else(|| "unregistered-cell".to_string());
                stalled.push(format!(
                    "{}\n  outputName={} outputId={} encoding={} url={}\n  phase={}\n  terminalStage={}\n  blockedBy={}\n  blockedByPhase={}\n  backend={} waitMs={}\n  lastError={}",
                    cell, name, output_id, encoding, url, phase, terminal_stage, blocked_by_stage, blocked_by_phase, backend, wait_ms, last_error
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
