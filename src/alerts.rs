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

const REPEATED_RESYNC_WARN_THRESHOLD: u64 = 5;

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

impl Alert {
    /// Build an alert with no pipeline/stage/output scoping and no dedup
    /// history. Callers that need `pipeline_id`/`stage_id`/`output_id` set
    /// override them with struct-update syntax: `Alert { pipeline_id:
    /// Some(...), ..Alert::new(...) }`.
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: String,
        severity: Severity,
        scope: Scope,
        title: impl Into<String>,
        cause: impl Into<String>,
        evidence: Vec<String>,
        recommended_action: impl Into<String>,
        generated_at: &str,
    ) -> Self {
        Alert {
            id,
            severity,
            scope,
            pipeline_id: None,
            stage_id: None,
            output_id: None,
            title: title.into(),
            cause: cause.into(),
            evidence,
            recommended_action: recommended_action.into(),
            generated_at: generated_at.to_string(),
            first_seen: None,
            last_seen: None,
        }
    }
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
        alerts.push(Alert::new(
            "engine:srt_listener:udp_drops".into(),
            Severity::Warning,
            Scope::Engine,
            "SRT listener UDP drops detected",
            "The SRT listener's kernel receive queue is overflowing.",
            vec![format!("udpDrops = {}", udp_drops)],
            "Increase SO_RCVBUF or reduce SRT publisher bandwidth.",
            &generated_at,
        ));
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
        alerts.push(Alert::new(
            "engine:runtime:nofile_limit_too_low".into(),
            Severity::Warning,
            Scope::Engine,
            "Runtime file descriptor limit is below configured target",
            "The process cannot open enough sockets/files for high fanout workloads.",
            evidence,
            "Run the documented host bootstrap/configuration and restart Restream with the requested nofile limit available.",
            &generated_at,
        ));
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
        alerts.push(Alert::new(
            "engine:rtmp_listener:fd_exhaustion".into(),
            Severity::Critical,
            Scope::Engine,
            "RTMP listener exhausted file descriptors",
            "The RTMP listener hit the process or host open-file limit while accepting connections.",
            vec![
                format!("fdExhaustionErrors = {}", fd_exhaustion),
                format!("acceptErrors = {}", accept_errors),
            ],
            "Raise the process/host nofile limit, reduce concurrent connections, and restart affected publishers.",
            &generated_at,
        ));
    }

    // ── Per-pipeline checks ───────────────────────────────────────────────────

    if let Some(pipelines) = snapshot.get("pipelines").and_then(|v| v.as_object()) {
        for (pipeline_id, pipeline) in pipelines {
            let input = &pipeline["input"];

            // No publisher
            let input_status = input.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if input_status == "off" {
                alerts.push(Alert {
                    pipeline_id: Some(pipeline_id.clone()),
                    ..Alert::new(
                        format!("pipeline:{}:no_publisher", pipeline_id),
                        Severity::Critical,
                        Scope::Pipeline,
                        "No active publisher",
                        "The pipeline is configured but not receiving a stream.",
                        vec!["input.status = off".into()],
                        "Start the publisher or check the stream key and connection.",
                        &generated_at,
                    )
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
                let severity = if critical {
                    Severity::Critical
                } else {
                    Severity::Warning
                };
                let title = if critical {
                    "SRT publisher ingest is not being drained"
                } else {
                    "SRT publisher ingest receive buffer is filling"
                };
                alerts.push(Alert {
                    pipeline_id: Some(pipeline_id.clone()),
                    ..Alert::new(
                        format!("pipeline:{}:input:srt_recv_buffer_saturated", pipeline_id),
                        severity,
                        Scope::Pipeline,
                        title,
                        "The SRT application receive buffer is full or nearly full. The publisher can still be connected while Restream is not draining ingest data, so downstream outputs will stall.",
                        vec![
                            format!(
                                "srtRecvBufBytes = {} / {} ({:.0}%)",
                                recv_bytes, total_bytes, pct
                            ),
                            "kernel UDP queue may still be empty because packets have already entered libsrt".into(),
                        ],
                        "Treat this as an input/ingest issue first: restart the affected publisher or Restream, then inspect SRT ingest readiness if it recurs.",
                        &generated_at,
                    )
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
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: Some(name.to_string()),
                            ..Alert::new(
                                format!("pipeline:{}:stage:{}:lag", pipeline_id, name),
                                Severity::Warning,
                                Scope::Stage,
                                format!("Stage '{}' is lagging behind the ring buffer", name),
                                "The consumer is reading slower than the producer is writing.",
                                vec![format!(
                                    "lagSlots = {} (threshold {})",
                                    lag, LAG_SLOTS_WARN
                                )],
                                "Check downstream network/encoder throughput or reduce output bitrate.",
                                &generated_at,
                            )
                        });
                    }

                    let overflows = reader
                        .get("overflowCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if overflows > 0 {
                        alerts.push(Alert {
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: Some(name.to_string()),
                            ..Alert::new(
                                format!("pipeline:{}:stage:{}:overflow", pipeline_id, name),
                                Severity::Warning,
                                Scope::Stage,
                                format!(
                                    "Stage '{}' has overflowed the ring buffer {} time(s)",
                                    name, overflows
                                ),
                                "The ring buffer was full when this reader tried to consume packets; \
                                    some packets were skipped.",
                                vec![format!("overflowCount = {}", overflows)],
                                "Reduce output count or increase processing throughput.",
                                &generated_at,
                            )
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
                                pipeline_id: Some(pipeline_id.clone()),
                                output_id: Some(output_id.clone()),
                                ..Alert::new(
                                    format!(
                                        "pipeline:{}:output:{}:retry_admission_saturation",
                                        pipeline_id, output_id
                                    ),
                                    Severity::Warning,
                                    Scope::Output,
                                    format!(
                                        "Output '{}' is close to exhausting its retry budget",
                                        output_id
                                    ),
                                    "The output has failed and retried repeatedly; once it \
                                    reaches the configured retry ceiling it stops retrying and \
                                    requires manual intervention to restart.",
                                    vec![format!(
                                        "retryAttempts = {attempts} / outputMaxRetries = {output_max_retries}, retryBackoffMs = {backoff_ms}"
                                    )],
                                    "Investigate why the destination keeps rejecting connections \
                                    before the retry budget is exhausted, or raise RESTREAM_OUTPUT_MAX_RETRIES.",
                                    &generated_at,
                                )
                            });
                            continue;
                        }

                        alerts.push(Alert {
                            pipeline_id: Some(pipeline_id.clone()),
                            output_id: Some(output_id.clone()),
                            ..Alert::new(
                                format!(
                                    "pipeline:{}:output:{}:not_running",
                                    pipeline_id, output_id
                                ),
                                Severity::Warning,
                                Scope::Output,
                                format!("Output '{}' is not running", output_id),
                                format!(
                                    "Output status is '{}' while the pipeline has an active publisher.",
                                    status
                                ),
                                vec![format!("output.status = {}", status)],
                                "Check the destination URL, credentials, and network reachability.",
                                &generated_at,
                            )
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
                            pipeline_id: Some(pipeline_id.clone()),
                            output_id: Some(output_id.clone()),
                            ..Alert::new(
                                format!("pipeline:{}:output:{}:failed_phase", pipeline_id, output_id),
                                Severity::Warning,
                                Scope::Output,
                                format!("Output '{}' reported an egress failure", output_id),
                                format!("Output failed during the '{}' phase.", failure_phase),
                                vec![
                                    format!("output.phase = {}", phase),
                                    format!("lastError = {}", last_error),
                                ],
                                "Check destination reachability, credentials, and protocol settings.",
                                &generated_at,
                            )
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
                            pipeline_id: Some(pipeline_id.clone()),
                            stage_id: Some(stage.to_string()),
                            output_id: Some(output_id.clone()),
                            ..Alert::new(
                                format!(
                                    "pipeline:{}:output:{}:blocked_by_stage",
                                    pipeline_id, output_id
                                ),
                                Severity::Warning,
                                Scope::Output,
                                format!("Output '{}' is blocked by upstream stage", output_id),
                                format!(
                                    "The output is waiting on stage '{}' in phase '{}'.",
                                    stage, blocked_phase
                                ),
                                vec![
                                    format!("blockedBy.stage = {}", stage),
                                    format!("blockedBy.phase = {}", blocked_phase),
                                    format!("blockedBy.backend = {}", backend),
                                ],
                                blocked_output_action(blocked_phase),
                                &generated_at,
                            )
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
                            pipeline_id: Some(pipeline_id.clone()),
                            output_id: Some(output_id.clone()),
                            ..Alert::new(
                                format!("pipeline:{}:output:{}:stale_progress", pipeline_id, output_id),
                                Severity::Warning,
                                Scope::Output,
                                format!("Output '{}' has stopped making progress", output_id),
                                "The output is still registered but has not completed a send recently.",
                                vec![format!(
                                    "lastProgressAgeMs = {} (threshold 10000)",
                                    last_progress_age_ms
                                )],
                                "Check downstream network health or restart the output if it remains stale.",
                                &generated_at,
                            )
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
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        ..Alert::new(
                            format!("pipeline:{}:stage:{}:failed", pipeline_id, stage_kind),
                            Severity::Warning,
                            Scope::Stage,
                            format!("Stage '{}' has failed", stage_kind),
                            format!("The processing stage failed with error: {}.", last_error),
                            vec![
                                "phase = failed".into(),
                                format!("lastError = {}", last_error),
                            ],
                            "Check the transcoder logs, resource limits, and media source compatibility.",
                            &generated_at,
                        )
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
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        ..Alert::new(
                            format!("pipeline:{}:stage:{}:no_output", pipeline_id, stage_kind),
                            Severity::Warning,
                            Scope::Stage,
                            format!("Stage '{}' is receiving input but has no output", stage_kind),
                            "The stage backend has accepted input but has not produced any packets.",
                            vec![
                                format!("phase = {}", phase_name),
                                format!("packetsIn = {}", packets_in),
                                format!("packetsOut = {}", packets_out),
                                format!("bytesIn = {}", bytes_in),
                                format!("bytesOut = {}", bytes_out),
                            ],
                            "Check backend stderr, codec compatibility, and downstream stage readiness.",
                            &generated_at,
                        )
                    });
                }

                if phase_name == "waitingForKeyframe"
                    && (stage_kind.starts_with("preview:") || stage_kind == "hls")
                {
                    alerts.push(Alert {
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        ..Alert::new(
                            format!(
                                "pipeline:{}:stage:{}:waiting_for_keyframe",
                                pipeline_id, stage_kind
                            ),
                            Severity::Warning,
                            Scope::Stage,
                            format!("Preview stage '{}' is waiting for a keyframe", stage_kind),
                            "HLS/preview output cannot start until the source produces a video keyframe.",
                            vec![format!("phase = {}", phase_name)],
                            "Shorten the source GOP/keyframe interval or wait for the next keyframe.",
                            &generated_at,
                        )
                    });
                }

                let capacity_wait_ms = stage_info
                    .get("capacityWaitMs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if phase_name == "waitingForCapacity" || capacity_wait_ms >= CAPACITY_WAIT_WARN_MS {
                    alerts.push(Alert {
                        pipeline_id: Some(pipeline_id.to_string()),
                        stage_id: Some(stage_kind.to_string()),
                        ..Alert::new(
                            format!("pipeline:{}:stage:{}:capacity_exhausted", pipeline_id, stage_kind),
                            Severity::Warning,
                            Scope::Stage,
                            format!("Transcoding capacity exhausted for stage '{}'", stage_kind),
                            "The stage is waiting for transcoding capacity/permits to become available.",
                            vec![
                                format!("phase = {}", phase_name),
                                format!(
                                    "capacityWaitMs = {} ms (threshold {})",
                                    capacity_wait_ms, CAPACITY_WAIT_WARN_MS
                                ),
                            ],
                            "Increase RESTREAM_EXTERNAL_FFMPEG_PERMITS, reduce pipeline count, or lower encoding presets.",
                            &generated_at,
                        )
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
                alerts.push(Alert::new(
                    format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:panicked"),
                    Severity::Critical,
                    Scope::Engine,
                    format!("Egress fabric shard panicked ({shard_label})"),
                    "The shard's OS thread panicked and every output assigned to it lost its connection until the supervisor replaces the shard.",
                    vec![format!("state = panicked ({shard_label})")],
                    "Check logs for the panic message and stack trace at the time this shard stopped; outputs on this shard will reconnect once the supervisor replaces it.",
                    &generated_at,
                ));
            }
            "stalled" => {
                let progress_age_ms = shard.get("progressAgeMs").and_then(|v| v.as_u64());
                let mut evidence = vec![format!("state = stalled ({shard_label})")];
                if let Some(age) = progress_age_ms {
                    evidence.push(format!("progressAgeMs = {age}"));
                }
                alerts.push(Alert::new(
                    format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:stalled"),
                    Severity::Warning,
                    Scope::Engine,
                    format!("Egress fabric shard has made no progress ({shard_label})"),
                    "The shard has produced no media ticks for longer than the stall threshold; every output assigned to it may be stuck.",
                    evidence,
                    "Check whether this shard's assigned outputs have healthy destinations; a genuinely idle shard with no assigned outputs is expected to age past the threshold and is not itself a problem.",
                    &generated_at,
                ));
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
                alerts.push(Alert::new(
                    format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:command_overload"),
                    Severity::Warning,
                    Scope::Engine,
                    format!("Egress fabric shard command channel near capacity ({shard_label})"),
                    "The shard's command channel (add/remove/update dispatch) is close to full; further commands risk being rejected until the shard catches up.",
                    vec![format!(
                        "commandDepth = {command_depth} / commandCapacity = {command_capacity} ({occupancy_pct:.1}%)"
                    )],
                    "Reduce the rate of output add/remove/update churn against this shard, or raise RESTREAM_EGRESS_COMMAND_CAPACITY.",
                    &generated_at,
                ));
            }
        }

        let resync_count = shard
            .get("resyncCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if resync_count >= REPEATED_RESYNC_WARN_THRESHOLD {
            alerts.push(Alert::new(
                format!("engine:egress_fabric:{protocol}:{feed_id}:{shard_index}:repeated_resync"),
                Severity::Warning,
                Scope::Engine,
                format!("Repeated egress resynchronizations detected ({shard_label})"),
                "The egress shard is experiencing repeated feed overruns and resynchronizing leaf cursors.",
                vec![format!(
                    "resyncCount = {resync_count} (threshold {REPEATED_RESYNC_WARN_THRESHOLD})"
                )],
                "Check pipeline ring buffer capacity and downstream network bandwidth.",
                &generated_at,
            ));
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
