//! Whether one resource-map node limits the pipeline, judged from the
//! capacity model: the service center the node consumes, that center's
//! utilization against its calibrated limit, and the engine's flow
//! diagnosis. A node that consumes no modelled center gets no judgement.

use serde::Serialize;
use serde_json::Value;

use crate::capacity::{CapacityModel, CapacitySnapshot, FlowStatus, ServiceCenter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Verdict {
    /// The node's center is the one the flow diagnosis names as saturated or
    /// overloaded, or the node itself (a shard) is not healthy.
    Limiting,
    /// The node's center is at or above the safe utilization, but the flow
    /// diagnosis does not name it.
    Pressured,
    /// Below the safe utilization and not named by the flow diagnosis.
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NodeJudgement {
    pub verdict: Verdict,
    pub center: ServiceCenter,
    /// The center's utilization against its limit; `None` when the model
    /// prices it only through the flow diagnosis.
    pub utilization: Option<f64>,
    /// `true` when the verdict rests on the measured shard flow; otherwise
    /// it rests on the calibration priors (observe-only).
    pub measured: bool,
    pub reason: String,
}

fn center_of(node: &Value) -> Option<ServiceCenter> {
    let kind = node.get("kind").and_then(Value::as_str)?;
    let execution = node.get("execution").and_then(Value::as_str);
    match kind {
        "stage" if execution == Some("child_process") => Some(ServiceCenter::Ffmpeg),
        "child_process_group" => Some(ServiceCenter::Ffmpeg),
        "egress" | "egress_shard" => Some(ServiceCenter::Egress),
        "source_ring" => Some(ServiceCenter::Memory),
        _ => None,
    }
}

fn utilization_of(center: ServiceCenter, capacity: &CapacitySnapshot) -> Option<f64> {
    let util = match center {
        ServiceCenter::Egress => capacity.hottest_shard_util,
        ServiceCenter::Ffmpeg => capacity.ffmpeg_util,
        ServiceCenter::Memory => capacity.memory_util,
        ServiceCenter::Nic => capacity.nic_util,
        ServiceCenter::Disk => capacity.disk_util,
        ServiceCenter::Ingress | ServiceCenter::Cpu => return None,
    };
    Some(f64::from(util))
}

fn center_name(center: ServiceCenter) -> &'static str {
    match center {
        ServiceCenter::Ingress => "ingress",
        ServiceCenter::Egress => "egress",
        ServiceCenter::Cpu => "cpu",
        ServiceCenter::Nic => "nic",
        ServiceCenter::Memory => "memory",
        ServiceCenter::Ffmpeg => "ffmpeg",
        ServiceCenter::Disk => "disk",
    }
}

fn percent(ratio: f64) -> String {
    format!("{:.0}%", ratio * 100.0)
}

pub(crate) fn judge_node(node: &Value, capacity: &CapacitySnapshot) -> Option<NodeJudgement> {
    let center = center_of(node)?;
    let utilization = utilization_of(center, capacity);
    let name = center_name(center);
    let judgement = |verdict, measured, reason| NodeJudgement {
        verdict,
        center,
        utilization,
        measured,
        reason,
    };
    if node.get("kind").and_then(Value::as_str) == Some("egress_shard")
        && let Some(state) = node.get("state").and_then(Value::as_str)
        && state != "healthy"
    {
        return Some(judgement(
            Verdict::Limiting,
            true,
            format!("this shard is {state}"),
        ));
    }
    let flow = &capacity.flow;
    if flow.center == center && flow.status != FlowStatus::Healthy {
        let status = if flow.status == FlowStatus::Overloaded {
            "overloaded"
        } else {
            "saturated"
        };
        return Some(judgement(
            Verdict::Limiting,
            capacity.flow_measured,
            format!(
                "the flow diagnosis names {name} {status} at {}",
                percent(flow.utilization)
            ),
        ));
    }
    let safe = CapacityModel::default().safe_utilization;
    Some(match utilization {
        Some(util) if util >= safe => judgement(
            Verdict::Pressured,
            false,
            format!(
                "{name} at {} of its limit, at or above the safe {}",
                percent(util),
                percent(safe)
            ),
        ),
        Some(util) => judgement(
            Verdict::Clear,
            false,
            format!(
                "{name} at {} of its limit, below the safe {}; the flow diagnosis does not name it",
                percent(util),
                percent(safe)
            ),
        ),
        None => judgement(
            Verdict::Clear,
            capacity.flow_measured,
            format!("the flow diagnosis does not name {name}"),
        ),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::capacity::{CapacityLimits, Workload};

    fn snapshot(stages: u32) -> CapacitySnapshot {
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

    #[test]
    fn a_node_that_consumes_no_modelled_center_gets_no_judgement() {
        let snapshot = snapshot(1);
        assert_eq!(judge_node(&json!({ "kind": "ingest" }), &snapshot), None);
        // A stage that runs in-process is not an FFmpeg child.
        assert_eq!(
            judge_node(
                &json!({ "kind": "stage", "execution": "tokio_task" }),
                &snapshot
            ),
            None
        );
    }

    #[test]
    fn a_child_stage_below_the_safe_utilization_is_clear() {
        let judgement = judge_node(&transcoder(), &snapshot(1)).expect("an FFmpeg stage");
        assert_eq!(judgement.verdict, Verdict::Clear);
        assert_eq!(judgement.center, ServiceCenter::Ffmpeg);
        assert_eq!(judgement.utilization, Some(0.25));
        assert!(!judgement.measured, "priors, not a measurement");
    }

    #[test]
    fn the_center_the_flow_diagnosis_names_is_limiting_and_one_above_safe_is_pressured() {
        // 4 of 4 stages: FFmpeg is the hottest center and saturated.
        let full = snapshot(4);
        assert_eq!(full.flow.center, ServiceCenter::Ffmpeg);
        assert_ne!(full.flow.status, FlowStatus::Healthy);
        assert_eq!(
            judge_node(&transcoder(), &full).map(|j| j.verdict),
            Some(Verdict::Limiting)
        );
        // The flow names FFmpeg, not egress: an idle egress is not limiting.
        assert_eq!(
            judge_node(&json!({ "kind": "egress" }), &full).map(|j| j.verdict),
            Some(Verdict::Clear)
        );

        // At the safe utilization but not named by the flow: pressured.
        let mut pressured = snapshot(1);
        pressured.ffmpeg_util = 0.8;
        assert_eq!(
            judge_node(&transcoder(), &pressured).map(|j| j.verdict),
            Some(Verdict::Pressured)
        );
    }

    #[test]
    fn an_unhealthy_shard_is_limiting_whatever_the_model_says() {
        let judgement = judge_node(
            &json!({ "kind": "egress_shard", "state": "stalled" }),
            &snapshot(0),
        )
        .expect("a shard");
        assert_eq!(judgement.verdict, Verdict::Limiting);
        assert!(judgement.measured);
        assert_eq!(judgement.reason, "this shard is stalled");
        assert_eq!(
            judge_node(
                &json!({ "kind": "egress_shard", "state": "healthy" }),
                &snapshot(0)
            )
            .map(|j| j.verdict),
            Some(Verdict::Clear)
        );
    }
}
