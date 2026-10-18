//! Result-JSON assembly helpers for the `hls-push-duty` mode.
//!
//! The isolation comparison needs the same derived numbers from both arms, so
//! the joins that turn the dashboard runtime, engine telemetry and sink
//! summaries into per-output rows live here as pure functions and are unit
//! tested without a live engine.

use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A shard is identified by its protocol registry and its index inside it:
/// shard indices repeat across the RTMP, SRT and sink registries.
pub(crate) type ShardKey = (String, u64);

/// Index `/metrics/system` (or dashboard-runtime metrics) `egressShards` by
/// `(protocol, shardIndex)` so an output's `shardId` can be joined to its
/// counters.
pub(crate) fn egress_shard_lookup(shards: &[Value]) -> BTreeMap<ShardKey, Value> {
    let mut lookup = BTreeMap::new();
    for shard in shards {
        let Some(protocol) = shard.get("protocol").and_then(Value::as_str) else {
            continue;
        };
        let Some(index) = shard.get("shardIndex").and_then(Value::as_u64) else {
            continue;
        };
        lookup.insert((protocol.to_string(), index), shard.clone());
    }
    lookup
}

/// Sum one numeric field across a shard's `srtOwners` entries (only present
/// owners count; a missing field contributes nothing).
fn sum_owner_field(shard: &Value, field: &str) -> u64 {
    shard
        .get("srtOwners")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|owner| owner.get("present").and_then(Value::as_bool) == Some(true))
        .filter_map(|owner| owner.get(field).and_then(Value::as_u64))
        .sum()
}

/// Per-output egress counters for the isolation comparison: resync events,
/// whether the delivering shard is stalled, and the drop counters (fabric
/// queue overflows plus the SRT Owner's RX ring and TX failure drops). The
/// values come straight from the runtime; a counter the current product does
/// not expose reads `null`, never a fabricated zero.
pub(crate) fn pipeline_egress_summary(egress: Option<&Value>, shard: Option<&Value>) -> Value {
    let counter = |source: Option<&Value>, field: &str| -> Value {
        source
            .and_then(|value| value.get(field))
            .and_then(Value::as_u64)
            .map_or(Value::Null, Value::from)
    };
    let shard_state = shard
        .and_then(|shard| shard.get("state"))
        .and_then(Value::as_str);
    json!({
        "resyncCount": counter(egress, "resyncCount"),
        "feedLagUnits": counter(egress, "feedLagUnits"),
        "shardId": counter(egress, "shardId"),
        "shardFeed": egress
            .and_then(|value| value.get("fabricFeed"))
            .cloned()
            .unwrap_or(Value::Null),
        "shardState": shard_state.map_or(Value::Null, |state| json!(state)),
        "stalled": shard_state == Some("stalled"),
        "queueOverflows": counter(shard, "queueOverflows"),
        "budgetExhaustions": counter(shard, "budgetExhaustions"),
        "rxRingDropped": shard.map_or(Value::Null, |shard| json!(sum_owner_field(shard, "rxRingDropped"))),
        "txFailuresDropped": shard.map_or(Value::Null, |shard| json!(sum_owner_field(shard, "txFailuresDropped"))),
    })
}

/// Input-side published-media summary from the pipeline probe snapshot
/// (`/api/v1/pipelines/{id}/probe`), which nests ingest counters and exposes
/// the video/audio probe plus the GOP keyframe count.
pub(crate) fn input_published_summary(probe: &Value) -> Value {
    let video_present = probe.get("video").is_some_and(|video| !video.is_null());
    let audio_tracks = probe
        .get("audioTracks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    json!({
        "protocol": probe.pointer("/ingest/protocol").cloned().unwrap_or(Value::Null),
        "bytesReceived": probe.pointer("/ingest/bytesReceived").cloned().unwrap_or(Value::Null),
        "bitrateKbps": probe.pointer("/ingest/bitrateKbps").cloned().unwrap_or(Value::Null),
        "videoPresent": video_present,
        "audioTracks": audio_tracks,
        "keyframeCount": gop_keyframe_count(probe),
    })
}

/// `keyframeCount` from a pipeline-telemetry `gop` block, when the ingest has
/// released at least two keyframes.
pub(crate) fn gop_keyframe_count(pipeline_telemetry: &Value) -> Option<u64> {
    pipeline_telemetry
        .get("gop")
        .and_then(|gop| gop.get("keyframeCount"))
        .and_then(Value::as_u64)
}

/// The cumulative executor poll counters a window subtracts.
const POLL_COUNTERS: [&str; 6] = [
    "polls",
    "busyUs",
    "cpuUs",
    "pollsOverCpu1Ms",
    "pollsOverCpu4Ms",
    "pollsOverCpu16Ms",
];
/// The `usage.all` / `usage.overCpu1Ms` fields (`RESTREAM_MEDIA_POLL_USAGE`).
const USAGE_FIELDS: [&str; 4] = ["userUs", "sysUs", "minorFaults", "majorFaults"];

