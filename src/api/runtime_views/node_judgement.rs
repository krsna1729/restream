//! Whether one resource-map node limits the pipeline.
//!
//! - An egress shard is judged on its own command queue, progress age and
//!   state, through the flow diagnosis the engine applies to its deepest
//!   shard ([`shard_flow`]).
//! - A fabric output takes the judgement of the shard it is placed on: a
//!   sibling shard's stall does not touch it.
//! - An FFmpeg child (stage or group) and the source ring draw on a pooled
//!   center (`ffmpeg`, `memory`) that every member shares, so they take the
//!   pool's utilization against its calibrated limit (priors, not a
//!   measurement).
//!
//! Any other node gets no judgement.

use serde::Serialize;
use serde_json::Value;

use crate::application::capacity::{
    CapacityModel, CapacitySnapshot, FlowStatus, ServiceCenter, shard_flow,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Verdict {
    /// The node's own flow is saturated or overloaded, or its pool is full.
    Limiting,
    /// Its pool is at or above the safe utilization.
    Pressured,
    /// Neither.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeJudgement {
    pub verdict: Verdict,
    pub center: ServiceCenter,
    /// The shard's command queue against its bound, or the pool's
    /// utilization against its calibrated limit.
    pub utilization: f64,
    /// `true` for a shard's (or its output's) own measurement; `false` for
    /// the calibration priors.
    pub measured: bool,
    pub reason: String,
}

fn percent(ratio: f64) -> String {
    format!("{:.0}%", ratio * 100.0)
}

/// One egress shard, from its own command queue, progress age and state.
pub(crate) fn judge_shard(
    command_depth: u32,
    command_capacity: u32,
    progress_age_ms: Option<u64>,
    state: &str,
) -> NodeJudgement {
    let flow = shard_flow(
        f64::from(command_depth),
        f64::from(command_capacity),
        progress_age_ms.unwrap_or_default() as f64,
        state != "healthy",
    );
    let (verdict, reason) = if state != "healthy" {
        (Verdict::Limiting, format!("the shard is {state}"))
    } else if flow.status != FlowStatus::Healthy {
        let status = if flow.status == FlowStatus::Overloaded {
            "overloaded"
        } else {
            "saturated"
        };
        (
            Verdict::Limiting,
            format!(
                "the shard is {status}: command queue at {} of its bound, progress {} ms ago",
                percent(flow.utilization),
                progress_age_ms.unwrap_or_default()
            ),
        )
    } else {
        (
            Verdict::Clear,
            format!(
                "the shard's command queue is at {} of its bound",
                percent(flow.utilization)
            ),
        )
    };
    NodeJudgement {
        verdict,
        center: ServiceCenter::Egress,
        utilization: flow.utilization,
        measured: true,
        reason,
    }
}

/// A fabric output, from the shard it is placed on.
pub(crate) fn judge_output(shard: &NodeJudgement, shard_index: u64) -> NodeJudgement {
    NodeJudgement {
        reason: format!("on shard {shard_index}: {}", shard.reason),
        ..shard.clone()
    }
}

/// An FFmpeg child or the source ring, from its pool's prior utilization.
pub(crate) fn judge_pooled(node: &Value, priors: &CapacitySnapshot) -> Option<NodeJudgement> {
    let kind = node.get("kind").and_then(Value::as_str)?;
    let execution = node.get("execution").and_then(Value::as_str);
    let (center, name, utilization) = match kind {
        "stage" if execution == Some("child_process") => {
            (ServiceCenter::Ffmpeg, "FFmpeg", priors.ffmpeg_util)
        }
        "child_process_group" => (ServiceCenter::Ffmpeg, "FFmpeg", priors.ffmpeg_util),
        "source_ring" => (ServiceCenter::Memory, "memory", priors.memory_util),
        _ => return None,
    };
    let utilization = f64::from(utilization);
    let safe = CapacityModel::default().safe_utilization;
    let verdict = if utilization >= 1.0 {
        Verdict::Limiting
    } else if utilization >= safe {
        Verdict::Pressured
    } else {
        Verdict::Clear
    };
    Some(NodeJudgement {
        verdict,
        center,
        utilization,
        measured: false,
        reason: format!(
            "the {name} pool every pipeline shares is at {} of its limit (safe below {})",
            percent(utilization),
            percent(safe)
        ),
    })
}

/// Judge every node of a map in place: shards first, so each fabric output
/// can take its shard's judgement (`fabricFeed`/`shardId` against
/// `feedId`/`shardIndex`); then the pooled centers.
pub(crate) fn judge_nodes(nodes: &mut [Value], priors: &CapacitySnapshot) {
    let slot = |node: &Value, feed: &str, shard: &str| {
        Some((
            node.get(feed)?.as_str()?.to_owned(),
            node.get(shard)?.as_u64()?,
        ))
    };
    let mut shards = std::collections::HashMap::new();
    for node in nodes.iter_mut() {
        if node.get("kind").and_then(Value::as_str) != Some("egress_shard") {
            continue;
        }
        let metric = |key: &str| {
            node.get("metrics")
                .and_then(|metrics| metrics.get(key))
                .and_then(Value::as_u64)
        };
        let judgement = judge_shard(
            u32::try_from(metric("commandDepth").unwrap_or_default()).unwrap_or(u32::MAX),
            u32::try_from(metric("commandCapacity").unwrap_or_default()).unwrap_or(u32::MAX),
            metric("progressAgeMs"),
            node.get("state")
                .and_then(Value::as_str)
                .unwrap_or("healthy"),
        );
        if let Some(key) = slot(node, "feedId", "shardIndex") {
            shards.insert(key, judgement.clone());
        }
        node["judgement"] = serde_json::json!(judgement);
    }
    for node in nodes.iter_mut() {
        let judgement = match node.get("kind").and_then(Value::as_str) {
            Some("egress_shard") => continue,
            Some("egress") => slot(node, "fabricFeed", "shardId")
                .and_then(|key| shards.get(&key).map(|shard| judge_output(shard, key.1))),
            _ => judge_pooled(node, priors),
        };
        if let Some(judgement) = judgement {
            node["judgement"] = serde_json::json!(judgement);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::application::capacity::{CapacityLimits, Workload};

    fn priors(stages: u32) -> CapacitySnapshot {
        // Four FFmpeg stages fit the limit: 1 stage is 25%, 4 are 100%.
        CapacityModel {
            limits: CapacityLimits {
                ffmpeg_stages: 4.0,
                ..CapacityLimits::default()
            },
            ..CapacityModel::default()
        }
        .project(Workload {
            stage_count: stages,
            ..Workload::default()
        })
    }

    fn transcoder() -> Value {
        json!({ "kind": "stage", "execution": "child_process" })
    }

    fn verdict(node: &Value, priors: &CapacitySnapshot) -> Option<Verdict> {
        judge_pooled(node, priors).map(|judgement| judgement.verdict)
    }

    #[test]
    fn only_pooled_centers_get_a_pooled_judgement() {
        let priors = priors(4);
        assert_eq!(verdict(&json!({ "kind": "ingest" }), &priors), None);
        // An output or a shard is judged on its shard, never on a pool.
        assert_eq!(verdict(&json!({ "kind": "egress" }), &priors), None);
        assert_eq!(verdict(&json!({ "kind": "egress_shard" }), &priors), None);
        // A stage that runs in-process is not an FFmpeg child.
        assert_eq!(
            verdict(
                &json!({ "kind": "stage", "execution": "tokio_task" }),
                &priors
            ),
            None
        );
    }

    #[test]
    fn an_ffmpeg_child_follows_its_pool_clear_pressured_then_limiting() {
        let quarter = judge_pooled(&transcoder(), &priors(1)).expect("an FFmpeg child");
        assert_eq!(quarter.verdict, Verdict::Clear);
        assert_eq!(quarter.center, ServiceCenter::Ffmpeg);
        assert_eq!(quarter.utilization, 0.25);
        assert!(!quarter.measured, "priors, not a measurement");

        let mut pool = priors(1);
        pool.ffmpeg_util = 0.79;
        assert_eq!(verdict(&transcoder(), &pool), Some(Verdict::Clear));
        pool.ffmpeg_util = 0.8;
        assert_eq!(verdict(&transcoder(), &pool), Some(Verdict::Pressured));
        // Full: every child of the pool is bound by it.
        assert_eq!(verdict(&transcoder(), &priors(4)), Some(Verdict::Limiting));
    }

    #[test]
    fn a_shard_is_judged_on_its_own_flow_so_a_stalled_sibling_does_not_limit_it() {
        let stalled = judge_shard(3, 1024, Some(4_000), "stalled");
        assert_eq!(stalled.verdict, Verdict::Limiting);
        assert!(stalled.measured);
        assert_eq!(stalled.reason, "the shard is stalled");

        let healthy = judge_shard(1, 1024, Some(5), "healthy");
        assert_eq!(healthy.verdict, Verdict::Clear);
        assert!(healthy.measured);

        // Healthy state, but a full queue: its own flow limits it.
        let full = judge_shard(1024, 1024, Some(5), "healthy");
        assert_eq!(full.verdict, Verdict::Limiting);
        assert_eq!(full.utilization, 1.0);
        // Healthy state and a short queue, but no progress for 2 s: the
        // deadline is missed.
        assert_eq!(
            judge_shard(1, 1024, Some(2_000), "healthy").verdict,
            Verdict::Limiting
        );
    }

    #[test]
    fn an_output_carries_its_own_shards_judgement() {
        let healthy = judge_shard(1, 1024, Some(5), "healthy");
        let output = judge_output(&healthy, 2);
        assert_eq!(output.verdict, Verdict::Clear);
        assert!(output.reason.starts_with("on shard 2: "));
    }

    #[test]
    fn in_one_map_a_stalled_shard_limits_only_its_own_outputs() {
        let shard = |index: u64, state: &str, age: u64| {
            json!({
                "kind": "egress_shard", "feedId": "srt:p:source", "shardIndex": index,
                "state": state,
                "metrics": { "commandDepth": 1, "commandCapacity": 1024, "progressAgeMs": age },
            })
        };
        let output = |shard: u64| json!({ "kind": "egress", "fabricFeed": "srt:p:source", "shardId": shard });
        let mut nodes = vec![
            output(0),
            output(1),
            shard(0, "stalled", 12_000),
            shard(1, "healthy", 5),
            // Not on the fabric: no shard, no judgement.
            json!({ "kind": "egress" }),
            transcoder(),
        ];
        judge_nodes(&mut nodes, &priors(1));
        let verdict_of = |node: &Value| node["judgement"]["verdict"].as_str().map(str::to_owned);
        let verdicts: Vec<_> = nodes.iter().map(verdict_of).collect();
        assert_eq!(
            verdicts,
            [
                Some("limiting"),
                Some("clear"),
                Some("limiting"),
                Some("clear"),
                None,
                Some("clear")
            ]
            .map(|v| v.map(str::to_owned))
        );
        assert_eq!(
            nodes[0]["judgement"]["reason"],
            "on shard 0: the shard is stalled"
        );
    }
}
