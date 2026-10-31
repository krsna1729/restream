//! Output alerts, from the one output diagnosis
//! ([`crate::diagnosis`]).
//!
//! Each output in the health view is reduced to [`OutputSignals`] and
//! diagnosed. The cause picks the alert. An output whose cause is upstream
//! of it gets no alert of its own: the ingest has the pipeline's
//! `no_publisher` alert and the shard has its engine alert (which counts
//! the outputs it affects), so one cause raises one alert.

use std::collections::HashMap;

use super::{Alert, Evidence, Scope, Severity, blocked_output_action, stage_phase_name};
use crate::diagnosis::{Cause, Diagnosis, Layer, OutputSignals, ShardHealth, diagnose};
use restream_domain::domain::state::EGRESS_PROGRESS_STALE_MS;

/// `(feed id, shard index)` → health, from `egressFabricShards`.
pub(super) type ShardHealthMap<'a> = HashMap<(&'a str, u64), ShardHealth>;

pub(super) fn shard_health_map(snapshot: &serde_json::Value) -> ShardHealthMap<'_> {
    snapshot["egressFabricShards"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|shard| {
            let feed = shard.get("feedId").and_then(|v| v.as_str()).unwrap_or("");
            let index = shard
                .get("shardIndex")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let health = match shard.get("state").and_then(|v| v.as_str()) {
                Some("panicked") => ShardHealth::Panicked,
                Some("stalled") => ShardHealth::Stalled,
                Some("stopped") => ShardHealth::Stopped,
                _ => ShardHealth::Healthy,
            };
            ((feed, index), health)
        })
        .collect()
}

