//! Read-only observability surfaces: logs, fleet history, and the flight
//! recorder.

use super::AppState;

impl AppState {
    /// Subscribe this request to the live log stream.
    pub(crate) fn subscribe_logs(
        &self,
    ) -> tokio::sync::broadcast::Receiver<crate::logging::LogBroadcast> {
        self.log_broadcast.subscribe()
    }

    /// The fleet history samples in `[since_ms, until_ms]`.
    pub(crate) fn fleet_history_window(
        &self,
        since_ms: u64,
        until_ms: u64,
    ) -> crate::application::fleet_history::FleetHistoryWindow {
        self.fleet_history.window(since_ms, until_ms)
    }

    /// The pipeline's flight trace over `[since_ms, until_ms]`.
    pub(crate) async fn flight_recorder_trace(
        &self,
        pipeline_id: &str,
        since_ms: u64,
        until_ms: u64,
        max_points: usize,
    ) -> Option<crate::application::flight_recorder::FlightTrace> {
        crate::application::flight_recorder::trace_window(
            &self.flight_recorder,
            &self.db,
            pipeline_id,
            since_ms,
            until_ms,
            max_points,
        )
        .await
    }

    /// Destination anomalies the alert evaluator reads.
    pub(crate) fn flight_recorder_anomalies(
        &self,
    ) -> Vec<restream_health::destination_baselines::Anomaly> {
        self.flight_recorder.baseline_anomalies()
    }
}
