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
    let started = Instant::now();
    let mut next_log = started + Duration::from_secs(10);
    println!(
        "[harness-progress] outputs-progress start pipeline={pipeline_id} outputs={} timeout={}s",
        output_ids.len(),
        timeout.as_secs()
    );
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
                    "{}\n  outputName={} outputId={} encoding={} url={}\n  phase={}\n  terminalStage={}\n  blockedBy={}\n  blockedByPhase={}\n  backend={} waitMs={}\n  blockedMetrics=packetsIn:{} packetsOut:{} bytesIn:{} bytesOut:{}\n  lastError={}",
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
                    blocked_by.map(|blocked| blocked.packets_in).unwrap_or(0),
                    blocked_by.map(|blocked| blocked.packets_out).unwrap_or(0),
                    blocked_by.map(|blocked| blocked.bytes_in).unwrap_or(0),
                    blocked_by.map(|blocked| blocked.bytes_out).unwrap_or(0),
                    status.last_error.as_deref().unwrap_or("")
                ));
            }
        }
        if progressed == output_ids.len() {
            println!(
                "[harness-progress] outputs-progress pass pipeline={pipeline_id} outputs={}/{} elapsed={}s",
                progressed,
                output_ids.len(),
                started.elapsed().as_secs()
            );
            return Ok(());
        }
        if Instant::now() >= next_log {
            let first_stalled = stalled
                .first()
                .map(|entry| entry.lines().next().unwrap_or("unknown"))
                .unwrap_or("none");
            println!(
                "[harness-progress] outputs-progress wait pipeline={pipeline_id} outputs={}/{} elapsed={}s remaining={}s firstStalled={first_stalled}",
                progressed,
                output_ids.len(),
                started.elapsed().as_secs(),
                deadline.saturating_duration_since(Instant::now()).as_secs()
            );
            next_log += Duration::from_secs(10);
        }
        if Instant::now() >= deadline {
            let stage_diagnostics = pipeline_stage_diagnostics(api, pipeline_id)
                .await
                .unwrap_or_default();
            return Err(format!(
                "timed out waiting for outputs to make progress for pipeline {pipeline_id} within {:?}: {progressed}/{}; stalled={}{}",
                timeout,
                output_ids.len(),
                stalled.join(", "),
                stage_diagnostics
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn pipeline_stage_diagnostics(api: &RampApi, pipeline_id: &str) -> Result<String, String> {
    let telemetry = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
        .await?;
    let Some(stages) = telemetry["stages"].as_array() else {
        return Ok(String::new());
    };

    let mut rows = Vec::new();
    for stage in stages {
        let stage_key = stage["stageKey"].as_str().unwrap_or("unknown");
        let kind = stage["kind"].as_str().unwrap_or("unknown");
        let lifecycle = &stage["lifecycle"];
        let phase = lifecycle
            .get("phase")
            .and_then(|phase| phase.as_str())
            .unwrap_or("unknown");
        let backend = lifecycle["backend"].as_str().unwrap_or("unknown");
        let metrics = &stage["metrics"];
        let pipe_metrics = &stage["pipeMetrics"];
        let packets_in = metrics["packetsIn"].as_u64().unwrap_or(0);
        let packets_out = metrics["packetsOut"].as_u64().unwrap_or(0);
        let bytes_in = metrics["bytesIn"].as_u64().unwrap_or(0);
        let bytes_out = metrics["bytesOut"].as_u64().unwrap_or(0);
        let pipe_packets_in = pipe_metrics["packetsIn"].as_u64().unwrap_or(0);
        let pipe_packets_out = pipe_metrics["packetsOut"].as_u64().unwrap_or(0);
        let relevant = stage_key.contains("hevc")
            || stage_key.contains("h264")
            || stage_key.contains("atrack")
            || kind == "codec_edge"
            || kind == "audio_filter";
        if relevant || packets_out == 0 || bytes_out == 0 {
            rows.push(format!(
                "{} kind={} phase={} backend={} metrics=in:{}/{} out:{}/{} pipe=in:{} out:{}",
                stage_key,
                kind,
                phase,
                backend,
                packets_in,
                bytes_in,
                packets_out,
                bytes_out,
                pipe_packets_in,
                pipe_packets_out
            ));
        }
    }

    if rows.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("; stageDiagnostics=[{}]", rows.join(" | ")))
    }
}