/// The signals the health view carries for one output of a pipeline whose
/// input is `input_on`.
pub(super) fn output_signals(
    output: &serde_json::Value,
    input_on: bool,
    shards: &ShardHealthMap<'_>,
    max_retries: u64,
) -> OutputSignals {
    let str_field = |key: &str| output.get(key).and_then(|v| v.as_str());
    let status = str_field("status").unwrap_or("");
    let phase = str_field("phase").unwrap_or("");
    let blocked_by = output
        .get("blockedBy")
        .filter(|value| value.is_object())
        .map(|blocked| {
            let stage = blocked
                .get("stage")
                .and_then(|v| v.as_str())
                .or_else(|| str_field("terminalStage"))
                .unwrap_or("unknown");
            (stage.to_string(), stage_phase_name(blocked).to_string())
        });
    let shard = match (
        str_field("fabricFeed"),
        output.get("shardId").and_then(|v| v.as_u64()),
    ) {
        (Some(feed), Some(index)) => Some((
            index as u32,
            shards
                .get(&(feed, index))
                .copied()
                .unwrap_or(ShardHealth::Healthy),
        )),
        _ => None,
    };
    let total_size = output
        .get("totalSize")
        .or_else(|| output.get("bytesOut"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let progress_age_ms = output
        .get("lastProgressAgeMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    OutputSignals {
        // The health view lists outputs that run or ran; the pipeline's
        // input decides whether they are expected to deliver.
        wants_running: true,
        retries_exhausted: false,
        has_ingest: input_on,
        blocked_by,
        shard,
        failing: matches!(status, "retrying" | "failed" | "stopped") || phase == "failed",
        last_failure: str_field("lastError").map(|error| {
            (
                str_field("failurePhase").map(str::to_string),
                error.to_string(),
            )
        }),
        retry: output
            .get("retryAttempts")
            .and_then(|v| v.as_u64())
            .filter(|_| status == "retrying")
            .map(|attempts| (attempts as u32, max_retries)),
        backpressure: str_field("backpressureReason").map(str::to_string),
        stalled: status == "stalled"
            || (total_size > 0 && progress_age_ms >= EGRESS_PROGRESS_STALE_MS),
    }
}

/// The alert for one diagnosed output, if its cause is its own.
pub(super) fn output_alert(
    pipeline_id: &str,
    output_id: &str,
    output: &serde_json::Value,
    diagnosis: Diagnosis,
    generated_at: &str,
) -> Option<Alert> {
    let str_field = |key: &str| output.get(key).and_then(|v| v.as_str());
    let status = str_field("status").unwrap_or("");
    let id = |code: &str| format!("pipeline:{pipeline_id}:output:{output_id}:{code}");
    let mut stage_id = None;
    let alert = match &diagnosis.cause {
        Cause::NoPublisher
        | Cause::ShardPanicked { .. }
        | Cause::ShardStalled { .. }
        | Cause::ShardStopped { .. } => return None,
        // The health view carries no restart-budget flag (`retries_exhausted`
        // stays false here); an exhausted output shows as `failed` and is
        // `rejected`. The runtime state reports `retriesExhausted`.
        Cause::RetriesExhausted => return None,
        // Transient pressure within the deadline: shown in the runtime
        // state, not an alert.
        Cause::Backpressured { stalled: false } => return None,
        Cause::StageNotProducing { stage, phase } => {
            let backend = output["blockedBy"]
                .get("backend")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            stage_id = Some(stage.clone());
            Alert::new(
                id("blocked_by_stage"),
                Severity::Warning,
                Scope::Output,
                format!("Output '{output_id}' is blocked by upstream stage"),
                format!("The output is waiting on stage '{stage}' in phase '{phase}'."),
                vec![
                    // Which stage blocks it is the diagnosis's attribution;
                    // that stage's phase and backend are readings.
                    Evidence::derived(format!("blockedBy.stage = {stage}")),
                    Evidence::measured(format!("blockedBy.phase = {phase}")),
                    Evidence::measured(format!("blockedBy.backend = {backend}")),
                ],
                blocked_output_action(phase),
                generated_at,
            )
        }
        Cause::CapacityRefused { error } => Alert::new(
            id("egress_capacity_refused"),
            Severity::Warning,
            Scope::Output,
            format!("Output '{output_id}' was refused: egress capacity is full"),
            "Admitting this output would push the feed's least-loaded egress shard past \
             its load ceiling and degrade every output on it, so it was refused. It \
             retries on its restart backoff until its retry budget is spent.",
            vec![Evidence::measured(error.clone())],
            "Stop other outputs of this pipeline, or run this destination on a host with \
             more CPUs.",
            generated_at,
        ),
        Cause::RetryBudgetNearlySpent { attempts, max } => {
            let backoff_ms = output
                .get("retryBackoffMs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Alert::new(
                id("retry_admission_saturation"),
                Severity::Warning,
                Scope::Output,
                format!("Output '{output_id}' is close to exhausting its retry budget"),
                "The output has failed and retried repeatedly; once it reaches the configured \
                 retry ceiling it stops retrying and requires manual intervention to restart.",
                vec![Evidence::measured(format!(
                    "retryAttempts = {attempts} / outputMaxRetries = {max}, retryBackoffMs = {backoff_ms}"
                ))],
                "Investigate why the destination keeps rejecting connections before the retry \
                 budget is exhausted, or raise RESTREAM_OUTPUT_MAX_RETRIES.",
                generated_at,
            )
        }
        Cause::Rejected { phase, error } if str_field("phase") == Some("failed") => {
            let phase = phase.as_deref().unwrap_or("unknown");
            Alert::new(
                id("failed_phase"),
                Severity::Warning,
                Scope::Output,
                format!("Output '{output_id}' reported an egress failure"),
                format!("Output failed during the '{phase}' phase."),
                vec![
                    Evidence::measured("output.phase = failed"),
                    Evidence::measured(format!("lastError = {error}")),
                ],
                "Check destination reachability, credentials, and protocol settings.",
                generated_at,
            )
        }
        Cause::Rejected { .. } => Alert::new(
            id("not_running"),
            Severity::Warning,
            Scope::Output,
            format!("Output '{output_id}' is not running"),
            format!("Output status is '{status}' while the pipeline has an active publisher."),
            vec![Evidence::measured(format!("output.status = {status}"))],
            "Check the destination URL, credentials, and network reachability.",
            generated_at,
        ),
        Cause::Backpressured { stalled: true } => Alert::new(
            id("send_path_stalled"),
            Severity::Warning,
            Scope::Output,
            format!("Output '{output_id}' cannot drain its send path"),
            "Data waits on the send path and has not drained within the deadline.",
            // The stall classifier's verdict from lag and age thresholds.
            vec![Evidence::derived("backpressureReason = stalled")],
            "Check the destination's receive rate and the network path to it.",
            generated_at,
        ),
        Cause::NoProgress => {
            let age_ms = output
                .get("lastProgressAgeMs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Alert::new(
                id("stale_progress"),
                Severity::Warning,
                Scope::Output,
                format!("Output '{output_id}' has stopped making progress"),
                "The output is still registered but has not completed a send recently.",
                vec![Evidence::measured(format!(
                    "lastProgressAgeMs = {age_ms} (threshold {EGRESS_PROGRESS_STALE_MS})"
                ))],
                "Check downstream network health or restart the output if it remains stale.",
                generated_at,
            )
        }
    };
    let mut alert = Alert {
        pipeline_id: Some(pipeline_id.to_string()),
        stage_id,
        output_id: Some(output_id.to_string()),
        ..alert
    };
    if !diagnosis.ruled_out.is_empty() {
        // Concluded by the diagnosis rules from the upstream layers' signals.
        alert.evidence.push(Evidence::derived(format!(
            "ruled out: {}",
            diagnosis
                .ruled_out
                .iter()
                .map(|layer| layer_name(*layer))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    alert.diagnosis = Some(diagnosis);
    Some(alert)
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Ingest => "ingest",
        Layer::Feed => "feed",
        Layer::Shard => "shard",
        Layer::Destination => "destination",
    }
}

/// Diagnoses every output of one pipeline: pushes its own alerts and counts
/// the outputs each unhealthy shard affects.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_output_alerts<'a>(
    pipeline_id: &str,
    pipeline: &serde_json::Value,
    input_on: bool,
    shards: &ShardHealthMap<'a>,
    max_retries: u64,
    generated_at: &str,
    affected: &mut HashMap<(String, u64), u64>,
    alerts: &mut Vec<Alert>,
) {
    let Some(outputs) = pipeline.get("outputs").and_then(|v| v.as_object()) else {
        return;
    };
    for (output_id, output) in outputs {
        let signals = output_signals(output, input_on, shards, max_retries);
        let Some(diagnosis) = diagnose(&signals) else {
            continue;
        };
        if diagnosis.layer == Layer::Shard
            && let (Some(feed), Some(index)) = (
                output.get("fabricFeed").and_then(|v| v.as_str()),
                output.get("shardId").and_then(|v| v.as_u64()),
            )
        {
            *affected.entry((feed.to_string(), index)).or_default() += 1;
        }
        if let Some(alert) = output_alert(pipeline_id, output_id, output, diagnosis, generated_at) {
            alerts.push(alert);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Evidence, derive_alerts};
    use crate::diagnosis::{Cause, Layer};
    use serde_json::json;

    fn snapshot(shard_state: &str) -> serde_json::Value {
        json!({
            "generatedAt": "2026-10-12T00:00:00Z",
            "egressFabricShards": [
                { "protocol": "srt", "feedId": "srt:pipe1:mux", "shardIndex": 1, "state": shard_state }
            ],
            "pipelines": {
                "pipe1": {
                    "input": { "status": "on", "readerMetrics": [] },
                    "outputs": {
                        "out1": {
                            "status": "stalled",
                            "phase": "sending",
                            "fabricFeed": "srt:pipe1:mux",
                            "shardId": 1,
                            "totalSize": 1316,
                            "lastProgressAgeMs": 30_000
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn an_output_on_a_stalled_shard_raises_the_shard_alert_only() {
        let alerts = derive_alerts(&snapshot("stalled"));
        assert_eq!(alerts.len(), 1, "{alerts:#?}");
        assert_eq!(
            alerts[0].id,
            "engine:egress_fabric:srt:srt:pipe1:mux:1:stalled"
        );
        assert!(
            alerts[0]
                .evidence
                .iter()
                .any(|e| e == &Evidence::derived("affectedOutputs = 1"))
        );
    }

    #[test]
    fn a_stopped_shard_alerts_only_while_outputs_run_on_it() {
        let alerts = derive_alerts(&snapshot("stopped"));
        assert_eq!(alerts.len(), 1, "{alerts:#?}");
        assert_eq!(
            alerts[0].id,
            "engine:egress_fabric:srt:srt:pipe1:mux:1:stopped"
        );

        let mut idle = snapshot("stopped");
        idle["pipelines"]["pipe1"]["outputs"] = json!({});
        assert!(derive_alerts(&idle).is_empty());
    }

    #[test]
    fn a_stall_on_a_healthy_shard_is_the_destination_with_the_shard_ruled_out() {
        let alerts = derive_alerts(&snapshot("healthy"));
        assert_eq!(alerts.len(), 1, "{alerts:#?}");
        assert_eq!(alerts[0].id, "pipeline:pipe1:output:out1:stale_progress");
        let diagnosis = alerts[0].diagnosis.as_ref().unwrap();
        assert_eq!(diagnosis.layer, Layer::Destination);
        assert_eq!(diagnosis.cause, Cause::NoProgress);
        assert_eq!(
            diagnosis.ruled_out,
            [Layer::Ingest, Layer::Feed, Layer::Shard]
        );
        assert!(
            alerts[0]
                .evidence
                .iter()
                .any(|e| e == &Evidence::derived("ruled out: ingest, feed, shard"))
        );
    }

    #[test]
    fn a_stalled_send_path_is_the_classifiers_verdict_not_a_reading() {
        let mut held = snapshot("healthy");
        held["pipelines"]["pipe1"]["outputs"]["out1"] = json!({
            "status": "running",
            "phase": "sending",
            "fabricFeed": "srt:pipe1:mux",
            "shardId": 1,
            "backpressureReason": "stalled"
        });
        let alerts = derive_alerts(&held);
        assert_eq!(alerts.len(), 1, "{alerts:#?}");
        assert_eq!(alerts[0].id, "pipeline:pipe1:output:out1:send_path_stalled");
        assert!(
            alerts[0]
                .evidence
                .contains(&Evidence::derived("backpressureReason = stalled"))
        );
    }
}
