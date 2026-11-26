//! Alert tracker operations shared by the alert, incident, and pipeline
//! observability surfaces.

use std::time::SystemTime;

use super::AppState;
use restream_health::alerts::{Alert, Evaluation, ResolvedWindow};

impl AppState {
    /// Stamps a freshly derived alert list with tracker history.
    pub(crate) fn track_alerts(&self, alerts: &mut [Alert]) {
        self.alert_tracker.track(alerts);
    }

    /// Publishes the evaluator's latest alert list.
    pub(crate) fn publish_alerts(&self, generated_at: String, alerts: &[Alert]) {
        self.alert_tracker.publish(generated_at, alerts);
    }

    /// Returns the latest published evaluation.
    pub(crate) fn latest_alerts(&self) -> Option<Evaluation> {
        self.alert_tracker.latest()
    }

    /// Stamps reader-derived alerts without changing tracker history.
    pub(crate) fn stamp_alerts(&self, alerts: &mut [Alert]) {
        self.alert_tracker.stamp_only(alerts);
    }

    /// Returns alerts resolved since one instant.
    pub(crate) fn resolved_alerts_since(&self, since: SystemTime) -> ResolvedWindow {
        self.alert_tracker.resolved_since(since)
    }
}
