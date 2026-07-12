//! API/runtime status adapters for live operational snapshots.
//! This file owns HTTP-facing shaping for output status and health views that
//! read current engine state plus recent outcomes, retry state, recording, and
//! HLS activity without pushing those JSON concerns back into `MediaEngine`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use crate::api_view_models;
use crate::media::engine::MediaEngine;

const REQUIRED_RMEM_MAX: u64 = 26_214_400;
const REQUIRED_WMEM_MAX: u64 = 8_388_608;

#[cfg(unix)]
fn nofile_limit_json(configured: u64) -> serde_json::Value {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes to the provided rlimit struct for the current
    // process. The pointer is valid for the duration of the call.
    let read_ok = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 };
    if !read_ok {
        return serde_json::json!({
            "configured": configured,
            "soft": null,
            "hard": null,
            "satisfied": false,
            "error": "getrlimit failed",
        });
    }
    serde_json::json!({
        "configured": configured,
        "soft": limit.rlim_cur,
        "hard": limit.rlim_max,
        "satisfied": limit.rlim_cur >= configured,
    })
}

#[cfg(not(unix))]
fn nofile_limit_json(configured: u64) -> serde_json::Value {
    serde_json::json!({
        "configured": configured,
        "soft": null,
        "hard": null,
        "satisfied": true,
        "unsupported": true,
    })
}

fn proc_sys_u64(key: &str) -> Option<u64> {
    let path = format!("/proc/sys/{}", key.replace('.', "/"));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn host_info_setting_json(
    key: &str,
    label: &str,
    current: serde_json::Value,
    unit: &str,
    detail: impl Into<String>,
) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "label": label,
        "current": current,
        "required": null,
        "unit": unit,
        "status": "ok",
        "detail": detail.into(),
    })
}

fn host_setting_json(
    key: &str,
    label: &str,
    current: Option<u64>,
    required: u64,
    unit: &str,
    detail: Option<String>,
) -> serde_json::Value {
    let status = current.map_or(
        "unknown",
        |value| {
            if value >= required { "ok" } else { "warning" }
        },
    );
    serde_json::json!({
        "key": key,
        "label": label,
        "current": current,
        "required": required,
        "unit": unit,
        "status": status,
        "detail": detail,
    })
}

#[cfg(target_os = "linux")]
fn proc_status_value(key: &str) -> Option<String> {
    let contents = std::fs::read_to_string("/proc/self/status").ok()?;
    contents.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key).then(|| value.trim().to_string())
    })
}

#[cfg(target_os = "linux")]
fn parse_cpu_list_count(list: &str) -> Option<u64> {
    let mut count = 0u64;
    for part in list
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((start, end)) = part.split_once('-') else {
            part.parse::<u64>().ok()?;
            count = count.checked_add(1)?;
            continue;
        };
        let start = start.trim().parse::<u64>().ok()?;
        let end = end.trim().parse::<u64>().ok()?;
        if end < start {
            return None;
        }
        count = count.checked_add(end - start + 1)?;
    }
    Some(count)
}

#[cfg(target_os = "linux")]
fn read_cgroup_cpu_max() -> Option<(String, Option<f64>)> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let unified_path = cgroup.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        (hierarchy == "0" && controllers.is_empty()).then_some(path)
    })?;
    let relative = unified_path.trim_start_matches('/');
    let cpu_max_path = std::path::Path::new("/sys/fs/cgroup")
        .join(relative)
        .join("cpu.max");
    let value = std::fs::read_to_string(cpu_max_path).ok()?;
    Some(parse_cgroup_cpu_max(value.trim()))
}

#[cfg(target_os = "linux")]
fn parse_cgroup_cpu_max(value: &str) -> (String, Option<f64>) {
    let mut parts = value.split_whitespace();
    let quota = parts.next().unwrap_or("max");
    let period = parts.next().unwrap_or("");
    if quota == "max" {
        return (value.to_string(), None);
    }
    let quota = quota.parse::<f64>().ok();
    let period = period.parse::<f64>().ok();
    let cpus = match (quota, period) {
        (Some(quota), Some(period)) if period > 0.0 => Some(quota / period),
        _ => None,
    };
    (value.to_string(), cpus)
}

