//! Resource attribution map for operator and agent inspection.
//!
//! The map intentionally separates measured process values from derived
//! runtime attribution. Stage and queue memory is useful directional evidence,
//! but the process RSS remains the authoritative measured total.

use crate::media::engine::MediaEngine;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const DEFAULT_TOP_N: usize = 25;
const MAX_TOP_N: usize = 200;

static CHILD_PROCESS_CPU_SAMPLES: OnceLock<Mutex<HashMap<u32, ChildProcessCpuSample>>> =
    OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct ProcessResourceSnapshot {
    pub cpu_percent: f64,
    pub restream_cpu_percent: f64,
    pub external_ffmpeg_cpu_percent: f64,
    pub cpu_sample_ready: bool,
    pub restream_memory_bytes: u64,
    pub external_ffmpeg_memory_bytes: u64,
    pub total_memory_bytes: u64,
    pub external_ffmpeg_count: u64,
    pub process_thread_count: u64,
    pub fd_count: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct ChildProcessCpuSample {
    total_ticks: u64,
    process_ticks: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ChildProcessResource {
    cpu_percent: Option<f64>,
    memory_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceMapView {
    Summary,
    Grouped,
    Detail,
}

#[derive(Clone, Copy, Debug)]
pub struct ResourceMapOptions {
    pub view: ResourceMapView,
    pub top_n: usize,
}

impl Default for ResourceMapOptions {
    fn default() -> Self {
        Self {
            view: ResourceMapView::Grouped,
            top_n: DEFAULT_TOP_N,
        }
    }
}

impl ResourceMapOptions {
    pub fn new(view: ResourceMapView, top_n: Option<usize>) -> Self {
        Self {
            view,
            top_n: top_n.unwrap_or(DEFAULT_TOP_N).clamp(1, MAX_TOP_N),
        }
    }

    #[allow(dead_code)]
    pub fn summary() -> Self {
        Self {
            view: ResourceMapView::Summary,
            top_n: 1,
        }
    }
}

fn number_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn memory(attributed_bytes: u64, confidence: &str, source: &str) -> Value {
    json!({
        "attributedBytes": attributed_bytes,
        "confidence": confidence,
        "source": source,
    })
}

fn proc_rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kib = status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?.trim();
        value
            .split_whitespace()
            .next()
            .and_then(|number| number.parse::<u64>().ok())
    })?;
    Some(rss_kib.saturating_mul(1024))
}

fn child_process_resources(
    pids: impl IntoIterator<Item = u32>,
) -> HashMap<u32, ChildProcessResource> {
    let pids = pids.into_iter().collect::<Vec<_>>();
    if pids.is_empty() {
        return HashMap::new();
    }
    let total_ticks = crate::api::telemetry::proc_total_ticks();
    let core_count = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(1);
    let sample_store = CHILD_PROCESS_CPU_SAMPLES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut previous = match sample_store.lock() {
        Ok(lock) => lock,
        Err(_) => return HashMap::new(),
    };
    let mut resources = HashMap::new();
    for pid in pids {
        let process_ticks = crate::api::telemetry::proc_process_ticks(pid);
        let cpu_percent = total_ticks.zip(process_ticks).and_then(|(total, ticks)| {
            let sample = ChildProcessCpuSample {
                total_ticks: total,
                process_ticks: ticks,
            };
            let cpu = previous.get(&pid).and_then(|prev| {
                let total_delta = sample.total_ticks.saturating_sub(prev.total_ticks);
                if total_delta == 0 {
                    return None;
                }
                let process_delta = sample.process_ticks.saturating_sub(prev.process_ticks);
                let scale = core_count.max(1) as f64 * 100.0 / total_delta as f64;
                Some(process_delta as f64 * scale)
            });
            previous.insert(pid, sample);
            cpu
        });
        let memory_bytes = proc_rss_bytes(pid);
        resources.insert(
            pid,
            ChildProcessResource {
                cpu_percent,
                memory_bytes,
            },
        );
    }
    resources
}

