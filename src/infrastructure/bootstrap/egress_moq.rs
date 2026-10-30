//! `moqt://` outputs on the MoQ fabric: start-up preparation and the
//! per-attempt task body, mirroring the RTMP fabric path in
//! `egress_task.rs` (retain runtime → startup snapshot before `Add` →
//! dispatch `Add` → wait for stop or leaf failure → `Remove` → release).
//! Kept in its own file so `egress_task.rs` stays under the source-audit
//! line cap.

use std::sync::Arc;

use crate::application::diagnosis::CAPACITY_REFUSED_PHASE;
use crate::application::egress::PreparedOutput;
use crate::application::models::Output;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::{EgressCommand, FeedId, OutputSpec};
use crate::media::engine::{EgressRegistration, MediaEngine};
use crate::media::moq::egress::MoqPublishStartup;
use restream_domain::domain::state::EgressPhase;

use super::EgressTask;

#[derive(Clone)]
pub(crate) struct MoqFabricTask {
    pub(crate) feed_id: FeedId,
    pub(crate) feed: Arc<RingFeed>,
    pub(crate) spec: OutputSpec,
    pub(crate) startup: MoqPublishStartup,
    /// See `SrtFabricTask::terminated`.
    pub(crate) terminated: Arc<std::sync::atomic::AtomicBool>,
}

/// The fabric task for a `moqt://` output, or `None` after recording why it
/// cannot start (an unusable URL or `ca` file).
pub(crate) async fn prepare_moq_fabric_task(
    engine: &Arc<MediaEngine>,
    output: &Output,
    prepared: &PreparedOutput,
    registration: &EgressRegistration,
) -> Option<MoqFabricTask> {
    use crate::application::egress_moq_fabric as moq;
    let startup = match moq::prepare_moq_fabric_startup(engine, output, prepared).await {
        Ok(startup) => startup,
        Err(error) => {
            engine
                .record_egress_error_if_current(
                    &output.id,
                    registration,
                    "moq_fabric_startup",
                    error,
                )
                .await;
            return None;
        }
    };
    let feed = moq::prepare_moq_fabric_feed(prepared);
    let mut spec =
        moq::moq_fabric_output_spec(output, registration.attempt_id, feed.feed_id.clone());
    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(sink) = engine
        .with_active_egress(&output.id, |egress| {
            crate::media::egress::leaf::EgressProgressSink {
                bytes_sent: Some(egress.bytes_sent.clone()),
                metrics: Some(egress.metrics.clone()),
                last_progress_ms: Some(egress.last_progress_ms.clone()),
                resync_count: Some(egress.resync_count.clone()),
                feed_lag_units: Some(egress.feed_lag_units.clone()),
                backpressure_reason: Some(egress.backpressure_reason.clone()),
                quality: Some(egress.quality.clone()),
                terminated_unexpectedly: Some(terminated.clone()),
                upload_status: None,
            }
        })
        .await
    {
        spec.progress = sink;
    }
    Some(MoqFabricTask {
        feed_id: feed.feed_id,
        feed: feed.feed,
        spec,
        startup,
        terminated,
    })
}

impl EgressTask {
    pub(crate) async fn run_moq_fabric(&self, fabric: MoqFabricTask) {
        if let Err(error) = self
            .engine
            .retain_moq_fabric_runtime(fabric.feed_id.clone(), fabric.feed.as_ref())
            .await
        {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "moq_fabric_ensure",
                    format!("{error:?}"),
                )
                .await;
            return;
        }

        // Must land before the Add: the shard thread reads it and never
        // queries the engine itself.
        self.engine
            .set_moq_publish_startup(&fabric.feed_id, fabric.spec.id.clone(), fabric.startup)
            .await;

        match self
            .engine
            .dispatch_moq_fabric_command(&fabric.feed_id, EgressCommand::Add(fabric.spec.clone()))
            .await
        {
            Ok(outcome) => {
                self.record_fabric_placement(&fabric.spec.feed, &outcome)
                    .await;
                self.engine
                    .update_egress_phase_if_current(
                        &self.output_id,
                        &self.registration,
                        EgressPhase::Sending,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self
                    .engine
                    .release_moq_fabric_runtime(&fabric.feed_id)
                    .await;
                let (phase, message) = match error.refused() {
                    Some(saturated) => (CAPACITY_REFUSED_PHASE, saturated.to_string()),
                    None => ("moq_fabric_dispatch", format!("{error:?}")),
                };
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        phase,
                        message,
                    )
                    .await;
                return;
            }
        }

        if self.wait_for_stop_or_leaf_failure(&fabric.terminated).await {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "moq_fabric_leaf",
                    "MoQ fabric leaf terminated unexpectedly (relay closed, protocol failure, or stall recovery)",
                )
                .await;
        }
        let _ = self
            .engine
            .dispatch_moq_fabric_command(&fabric.feed_id, EgressCommand::Remove(fabric.spec.id))
            .await;
        let _ = self
            .engine
            .release_moq_fabric_runtime(&fabric.feed_id)
            .await;
    }
}
