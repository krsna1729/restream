//! Typed alert model and pure derivation from health snapshots.
//!
//! `derive_alerts` is pure — it takes a `health_snapshot()` JSON value and
//! returns a sorted `Vec<Alert>` (Critical before Warning). No I/O, no locks.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

// ─── Severity ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Critical,
    Warning,
}

impl Severity {
    fn rank(&self) -> u8 {
        match self {
            Severity::Critical => 0,
            Severity::Warning => 1,
        }
    }
}

// ─── Scope ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    Engine,
    Pipeline,
    Stage,
    Output,
}

// ─── Alert ───────────────────────────────────────────────────────────────────

/// A single derived health alert. The `id` field is a stable key for dedup
/// (same condition on the same entity always produces the same id).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: String,
    pub severity: Severity,
    pub scope: Scope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    pub title: String,
    pub cause: String,
    pub evidence: Vec<String>,
    pub recommended_action: String,
    /// Copied from `snapshot.generatedAt`.
    pub generated_at: String,
    /// When this alert condition was first observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seen: Option<String>,
    /// When this alert condition was most recently observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
}

// ─── Thresholds ──────────────────────────────────────────────────────────────

/// Ring-buffer lag slots above this threshold trigger a Warning.
/// 256 slots ≈ one full ring at standard frame rates (ring capacity is 512).
const LAG_SLOTS_WARN: u64 = 256;
const CAPACITY_WAIT_WARN_MS: u64 = 5_000;
const SRT_RECV_BUFFER_WARN_PCT: f64 = 80.0;
const SRT_RECV_BUFFER_CRITICAL_PCT: f64 = 95.0;
const COMMAND_CHANNEL_OVERLOAD_WARN_PCT: f64 = 80.0;
const RETRY_ADMISSION_WARN_PCT: u32 = 80;

// ─── Derivation ──────────────────────────────────────────────────────────────

