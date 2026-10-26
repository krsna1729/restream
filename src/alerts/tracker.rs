//! Alert lifetimes: when each open condition was first and last seen, and a
//! bounded history of the ones that resolved, so a reader can ask what
//! closed in a window the open set no longer shows.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::Alert;

/// Distinct resolved conditions kept (each at its latest closing); the
/// oldest closing is dropped first.
pub const RESOLVED_CAP: usize = 256;

#[derive(Debug, Clone)]
struct AlertHistory {
    first_seen: String,
    /// The alert as last observed: what the resolved entry reports.
    last: Alert,
}

/// An alert that is no longer observed. `resolvedAt` is the first snapshot
/// that did not list it; `alert.lastSeen` the last one that did, so the
/// condition ended between the two.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedAlert {
    pub alert: Alert,
    pub resolved_at: DateTime<Utc>,
}

struct TrackerState {
    open: HashMap<String, AlertHistory>,
    resolved: VecDeque<ResolvedAlert>,
    /// After when `resolved` is complete (an exclusive bound): the tracker's
    /// start (a restart forgets), moved to the latest evicted entry's
    /// `resolved_at` once the ring is full.
    complete_from: DateTime<Utc>,
    /// The notifier's last whole evaluation, which readers are served.
    latest: Option<Evaluation>,
}

/// One whole alert evaluation: the open alerts, stamped, and the time of the
/// health snapshot they were derived from.
#[derive(Debug, Clone, Default)]
pub struct Evaluation {
    pub generated_at: String,
    pub alerts: Vec<Alert>,
}

/// Tracks `first_seen`/`last_seen` timestamps for recurring alert conditions.
///
/// The alert evaluator calls `track` after each whole `derive_alerts` and
/// `publish`es the result; readers use `latest`, `stamp_only` and
/// `resolved_since`, which change nothing.
pub struct AlertTracker {
    state: Mutex<TrackerState>,
}

impl Default for AlertTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// The resolved alerts a window asked for, and from when the kept history
/// is complete: a window that starts earlier may be missing some.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedWindow {
    pub alerts: Vec<ResolvedAlert>,
    pub complete_from: DateTime<Utc>,
}

impl AlertTracker {
    pub fn new() -> Self {
        Self::started_at(Utc::now())
    }

    fn started_at(start: DateTime<Utc>) -> Self {
        Self {
            state: Mutex::new(TrackerState {
                open: HashMap::new(),
                resolved: VecDeque::with_capacity(RESOLVED_CAP),
                complete_from: start,
                latest: None,
            }),
        }
    }

    /// Stamp alerts from a complete snapshot and resolve every absent entry.
    pub fn track(&self, alerts: &mut [Alert]) {
        self.track_at(alerts, Utc::now());
    }

    /// Keep `alerts`, already tracked, as the evaluation readers are served.
    pub fn publish(&self, generated_at: String, alerts: &[Alert]) {
        crate::sync::lock(&self.state).latest = Some(Evaluation {
            generated_at,
            alerts: alerts.to_vec(),
        });
    }

    /// The last published evaluation, if one has run.
    pub fn latest(&self) -> Option<Evaluation> {
        crate::sync::lock(&self.state).latest.clone()
    }

    /// Stamp alerts that a reader derived itself (a pipeline view) with the
    /// history the evaluator keeps. Reads change nothing: only the evaluator
    /// opens or resolves an alert, so the history has one writer however
    /// many pages are open.
    pub fn stamp_only(&self, alerts: &mut [Alert]) {
        let stamp = Utc::now().to_rfc3339();
        let state = crate::sync::lock(&self.state);
        for alert in alerts.iter_mut() {
            if let Some(history) = state.open.get(&alert.id) {
                alert.first_seen = Some(history.first_seen.clone());
                alert.last_seen = Some(stamp.clone());
            }
        }
    }