#[cfg(target_os = "linux")]
fn cpu_capacity_settings() -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    let online = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .ok()
        .and_then(|cpus| u64::try_from(cpus).ok());
    if let Some(cpus) = online {
        rows.push(host_info_setting_json(
            "runtime.cpu.available_parallelism",
            "Available CPU parallelism",
            serde_json::json!(cpus),
            "cpus",
            "basis for default Tokio worker sizing before workload-specific tuning",
        ));
    }
    if let Some(mask) = proc_status_value("Cpus_allowed_list") {
        let count = parse_cpu_list_count(&mask);
        rows.push(host_info_setting_json(
            "runtime.cpu.allowed_list",
            "Allowed CPU mask",
            serde_json::json!(mask),
            "cpuset",
            format!(
                "process scheduler affinity{}; container cpusets can make this smaller than the host",
                count.map(|value| format!(" ({value} CPUs)")).unwrap_or_default()
            ),
        ));
    }
    if let Some((raw, cpus)) = read_cgroup_cpu_max() {
        rows.push(host_info_setting_json(
            "runtime.cpu.cgroup_max",
            "Cgroup CPU quota",
            serde_json::json!(raw),
            "quota",
            cpus.map(|value| format!("effective quota {:.2} CPUs", value))
                .unwrap_or_else(|| {
                    "no cgroup CPU quota; scheduling is cpuset/host limited".to_string()
                }),
        ));
    }
    rows
}

#[cfg(not(target_os = "linux"))]
fn cpu_capacity_settings() -> Vec<serde_json::Value> {
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .ok()
        .and_then(|cpus| u64::try_from(cpus).ok());
    cpus.map(|cpus| {
        vec![host_info_setting_json(
            "runtime.cpu.available_parallelism",
            "Available CPU parallelism",
            serde_json::json!(cpus),
            "cpus",
            "basis for default Tokio worker sizing before workload-specific tuning",
        )]
    })
    .unwrap_or_default()
}

fn host_settings_json(engine: &MediaEngine) -> serde_json::Value {
    let nofile = nofile_limit_json(engine.config.tuning.nofile_limit);
    let nofile_soft = nofile.get("soft").and_then(|value| value.as_u64());
    let nofile_hard = nofile.get("hard").and_then(|value| value.as_u64());
    let nofile_detail = nofile_hard.map(|hard| format!("hard limit {hard}"));

    let mut rows = vec![
        host_setting_json(
            "runtime.nofile",
            "Open file descriptors",
            nofile_soft,
            engine.config.tuning.nofile_limit,
            "fds",
            nofile_detail,
        ),
        host_setting_json(
            "net.core.rmem_max",
            "Kernel receive buffer ceiling",
            proc_sys_u64("net.core.rmem_max"),
            REQUIRED_RMEM_MAX,
            "bytes",
            Some("needed for SRT UDP receive buffers".to_string()),
        ),
        host_setting_json(
            "net.core.wmem_max",
            "Kernel send buffer ceiling",
            proc_sys_u64("net.core.wmem_max"),
            REQUIRED_WMEM_MAX,
            "bytes",
            Some("needed for SRT UDP send buffers".to_string()),
        ),
        host_info_setting_json(
            "runtime.tokio.worker_threads",
            "Tokio async workers",
            serde_json::json!(engine.config.tokio_runtime.worker_threads),
            "threads",
            "async scheduler worker count; too many workers increased migrations and cache misses in MSR profiling",
        ),
        host_info_setting_json(
            "runtime.tokio.max_blocking_threads",
            "Tokio blocking thread cap",
            serde_json::json!(engine.config.tokio_runtime.max_blocking_threads),
            "threads",
            "upper bound for spawn_blocking work such as SRT handshakes and epoll waiters; protects ramp-up latency without unbounded idle thread footprint",
        ),
    ];
    rows.extend(cpu_capacity_settings());
    serde_json::json!(rows)
}

