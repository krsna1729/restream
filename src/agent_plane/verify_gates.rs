//! The fixed gates an applied change must pass to be verified. The caller
//! measures; these judge, so each rule has one place and one test.

use serde::Serialize;

use crate::capacity::{CapacitySnapshot, FlowStatus};
/// The engine's one delivery floor, the share of wanted-running outputs on
/// the changed pipelines that must deliver: the same floor feed delivery
/// and Monitor judge by.
use crate::media::egress::delivery::DELIVERY_FLOOR;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyGate {
    pub gate: &'static str,
    pub passed: bool,
    pub detail: String,
}

/// Every requested change reached the state it asked for.
pub fn desired_state(passed: usize, checks: usize) -> VerifyGate {
    VerifyGate {
        gate: "desiredState",
        passed: passed == checks,
        detail: format!("{passed} of {checks} changes reached their requested state"),
    }
}

/// Of the outputs the changed pipelines want running, at least
/// [`DELIVERY_FLOOR`] deliver. None wanted passes: nothing is short.
pub fn delivery_floor(wanted: usize, delivering: usize) -> VerifyGate {
    let share = if wanted == 0 {
        1.0
    } else {
        delivering as f64 / wanted as f64
    };
    VerifyGate {
        gate: "deliveryFloor",
        passed: share >= DELIVERY_FLOOR,
        detail: format!("{delivering} of {wanted} wanted outputs deliver (floor {DELIVERY_FLOOR})"),
    }
}

/// No critical alert open now that was not open just before the apply. By
/// id, so one clearing does not hide a new one.
pub fn new_critical_alerts(before: &[String], after: &[String]) -> VerifyGate {
    let new: Vec<&str> = after
        .iter()
        .filter(|id| !before.contains(id))
        .map(String::as_str)
        .collect();
    VerifyGate {
        gate: "newCriticalAlerts",
        passed: new.is_empty(),
        detail: if new.is_empty() {
            format!(
                "no new critical alert ({} open before, {} now)",
                before.len(),
                after.len()
            )
        } else {
            format!("new critical alerts: {}", new.join(", "))
        },
    }
}

/// The host has headroom: the measured flow is healthy and the hottest
/// service center, on the priors, stays under the safe threshold.
pub fn capacity_state(snapshot: &CapacitySnapshot, safe_utilization: f64) -> VerifyGate {
    let utilization = f64::from(snapshot.projected_utilization);
    let passed = snapshot.flow.status == FlowStatus::Healthy && utilization < safe_utilization;
    VerifyGate {
        gate: "capacityState",
        passed,
        detail: format!(
            "{:?} flow; hottest center {:?} at {:.0}% (safe below {:.0}%)",
            snapshot.flow.status,
            snapshot.hottest_center,
            utilization * 100.0,
            safe_utilization * 100.0
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::{CapacityLimits, CapacityModel, Workload};

    #[test]
    fn delivery_floor_fails_one_short_of_the_floor() {
        // 19 of 20 is 0.95: on the floor.
        assert!(delivery_floor(20, 19).passed);
        // 18 of 20 is 0.90: under it.
        assert!(!delivery_floor(20, 18).passed);
        // A pipeline that wants nothing running is not short.
        assert!(delivery_floor(0, 0).passed);
    }

    #[test]
    fn a_new_critical_alert_fails_even_when_an_old_one_clears() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert!(!new_critical_alerts(&ids(&["a"]), &ids(&["a", "b"])).passed);
        assert!(new_critical_alerts(&ids(&["a", "b"]), &ids(&["a"])).passed);
        // The same count, a different alert: "a" cleared, "b" is new.
        let swap = new_critical_alerts(&ids(&["a"]), &ids(&["b"]));
        assert!(!swap.passed);
        assert_eq!(swap.detail, "new critical alerts: b");
        assert!(new_critical_alerts(&[], &[]).passed);
    }

    #[test]
    fn desired_state_needs_every_change() {
        assert!(desired_state(2, 2).passed);
        assert!(!desired_state(1, 2).passed);
    }

    #[test]
    fn capacity_fails_on_a_hot_projection_or_an_unhealthy_flow() {
        let model = CapacityModel {
            limits: CapacityLimits::from_parallelism(4),
            safe_utilization: 0.8,
        };
        let idle = model.project(Workload::default());
        assert!(capacity_state(&idle, 0.8).passed);

        let mut hot = idle;
        hot.projected_utilization = 0.85;
        assert!(!capacity_state(&hot, 0.8).passed);

        let mut stalled = idle;
        stalled.flow.status = FlowStatus::Saturated;
        assert!(!capacity_state(&stalled, 0.8).passed);
    }
}