    fn track_at(&self, alerts: &mut [Alert], now: DateTime<Utc>) {
        let stamp = now.to_rfc3339();
        let mut state = crate::sync::lock(&self.state);
        let TrackerState {
            open,
            resolved,
            complete_from,
            ..
        } = &mut *state;
        let active: HashSet<&str> = alerts.iter().map(|alert| alert.id.as_str()).collect();

        let gone: Vec<String> = open
            .iter()
            .filter(|(id, _)| !active.contains(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            if let Some(history) = open.remove(&id) {
                // One entry per condition: its latest closing. A flapping
                // alert must not evict other alerts' history.
                resolved.retain(|entry| entry.alert.id != id);
                if resolved.len() == RESOLVED_CAP
                    && let Some(evicted) = resolved.pop_front()
                {
                    *complete_from = evicted.resolved_at;
                }
                resolved.push_back(ResolvedAlert {
                    alert: history.last,
                    resolved_at: now,
                });
            }
        }

        for alert in alerts.iter_mut() {
            let entry = open
                .entry(alert.id.clone())
                .or_insert_with(|| AlertHistory {
                    first_seen: stamp.clone(),
                    last: alert.clone(),
                });
            alert.first_seen = Some(entry.first_seen.clone());
            alert.last_seen = Some(stamp.clone());
            entry.last = alert.clone();
        }
    }

    pub fn active_count(&self) -> usize {
        crate::sync::lock(&self.state).open.len()
    }

    /// Alerts resolved at or after `since`, newest first.
    pub fn resolved_since(&self, since: DateTime<Utc>) -> ResolvedWindow {
        let state = crate::sync::lock(&self.state);
        ResolvedWindow {
            alerts: state
                .resolved
                .iter()
                .rev()
                .filter(|entry| entry.resolved_at >= since)
                .cloned()
                .collect(),
            complete_from: state.complete_from,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::{Scope, Severity};

    fn alert(id: &str, pipeline: &str) -> Alert {
        let mut alert = Alert::new(
            id.to_string(),
            Severity::Warning,
            Scope::Pipeline,
            id.to_string(),
            "",
            Vec::new(),
            "",
            "",
        );
        alert.pipeline_id = Some(pipeline.to_string());
        alert
    }

    fn at(minute: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-12T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minute)
    }

    fn ids(entries: &[ResolvedAlert]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.alert.id.as_str())
            .collect()
    }

    #[test]
    fn a_resolved_alert_keeps_its_last_observation_and_when_it_closed() {
        let tracker = AlertTracker::started_at(at(-60));
        tracker.track_at(&mut [alert("a", "p1"), alert("b", "p1")], at(0));
        tracker.track_at(&mut [alert("b", "p1")], at(1));
        tracker.track_at(&mut [], at(2));

        let resolved = tracker.resolved_since(at(-60)).alerts;
        assert_eq!(ids(&resolved), ["b", "a"], "newest first");
        assert_eq!(resolved[0].resolved_at, at(2));
        assert_eq!(resolved[0].alert.first_seen, Some(at(0).to_rfc3339()));
        assert_eq!(resolved[0].alert.last_seen, Some(at(1).to_rfc3339()));
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn since_is_an_inclusive_lower_bound() {
        let tracker = AlertTracker::started_at(at(-60));
        for minute in 0..3 {
            let id = format!("m{minute}");
            tracker.track_at(&mut [alert(&id, "p1")], at(minute));
        }
        tracker.track_at(&mut [], at(3));
        // m0 resolved at 1, m1 at 2, m2 at 3.
        assert_eq!(ids(&tracker.resolved_since(at(2)).alerts), ["m2", "m1"]);
        assert!(tracker.resolved_since(at(4)).alerts.is_empty());
    }

    #[test]
    fn a_flapping_alert_keeps_one_entry_and_evicts_nothing_else() {
        let tracker = AlertTracker::started_at(at(-60));
        tracker.track_at(&mut [alert("steady", "p2")], at(0));
        tracker.track_at(&mut [], at(0));
        for minute in 1..=(RESOLVED_CAP as i64 + 44) {
            tracker.track_at(&mut [alert("flap", "p1")], at(minute));
            tracker.track_at(&mut [], at(minute));
        }
        let window = tracker.resolved_since(at(-60));
        assert_eq!(ids(&window.alerts), ["flap", "steady"]);
        assert_eq!(window.alerts[0].resolved_at, at(RESOLVED_CAP as i64 + 44));
        assert_eq!(window.complete_from, at(-60), "nothing was evicted");
    }

    #[test]
    fn a_stamp_only_snapshot_opens_nothing() {
        let tracker = AlertTracker::started_at(at(-60));
        // Only the summary derives it: the full view never reported it.
        let mut summary_only = [alert("summary-cause", "p1")];
        tracker.stamp_only(&mut summary_only);
        assert_eq!(summary_only[0].first_seen, None);
        tracker.track_at(&mut [], at(1));
        assert!(tracker.resolved_since(at(-60)).alerts.is_empty());
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn a_stamp_only_snapshot_resolves_nothing_and_keeps_first_seen() {
        let tracker = AlertTracker::started_at(at(-60));
        let mut full = [alert("stage", "p1")];
        tracker.track_at(&mut full, at(0));
        let first_seen = full[0].first_seen.clone();
        // The summary does not derive the stage alert: absent, not ended.
        tracker.stamp_only(&mut []);
        let mut again = [alert("stage", "p1")];
        tracker.track_at(&mut again, at(1));
        assert!(tracker.resolved_since(at(-60)).alerts.is_empty());
        assert_eq!(again[0].first_seen, first_seen);
    }

    #[test]
    fn the_history_is_bounded_and_drops_the_oldest() {
        let tracker = AlertTracker::started_at(at(-60));
        for n in 0..=RESOLVED_CAP {
            tracker.track_at(&mut [alert(&format!("a{n}"), "p1")], at(0));
        }
        tracker.track_at(&mut [], at(1));
        let window = tracker.resolved_since(at(-60));
        assert_eq!(window.alerts.len(), RESOLVED_CAP);
        assert!(
            !ids(&window.alerts).contains(&"a0"),
            "the oldest went first"
        );
        // a0 closed at minute 0: from then on the history may be missing some.
        assert_eq!(window.complete_from, at(0));
    }
}