pub(crate) async fn output_status(
    engine: &MediaEngine,
    output_id: &str,
) -> Option<serde_json::Value> {
    let retry = engine.egresses.retry.read().await.get(output_id).cloned();
    let recent = engine.egresses.recent.read().await.get(output_id).cloned();
    let active = {
        let egresses = engine.egresses.active.read().await;
        egresses.get(output_id).map(|egress| {
            let terminal_stage_key = egress.terminal_stage_key.clone();
            let mut value = api_view_models::egress_runtime_json(egress, false, true, None);
            api_view_models::apply_recent_egress_instability_json(&mut value, recent.as_ref());
            api_view_models::apply_egress_retry_state_json(&mut value, retry.as_ref());
            value["totalSize"] = serde_json::json!(egress.bytes_sent.load(Ordering::Relaxed));
            value["bitrateKbps"] =
                serde_json::json!(MediaEngine::sample_egress_bitrate_kbps(egress));
            value["startedAt"] = serde_json::Value::String(egress.started_at.clone());

            let explanation = crate::runtime::output::OutputRuntimeExplanation {
                output_id: crate::domain::ids::OutputId::new(&egress.output_id),
                output_name: egress.output_name.clone(),
                encoding: egress.encoding.clone(),
                url: egress.target_url.clone(),
                phase: *egress.phase.lock().unwrap_or_else(|e| e.into_inner()),
                terminal_stage: terminal_stage_key.clone(),
                blocked_by: None,
            };
            (value, explanation, terminal_stage_key)
        })
    };

    if let Some((mut value, mut explanation, terminal_stage_key)) = active {
        let blocked_by = if let Some(key) = terminal_stage_key.as_ref() {
            engine.egress_blocked_by_stage_snapshot(key).await
        } else {
            None
        };
        if let Some(blocked_by) = blocked_by {
            explanation.blocked_by = Some(blocked_by.key.clone());
            value["blockedBy"] = blocked_by.to_json();
        }
        value["explanation"] = api_view_models::output_runtime_explanation_json(&explanation);

        return Some(value);
    }

    recent.as_ref().map(|outcome| {
        let mut value = api_view_models::recent_egress_runtime_json(outcome, false);
        api_view_models::apply_recent_egress_instability_json(&mut value, Some(outcome));
        api_view_models::apply_egress_retry_state_json(&mut value, retry.as_ref());
        value["totalSize"] = serde_json::json!(outcome.bytes_sent);
        value["bitrateKbps"] = serde_json::Value::Null;
        value["startedAt"] = serde_json::Value::String(outcome.started_at.clone());
        value
    })
}

