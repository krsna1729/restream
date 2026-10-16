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
