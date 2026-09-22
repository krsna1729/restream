//! `/metrics/system` counter snapshots and their deltas, the owner-fault and
//! shard-index reads, and the per-output telemetry samples the rated window
//! records.

use super::super::*;
use std::collections::BTreeMap;

use super::*;
/// The product's effective per-visit media hand-off bound, read from the
/// product's own `restream.config.effective` startup line rather than assumed
/// from a source default. `None` when the line is absent or unparsable, so a
/// missing reading stays `null` instead of a fabricated number.
pub(super) fn parse_visit_max_bytes(restream_log: &str) -> Option<u64> {
    let marker = "\"visitMaxBytes\":";
    let start = restream_log.find(marker)? + marker.len();
    let rest = &restream_log[start..];
    let digits: String = rest
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    digits.parse::<u64>().ok()
}

// ---------------------------------------------------------------------------
// Engine counter snapshots (`GET /metrics/system`)
// ---------------------------------------------------------------------------

/// Keys of the SRT owner/gauge snapshot, all summed over SRT shards' present
/// owners. `None` (rendered `null`) when any owner did not publish the field —
/// an absent counter is never a zero.
pub(super) const OWNER_COUNTER_FIELDS: &[&str] = &[
    "txPackets",
    "txCompletedOk",
    "txInFlight",
    "txFailedSends",
    "txExhaustions",
    "serviceBudgetExhausted",
    "rxPackets",
    "rxRingDropped",
    "rxTruncated",
    "serviceVisits",
    "serviceActions",
    "maintenanceActions",
];

/// `txClass` keys: first-transmission DATA, retransmitted DATA, and every
/// control class the snapshot carries.
pub(super) const TX_CLASS_FIELDS: &[&str] = &[
    "dataFirst",
    "dataRetransmit",
    "ack",
    "ackack",
    "nak",
    "keepalive",
    "handshake",
    "dropRequest",
    "keyMaterial",
    "shutdown",
    "otherControl",
];

/// SRT shard-level counters (the shard loop's own work, not the Owner's).
pub(super) const SHARD_COUNTER_FIELDS: &[&str] = &[
    "loopIterations",
    "mediaTicks",
    "readyVisits",
    "resyncCount",
    "queueOverflows",
    "driverBudgetViolations",
    "retryEvents",
];

pub(super) fn sum_optional(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    let mut total = 0_u64;
    let mut seen = false;
    for value in values {
        seen = true;
        total = total.saturating_add(value?);
    }
    seen.then_some(total)
}

pub(super) fn opt_counter(value: Option<u64>) -> Value {
    value.map_or(Value::Null, |value| json!(value))
}

/// One `/metrics/system` snapshot reduced to the SRT egress counters Stage D
/// records. Every leaf is a number or `null`.
pub(super) fn engine_srt_counters(system: &Value) -> Value {
    let shards: Vec<&Value> = system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|shard| shard["protocol"] == "srt")
        .collect();
    let owners: Vec<&Value> = shards
        .iter()
        .flat_map(|shard| shard["srtOwners"].as_array().into_iter().flatten())
        .filter(|owner| owner["present"] == true)
        .collect();
    let owner_field = |key: &str| {
        sum_optional(
            owners
                .iter()
                .map(|owner| owner.get(key).and_then(Value::as_u64)),
        )
    };
    let class_field = |key: &str| {
        sum_optional(owners.iter().map(|owner| {
            owner
                .get("txClass")
                .and_then(|class| class.get(key))
                .and_then(Value::as_u64)
        }))
    };
    let shard_field = |key: &str| {
        sum_optional(
            shards
                .iter()
                .map(|shard| shard.get(key).and_then(Value::as_u64)),
        )
    };
    let mut tx_class = serde_json::Map::new();
    for key in TX_CLASS_FIELDS {
        tx_class.insert((*key).to_string(), opt_counter(class_field(key)));
    }
    let mut counters = serde_json::Map::new();
    counters.insert("shardCount".to_string(), json!(shards.len()));
    counters.insert("ownerCount".to_string(), json!(owners.len()));
    for key in OWNER_COUNTER_FIELDS {
        counters.insert((*key).to_string(), opt_counter(owner_field(key)));
    }
    counters.insert("txClass".to_string(), Value::Object(tx_class));
    for key in SHARD_COUNTER_FIELDS {
        counters.insert((*key).to_string(), opt_counter(shard_field(key)));
    }
    Value::Object(counters)
}