pub(crate) async fn health_snapshot(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    let mut hls_snapshots = HashMap::new();
    for pipeline_id in pipeline_ids {
        hls_snapshots.insert(
            pipeline_id.clone(),
            engine.hls_dependency_snapshot(pipeline_id).await,
        );
    }

    let recent_ingests = engine.ingests.recent.read().await.clone();
    let recent_egresses = engine.egresses.recent.read().await.clone();
    let retry_egresses = engine.egresses.retry.read().await.clone();
    let rec_active_by_pipeline: HashMap<String, bool> = engine
        .recordings
        .cancel_tokens
        .read()
        .await
        .iter()
        .map(|(pipeline_id, token)| (pipeline_id.clone(), !token.is_cancelled()))
        .collect();
    let reader_metrics_by_pipeline: HashMap<String, (usize, Vec<serde_json::Value>)> = engine
        .ingests
        .pipelines
        .read()
        .await
        .iter()
        .map(|(pipeline_id, rb)| {
            let reader_snapshots = rb.reader_snapshots();
            let readers_count = reader_snapshots.len();
            let reader_metrics = reader_snapshots
                .iter()
                .map(api_view_models::reader_snapshot_json)
                .collect();
            (pipeline_id.clone(), (readers_count, reader_metrics))
        })
        .collect();
    let active_ingest_ids: HashSet<String> =
        engine.ingests.active.read().await.keys().cloned().collect();

    let (total_bytes_by_pipeline, mut outputs_by_pipeline, blocked_requests) = {
        let egresses = engine.egresses.active.read().await;
        let mut totals: HashMap<String, u64> = HashMap::new();
        let mut outputs: HashMap<String, serde_json::Map<String, serde_json::Value>> =
            HashMap::new();
        let mut blocked_requests = Vec::new();

        for (output_id, egress) in egresses.iter() {
            let pipeline_id = egress.pipeline_id.clone();
            let bytes_sent = egress.bytes_sent.load(Ordering::Relaxed);
            *totals.entry(pipeline_id.clone()).or_default() += bytes_sent;

            let bitrate_kbps = MediaEngine::sample_egress_bitrate_kbps(egress);
            let has_ingest = active_ingest_ids.contains(pipeline_id.as_str());
            let mut output_json =
                api_view_models::egress_runtime_json(egress, false, has_ingest, None);
            api_view_models::apply_recent_egress_instability_json(
                &mut output_json,
                recent_egresses.get(output_id),
            );
            api_view_models::apply_egress_retry_state_json(
                &mut output_json,
                retry_egresses.get(output_id),
            );
            output_json["totalSize"] = serde_json::json!(bytes_sent);
            output_json["bitrateKbps"] = serde_json::json!(bitrate_kbps);
            output_json["startedAt"] = serde_json::Value::String(egress.started_at.clone());
            if let Some(key) = egress.terminal_stage_key.clone() {
                blocked_requests.push((pipeline_id.clone(), output_id.to_string(), key));
            }
            outputs
                .entry(pipeline_id)
                .or_default()
                .insert(output_id.to_string(), output_json);
        }

        (totals, outputs, blocked_requests)
    };

    let active_inputs: HashMap<String, serde_json::Value> = {
        let ingests = engine.ingests.active.read().await;
        pipeline_ids
            .iter()
            .filter_map(|pipeline_id| {
                let ingest = ingests.get(pipeline_id.as_str())?;
                let (readers_count, reader_metrics) = reader_metrics_by_pipeline
                    .get(pipeline_id)
                    .cloned()
                    .unwrap_or_default();
                let total_bytes_sent = total_bytes_by_pipeline
                    .get(pipeline_id)
                    .copied()
                    .unwrap_or(0);
                Some((
                    pipeline_id.clone(),
                    api_view_models::active_pipeline_input_json(
                        ingest,
                        recent_ingests.get(pipeline_id.as_str()),
                        total_bytes_sent,
                        readers_count,
                        reader_metrics,
                    ),
                ))
            })
            .collect()
    };

    let mut pipelines_json = serde_json::Map::new();
    for pipeline_id in pipeline_ids {
        let total_bytes_sent = total_bytes_by_pipeline
            .get(pipeline_id)
            .copied()
            .unwrap_or(0);
        let (readers_count, reader_metrics) = reader_metrics_by_pipeline
            .get(pipeline_id)
            .cloned()
            .unwrap_or_default();
        let input_json = active_inputs.get(pipeline_id).cloned().unwrap_or_else(|| {
            api_view_models::inactive_pipeline_input_json(
                recent_ingests.get(pipeline_id.as_str()),
                total_bytes_sent,
                readers_count,
                reader_metrics,
                disconnect_grace_ms,
            )
        });

        let mut outputs_json = outputs_by_pipeline.remove(pipeline_id).unwrap_or_default();
        for (output_id, outcome) in recent_egresses.iter() {
            if outcome.pipeline_id == *pipeline_id && !outputs_json.contains_key(output_id) {
                let mut output_json = api_view_models::recent_egress_runtime_json(outcome, false);
                api_view_models::apply_recent_egress_instability_json(
                    &mut output_json,
                    Some(outcome),
                );
                api_view_models::apply_egress_retry_state_json(
                    &mut output_json,
                    retry_egresses.get(output_id),
                );
                output_json["totalSize"] = serde_json::json!(outcome.bytes_sent);
                output_json["bitrateKbps"] = serde_json::Value::Null;
                output_json["startedAt"] = serde_json::Value::String(outcome.started_at.clone());
                outputs_json.insert(output_id.to_string(), output_json);
            }
        }

        let rec_enabled = recording_enabled.get(pipeline_id).copied().unwrap_or(false);
        let rec_active = rec_active_by_pipeline
            .get(pipeline_id)
            .copied()
            .unwrap_or(false);
        let hls_snapshot = hls_snapshots
            .get(pipeline_id)
            .expect("precomputed HLS snapshot");

        pipelines_json.insert(
            pipeline_id.clone(),
            api_view_models::pipeline_health_json(
                input_json,
                outputs_json,
                rec_enabled,
                rec_active,
                api_view_models::hls_preview_json(
                    hls_snapshot.active,
                    hls_snapshot.persistent_consumers,
                    hls_snapshot.last_access_age_ms,
                    hls_snapshot.segments,
                    hls_snapshot.playlist_bytes,
                ),
            ),
        );
    }

    for (pipeline_id, output_id, key) in blocked_requests {
        let Some(blocked_by) = engine.egress_blocked_by_stage_snapshot(&key).await else {
            continue;
        };
        if let Some(output_json) = pipelines_json
            .get_mut(&pipeline_id)
            .and_then(|pipeline| pipeline.get_mut("outputs"))
            .and_then(|outputs| outputs.as_object_mut())
            .and_then(|outputs| outputs.get_mut(&output_id))
        {
            output_json["blockedBy"] = blocked_by.to_json();
        }
    }

    let rx_queue = engine
        .runtime
        .listener_stats
        .rx_queue_bytes
        .load(Ordering::Relaxed);
    let rx_max = engine
        .runtime
        .listener_stats
        .rx_queue_max_bytes
        .load(Ordering::Relaxed);
    let drops = engine.runtime.listener_stats.drops.load(Ordering::Relaxed);
    let bonding_available = engine
        .runtime
        .listener_stats
        .bonding_available
        .load(Ordering::Relaxed);
    let rtmp_accept_errors = engine
        .runtime
        .rtmp_listener_stats
        .rtmp_accept_errors
        .load(Ordering::Relaxed);
    let rtmp_fd_exhaustion_errors = engine
        .runtime
        .rtmp_listener_stats
        .rtmp_fd_exhaustion_errors
        .load(Ordering::Relaxed);

    let mut stages_json = serde_json::Map::new();
    for pipeline_id in pipeline_ids {
        for snap in engine.pipeline_stage_runtime_snapshots(pipeline_id).await {
            let key_str = snap.key.to_string();
            stages_json.insert(key_str, snap.to_json());
        }
    }

    serde_json::json!({
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "status": "ready",
        "pipelines": serde_json::Value::Object(pipelines_json),
        "stages": serde_json::Value::Object(stages_json),
        "runtimeLimits": {
            "nofile": nofile_limit_json(engine.config.tuning.nofile_limit),
        },
        "hostSettings": host_settings_json(engine),
        "rtmpListener": {
            "acceptErrors": rtmp_accept_errors,
            "fdExhaustionErrors": rtmp_fd_exhaustion_errors,
        },
        "srtListener": {
            "bondingAvailable": bonding_available,
            "udpRxQueueBytes": rx_queue,
            "udpRxQueuePeakBytes": rx_max,
            "udpDrops": drops,
        },
    })
}

