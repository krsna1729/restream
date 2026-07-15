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

// ─── Derivation ──────────────────────────────────────────────────────────────

/// Derive alerts from a `health_snapshot()` JSON value.
/// Returns alerts sorted Critical-first, then Warning, then by pipeline id.
pub fn derive_alerts(snapshot: &serde_json::Value) -> Vec<Alert> {
    let generated_at = snapshot
        .get("generatedAt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

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
mod tests {
    use super::*;
    use crate::domain::stage::{StageKey, StageKind};
    use crate::domain::state::{StageBackendKind, StagePhase};
    use crate::runtime::stage::{StageRuntimeSnapshot, phase_name};
    use serde_json::json;

    fn snapshot_with_pipeline(pipeline_id: &str, input_status: &str) -> serde_json::Value {
        json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                pipeline_id: {
                    "input": {
                        "status": input_status,
                        "readerMetrics": []
                    },
                    "outputs": {}
                }
            }
        })
    }

    fn stage_snapshot(
        key: StageKey,
        phase: StagePhase,
        bytes_in: u64,
        bytes_out: u64,
        last_error: Option<&str>,
    ) -> StageRuntimeSnapshot {
        let backend = match &phase {
            StagePhase::WaitingForCapacity { backend }
            | StagePhase::CapacityAcquired { backend }
            | StagePhase::StartingBackend { backend }
            | StagePhase::BackendSpawned { backend, .. } => *backend,
            _ => StageBackendKind::ExternalFfmpeg,
        };
        StageRuntimeSnapshot {
            key,
            backend,
            phase,
            backend_pid: None,
            bytes_in,
            bytes_out,
            packets_in: bytes_in.min(1),
            packets_out: bytes_out.min(1),
            first_input_at: None,
            first_output_at: None,
            last_error: last_error.map(ToString::to_string),
            capacity_permits_total: None,
            capacity_permits_available: None,
            capacity_wait_ms: None,
        }
    }

    #[test]
    fn clean_snapshot_yields_no_alerts() {
        let snap = snapshot_with_pipeline("pipe1", "on");
        assert!(derive_alerts(&snap).is_empty());
    }

    #[test]
    fn publisher_absent_yields_critical_alert() {
        let snap = snapshot_with_pipeline("pipe1", "off");
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Critical);
        assert_eq!(alerts[0].scope, Scope::Pipeline);
        assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe1"));
        assert!(alerts[0].id.contains("no_publisher"));
    }

    #[test]
    fn reader_lag_above_threshold_yields_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": {
                        "status": "on",
                        "readerMetrics": [
                            { "name": "rtmp_egress", "lagSlots": 300, "overflowCount": 0 }
                        ]
                    },
                    "outputs": {}
                }
            }
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Stage);
        assert!(alerts[0].id.contains("lag"));
    }

    #[test]
    fn reader_lag_below_threshold_yields_no_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": {
                        "status": "on",
                        "readerMetrics": [
                            { "name": "rtmp_egress", "lagSlots": 10, "overflowCount": 0 }
                        ]
                    },
                    "outputs": {}
                }
            }
        });
        assert!(derive_alerts(&snap).is_empty());
    }

    #[test]
    fn reader_overflow_yields_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": {
                        "status": "on",
                        "readerMetrics": [
                            { "name": "hls", "lagSlots": 0, "overflowCount": 5 }
                        ]
                    },
                    "outputs": {}
                }
            }
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Stage);
        assert!(alerts[0].id.contains("overflow"));
    }

    #[test]
    fn stopped_output_with_active_publisher_yields_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": {
                        "status": "on",
                        "readerMetrics": []
                    },
                    "outputs": {
                        "out1": { "status": "stopped", "totalSize": 0 }
                    }
                }
            }
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Output);
        assert_eq!(alerts[0].output_id.as_deref(), Some("out1"));
    }

    #[test]
    fn stopped_output_without_publisher_yields_no_alert() {
        // Output warnings are suppressed when there's no publisher — nothing to forward.
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": {
                        "status": "off",
                        "readerMetrics": []
                    },
                    "outputs": {
                        "out1": { "status": "stopped", "totalSize": 0 }
                    }
                }
            }
        });
        let alerts = derive_alerts(&snap);
        // Only the Critical no_publisher alert, not a Warning for output.
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Critical);
    }

    #[test]
    fn failed_output_phase_yields_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": { "status": "on", "readerMetrics": [] },
                    "outputs": {
                        "out1": {
                            "status": "running",
                            "phase": "failed",
                            "failurePhase": "connect",
                            "lastError": "connection refused"
                        }
                    }
                }
            }
        });

        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].scope, Scope::Output);
        assert!(alerts[0].id.contains("failed_phase"));
        assert!(
            alerts[0]
                .evidence
                .iter()
                .any(|e| e.contains("connection refused"))
        );
    }

    #[test]
    fn output_blocked_by_stage_yields_causal_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": { "status": "on", "readerMetrics": [] },
                    "outputs": {
                        "out1": {
                            "status": "running",
                            "phase": "waitingUpstream",
                            "terminalStage": "pipe1:video:720p",
                            "blockedBy": {
                                "stage": "pipe1:video:720p",
                                "phase": "waitingForCapacity",
                                "backend": "externalFfmpeg",
                                "capacityWaitMs": 7000
                            }
                        }
                    }
                }
            }
        });

        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].scope, Scope::Output);
        assert_eq!(alerts[0].output_id.as_deref(), Some("out1"));
        assert_eq!(alerts[0].stage_id.as_deref(), Some("pipe1:video:720p"));
        assert!(alerts[0].id.contains("blocked_by_stage"));
        assert!(
            alerts[0]
                .recommended_action
                .contains("Increase external FFmpeg capacity")
        );
    }

    #[test]
    fn stage_phase_table_is_consistent_for_status_graph_and_alerts() {
        let dependency = StageKey::new("pipe-stage-table", StageKind::source());
        let cases = vec![
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("planned")),
                StagePhase::Planned,
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("registered")),
                StagePhase::Registered,
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("dependency")),
                StagePhase::WaitingForDependency {
                    dependency: dependency.clone(),
                },
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("metadata")),
                StagePhase::WaitingForMetadata,
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("parameters")),
                StagePhase::WaitingForParameterSets,
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new(
                    "pipe-stage-table",
                    StageKind::preview("720p", StageKind::source()),
                ),
                StagePhase::WaitingForKeyframe,
                0,
                0,
                None,
                Some("waiting_for_keyframe"),
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("capacity")),
                StagePhase::WaitingForCapacity {
                    backend: StageBackendKind::ExternalFfmpeg,
                },
                0,
                0,
                None,
                Some("capacity_exhausted"),
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("acquired")),
                StagePhase::CapacityAcquired {
                    backend: StageBackendKind::ExternalFfmpeg,
                },
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("starting")),
                StagePhase::StartingBackend {
                    backend: StageBackendKind::ExternalFfmpeg,
                },
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("spawned")),
                StagePhase::BackendSpawned {
                    backend: StageBackendKind::ExternalFfmpeg,
                    pid: Some(1234),
                },
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("first-input")),
                StagePhase::FirstInput,
                256,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("no-output")),
                StagePhase::RunningNoOutputYet,
                256,
                0,
                None,
                Some("no_output"),
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("failed")),
                StagePhase::Failed,
                0,
                0,
                Some("synthetic failure"),
                Some("failed"),
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("stopping")),
                StagePhase::Stopping,
                0,
                0,
                None,
                None,
            ),
            (
                StageKey::new("pipe-stage-table", StageKind::video_preset("stopped")),
                StagePhase::Stopped,
                0,
                0,
                None,
                None,
            ),
        ];

        let mut stages = serde_json::Map::new();
        let mut expected_alert_fragments = Vec::new();
        for (key, phase, bytes_in, bytes_out, last_error, expected_alert) in cases {
            let snapshot =
                stage_snapshot(key.clone(), phase.clone(), bytes_in, bytes_out, last_error);
            let status_json = snapshot.to_json();
            let graph_node = crate::api_view_models::processing_graph_stage_node(
                key.kind.graph_node_id(key.pipeline.as_str()),
                key.kind.graph_type(),
                key.kind.graph_label(),
                key.to_string(),
                Some(&snapshot),
                true,
                None,
                None,
                None,
                json!({}),
            );

            assert_eq!(status_json["phase"], phase_name(&phase));
            assert_eq!(graph_node["details"]["phase"], status_json["phase"]);
            assert_eq!(
                graph_node["details"]["phaseDetail"],
                status_json["phaseDetail"]
            );
            if let Some(fragment) = expected_alert {
                expected_alert_fragments.push(fragment);
            }
            stages.insert(key.to_string(), status_json);
        }

        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {},
            "stages": stages
        });
        let alerts = derive_alerts(&snap);
        let alert_ids = alerts
            .iter()
            .map(|alert| alert.id.as_str())
            .collect::<Vec<_>>();

        for fragment in expected_alert_fragments {
            assert!(
                alert_ids.iter().any(|id| id.contains(fragment)),
                "missing alert containing {fragment}; got {alert_ids:?}"
            );
        }
    }

    #[test]
    fn stale_output_progress_yields_warning_after_successful_send() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe1": {
                    "input": { "status": "on", "readerMetrics": [] },
                    "outputs": {
                        "out1": {
                            "status": "running",
                            "phase": "sending",
                            "totalSize": 1316,
                            "lastProgressAgeMs": 12_000
                        }
                    }
                }
            }
        });

        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].scope, Scope::Output);
        assert!(alerts[0].id.contains("stale_progress"));
    }

    #[test]
    fn srt_udp_drops_yield_engine_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 42 },
            "pipelines": {}
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Engine);
    }

    #[test]
    fn low_nofile_limit_yields_engine_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "runtimeLimits": {
                "nofile": {
                    "configured": 65536,
                    "soft": 1024,
                    "hard": 1024,
                    "satisfied": false
                }
            },
            "srtListener": { "udpDrops": 0 },
            "pipelines": {}
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Engine);
        assert_eq!(alerts[0].id, "engine:runtime:nofile_limit_too_low");
        assert!(
            alerts[0]
                .evidence
                .iter()
                .any(|evidence| evidence == "soft = 1024")
        );
    }

    #[test]
    fn rtmp_fd_exhaustion_yields_critical_engine_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "rtmpListener": {
                "acceptErrors": 7,
                "fdExhaustionErrors": 3
            },
            "srtListener": { "udpDrops": 0 },
            "pipelines": {}
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Critical);
        assert_eq!(alerts[0].scope, Scope::Engine);
        assert_eq!(alerts[0].id, "engine:rtmp_listener:fd_exhaustion");
        assert!(
            alerts[0]
                .evidence
                .iter()
                .any(|evidence| evidence == "fdExhaustionErrors = 3")
        );
    }

    #[test]
    fn alerts_sorted_critical_before_warning() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 1 },
            "pipelines": {
                "pipe1": {
                    "input": { "status": "off", "readerMetrics": [] },
                    "outputs": {}
                }
            }
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0].severity, Severity::Critical);
        assert_eq!(alerts[1].severity, Severity::Warning);
    }

    #[test]
    fn tracker_stamps_first_and_last_seen() {
        let tracker = AlertTracker::new();
        let snap = snapshot_with_pipeline("pipe1", "off");
        let mut alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].first_seen.is_none());

        tracker.track(&mut alerts);
        let first = alerts[0].first_seen.clone().unwrap();
        let last = alerts[0].last_seen.clone().unwrap();
        assert_eq!(first, last);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn tracker_updates_last_seen_preserves_first_seen() {
        let tracker = AlertTracker::new();
        let snap = snapshot_with_pipeline("pipe1", "off");

        let mut alerts1 = derive_alerts(&snap);
        tracker.track(&mut alerts1);
        let first = alerts1[0].first_seen.clone().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut alerts2 = derive_alerts(&snap);
        tracker.track(&mut alerts2);
        assert_eq!(alerts2[0].first_seen.as_ref().unwrap(), &first);
        assert_ne!(alerts2[0].last_seen.as_ref().unwrap(), &first);
    }

    #[test]
    fn saturated_srt_receive_buffer_yields_input_causal_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe-srt": {
                    "input": {
                        "status": "on",
                        "readerMetrics": [],
                        "publisher": {
                            "protocol": "srt",
                            "quality": {
                                "srtRecvBufBytes": 8_218_796,
                                "srtRecvBufAvailBytes": 1_500
                            }
                        }
                    },
                    "outputs": {
                        "out-a": { "status": "stalled" }
                    }
                }
            }
        });

        let alerts = derive_alerts(&snap);
        let alert = alerts
            .iter()
            .find(|alert| alert.id == "pipeline:pipe-srt:input:srt_recv_buffer_saturated")
            .expect("saturated ingest buffer should produce a causal input alert");

        assert_eq!(alert.severity, Severity::Critical);
        assert_eq!(alert.scope, Scope::Pipeline);
        assert!(alert.title.contains("SRT publisher ingest"));
        assert!(alert.cause.contains("not draining ingest data"));
        assert!(alert.evidence.iter().any(|line| line.contains("100%")));
    }

    #[test]
    fn tracker_prunes_resolved_alerts() {
        let tracker = AlertTracker::new();

        let snap_off = snapshot_with_pipeline("pipe1", "off");
        let mut alerts = derive_alerts(&snap_off);
        tracker.track(&mut alerts);
        assert_eq!(tracker.active_count(), 1);

        let snap_on = snapshot_with_pipeline("pipe1", "on");
        let mut alerts = derive_alerts(&snap_on);
        assert!(alerts.is_empty());
        tracker.track(&mut alerts);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn tracker_pipeline_scope_does_not_prune_other_pipelines() {
        let tracker = AlertTracker::new();
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {
                "pipe-a": {
                    "input": { "status": "off", "readerMetrics": [] },
                    "outputs": {}
                },
                "pipe-b": {
                    "input": { "status": "off", "readerMetrics": [] },
                    "outputs": {}
                }
            }
        });

        let mut all_alerts = derive_alerts(&snap);
        tracker.track(&mut all_alerts);
        assert_eq!(tracker.active_count(), 2);

        let pipe_a = snapshot_with_pipeline("pipe-a", "off");
        let mut pipe_a_alerts = derive_alerts(&pipe_a);
        tracker.track_pipeline("pipe-a", &mut pipe_a_alerts);

        assert_eq!(tracker.active_count(), 2);
        assert_eq!(
            pipe_a_alerts[0].first_seen,
            all_alerts
                .iter()
                .find(|alert| alert.pipeline_id.as_deref() == Some("pipe-a"))
                .and_then(|alert| alert.first_seen.clone())
        );
    }

    #[test]
    fn stage_failed_phase_yields_warning_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {},
            "stages": {
                "pipe1:video_preset(720p)": {
                    "phase": "failed",
                    "lastError": "FFmpeg process exited with code 1"
                }
            }
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Stage);
        assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe1"));
        assert_eq!(alerts[0].stage_id.as_deref(), Some("video_preset(720p)"));
        assert!(alerts[0].id.contains("failed"));
        assert!(alerts[0].cause.contains("exited with code 1"));
    }

    #[test]
    fn stage_waiting_for_capacity_or_high_wait_yields_warning_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {},
            "stages": {
                "pipe2:video_preset(1080p)": {
                    "phase": {
                        "phase": "waitingForCapacity",
                        "backend": "externalFfmpeg"
                    },
                    "capacityWaitMs": 6000
                }
            }
        });
        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].severity, Severity::Warning);
        assert_eq!(alerts[0].scope, Scope::Stage);
        assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe2"));
        assert_eq!(alerts[0].stage_id.as_deref(), Some("video_preset(1080p)"));
        assert!(alerts[0].id.contains("capacity_exhausted"));
    }

    #[test]
    fn stage_receiving_input_without_output_yields_warning_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {},
            "stages": {
                "pipe2:video:720p": {
                    "phase": "runningNoOutputYet",
                    "bytesIn": 4096,
                    "bytesOut": 0,
                    "packetsIn": 4,
                    "packetsOut": 0
                }
            }
        });

        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].scope, Scope::Stage);
        assert!(alerts[0].id.contains("no_output"));
        assert!(
            alerts[0]
                .evidence
                .iter()
                .any(|evidence| evidence == "packetsIn = 4")
        );
    }

    #[test]
    fn hls_preview_waiting_for_keyframe_yields_warning_alert() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "pipelines": {},
            "stages": {
                "pipe2:preview:low:from:source": {
                    "phase": {
                        "phase": "waitingForKeyframe"
                    }
                }
            }
        });

        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].scope, Scope::Stage);
        assert!(alerts[0].id.contains("waiting_for_keyframe"));
        assert!(alerts[0].cause.contains("keyframe"));
    }

    #[test]
    fn stage_alerts_are_derived_without_pipeline_object() {
        let snap = json!({
            "generatedAt": "2026-06-25T00:00:00Z",
            "srtListener": { "udpDrops": 0 },
            "stages": {
                "pipe3:video:720p": {
                    "phase": "failed",
                    "lastError": "synthetic stage failure"
                }
            }
        });

        let alerts = derive_alerts(&snap);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].id, "pipeline:pipe3:stage:video:720p:failed");
        assert_eq!(alerts[0].pipeline_id.as_deref(), Some("pipe3"));
        assert_eq!(alerts[0].stage_id.as_deref(), Some("video:720p"));
    }
}
