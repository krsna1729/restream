//! Read-only observability surfaces: logs, fleet history, and the flight
//! recorder.

use std::sync::Arc;

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

    /// The shared history handle used by the fleet sampler task.
    pub(crate) fn fleet_history_sampler_handle(
        &self,
    ) -> Arc<crate::application::fleet_history::FleetHistory> {
        Arc::clone(&self.fleet_history)
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

    /// The shared recorder handle used by the flight-recorder task.
    pub(crate) fn flight_recorder_sampler_handle(
        &self,
    ) -> Arc<crate::application::flight_recorder::FlightRecorder> {
        Arc::clone(&self.flight_recorder)
    }
}