/// Derive alerts from a `health_snapshot()` JSON value.
/// Returns alerts sorted Critical-first, then Warning, then by pipeline id.
pub fn derive_alerts(snapshot: &serde_json::Value) -> Vec<Alert> {
    let generated_at = snapshot
        .get("generatedAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let output_max_retries = snapshot["tuning"]["outputMaxRetries"]
        .as_u64()
        .unwrap_or(10);

    let mut alerts: Vec<Alert> = Vec::new();

    // ── Engine-level checks ───────────────────────────────────────────────────

    let srt = &snapshot["srtListener"];
    let udp_drops = srt.get("udpDrops").and_then(|v| v.as_u64()).unwrap_or(0);
    if udp_drops > 0 {
        alerts.push(Alert {
            id: "engine:srt_listener:udp_drops".into(),
            severity: Severity::Warning,
            scope: Scope::Engine,
            pipeline_id: None,
            stage_id: None,
            output_id: None,
            title: "SRT listener UDP drops detected".into(),
            cause: "The SRT listener's kernel receive queue is overflowing.".into(),
            evidence: vec![format!("udpDrops = {}", udp_drops)],
            recommended_action: "Increase SO_RCVBUF or reduce SRT publisher bandwidth.".into(),
            generated_at: generated_at.clone(),
            first_seen: None,
            last_seen: None,
        });
    }

    let nofile = &snapshot["runtimeLimits"]["nofile"];
    if nofile
        .get("satisfied")
        .and_then(|value| value.as_bool())
        .is_some_and(|satisfied| !satisfied)
    {
        let configured = nofile
            .get("configured")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        let soft = nofile.get("soft").and_then(|value| value.as_u64());
        let hard = nofile.get("hard").and_then(|value| value.as_u64());
        let mut evidence = vec![format!("configured = {}", configured)];
        if let Some(soft) = soft {
            evidence.push(format!("soft = {}", soft));
        }
        if let Some(hard) = hard {
            evidence.push(format!("hard = {}", hard));
        }
        alerts.push(Alert {
            id: "engine:runtime:nofile_limit_too_low".into(),
            severity: Severity::Warning,
            scope: Scope::Engine,
            pipeline_id: None,
            stage_id: None,
            output_id: None,
            title: "Runtime file descriptor limit is below configured target".into(),
            cause: "The process cannot open enough sockets/files for high fanout workloads."
                .into(),
            evidence,
            recommended_action:
                "Run the documented host bootstrap/configuration and restart Restream with the requested nofile limit available."
                    .into(),
            generated_at: generated_at.clone(),
            first_seen: None,
            last_seen: None,
        });
    }

    let rtmp = &snapshot["rtmpListener"];
    let fd_exhaustion = rtmp
        .get("fdExhaustionErrors")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if fd_exhaustion > 0 {
        let accept_errors = rtmp
            .get("acceptErrors")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        alerts.push(Alert {
            id: "engine:rtmp_listener:fd_exhaustion".into(),
            severity: Severity::Critical,
            scope: Scope::Engine,
            pipeline_id: None,
            stage_id: None,
            output_id: None,
            title: "RTMP listener exhausted file descriptors".into(),
            cause:
                "The RTMP listener hit the process or host open-file limit while accepting connections."
                    .into(),
            evidence: vec![
                format!("fdExhaustionErrors = {}", fd_exhaustion),
                format!("acceptErrors = {}", accept_errors),
            ],
            recommended_action:
                "Raise the process/host nofile limit, reduce concurrent connections, and restart affected publishers."
                    .into(),
            generated_at: generated_at.clone(),
            first_seen: None,
            last_seen: None,
        });
    }

    // ── Per-pipeline checks ───────────────────────────────────────────────────

    if let Some(pipelines) = snapshot.get("pipelines").and_then(|v| v.as_object()) {
        for (pipeline_id, pipeline) in pipelines {
            let input = &pipeline["input"];

            // No publisher
            let input_status = input.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if input_status == "off" {
                alerts.push(Alert {
                    id: format!("pipeline:{}:no_publisher", pipeline_id),
                    severity: Severity::Critical,
                    scope: Scope::Pipeline,
                    pipeline_id: Some(pipeline_id.clone()),
                    stage_id: None,
                    output_id: None,
                    title: "No active publisher".into(),
                    cause: "The pipeline is configured but not receiving a stream.".into(),
                    evidence: vec!["input.status = off".into()],
                    recommended_action:
                        "Start the publisher or check the stream key and connection.".into(),
                    generated_at: generated_at.clone(),
                    first_seen: None,
                    last_seen: None,
                });
            }

            let srt_recv_buffer = input
                .get("publisher")
                .and_then(|publisher| publisher.get("quality"))
                .and_then(srt_recv_buffer_occupancy);
            if let Some((recv_bytes, total_bytes, pct)) = srt_recv_buffer
                && pct >= SRT_RECV_BUFFER_WARN_PCT
            {
                let critical = pct >= SRT_RECV_BUFFER_CRITICAL_PCT;
                alerts.push(Alert {
                    id: format!("pipeline:{}:input:srt_recv_buffer_saturated", pipeline_id),
                    severity: if critical {
                        Severity::Critical
                    } else {
                        Severity::Warning
                    },
                    scope: Scope::Pipeline,
                    pipeline_id: Some(pipeline_id.clone()),
                    stage_id: None,
                    output_id: None,
                    title: if critical {
                        "SRT publisher ingest is not being drained".into()
                    } else {
                        "SRT publisher ingest receive buffer is filling".into()
                    },
                    cause: "The SRT application receive buffer is full or nearly full. The publisher can still be connected while Restream is not draining ingest data, so downstream outputs will stall.".into(),
                    evidence: vec![
                        format!(
                            "srtRecvBufBytes = {} / {} ({:.0}%)",
                            recv_bytes, total_bytes, pct
                        ),
                        "kernel UDP queue may still be empty because packets have already entered libsrt".into(),
                    ],
                    recommended_action:
                        "Treat this as an input/ingest issue first: restart the affected publisher or Restream, then inspect SRT ingest readiness if it recurs."
                            .into(),
                    generated_at: generated_at.clone(),
                    first_seen: None,
                    last_seen: None,
                });
            }

            // Per-reader: lag and overflow
            if let Some(readers) = input.get("readerMetrics").and_then(|v| v.as_array()) {
                for reader in readers {
                    let name = reader
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    let lag = reader.get("lagSlots").and_then(|v| v.as_u64()).unwrap_or(0);
                    if lag > LAG_SLOTS_WARN {
                        alerts.push(Alert {
                            id: format!("pipeline:{}:stage:{}:lag", pipeline_id, name),
                            severity: Severity::Warning,
                            scope: Scope::Stage,
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: Some(name.to_string()),
                            output_id: None,
                            title: format!("Stage '{}' is lagging behind the ring buffer", name),
                            cause: "The consumer is reading slower than the producer is writing."
                                .into(),
                            evidence: vec![format!(
                                "lagSlots = {} (threshold {})",
                                lag, LAG_SLOTS_WARN
                            )],
                            recommended_action:
                                "Check downstream network/encoder throughput or reduce output bitrate."
                                    .into(),
                            generated_at: generated_at.clone(),
                            first_seen: None,
                            last_seen: None,
                        });
                    }

                    let overflows = reader
                        .get("overflowCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if overflows > 0 {
                        alerts.push(Alert {
                            id: format!("pipeline:{}:stage:{}:overflow", pipeline_id, name),
                            severity: Severity::Warning,
                            scope: Scope::Stage,
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: Some(name.to_string()),
                            output_id: None,
                            title: format!(
                                "Stage '{}' has overflowed the ring buffer {} time(s)",
                                name, overflows
                            ),
                            cause:
                                "The ring buffer was full when this reader tried to consume packets; \
                                    some packets were skipped."
                                    .into(),
                            evidence: vec![format!("overflowCount = {}", overflows)],
                            recommended_action:
                                "Reduce output count or increase processing throughput.".into(),
                            generated_at: generated_at.clone(),
                            first_seen: None,
                            last_seen: None,
                        });
                    }
                }
            }

            // Per-output: non-running when there is an active publisher
            if input_status == "on"
                && let Some(outputs) = pipeline.get("outputs").and_then(|v| v.as_object())
            {
                for (output_id, output) in outputs {
                    let status = output.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    if status != "running" {
                        let retry_attempts = output.get("retryAttempts").and_then(|v| v.as_u64());
                        // A "retrying" output with attempts near the configured
                        // ceiling is a distinct, more urgent condition than an
                        // ordinary backoff cycle — it's about to exhaust its
                        // retry budget and give up, not just waiting out a
                        // transient failure. Give it a specific alert instead
                        // of the generic not_running one so an operator can
                        // tell "still retrying normally" from "about to stop
                        // retrying entirely."
                        if status == "retrying"
                            && let Some(attempts) = retry_attempts
                            && output_max_retries > 0
                            && attempts * 100
                                >= output_max_retries * RETRY_ADMISSION_WARN_PCT as u64
                        {
                            let backoff_ms = output
                                .get("retryBackoffMs")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            alerts.push(Alert {
                                id: format!(
                                    "pipeline:{}:output:{}:retry_admission_saturation",
                                    pipeline_id, output_id
                                ),
                                severity: Severity::Warning,
                                scope: Scope::Output,
                                pipeline_id: Some(pipeline_id.clone()),
                                stage_id: None,
                                output_id: Some(output_id.clone()),
                                title: format!(
                                    "Output '{}' is close to exhausting its retry budget",
                                    output_id
                                ),
                                cause: "The output has failed and retried repeatedly; once it \
                                    reaches the configured retry ceiling it stops retrying and \
                                    requires manual intervention to restart."
                                    .into(),
                                evidence: vec![format!(
                                    "retryAttempts = {attempts} / outputMaxRetries = {output_max_retries}, retryBackoffMs = {backoff_ms}"
                                )],
                                recommended_action:
                                    "Investigate why the destination keeps rejecting connections \
                                    before the retry budget is exhausted, or raise RESTREAM_OUTPUT_MAX_RETRIES."
                                        .into(),
                                generated_at: generated_at.clone(),
                                first_seen: None,
                                last_seen: None,
                            });
                            continue;
                        }

                        alerts.push(Alert {
                            id: format!(
                                "pipeline:{}:output:{}:not_running",
                                pipeline_id, output_id
                            ),
                            severity: Severity::Warning,
                            scope: Scope::Output,
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: None,
                            output_id: Some(output_id.clone()),
                            title: format!("Output '{}' is not running", output_id),
                            cause: format!(
                                "Output status is '{}' while the pipeline has an active publisher.",
                                status
                            ),
                            evidence: vec![format!("output.status = {}", status)],
                            recommended_action:
                                "Check the destination URL, credentials, and network reachability."
                                    .into(),
                            generated_at: generated_at.clone(),
                            first_seen: None,
                            last_seen: None,
                        });
                        continue;
                    }

                    let phase = output.get("phase").and_then(|v| v.as_str()).unwrap_or("");
                    if phase == "failed" {
                        let failure_phase = output
                            .get("failurePhase")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let last_error = output
                            .get("lastError")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        alerts.push(Alert {
                            id: format!("pipeline:{}:output:{}:failed_phase", pipeline_id, output_id),
                            severity: Severity::Warning,
                            scope: Scope::Output,
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: None,
                            output_id: Some(output_id.clone()),
                            title: format!("Output '{}' reported an egress failure", output_id),
                            cause: format!("Output failed during the '{}' phase.", failure_phase),
                            evidence: vec![
                                format!("output.phase = {}", phase),
                                format!("lastError = {}", last_error),
                            ],
                            recommended_action:
                                "Check destination reachability, credentials, and protocol settings."
                                    .into(),
                            generated_at: generated_at.clone(),
                            first_seen: None,
                            last_seen: None,
                        });
                        continue;
                    }

                    if let Some(blocked_by_value) = output.get("blockedBy")
                        && let Some(blocked_by) = blocked_by_value.as_object()
                    {
                        let stage = blocked_by
                            .get("stage")
                            .and_then(|v| v.as_str())
                            .or_else(|| output.get("terminalStage").and_then(|v| v.as_str()))
                            .unwrap_or("unknown");
                        let blocked_phase = stage_phase_name(blocked_by_value);
                        let backend = blocked_by
                            .get("backend")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        alerts.push(Alert {
                            id: format!(
                                "pipeline:{}:output:{}:blocked_by_stage",
                                pipeline_id, output_id
                            ),
                            severity: Severity::Warning,
                            scope: Scope::Output,
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: Some(stage.to_string()),
                            output_id: Some(output_id.clone()),
                            title: format!("Output '{}' is blocked by upstream stage", output_id),
                            cause: format!(
                                "The output is waiting on stage '{}' in phase '{}'.",
                                stage, blocked_phase
                            ),
                            evidence: vec![
                                format!("blockedBy.stage = {}", stage),
                                format!("blockedBy.phase = {}", blocked_phase),
                                format!("blockedBy.backend = {}", backend),
                            ],
                            recommended_action: blocked_output_action(blocked_phase).into(),
                            generated_at: generated_at.clone(),
                            first_seen: None,
                            last_seen: None,
                        });
                        continue;
                    }

                    let last_progress_age_ms = output
                        .get("lastProgressAgeMs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let total_size = output
                        .get("totalSize")
                        .and_then(|v| v.as_u64())
                        .unwrap_or_else(|| {
                            output.get("bytesOut").and_then(|v| v.as_u64()).unwrap_or(0)
                        });
                    if total_size > 0 && last_progress_age_ms >= 10_000 {
                        alerts.push(Alert {
                            id: format!("pipeline:{}:output:{}:stale_progress", pipeline_id, output_id),
                            severity: Severity::Warning,
                            scope: Scope::Output,
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: None,
                            output_id: Some(output_id.clone()),
                            title: format!("Output '{}' has stopped making progress", output_id),
                            cause:
                                "The output is still registered but has not completed a send recently."
                                    .into(),
                            evidence: vec![format!(
                                "lastProgressAgeMs = {} (threshold 10000)",
                                last_progress_age_ms
                            )],
                            recommended_action:
                                "Check downstream network health or restart the output if it remains stale."
                                    .into(),
                            generated_at: generated_at.clone(),
                            first_seen: None,
                            last_seen: None,
                        });
                    }
                }
            }
        }
    }

    // ── Per-stage checks ──────────────────────────────────────────────────────

    if let Some(stages) = snapshot.get("stages").and_then(|v| v.as_object()) {
        for (stage_key, stage_info) in stages {
            if let Some((pipeline_id, stage_kind)) = stage_key.split_once(':') {
                let phase_name = stage_phase_name(stage_info);

                if phase_name == "failed" {
                    let last_error = stage_info
                        .get("lastError")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error");
                    alerts.push(Alert {
                        id: format!("pipeline:{}:stage:{}:failed", pipeline_id, stage_kind),
                        severity: Severity::Warning,
                        scope: Scope::Stage,
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        output_id: None,
                        title: format!("Stage '{}' has failed", stage_kind),
                        cause: format!("The processing stage failed with error: {}.", last_error),
                        evidence: vec![
                            "phase = failed".into(),
                            format!("lastError = {}", last_error),
                        ],
                        recommended_action: "Check the transcoder logs, resource limits, and media source compatibility.".into(),
                        generated_at: generated_at.clone(),
                        first_seen: None,
                        last_seen: None,
                    });
                }

                let packets_in = stage_info
                    .get("packetsIn")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let packets_out = stage_info
                    .get("packetsOut")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let bytes_in = stage_info
                    .get("bytesIn")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let bytes_out = stage_info
                    .get("bytesOut")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if phase_name == "runningNoOutputYet"
                    && (packets_in > 0 || bytes_in > 0)
                    && packets_out == 0
                    && bytes_out == 0
                {
                    alerts.push(Alert {
                        id: format!("pipeline:{}:stage:{}:no_output", pipeline_id, stage_kind),
                        severity: Severity::Warning,
                        scope: Scope::Stage,
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        output_id: None,
                        title: format!("Stage '{}' is receiving input but has no output", stage_kind),
                        cause: "The stage backend has accepted input but has not produced any packets."
                            .into(),
                        evidence: vec![
                            format!("phase = {}", phase_name),
                            format!("packetsIn = {}", packets_in),
                            format!("packetsOut = {}", packets_out),
                            format!("bytesIn = {}", bytes_in),
                            format!("bytesOut = {}", bytes_out),
                        ],
                        recommended_action:
                            "Check backend stderr, codec compatibility, and downstream stage readiness."
                                .into(),
                        generated_at: generated_at.clone(),
                        first_seen: None,
                        last_seen: None,
                    });
                }

                if phase_name == "waitingForKeyframe"
                    && (stage_kind.starts_with("preview:") || stage_kind == "hls")
                {
                    alerts.push(Alert {
                        id: format!(
                            "pipeline:{}:stage:{}:waiting_for_keyframe",
                            pipeline_id, stage_kind
                        ),
                        severity: Severity::Warning,
                        scope: Scope::Stage,
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        output_id: None,
                        title: format!("Preview stage '{}' is waiting for a keyframe", stage_kind),
                        cause:
                            "HLS/preview output cannot start until the source produces a video keyframe."
                                .into(),
                        evidence: vec![format!("phase = {}", phase_name)],
                        recommended_action:
                            "Shorten the source GOP/keyframe interval or wait for the next keyframe."
                                .into(),
                        generated_at: generated_at.clone(),
                        first_seen: None,
                        last_seen: None,
                    });
                }

                let capacity_wait_ms = stage_info
                    .get("capacityWaitMs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if phase_name == "waitingForCapacity" || capacity_wait_ms >= CAPACITY_WAIT_WARN_MS {
                    alerts.push(Alert {
                        id: format!("pipeline:{}:stage:{}:capacity_exhausted", pipeline_id, stage_kind),
                        severity: Severity::Warning,
                        scope: Scope::Stage,
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        output_id: None,
                        title: format!("Transcoding capacity exhausted for stage '{}'", stage_kind),
                        cause: "The stage is waiting for transcoding capacity/permits to become available.".into(),
                        evidence: vec![
                            format!("phase = {}", phase_name),
                            format!(
                                "capacityWaitMs = {} ms (threshold {})",
                                capacity_wait_ms, CAPACITY_WAIT_WARN_MS
                            ),
                        ],
                        recommended_action: "Increase RESTREAM_EXTERNAL_FFMPEG_PERMITS, reduce pipeline count, or lower encoding presets.".into(),
                        generated_at: generated_at.clone(),
                        first_seen: None,
                        last_seen: None,
                    });
                }
            }
        }
    }

    // ── Egress fabric shard checks ──────────────────────────────────────────

    for shard in snapshot["egressFabricShards"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let protocol = shard.get("protocol").and_then(|v| v.as_str()).unwrap_or("");
        let feed_id = shard.get("feedId").and_then(|v| v.as_str()).unwrap_or("");
        let shard_index = shard
            .get("shardIndex")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let state = shard.get("state").and_then(|v| v.as_str()).unwrap_or("");
        let shard_label = format!("{protocol} fabric shard {shard_index} (feed {feed_id})");

        match state {
            "panicked" => {
                alerts.push(Alert {
                    id: format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:panicked"),
                    severity: Severity::Critical,
                    scope: Scope::Engine,
                    pipeline_id: None,
                    stage_id: None,
                    output_id: None,
                    title: format!("Egress fabric shard panicked ({shard_label})"),
                    cause: "The shard's OS thread panicked and every output assigned to it lost its connection until the supervisor replaces the shard.".into(),
                    evidence: vec![format!("state = panicked ({shard_label})")],
                    recommended_action: "Check logs for the panic message and stack trace at the time this shard stopped; outputs on this shard will reconnect once the supervisor replaces it.".into(),
                    generated_at: generated_at.clone(),
                    first_seen: None,
                    last_seen: None,
                });
            }
            "stalled" => {
                let progress_age_ms = shard.get("progressAgeMs").and_then(|v| v.as_u64());
                let mut evidence = vec![format!("state = stalled ({shard_label})")];
                if let Some(age) = progress_age_ms {
                    evidence.push(format!("progressAgeMs = {age}"));
                }
                alerts.push(Alert {
                    id: format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:stalled"),
                    severity: Severity::Warning,
                    scope: Scope::Engine,
                    pipeline_id: None,
                    stage_id: None,
                    output_id: None,
                    title: format!("Egress fabric shard has made no progress ({shard_label})"),
                    cause: "The shard has produced no media ticks for longer than the stall threshold; every output assigned to it may be stuck.".into(),
                    evidence,
                    recommended_action: "Check whether this shard's assigned outputs have healthy destinations; a genuinely idle shard with no assigned outputs is expected to age past the threshold and is not itself a problem.".into(),
                    generated_at: generated_at.clone(),
                    first_seen: None,
                    last_seen: None,
                });
            }
            _ => {}
        }

        let command_depth = shard
            .get("commandDepth")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let command_capacity = shard
            .get("commandCapacity")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if command_capacity > 0 {
            let occupancy_pct = command_depth as f64 / command_capacity as f64 * 100.0;
            if occupancy_pct >= COMMAND_CHANNEL_OVERLOAD_WARN_PCT {
                alerts.push(Alert {
                    id: format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:command_overload"),
                    severity: Severity::Warning,
                    scope: Scope::Engine,
                    pipeline_id: None,
                    stage_id: None,
                    output_id: None,
                    title: format!("Egress fabric shard command channel near capacity ({shard_label})"),
                    cause: "The shard's command channel (add/remove/update dispatch) is close to full; further commands risk being rejected until the shard catches up.".into(),
                    evidence: vec![format!(
                        "commandDepth = {command_depth} / commandCapacity = {command_capacity} ({occupancy_pct:.1}%)"
                    )],
                    recommended_action: "Reduce the rate of output add/remove/update churn against this shard, or raise RESTREAM_EGRESS_COMMAND_CAPACITY.".into(),
                    generated_at: generated_at.clone(),
                    first_seen: None,
                    last_seen: None,
                });
            }
        }
    }

    sorted(alerts)
}

fn stage_phase_name(stage_info: &serde_json::Value) -> &str {
    stage_info
        .get("phase")
        .and_then(|phase| {
            if phase.is_string() {
                phase.as_str()
            } else {
                phase.get("phase").and_then(|v| v.as_str())
            }
        })
        .unwrap_or("")
}

fn blocked_output_action(phase: &str) -> &'static str {
    match phase {
        "waitingForCapacity" => {
            "Increase external FFmpeg capacity, reduce concurrent transcode outputs, or lower encoding presets."
        }
        "waitingForKeyframe" => {
            "Shorten the source GOP/keyframe interval or wait for the next keyframe."
        }
        "runningNoOutputYet" => {
            "Check backend stderr, codec compatibility, and stage output progress."
        }
        "failed" => {
            "Inspect the upstream stage failure and restart or reconfigure the affected output."
        }
        _ => "Inspect the upstream stage lifecycle and dependency chain for the blocked output.",
    }
}

fn srt_recv_buffer_occupancy(quality: &serde_json::Value) -> Option<(u64, u64, f64)> {
    let recv = quality.get("srtRecvBufBytes")?.as_i64()?.max(0) as u64;
    let avail = quality.get("srtRecvBufAvailBytes")?.as_i64()?.max(0) as u64;
    let total = recv.saturating_add(avail);
    if total == 0 {
        return None;
    }
    Some((recv, total, recv as f64 / total as f64 * 100.0))
}

fn sorted(mut alerts: Vec<Alert>) -> Vec<Alert> {
    alerts.sort_by(|a, b| {
        a.severity
            .rank()
            .cmp(&b.severity.rank())
            .then(a.pipeline_id.cmp(&b.pipeline_id))
            .then(a.id.cmp(&b.id))
    });
    alerts
}

// ─── Alert Tracker ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct AlertHistory {
    first_seen: String,
    last_seen: String,
    pipeline_id: Option<String>,
}

