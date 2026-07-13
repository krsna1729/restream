//! Output lifecycle teardown helpers for mixed scenarios.

use super::*;

pub(crate) async fn stop_mixed_outputs(api: &RampApi, pipeline_id: &str, output_ids: &[String]) {
    for output_id in output_ids {
        let _ = api
            .post_null(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
            ))
            .await;
    }
}

pub(crate) async fn delete_mixed_outputs(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
) -> Result<(), String> {
    let mut errors = Vec::new();
    for output_id in output_ids {
        if let Err(error) = api
            .delete_json(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}"
            ))
            .await
        {
            errors.push(format!("{output_id}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "lifecycle: output delete failed: {}",
            errors.join("; ")
        ))
    }
}

pub(crate) async fn wait_for_outputs_stopped(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let config = api.get_json("/api/v1/settings").await?;
        let all_stopped = output_ids.iter().all(|output_id| {
            config["jobs"]
                .as_array()
                .and_then(|jobs| {
                    jobs.iter().find(|job| {
                        job["pipelineId"] == pipeline_id && job["outputId"] == output_id.as_str()
                    })
                })
                .and_then(|job| job["status"].as_str())
                .is_none_or(|status| matches!(status, "stopped" | "failed"))
        });
        if all_stopped {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("lifecycle: outputs did not all stop within 60 s".to_string());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub(crate) async fn delete_and_verify_mixed_outputs(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<Value, String> {
    delete_mixed_outputs(api, pipeline_id, output_ids).await?;
    wait_for_outputs_deleted(env, api, cfg, pipeline_id, output_ids, timeout).await
}

async fn wait_for_outputs_deleted(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let settings = api.get_json("/api/v1/settings").await?;
        let health = api.get_json("/api/v1/engine/health").await?;
        let telemetry = api.get_json("/api/v1/engine/telemetry").await?;
        let residue =
            output_cleanup_residue(pipeline_id, output_ids, &settings, &health, &telemetry);
        let summary = json!({
            "deleted": output_ids.len(),
            "residue": residue,
        });
        if summary["residue"]
            .as_array()
            .is_some_and(|items| items.is_empty())
        {
            write_cleanup_dump(
                env,
                cfg,
                "output-delete-pass",
                &settings,
                &health,
                &telemetry,
            )?;
            return Ok(summary);
        }
        if Instant::now() >= deadline {
            let failure_snapshot = json!({
                "summary": summary,
                "settings": settings,
                "health": health,
                "telemetry": telemetry,
            });
            write_json_pretty_atomic(
                &env.work_dir.join(format!(
                    "{}-cleanup-output-delete-failed.json",
                    safe_artifact_stem(cfg)
                )),
                &failure_snapshot,
            )?;
            return Err(format!(
                "lifecycle: deleted outputs still have runtime/config residue after {:?}: {}",
                timeout, failure_snapshot["summary"]["residue"]
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn write_cleanup_dump(
    env: &MixedEnv,
    cfg: &str,
    label: &str,
    settings: &Value,
    health: &Value,
    telemetry: &Value,
) -> Result<(), String> {
    let stem = safe_artifact_stem(cfg);
    write_json_pretty_atomic(
        &env.work_dir.join(format!("{stem}-{label}-settings.json")),
        settings,
    )?;
    write_json_pretty_atomic(
        &env.work_dir.join(format!("{stem}-{label}-health.json")),
        health,
    )?;
    write_json_pretty_atomic(
        &env.work_dir.join(format!("{stem}-{label}-telemetry.json")),
        telemetry,
    )
}

fn output_cleanup_residue(
    pipeline_id: &str,
    output_ids: &[String],
    settings: &Value,
    health: &Value,
    telemetry: &Value,
) -> Vec<Value> {
    let mut residue = Vec::new();
    for output_id in output_ids {
        if settings_output_exists(settings, pipeline_id, output_id) {
            residue.push(json!({"outputId": output_id, "surface": "settings.outputs"}));
        }
        if health_output_exists(health, pipeline_id, output_id) {
            residue.push(json!({"outputId": output_id, "surface": "engine.health.outputs"}));
        }
        if telemetry_egress_exists(telemetry, output_id) {
            residue.push(json!({"outputId": output_id, "surface": "engine.telemetry.egresses"}));
        }
        if telemetry_egress_queue_exists(telemetry, output_id) {
            residue.push(
                json!({"outputId": output_id, "surface": "engine.telemetry.avioEgressQueues"}),
            );
        }
    }
    residue
}

fn settings_output_exists(settings: &Value, pipeline_id: &str, output_id: &str) -> bool {
    settings["outputs"].as_array().is_some_and(|outputs| {
        outputs.iter().any(|output| {
            output["id"] == output_id
                && output["pipelineId"]
                    .as_str()
                    .is_none_or(|id| id == pipeline_id)
        })
    })
}

fn health_output_exists(health: &Value, pipeline_id: &str, output_id: &str) -> bool {
    health["pipelines"][pipeline_id]["outputs"]
        .as_object()
        .is_some_and(|outputs| outputs.contains_key(output_id))
}

fn telemetry_egress_exists(telemetry: &Value, output_id: &str) -> bool {
    telemetry["egresses"].as_array().is_some_and(|egresses| {
        egresses
            .iter()
            .any(|egress| egress["outputId"] == output_id || egress["id"] == output_id)
    })
}

fn telemetry_egress_queue_exists(telemetry: &Value, output_id: &str) -> bool {
    telemetry["memoryAccounting"]["avioEgressQueues"]
        .as_array()
        .is_some_and(|queues| {
            queues
                .iter()
                .any(|queue| queue["outputId"] == output_id || queue["id"] == output_id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_cleanup_residue_reports_persisted_runtime_and_queue_leaks() {
        let output_ids = vec!["out-1".to_string(), "out-2".to_string()];
        let settings = json!({
            "outputs": [{"id": "out-1", "pipelineId": "pipe-1"}],
        });
        let health = json!({
            "pipelines": {
                "pipe-1": {
                    "outputs": {
                        "out-2": {"status": "stopped"}
                    }
                }
            }
        });
        let telemetry = json!({
            "egresses": [{"outputId": "out-2"}],
            "memoryAccounting": {
                "avioEgressQueues": [{"outputId": "out-1"}]
            }
        });

        let residue = output_cleanup_residue("pipe-1", &output_ids, &settings, &health, &telemetry);

        assert_eq!(residue.len(), 4);
        assert!(
            residue
                .iter()
                .any(|entry| entry["surface"] == "settings.outputs")
        );
        assert!(
            residue
                .iter()
                .any(|entry| entry["surface"] == "engine.health.outputs")
        );
        assert!(
            residue
                .iter()
                .any(|entry| entry["surface"] == "engine.telemetry.egresses")
        );
        assert!(
            residue
                .iter()
                .any(|entry| entry["surface"] == "engine.telemetry.avioEgressQueues")
        );
    }

    #[test]
    fn output_cleanup_residue_accepts_clean_shutdown_surfaces() {
        let output_ids = vec!["out-1".to_string()];
        let residue = output_cleanup_residue(
            "pipe-1",
            &output_ids,
            &json!({"outputs": []}),
            &json!({"pipelines": {"pipe-1": {"outputs": {}}}}),
            &json!({"egresses": [], "memoryAccounting": {"avioEgressQueues": []}}),
        );

        assert!(residue.is_empty());
    }
}