fn counter(row: Option<&Value>, name: &str) -> u64 {
    row.and_then(|row| row.get(name))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn per_poll(total: u64, polls: u64) -> Value {
    if polls == 0 {
        Value::Null
    } else {
        json!((total as f64 / polls as f64 * 10.0).round() / 10.0)
    }
}

/// One executor row (a class or a service) over the window: each counter is
/// `end − start` (a row absent at the start counts from zero), `cpuUsPerPoll`
/// is the window's CPU per poll, and `maxCpuUs`/`maxPollUs` stay the
/// lifetime maxima the engine reports. `usage` is the window's per-poll
/// `getrusage` sum when the engine reports it.
fn poll_row_window(start: Option<&Value>, end: &Value) -> serde_json::Map<String, Value> {
    let moved = |name: &str| counter(Some(end), name).saturating_sub(counter(start, name));
    let mut row: serde_json::Map<String, Value> = POLL_COUNTERS
        .iter()
        .map(|name| (name.to_string(), json!(moved(name))))
        .collect();
    row.insert(
        "cpuUsPerPoll".to_string(),
        per_poll(moved("cpuUs"), moved("polls")),
    );
    for name in ["maxCpuUs", "maxPollUs"] {
        row.insert(name.to_string(), json!(counter(Some(end), name)));
    }
    if let Some(usage) = end.get("usage") {
        let usage_window = |part: &str| {
            let before = start.and_then(|start| start.pointer(&format!("/usage/{part}")));
            let after = usage.get(part);
            let fields: serde_json::Map<String, Value> = USAGE_FIELDS
                .iter()
                .map(|name| {
                    let moved = counter(after, name).saturating_sub(counter(before, name));
                    (name.to_string(), json!(moved))
                })
                .collect();
            Value::Object(fields)
        };
        row.insert(
            "usage".to_string(),
            json!({"all": usage_window("all"), "overCpu1Ms": usage_window("overCpu1Ms")}),
        );
    }
    row
}

fn rows<'a>(executor: &'a Value, list: &str) -> impl Iterator<Item = &'a Value> {
    executor
        .get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The media executor's per-class rows over the rated window, for the
/// classes that polled in it. `perMinutePerSession` divides the window's
/// polls above 1, 4 and 16 ms of thread CPU by the window's minutes and by
/// the class's active services at the end (the Q-074 gate's unit: for
/// `hlsIngest`, one service is one HLS session).
pub(crate) fn executor_class_windows(start: &Value, end: &Value, window_secs: f64) -> Vec<Value> {
    rows(end, "classes")
        .filter_map(|end_row| {
            let class = end_row.get("class")?;
            let start_row = rows(start, "classes").find(|row| row.get("class") == Some(class));
            let mut row = poll_row_window(start_row, end_row);
            if row.get("polls").and_then(Value::as_u64) == Some(0) {
                return None;
            }
            let sessions = counter(Some(end_row), "active");
            let minutes = window_secs / 60.0;
            let rate = |name: &str| {
                if sessions == 0 || minutes <= 0.0 {
                    return Value::Null;
                }
                let count = row.get(name).and_then(Value::as_u64).unwrap_or(0) as f64;
                json!((count / minutes / sessions as f64 * 1000.0).round() / 1000.0)
            };
            let per_minute = json!({
                "over1Ms": rate("pollsOverCpu1Ms"),
                "over4Ms": rate("pollsOverCpu4Ms"),
                "over16Ms": rate("pollsOverCpu16Ms"),
            });
            row.insert("class".to_string(), class.clone());
            row.insert("active".to_string(), json!(sessions));
            row.insert("perMinutePerSession".to_string(), per_minute);
            Some(Value::Object(row))
        })
        .collect()
}

/// One pipeline's media executor services over the rated window: the window
/// counters summed over its service rows (matched by `serviceId`, so a
/// service started inside the window counts from zero), CPU per poll, and
/// the largest lifetime `maxCpuUs` among them. A pipeline with no executor
/// service reads zero polls.
pub(crate) fn pipeline_poll_window(start: &Value, end: &Value, pipeline_id: &str) -> Value {
    let mut totals: BTreeMap<&str, u64> = BTreeMap::new();
    let mut max_cpu_us = 0;
    let mut classes: Vec<Value> = Vec::new();
    let mut services = 0;
    for end_row in rows(end, "services")
        .filter(|row| row.get("pipelineId").and_then(Value::as_str) == Some(pipeline_id))
    {
        let id = end_row.get("serviceId");
        let start_row = rows(start, "services").find(|row| row.get("serviceId") == id);
        let window = poll_row_window(start_row, end_row);
        for name in POLL_COUNTERS {
            *totals.entry(name).or_default() +=
                window.get(name).and_then(Value::as_u64).unwrap_or(0);
        }
        max_cpu_us = max_cpu_us.max(counter(Some(end_row), "maxCpuUs"));
        if let Some(class) = end_row.get("class")
            && !classes.contains(class)
        {
            classes.push(class.clone());
        }
        services += 1;
    }
    let total = |name: &str| totals.get(name).copied().unwrap_or(0);
    json!({
        "services": services,
        "classes": classes,
        "polls": total("polls"),
        "cpuUs": total("cpuUs"),
        "cpuUsPerPoll": per_poll(total("cpuUs"), total("polls")),
        "busyUs": total("busyUs"),
        "maxCpuUs": max_cpu_us,
        "pollsOverCpu4Ms": total("pollsOverCpu4Ms"),
    })
}