fn node_memory_bytes(node: &Value) -> u64 {
    node.get("memory")
        .and_then(|memory| memory.get("attributedBytes"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn node_cpu_percent(node: &Value) -> f64 {
    node.get("cpuPercent")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn node_score(node: &Value) -> f64 {
    node_memory_bytes(node) as f64 + node_cpu_percent(node) * 1024.0 * 1024.0
}

fn group_key(node: &Value) -> String {
    let kind = node
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if kind == "egress" {
        let execution = node
            .get("execution")
            .and_then(Value::as_str)
            .unwrap_or("shared");
        let protocol = node
            .get("label")
            .and_then(Value::as_str)
            .and_then(|label| label.split_whitespace().next())
            .unwrap_or("unknown");
        return format!("egress:{protocol}:{execution}");
    }
    if kind == "stage" {
        let execution = node
            .get("execution")
            .and_then(Value::as_str)
            .unwrap_or("shared");
        return format!("stage:{execution}");
    }
    kind.to_string()
}

fn group_label(key: &str, count: u64) -> String {
    let mut parts = key.split(':');
    match parts.next().unwrap_or("unknown") {
        "egress" => {
            let protocol = parts.next().unwrap_or("unknown").to_uppercase();
            format!("{protocol} outputs ({count})")
        }
        "stage" => {
            let execution = parts.next().unwrap_or("shared");
            format!("{execution} stages ({count})")
        }
        "source_ring" => format!("Source rings ({count})"),
        "runtime_process" => "restream".to_string(),
        "child_process_group" => "External FFmpeg".to_string(),
        other => format!("{other} ({count})"),
    }
}

fn merge_thread_counts(target: &mut serde_json::Map<String, Value>, node: &Value) {
    let Some(threads) = node.get("threads").and_then(Value::as_object) else {
        return;
    };
    for (key, value) in threads {
        let Some(count) = value.as_u64() else {
            continue;
        };
        let next = target.get(key).and_then(Value::as_u64).unwrap_or(0) + count;
        target.insert(key.clone(), Value::from(next));
    }
}

fn append_hotspots(target: &mut Vec<String>, node: &Value) {
    let Some(hotspots) = node.get("hotspots").and_then(Value::as_array) else {
        return;
    };
    for hotspot in hotspots.iter().filter_map(Value::as_str) {
        if !target.iter().any(|existing| existing == hotspot) {
            target.push(hotspot.to_string());
        }
    }
}

fn grouped_nodes(nodes: &[Value], top_n: usize) -> Vec<Value> {
    let mut groups = std::collections::BTreeMap::<String, Vec<&Value>>::new();
    for node in nodes {
        groups.entry(group_key(node)).or_default().push(node);
    }

    let mut grouped = groups
        .into_iter()
        .map(|(key, members)| {
            let count = members.len() as u64;
            let cpu_percent = members
                .iter()
                .map(|node| node_cpu_percent(node))
                .sum::<f64>();
            let memory_bytes = members
                .iter()
                .map(|node| node_memory_bytes(node))
                .sum::<u64>();
            let measured_count = members
                .iter()
                .filter(|node| {
                    node.get("memory")
                        .and_then(|memory| memory.get("confidence"))
                        .and_then(Value::as_str)
                        == Some("measured")
                })
                .count();
            let confidence = if measured_count == members.len() {
                "measured"
            } else if measured_count == 0 {
                "derived"
            } else {
                "estimated"
            };
            let execution = members
                .first()
                .and_then(|node| node.get("execution"))
                .and_then(Value::as_str)
                .unwrap_or("shared");
            let mut threads = serde_json::Map::new();
            let mut hotspots = Vec::new();
            for node in &members {
                merge_thread_counts(&mut threads, node);
                append_hotspots(&mut hotspots, node);
            }
            json!({
                "id": format!("group:{key}"),
                "kind": "resource_group",
                "groupKind": key.clone(),
                "label": group_label(&key, count),
                "execution": execution,
                "cpuPercent": cpu_percent,
                "memory": memory(memory_bytes, confidence, "grouped_resource_nodes"),
                "threads": Value::Object(threads),
                "nodeCount": count,
                "hotspots": hotspots,
            })
        })
        .collect::<Vec<_>>();
    grouped.sort_by(|a, b| {
        node_score(b)
            .partial_cmp(&node_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    grouped.truncate(top_n);
    grouped
}

fn top_nodes(mut nodes: Vec<Value>, top_n: usize) -> Vec<Value> {
    nodes.sort_by(|a, b| {
        node_score(b)
            .partial_cmp(&node_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    nodes.truncate(top_n);
    nodes
}

fn execution_for_stage(stage: &Value) -> &'static str {
    let backend = stage
        .get("lifecycle")
        .and_then(|lifecycle| lifecycle.get("backend"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match backend {
        "externalFfmpeg" | "ExternalFfmpeg" => "child_process",
        "internalFfmpeg" | "InternalFfmpeg" | "recording" | "Recording" => "os_thread",
        "audioRouter" | "AudioRouter" | "hlsSegmenter" | "HlsSegmenter" => "tokio_task",
        _ => "shared",
    }
}

fn stage_memory_bytes(stage: &Value) -> u64 {
    stage
        .get("payloadStats")
        .and_then(|payload| payload.get("payloadBytes"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn stage_backend_pid(stage: &Value) -> Option<u32> {
    stage
        .get("lifecycle")
        .and_then(|lifecycle| lifecycle.get("backendPid"))
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
}

fn queue_hotspots(len: u64, capacity: u64, blocked_writes: u64) -> Vec<&'static str> {
    let mut hotspots = Vec::new();
    if capacity > 0 && len.saturating_mul(100) >= capacity.saturating_mul(75) {
        hotspots.push("queue_high");
    }
    if blocked_writes > 0 {
        hotspots.push("backpressure");
    }
    hotspots
}

fn stage_node(stage: &Value, child_resources: &HashMap<u32, ChildProcessResource>) -> Value {
    let stage_key = stage
        .get("stageKey")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = stage
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or(stage_key);
    let backend_pid = stage_backend_pid(stage);
    let child_resource = backend_pid.and_then(|pid| child_resources.get(&pid).copied());
    let attributed = child_resource
        .and_then(|resource| resource.memory_bytes)
        .unwrap_or_else(|| stage_memory_bytes(stage));
    let memory_confidence = if child_resource
        .and_then(|resource| resource.memory_bytes)
        .is_some()
    {
        "measured"
    } else {
        "derived"
    };
    let memory_source = if memory_confidence == "measured" {
        "child_process_rss"
    } else {
        "stage_payload_stats"
    };
    let mut hotspots = Vec::new();
    let metrics = stage.get("metrics").cloned().unwrap_or_else(|| json!({}));
    if metrics
        .get("processingUs")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        hotspots.push("processing");
    }
    if child_resource
        .and_then(|resource| resource.cpu_percent)
        .is_some_and(|cpu| cpu >= 75.0)
    {
        hotspots.push("cpu");
    }
    let mut node = json!({
        "id": stage_key,
        "kind": "stage",
        "label": kind,
        "pipelineId": stage.get("pipelineId").cloned().unwrap_or(Value::Null),
        "execution": execution_for_stage(stage),
        "memory": memory(attributed, memory_confidence, memory_source),
        "threads": {
            "appOwned": if execution_for_stage(stage) == "os_thread" { 1 } else { 0 },
            "childProcess": if execution_for_stage(stage) == "child_process" { 1 } else { 0 },
        },
        "metrics": metrics,
        "hotspots": hotspots,
    });
    if let Some(pid) = backend_pid {
        node["backendPid"] = json!(pid);
    }
    if let Some(cpu_percent) = child_resource.and_then(|resource| resource.cpu_percent) {
        node["cpuPercent"] = json!(cpu_percent);
    }
    node
}

fn egress_node(egress: &Value, avio_queues: &[Value]) -> Value {
    let output_id = egress
        .get("outputId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let protocol = egress
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let queue = avio_queues.iter().find(|queue| {
        queue
            .get("outputId")
            .and_then(Value::as_str)
            .is_some_and(|id| id == output_id)
    });
    let len = queue.map(|q| number_field(q, "lenBytes")).unwrap_or(0);
    let capacity = queue.map(|q| number_field(q, "capacityBytes")).unwrap_or(0);
    let blocked = queue.map(|q| number_field(q, "blockedWrites")).unwrap_or(0);
    json!({
        "id": output_id,
        "kind": "egress",
        "label": format!("{protocol} output"),
        "pipelineId": egress.get("pipelineId").cloned().unwrap_or(Value::Null),
        "execution": if protocol == "srt" { "os_thread" } else { "tokio_task" },
        "memory": memory(len, "derived", "avio_egress_queue_len"),
        "threads": {
            "appOwned": if protocol == "srt" { 1 } else { 0 },
            "childProcess": 0,
        },
        "status": egress.get("status").cloned().unwrap_or(Value::Null),
        "phase": egress.get("phase").cloned().unwrap_or(Value::Null),
        "metrics": egress.get("metrics").cloned().unwrap_or_else(|| json!({})),
        "queue": queue.cloned().unwrap_or(Value::Null),
        "hotspots": queue_hotspots(len, capacity, blocked),
    })
}

fn source_ring_node(ring: &Value) -> Value {
    let pipeline_id = ring
        .get("pipelineId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload_bytes = ring
        .get("payloadStats")
        .and_then(|stats| stats.get("payloadBytes"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "id": format!("{pipeline_id}:source-ring"),
        "kind": "source_ring",
        "label": "Source ring",
        "pipelineId": pipeline_id,
        "execution": "shared",
        "memory": memory(payload_bytes, "derived", "ring_payload_stats"),
        "threads": { "appOwned": 0, "childProcess": 0 },
        "metrics": ring.get("payloadStats").cloned().unwrap_or_else(|| json!({})),
        "hotspots": if payload_bytes > 0 { vec!["retained_payload"] } else { Vec::<&str>::new() },
    })
}

pub(crate) async fn resource_map(
    engine: &MediaEngine,
    process: ProcessResourceSnapshot,
    pipeline_id: Option<&str>,
    options: ResourceMapOptions,
) -> Value {
    let engine_telemetry = super::telemetry::engine_telemetry(engine).await;
    let telemetry = if let Some(pipeline_id) = pipeline_id {
        super::telemetry::pipeline_telemetry(engine, pipeline_id).await
    } else {
        engine_telemetry.clone()
    };
    let memory_accounting = engine_telemetry
        .get("memoryAccounting")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let avio_egress_queues: Vec<Value> = memory_accounting
        .get("avioQueues")
        .and_then(|queues| queues.get("egressQueues"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let stages = engine_telemetry
        .get("stages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|stage| {
            pipeline_id.is_none_or(|pipeline_id| {
                stage
                    .get("pipelineId")
                    .and_then(Value::as_str)
                    .is_some_and(|stage_pipeline| stage_pipeline == pipeline_id)
            })
        })
        .collect::<Vec<_>>();
    let egresses = engine_telemetry
        .get("egresses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|egress| {
            pipeline_id.is_none_or(|pipeline_id| {
                egress
                    .get("pipelineId")
                    .and_then(Value::as_str)
                    .is_some_and(|egress_pipeline| egress_pipeline == pipeline_id)
            })
        })
        .collect::<Vec<_>>();
    let ingests = telemetry
        .get("ingests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            telemetry
                .get("ingest")
                .filter(|ingest| !ingest.is_null())
                .cloned()
                .into_iter()
                .collect()
        });

    let mut nodes = Vec::new();
    let child_resources = child_process_resources(stages.iter().filter_map(stage_backend_pid));
    nodes.push(json!({
        "id": "runtime:restream",
        "kind": "runtime_process",
        "label": "restream",
        "execution": "process",
        "cpuPercent": process.restream_cpu_percent,
        "memory": memory(process.restream_memory_bytes, "measured", "process_rss"),
        "threads": { "process": process.process_thread_count },
        "hotspots": if process.restream_cpu_percent >= 75.0 { vec!["cpu"] } else { Vec::<&str>::new() },
    }));
    if process.external_ffmpeg_count > 0 {
        nodes.push(json!({
            "id": "runtime:external-ffmpeg",
            "kind": "child_process_group",
            "label": "External FFmpeg",
            "execution": "child_process",
            "cpuPercent": process.external_ffmpeg_cpu_percent,
            "memory": memory(process.external_ffmpeg_memory_bytes, "measured", "child_process_rss"),
            "threads": { "childProcess": process.external_ffmpeg_count },
            "hotspots": if process.external_ffmpeg_cpu_percent >= 75.0 { vec!["cpu"] } else { Vec::<&str>::new() },
        }));
    }

    if pipeline_id.is_none() {
        for ring in memory_accounting
            .get("sourceRings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            nodes.push(source_ring_node(ring));
        }
    } else if let Some(ring) = telemetry.get("sourceRing").filter(|ring| !ring.is_null()) {
        let mut ring = ring.clone();
        ring["pipelineId"] = Value::String(pipeline_id.unwrap_or_default().to_string());
        nodes.push(source_ring_node(&ring));
    }

    nodes.extend(
        stages
            .iter()
            .map(|stage| stage_node(stage, &child_resources)),
    );
    nodes.extend(
        egresses
            .iter()
            .map(|egress| egress_node(egress, &avio_egress_queues)),
    );

    let srt_sender_limit = 512u64;
    let srt_sender_threads =
        srt_sender_limit.saturating_sub(engine.runtime.sender_semaphore.available_permits() as u64);
    let registered_os_threads = engine
        .runtime
        .os_threads
        .lock()
        .map(|threads| {
            threads
                .iter()
                .filter(|thread| !thread.is_finished())
                .count() as u64
        })
        .unwrap_or(0);
    let retained_payload_bytes = number_field(&memory_accounting, "retainedPayloadBytes");
    let avio_queues = memory_accounting
        .get("avioQueues")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let total_node_count = nodes.len();
    let returned_nodes = match options.view {
        ResourceMapView::Summary => Vec::new(),
        ResourceMapView::Grouped => grouped_nodes(&nodes, options.top_n),
        ResourceMapView::Detail => top_nodes(nodes, options.top_n),
    };
    let memory_accounting_response = if options.view == ResourceMapView::Detail {
        memory_accounting
    } else {
        Value::Null
    };
    let returned_node_count = returned_nodes.len();
    let view = match options.view {
        ResourceMapView::Summary => "summary",
        ResourceMapView::Grouped => "grouped",
        ResourceMapView::Detail => "detail",
    };

    json!({
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "scope": {
            "kind": if pipeline_id.is_some() { "pipeline" } else { "runtime" },
            "pipelineId": pipeline_id,
        },
        "view": view,
        "limits": {
            "topN": options.top_n,
            "totalNodeCount": total_node_count,
            "returnedNodeCount": returned_node_count,
            "truncatedNodeCount": total_node_count.saturating_sub(returned_node_count),
            "maxTopN": MAX_TOP_N,
        },
        "summary": {
            "cpuPercent": process.cpu_percent,
            "restreamCpuPercent": process.restream_cpu_percent,
            "externalFfmpegCpuPercent": process.external_ffmpeg_cpu_percent,
            "cpuSampleReady": process.cpu_sample_ready,
            "memoryBytes": process.restream_memory_bytes,
            "restreamMemoryBytes": process.restream_memory_bytes,
            "externalFfmpegMemoryBytes": process.external_ffmpeg_memory_bytes,
            "totalMemoryBytes": process.total_memory_bytes,
            "externalFfmpegCount": process.external_ffmpeg_count,
            "processThreadCount": process.process_thread_count,
            "registeredOsThreads": registered_os_threads,
            "srtSenderThreads": srt_sender_threads,
            "srtSenderThreadLimit": srt_sender_limit,
            "fdCount": process.fd_count,
            "retainedPayloadBytes": retained_payload_bytes,
            "avioQueueLenBytes": number_field(&avio_queues, "totalLenBytes"),
            "avioQueueCapacityBytes": number_field(&avio_queues, "totalCapacityBytes"),
            "activeTranscoderBuffers": number_field(&engine_telemetry, "activeTranscoderBuffers"),
            "ingestCount": ingests.len(),
            "stageCount": stages.len(),
            "egressCount": egresses.len(),
        },
        "memoryAccounting": memory_accounting_response,
        "nodes": returned_nodes,
        "edges": [],
        "attribution": {
            "measured": ["process_rss", "child_process_rss", "process_thread_count", "fd_count"],
            "derived": ["ring_payload_stats", "avio_queue_len", "stage_metrics"],
            "estimated": ["tokio_task_overhead", "libsrt_internal_buffers"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_node_reports_child_process_resources_when_pid_is_known() {
        let stage = json!({
            "stageKey": "pipe-1:transcoder:720p",
            "kind": "video:720p:codec:hevc",
            "pipelineId": "pipe-1",
            "lifecycle": {
                "backend": "externalFfmpeg",
                "backendPid": 4242
            },
            "payloadStats": {
                "payloadBytes": 1024
            },
            "metrics": {
                "processingUs": 0
            }
        });
        let child_resources = HashMap::from([(
            4242,
            ChildProcessResource {
                cpu_percent: Some(12.5),
                memory_bytes: Some(64 * 1024 * 1024),
            },
        )]);

        let node = stage_node(&stage, &child_resources);

        assert_eq!(node.get("backendPid").and_then(Value::as_u64), Some(4242));
        assert_eq!(node.get("cpuPercent").and_then(Value::as_f64), Some(12.5));
        assert_eq!(
            node.pointer("/memory/attributedBytes")
                .and_then(Value::as_u64),
            Some(64 * 1024 * 1024)
        );
        assert_eq!(
            node.pointer("/memory/confidence").and_then(Value::as_str),
            Some("measured")
        );
        assert_eq!(
            node.pointer("/memory/source").and_then(Value::as_str),
            Some("child_process_rss")
        );
    }

    #[test]
    fn resource_map_options_new_clamps_top_n_to_valid_range() {
        assert_eq!(
            ResourceMapOptions::new(ResourceMapView::Grouped, None).top_n,
            DEFAULT_TOP_N
        );
        assert_eq!(
            ResourceMapOptions::new(ResourceMapView::Grouped, Some(0)).top_n,
            1,
            "zero must clamp up to the minimum, not produce an empty view"
        );
        assert_eq!(
            ResourceMapOptions::new(ResourceMapView::Grouped, Some(MAX_TOP_N + 1_000)).top_n,
            MAX_TOP_N,
            "oversized requests must clamp down rather than allocate unbounded nodes"
        );
        assert_eq!(
            ResourceMapOptions::new(ResourceMapView::Grouped, Some(50)).top_n,
            50
        );
    }

    #[test]
    fn number_field_defaults_to_zero_for_missing_or_non_integer_values() {
        assert_eq!(number_field(&json!({}), "missing"), 0);
        assert_eq!(number_field(&json!({"n": "not a number"}), "n"), 0);
        assert_eq!(
            number_field(&json!({"n": -1}), "n"),
            0,
            "negative is not a u64"
        );
        assert_eq!(
            number_field(&json!({"n": 1.5}), "n"),
            0,
            "fractional is not a u64"
        );
        assert_eq!(number_field(&json!({"n": u64::MAX}), "n"), u64::MAX);
    }

    #[test]
    fn group_key_falls_back_to_defaults_on_missing_fields() {
        assert_eq!(group_key(&json!({})), "unknown");
        assert_eq!(
            group_key(&json!({"kind": "egress"})),
            "egress:unknown:shared"
        );
        assert_eq!(
            group_key(&json!({"kind": "egress", "execution": "os_thread", "label": "RTMP output"})),
            "egress:RTMP:os_thread"
        );
        assert_eq!(
            group_key(&json!({"kind": "egress", "label": "   "})),
            "egress:unknown:shared",
            "an all-whitespace label has no first word, so it falls back to unknown"
        );
        assert_eq!(group_key(&json!({"kind": "stage"})), "stage:shared");
        assert_eq!(group_key(&json!({"kind": "source_ring"})), "source_ring");
    }

    #[test]
    fn group_label_formats_each_known_prefix_and_falls_back_for_unknown_keys() {
        assert_eq!(group_label("egress:rtmp:tokio_task", 3), "RTMP outputs (3)");
        assert_eq!(group_label("stage:os_thread", 2), "os_thread stages (2)");
        assert_eq!(group_label("source_ring", 1), "Source rings (1)");
        assert_eq!(group_label("runtime_process", 1), "restream");
        assert_eq!(group_label("child_process_group", 1), "External FFmpeg");
        assert_eq!(group_label("unknown", 5), "unknown (5)");
        assert_eq!(
            group_label("", 1),
            " (1)",
            "an empty key still yields a single empty part, not a panic"
        );
    }

    #[test]
    fn merge_thread_counts_accumulates_and_ignores_non_numeric_entries() {
        let mut target = serde_json::Map::new();
        merge_thread_counts(
            &mut target,
            &json!({"threads": {"appOwned": 1, "childProcess": 2}}),
        );
        merge_thread_counts(
            &mut target,
            &json!({"threads": {"appOwned": 3, "bogus": "not a number"}}),
        );
        merge_thread_counts(&mut target, &json!({}));

        assert_eq!(target.get("appOwned").and_then(Value::as_u64), Some(4));
        assert_eq!(target.get("childProcess").and_then(Value::as_u64), Some(2));
        assert_eq!(target.get("bogus"), None);
    }

    #[test]
    fn append_hotspots_deduplicates_and_ignores_non_string_entries() {
        let mut target = vec!["processing".to_string()];
        append_hotspots(
            &mut target,
            &json!({"hotspots": ["processing", "cpu", 42, "cpu"]}),
        );
        assert_eq!(target, vec!["processing", "cpu"]);
    }

    #[test]
    fn queue_hotspots_high_watermark_is_inclusive_at_75_percent() {
        assert_eq!(queue_hotspots(75, 100, 0), vec!["queue_high"]);
        assert_eq!(queue_hotspots(74, 100, 0), Vec::<&str>::new());
        assert_eq!(
            queue_hotspots(u64::MAX, 0, 0),
            Vec::<&str>::new(),
            "zero capacity must not report a hotspot even for a huge queue length"
        );
        assert_eq!(queue_hotspots(0, 100, 1), vec!["backpressure"]);
        assert_eq!(
            queue_hotspots(u64::MAX, u64::MAX, u64::MAX),
            vec!["queue_high", "backpressure"]
        );
    }

    #[test]
    fn execution_for_stage_maps_every_known_backend_and_defaults_to_shared() {
        let backend = |name: &str| json!({"lifecycle": {"backend": name}});
        assert_eq!(
            execution_for_stage(&backend("externalFfmpeg")),
            "child_process"
        );
        assert_eq!(
            execution_for_stage(&backend("ExternalFfmpeg")),
            "child_process"
        );
        assert_eq!(execution_for_stage(&backend("internalFfmpeg")), "os_thread");
        assert_eq!(execution_for_stage(&backend("recording")), "os_thread");
        assert_eq!(execution_for_stage(&backend("Recording")), "os_thread");
        assert_eq!(execution_for_stage(&backend("audioRouter")), "tokio_task");
        assert_eq!(execution_for_stage(&backend("hlsSegmenter")), "tokio_task");
        assert_eq!(execution_for_stage(&backend("somethingElse")), "shared");
        assert_eq!(execution_for_stage(&json!({})), "shared");
    }

    #[test]
    fn stage_backend_pid_rejects_values_that_overflow_u32() {
        assert_eq!(
            stage_backend_pid(&json!({"lifecycle": {"backendPid": 4242}})),
            Some(4242)
        );
        assert_eq!(stage_backend_pid(&json!({})), None);
        assert_eq!(
            stage_backend_pid(&json!({"lifecycle": {"backendPid": u64::MAX}})),
            None,
            "a pid that does not fit in u32 must not silently truncate"
        );
    }

    #[test]
    fn node_score_weighs_cpu_percent_far_above_a_single_byte_of_memory() {
        let cpu_heavy = node_score(&json!({"cpuPercent": 1.0}));
        let memory_heavy = node_score(&json!({"memory": {"attributedBytes": 1024 * 1024 - 1}}));
        assert!(cpu_heavy > memory_heavy);
    }

    #[test]
    fn top_nodes_sorts_descending_by_score_and_truncates() {
        let nodes = vec![
            json!({"id": "low", "cpuPercent": 1.0}),
            json!({"id": "high", "cpuPercent": 90.0}),
            json!({"id": "mid", "cpuPercent": 40.0}),
        ];
        let top = top_nodes(nodes, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0]["id"], "high");
        assert_eq!(top[1]["id"], "mid");
    }

    #[test]
    fn top_nodes_truncate_to_zero_yields_empty_without_panicking() {
        let nodes = vec![json!({"id": "only"})];
        assert!(top_nodes(nodes, 0).is_empty());
    }

    #[test]
    fn egress_node_uses_os_thread_only_for_srt_protocol() {
        let srt = egress_node(&json!({"outputId": "o1", "protocol": "srt"}), &[]);
        assert_eq!(srt["execution"], "os_thread");
        assert_eq!(srt["threads"]["appOwned"], 1);

        let rtmp = egress_node(&json!({"outputId": "o2", "protocol": "rtmp"}), &[]);
        assert_eq!(rtmp["execution"], "tokio_task");
        assert_eq!(rtmp["threads"]["appOwned"], 0);
    }

    #[test]
    fn egress_node_with_no_matching_queue_reports_zeroed_stats_and_no_hotspots() {
        let node = egress_node(
            &json!({"outputId": "missing-queue", "protocol": "rtmp"}),
            &[],
        );
        assert_eq!(node["memory"]["attributedBytes"], 0);
        assert_eq!(node["hotspots"], json!([]));
        assert_eq!(node["queue"], Value::Null);
    }

    #[test]
    fn source_ring_node_reports_retained_payload_hotspot_only_when_bytes_present() {
        let empty = source_ring_node(&json!({"pipelineId": "pipe-1"}));
        assert_eq!(empty["hotspots"], json!([]));

        let retaining = source_ring_node(&json!({
            "pipelineId": "pipe-1",
            "payloadStats": {"payloadBytes": 100}
        }));
        assert_eq!(retaining["hotspots"], json!(["retained_payload"]));
    }
}