/// Tracks `first_seen`/`last_seen` timestamps for recurring alert conditions.
///
/// Call one of the `track_*` methods after each `derive_alerts` invocation. It
/// stamps each alert with its history and prunes entries only for the snapshot
/// scope that was actually observed.
pub struct AlertTracker {
    history: Mutex<HashMap<String, AlertHistory>>,
}

impl Default for AlertTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AlertTracker {
    pub fn new() -> Self {
        Self {
            history: Mutex::new(HashMap::new()),
        }
    }

    /// Stamp alerts from a complete snapshot and prune every resolved entry.
    pub fn track(&self, alerts: &mut [Alert]) {
        self.track_with_prune(alerts, |_| true);
    }

    /// Stamp alerts from a single-pipeline snapshot and prune only resolved
    /// entries for that same pipeline. Alerts for other pipelines remain intact
    /// because this snapshot did not observe them.
    pub fn track_pipeline(&self, pipeline_id: &str, alerts: &mut [Alert]) {
        self.track_with_prune(alerts, |history| {
            history.pipeline_id.as_deref() == Some(pipeline_id)
        });
    }

    fn track_with_prune(
        &self,
        alerts: &mut [Alert],
        mut should_prune_if_absent: impl FnMut(&AlertHistory) -> bool,
    ) {
        let now = chrono::Utc::now().to_rfc3339();
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let mut active_ids: HashMap<&str, ()> = HashMap::with_capacity(alerts.len());

        for alert in alerts.iter_mut() {
            active_ids.insert(&alert.id, ());
            let entry = history
                .entry(alert.id.clone())
                .or_insert_with(|| AlertHistory {
                    first_seen: now.clone(),
                    last_seen: now.clone(),
                    pipeline_id: alert.pipeline_id.clone(),
                });
            entry.last_seen = now.clone();
            alert.first_seen = Some(entry.first_seen.clone());
            alert.last_seen = Some(entry.last_seen.clone());
        }

        history.retain(|id, entry| {
            active_ids.contains_key(id.as_str()) || !should_prune_if_absent(entry)
        });
    }

    pub fn active_count(&self) -> usize {
        self.history.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;
