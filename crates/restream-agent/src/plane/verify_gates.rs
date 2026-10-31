//! The fixed gates an applied change must pass to be verified. The caller
//! measures; these judge, so each rule has one place and one test.

use std::collections::HashSet;

use serde::Serialize;

use restream_health::alerts::{Alert, Severity};

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

/// Every output the change set running is running. A share of running
/// outputs is not the engine's per-leaf delivery ratio, so it borrows no
/// floor: one output that did not come up fails the gate (verification can
/// run again once it connects).
pub fn outputs_running(wanted: usize, running: usize) -> VerifyGate {
    VerifyGate {
        gate: "outputsRunning",
        passed: running >= wanted,
        detail: format!("{running} of {wanted} outputs the change set running are running"),
    }
}

/// The ids of the critical alerts on the changed pipelines: a fault on
/// another pipeline, or engine-wide, is not this change's result.
pub fn critical_ids_on(alerts: &[Alert], changed: &HashSet<String>) -> Vec<String> {
    alerts
        .iter()
        .filter(|alert| {
            alert.severity == Severity::Critical
                && alert
                    .pipeline_id
                    .as_ref()
                    .is_some_and(|pipeline| changed.contains(pipeline))
        })
        .map(|alert| alert.id.clone())
        .collect()
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

/// The change did not push the host past its safe utilization: the hottest
/// service center on the calibration priors, before the apply and now. A
/// host already past it before the change passes, so a change that sheds
/// load can verify; the measured flow of the outputs the change set running
/// is `outputs_running`'s to judge. The priors are host-wide, so a change on
/// another pipeline that crosses the threshold between this apply and its
/// verify fails it too: a real host-wide event in a short window.
pub fn capacity_state(before: f32, after: f32, safe_utilization: f64) -> VerifyGate {
    let (before, after) = (f64::from(before), f64::from(after));
    VerifyGate {
        gate: "capacityState",
        passed: after < safe_utilization || before >= safe_utilization,
        detail: format!(
            "hottest center {:.0}% before, {:.0}% now (safe below {:.0}%)",
            before * 100.0,
            after * 100.0,
            safe_utilization * 100.0
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_output_that_did_not_come_up_fails() {
        assert!(outputs_running(3, 3).passed);
        assert!(!outputs_running(3, 2).passed);
        // A change that set nothing running (a stop, a removal) is not short.
        assert!(outputs_running(0, 0).passed);
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

    fn alert(id: &str, severity: Severity, pipeline: Option<&str>) -> Alert {
        Alert {
            id: id.to_string(),
            severity,
            scope: restream_health::alerts::Scope::Pipeline,
            pipeline_id: pipeline.map(str::to_string),
            stage_id: None,
            output_id: None,
            title: String::new(),
            cause: String::new(),
            evidence: Vec::new(),
            recommended_action: String::new(),
            generated_at: String::new(),
            first_seen: None,
            last_seen: None,
            diagnosis: None,
        }
    }

    #[test]
    fn only_critical_alerts_on_the_changed_pipelines_count() {
        let changed = HashSet::from(["a".to_string()]);
        let alerts = [
            alert("a:crit", Severity::Critical, Some("a")),
            alert("a:warn", Severity::Warning, Some("a")),
            alert("b:crit", Severity::Critical, Some("b")),
            alert("engine:crit", Severity::Critical, None),
        ];
        assert_eq!(critical_ids_on(&alerts, &changed), ["a:crit"]);
    }

    #[test]
    fn desired_state_needs_every_change() {
        assert!(desired_state(2, 2).passed);
        assert!(!desired_state(1, 2).passed);
    }

    #[test]
    fn capacity_fails_only_when_the_change_crosses_the_safe_threshold() {
        assert!(capacity_state(0.5, 0.7, 0.8).passed);
        assert!(!capacity_state(0.7, 0.85, 0.8).passed);
        // Already past it: shedding load (or not) cannot be blamed on the change.
        assert!(capacity_state(0.9, 0.85, 0.8).passed);
        assert!(capacity_state(0.9, 0.95, 0.8).passed);
    }
}
