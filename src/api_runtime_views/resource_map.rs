//! Resource attribution map for operator and agent inspection.
//!
//! The map intentionally separates measured process values from derived
//! runtime attribution. Stage and queue memory is useful directional evidence,
//! but the process RSS remains the authoritative measured total.

use crate::media::engine::MediaEngine;
use crate::system_sampling::{
    ChildProcessResourceSnapshot, ProcessResourceSnapshot, sample_child_process_resources,
};
use serde_json::{Value, json};
use std::collections::HashMap;

const DEFAULT_TOP_N: usize = 25;
const MAX_TOP_N: usize = 200;
/// See the matching constant/comment in `status.rs` — kept as a separate
/// constant here rather than shared, since resource_map and health_snapshot
/// are independent read paths with no other coupling.
const FABRIC_SHARD_STALL_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

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

fn stage_node(
    stage: &Value,
    child_resources: &HashMap<u32, ChildProcessResourceSnapshot>,
) -> Value {
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
    // A fabric-owned output runs on a shared shard thread it does not own
    // exclusively — attributing a whole app-owned OS thread to it here
    // would double-count the same fixed shard pool once per output.
    // Legacy SRT still spawns one sender thread per output; legacy RTMP
    // stays on the shared tokio pool.
    let is_fabric = egress
        .get("fabric")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let execution = if is_fabric {
        "shard_thread"
    } else if protocol == "srt" {
        "os_thread"
    } else {
        "tokio_task"
    };
    json!({
        "id": output_id,
        "kind": "egress",
        "label": format!("{protocol} output"),
        "pipelineId": egress.get("pipelineId").cloned().unwrap_or(Value::Null),
        "execution": execution,
        "fabric": is_fabric,
        "shardId": egress.get("shardId").cloned().unwrap_or(Value::Null),
        "memory": memory(len, "derived", "avio_egress_queue_len"),
        "threads": {
            "appOwned": if !is_fabric && protocol == "srt" { 1 } else { 0 },
            "childProcess": 0,
        },
        "status": egress.get("status").cloned().unwrap_or(Value::Null),
        "phase": egress.get("phase").cloned().unwrap_or(Value::Null),
        "metrics": egress.get("metrics").cloned().unwrap_or_else(|| json!({})),
        "queue": queue.cloned().unwrap_or(Value::Null),
        "hotspots": queue_hotspots(len, capacity, blocked),
    })
}

fn fabric_shard_node(
    status: &crate::media::engine_egress_fabric_diagnostics::EgressFabricShardStatus,
) -> Value {
    json!({
        "id": format!("fabric-shard:{}:{}:{}", status.protocol, status.feed_id, status.shard_index),
        "kind": "egress_shard",
        "label": format!("{} fabric shard {}", status.protocol, status.shard_index),
        "execution": "shard_thread",
        "protocol": status.protocol,
        "feedId": status.feed_id,
        "shardIndex": status.shard_index,
        "state": status.state_str(),
        "threads": { "appOwned": 1, "childProcess": 0 },
        "memory": memory(0, "unmeasured", "fabric_shard"),
        "metrics": {
            "loopIterations": status.loop_iterations,
            "mediaTicks": status.media_ticks,
            "progressAgeMs": status.progress_age_ms,
            "commandDepth": status.command_depth,
            "commandCapacity": status.command_capacity,
        },
        "hotspots": if status.state_str() != "healthy" { vec![status.state_str()] } else { Vec::new() },
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
    let child_resources =
        sample_child_process_resources(stages.iter().filter_map(stage_backend_pid));
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

    // Shard threads are a global, fixed-size process resource shared across
    // every pipeline's fabric-owned outputs — not owned by any one
    // pipeline, so (like the runtime_process/child_process_group nodes
    // above) they only make sense in the global view.
    let fabric_shard_statuses = if pipeline_id.is_none() {
        engine
            .egress_fabric_shard_statuses(FABRIC_SHARD_STALL_AFTER)
            .await
    } else {
        Vec::new()
    };
    nodes.extend(fabric_shard_statuses.iter().map(fabric_shard_node));

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
            "fabricShardThreadCount": fabric_shard_statuses.len(),
            "fabricShardStalledCount": fabric_shard_statuses
                .iter()
                .filter(|status| status.state_str() == "stalled")
                .count(),
            "fabricShardPanickedCount": fabric_shard_statuses
                .iter()
                .filter(|status| status.state_str() == "panicked")
                .count(),
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
#[path = "resource_map_projection_tests.rs"]
mod resource_map_projection_tests;