/// Any SRT owner faulted in this snapshot?
pub(super) fn engine_owner_faulted(system: &Value) -> bool {
    system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|shard| shard["protocol"] == "srt")
        .flat_map(|shard| shard["srtOwners"].as_array().into_iter().flatten())
        .any(|owner| owner["faulted"] == true)
}

/// Distinct SRT shard indices in one snapshot, ascending.
pub(super) fn engine_srt_shard_indices(system: &Value) -> Vec<u64> {
    let mut indices: Vec<u64> = system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|shard| shard["protocol"] == "srt")
        .filter_map(|shard| shard["shardIndex"].as_u64())
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// `after - before` over an identical counter tree. Any leaf whose sides are
/// not both numbers, or that went backwards, reads `null`.
pub(super) fn counter_deltas(before: &Value, after: &Value) -> Value {
    match (before, after) {
        (Value::Number(_), Value::Number(_)) => {
            let (Some(before), Some(after)) = (before.as_u64(), after.as_u64()) else {
                return Value::Null;
            };
            ticks_delta(after, before).map_or(Value::Null, |delta| json!(delta))
        }
        (Value::Object(before), Value::Object(after)) => {
            let mut out = serde_json::Map::new();
            for key in after.keys() {
                out.insert(key.clone(), counter_deltas(&before[key], &after[key]));
            }
            Value::Object(out)
        }
        _ => Value::Null,
    }
}

/// One counter leaf by path, if it is a number.
pub(super) fn counter_at(counters: &Value, path: &[&str]) -> Option<u64> {
    let mut current = counters;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_u64()
}

pub(super) fn collect_null_paths(counters: &Value, prefix: &str, out: &mut Vec<String>) {
    match counters {
        Value::Object(map) => {
            for (key, value) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_null_paths(value, &path, out);
            }
        }
        Value::Null => out.push(prefix.to_string()),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Output telemetry sampling
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct OutputSample {
    pub(super) output_id: String,
    pub(super) name: String,
    pub(super) url: String,
    pub(super) shard_id: Option<u64>,
    pub(super) status: String,
    pub(super) phase: String,
    pub(super) packets_out: Option<u64>,
    pub(super) bytes_out: Option<u64>,
    pub(super) metric_bytes_out: Option<u64>,
    pub(super) last_progress_age_ms: Option<u64>,
    pub(super) quality: Value,
}

impl OutputSample {
    pub(super) fn json(&self) -> Value {
        json!({
            "outputId": self.output_id,
            "outputName": self.name,
            "targetUrl": self.url,
            "shardId": self.shard_id,
            "status": self.status,
            "phase": self.phase,
            "packetsOut": self.packets_out,
            "stageBytesOut": self.metric_bytes_out,
            "egressBytesOut": self.bytes_out,
            "lastProgressAgeMs": self.last_progress_age_ms,
            "quality": self.quality,
        })
    }
}

pub(super) fn output_samples(
    telemetry: &Value,
    urls: &BTreeMap<String, String>,
) -> Vec<OutputSample> {
    telemetry["egresses"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|egress| {
            let output_id = egress["outputId"].as_str().unwrap_or_default().to_string();
            OutputSample {
                name: egress["outputName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                url: urls.get(&output_id).cloned().unwrap_or_default(),
                shard_id: egress["shardId"].as_u64(),
                status: egress["status"].as_str().unwrap_or("unknown").to_string(),
                phase: egress["phase"].as_str().unwrap_or("unknown").to_string(),
                packets_out: egress["metrics"]["packetsOut"].as_u64(),
                metric_bytes_out: egress["metrics"]["bytesOut"].as_u64(),
                bytes_out: egress["bytesOut"].as_u64(),
                last_progress_age_ms: egress["lastProgressAgeMs"].as_u64(),
                quality: egress["quality"].clone(),
                output_id,
            }
        })
        .collect()
}