pub(crate) async fn health_summary_snapshot(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    let recent_ingests = engine.ingests.recent.read().await.clone();
    let recent_egresses = engine.egresses.recent.read().await.clone();
    let retry_egresses = engine.egresses.retry.read().await.clone();
    let rec_active_by_pipeline: HashMap<String, bool> = engine
        .recordings
        .cancel_tokens
        .read()
        .await
        .iter()
        .map(|(pipeline_id, token)| (pipeline_id.clone(), !token.is_cancelled()))
        .collect();
    let reader_counts: HashMap<String, usize> = engine
        .ingests
        .pipelines
        .read()
        .await
        .iter()
        .map(|(pipeline_id, rb)| (pipeline_id.clone(), rb.reader_snapshots().len()))
        .collect();
    let active_ingest_ids: HashSet<String> =
        engine.ingests.active.read().await.keys().cloned().collect();

    let (total_bytes_by_pipeline, mut outputs_by_pipeline) = {
        let egresses = engine.egresses.active.read().await;
        let mut totals: HashMap<String, u64> = HashMap::new();
        let mut outputs: HashMap<String, serde_json::Map<String, serde_json::Value>> =
            HashMap::new();

        for (output_id, egress) in egresses.iter() {
            let pipeline_id = egress.pipeline_id.clone();
            let bytes_sent = egress.bytes_sent.load(Ordering::Relaxed);
            *totals.entry(pipeline_id.clone()).or_default() += bytes_sent;
            let bitrate_kbps = MediaEngine::sample_egress_bitrate_kbps(egress);
            let has_ingest = active_ingest_ids.contains(pipeline_id.as_str());
            let status = MediaEngine::egress_effective_status(egress, has_ingest);
            let retry_state = retry_egresses.get(output_id);

            outputs.entry(pipeline_id).or_default().insert(
                output_id.to_string(),
                serde_json::json!({
                    "status": if retry_state.is_some() {
                        "retrying".to_string()
                    } else {
                        status
                    },
                    "uptimeSecs": egress.start_instant.elapsed().as_secs_f64(),
                    "totalSize": bytes_sent,
                    "bitrateKbps": bitrate_kbps,
                    "retrying": retry_state.is_some(),
                }),
            );
        }

        (totals, outputs)
    };

    let active_inputs: HashMap<String, serde_json::Value> = {
        let ingests = engine.ingests.active.read().await;
        pipeline_ids
            .iter()
            .filter_map(|pipeline_id| {
                let ingest = ingests.get(pipeline_id.as_str())?;
                let total_bytes_sent = total_bytes_by_pipeline
                    .get(pipeline_id)
                    .copied()
                    .unwrap_or(0);
                let reader_count = reader_counts.get(pipeline_id).copied().unwrap_or(0);
                Some((
                    pipeline_id.clone(),
                    api_view_models::active_pipeline_input_summary_json(
                        ingest,
                        total_bytes_sent,
                        reader_count,
                    ),
                ))
            })
            .collect()
    };

    let mut pipelines_json = serde_json::Map::new();

    for pipeline_id in pipeline_ids {
        let total_bytes_sent = total_bytes_by_pipeline
            .get(pipeline_id)
            .copied()
            .unwrap_or(0);
        let reader_count = reader_counts.get(pipeline_id).copied().unwrap_or(0);
        let input_json = active_inputs.get(pipeline_id).cloned().unwrap_or_else(|| {
            api_view_models::inactive_pipeline_input_summary_json(
                recent_ingests.get(pipeline_id.as_str()),
                total_bytes_sent,
                reader_count,
                disconnect_grace_ms,
            )
        });

        let mut outputs_json = outputs_by_pipeline.remove(pipeline_id).unwrap_or_default();
        for (output_id, outcome) in recent_egresses.iter() {
            if outcome.pipeline_id != *pipeline_id || outputs_json.contains_key(output_id) {
                continue;
            }

            let retry_state = retry_egresses.get(output_id);
            outputs_json.insert(
                output_id.to_string(),
                serde_json::json!({
                    "status": if retry_state.is_some() {
                        "retrying".to_string()
                    } else {
                        outcome.status.to_string()
                    },
                    "uptimeSecs": outcome.uptime_secs,
                    "totalSize": outcome.bytes_sent,
                    "bitrateKbps": serde_json::Value::Null,
                    "retrying": retry_state.is_some(),
                }),
            );
        }

        let rec_enabled = recording_enabled.get(pipeline_id).copied().unwrap_or(false);
        let rec_active = rec_active_by_pipeline
            .get(pipeline_id)
            .copied()
            .unwrap_or(false);

        pipelines_json.insert(
            pipeline_id.clone(),
            api_view_models::pipeline_health_summary_json(
                input_json,
                outputs_json,
                rec_enabled,
                rec_active,
            ),
        );
    }

    serde_json::json!({
        "status": "ready",
        "pipelines": serde_json::Value::Object(pipelines_json),
        "runtimeLimits": {
            "nofile": nofile_limit_json(engine.config.tuning.nofile_limit),
        },
        "hostSettings": host_settings_json(engine),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::media::engine::MediaEngine;

    #[cfg(target_os = "linux")]
    use super::{parse_cgroup_cpu_max, parse_cpu_list_count};

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_list_count_handles_ranges_and_singletons() {
        assert_eq!(parse_cpu_list_count("0-3"), Some(4));
        assert_eq!(parse_cpu_list_count("0-1,4,7-9"), Some(6));
        assert_eq!(parse_cpu_list_count(" 2 , 5-6 "), Some(3));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_list_count_rejects_invalid_ranges() {
        assert_eq!(parse_cpu_list_count("4-2"), None);
        assert_eq!(parse_cpu_list_count("0,nope"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_cpu_max_reports_unlimited_and_quota() {
        assert_eq!(
            parse_cgroup_cpu_max("max 100000"),
            ("max 100000".to_string(), None)
        );
        let (raw, cpus) = parse_cgroup_cpu_max("250000 100000");
        assert_eq!(raw, "250000 100000");
        assert_eq!(cpus, Some(2.5));
    }

    #[tokio::test]
    async fn health_summary_snapshot_does_not_pin_ingest_guard_while_waiting_for_egress() {
        let engine = Arc::new(MediaEngine::new());
        let _blocked_egresses = engine.egresses.active.write().await;
        let health_engine = engine.clone();
        let health = tokio::spawn(async move {
            super::health_summary_snapshot(
                &health_engine,
                &["pipeline".to_string()],
                &HashMap::new(),
                0,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        let ingest_write =
            tokio::time::timeout(Duration::from_millis(100), engine.ingests.active.write())
                .await
                .expect(
                    "health summary must not hold an ingest read guard while blocked on egress",
                );
        drop(ingest_write);
        drop(_blocked_egresses);
        tokio::time::timeout(Duration::from_secs(1), health)
            .await
            .expect("health summary should complete after egress registry unblocks")
            .expect("health summary task should not panic");
    }

    #[tokio::test]
    async fn health_snapshot_does_not_pin_ingest_guard_while_waiting_for_egress() {
        let engine = Arc::new(MediaEngine::new());
        let _blocked_egresses = engine.egresses.active.write().await;
        let health_engine = engine.clone();
        let health = tokio::spawn(async move {
            super::health_snapshot(
                &health_engine,
                &["pipeline".to_string()],
                &HashMap::new(),
                0,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        let ingest_write =
            tokio::time::timeout(Duration::from_millis(100), engine.ingests.active.write())
                .await
                .expect("health must not hold an ingest read guard while blocked on egress");
        drop(ingest_write);
        drop(_blocked_egresses);
        tokio::time::timeout(Duration::from_secs(1), health)
            .await
            .expect("health should complete after egress registry unblocks")
            .expect("health task should not panic");
    }
}
