//! The shared lifecycle of one fabric output: retain the runtime, prepare
//! protocol state, place the `Add`, wait for a stop or leaf failure, and send
//! `Remove`/`Abandon` before releasing the runtime. Protocol-specific hooks
//! stay in each run type; `EgressTask::run_fabric` owns the order.
use std::future::Future;
use std::sync::Arc;

use super::*;
use restream_health::diagnosis::CAPACITY_REFUSED_PHASE;

/// A fabric `Add` refusal, normalized so every protocol records it the same
/// way. `phase` is already the capacity-refusal phase when a shard refused.
pub(super) struct FabricDispatchFailure {
    pub(super) phase: &'static str,
    pub(super) message: String,
}

impl FabricDispatchFailure {
    fn new(
        refused: Option<String>,
        fallback_phase: &'static str,
        error: impl std::fmt::Debug,
    ) -> Self {
        match refused {
            Some(saturated) => Self {
                phase: CAPACITY_REFUSED_PHASE,
                message: saturated,
            },
            None => Self {
                phase: fallback_phase,
                message: format!("{error:?}"),
            },
        }
    }
}

/// How a fabric run ended: an unexpected leaf closure to report, and whether
/// the output's stage ended under it (HLS abandons the upload session then).
pub(super) struct FabricEnd {
    pub(super) failed: bool,
    pub(super) stage_ended: bool,
}

/// The protocol hooks one fabric output provides to [`EgressTask::run_fabric`].
/// The sequence itself lives there, so every protocol retains, places,
/// reports failures, releases, and stops in the same order.
pub(super) trait FabricRun {
    fn phase(&self) -> EgressPhase;
    fn ensure_phase(&self) -> &'static str;
    fn leaf_phase(&self) -> &'static str;
    fn leaf_message(&self) -> &'static str;
    fn feed(&self) -> FeedId;
    fn prepare(&self) -> impl Future<Output = ()> + Send;
    fn ensure(&mut self) -> impl Future<Output = Result<(), String>> + Send;
    fn add(
        &self,
    ) -> impl Future<
        Output = Result<
            restream_engine::egress::manager::ManagerCommandOutcome,
            FabricDispatchFailure,
        >,
    > + Send;
    fn release(&self) -> impl Future<Output = ()> + Send;
    fn wait(&mut self) -> impl Future<Output = FabricEnd> + Send;
    fn stop(&self, stage_ended: bool) -> impl Future<Output = ()> + Send;
}

pub(super) struct SrtRun<'a> {
    task: &'a EgressTask,
    fabric: SrtFabricTask,
}

impl<'a> SrtRun<'a> {
    pub(super) fn new(task: &'a EgressTask, fabric: SrtFabricTask) -> Self {
        Self { task, fabric }
    }
}

impl FabricRun for SrtRun<'_> {
    fn phase(&self) -> EgressPhase {
        EgressPhase::Sending
    }
    fn ensure_phase(&self) -> &'static str {
        "srt_fabric_ensure"
    }
    fn leaf_phase(&self) -> &'static str {
        "srt_fabric_leaf"
    }
    fn leaf_message(&self) -> &'static str {
        "SRT fabric leaf terminated unexpectedly (peer closed, protocol failure, or stall recovery)"
    }
    fn feed(&self) -> FeedId {
        self.fabric.feed_id.clone()
    }
    async fn prepare(&self) {}
    async fn ensure(&mut self) -> Result<(), String> {
        self.task
            .engine
            .retain_srt_fabric_runtime(
                self.fabric.feed_id.clone(),
                self.fabric.feed.as_ref(),
                &self.fabric.source,
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }
    async fn add(
        &self,
    ) -> Result<restream_engine::egress::manager::ManagerCommandOutcome, FabricDispatchFailure>
    {
        self.task
            .engine
            .dispatch_srt_fabric_command(
                &self.fabric.feed_id,
                EgressCommand::Add(self.fabric.spec.clone()),
            )
            .await
            .map_err(|error| {
                FabricDispatchFailure::new(
                    error.refused().map(ToString::to_string),
                    "srt_fabric_dispatch",
                    &error,
                )
            })
    }
    async fn release(&self) {
        let _ = self
            .task
            .engine
            .release_srt_fabric_runtime(&self.fabric.feed_id)
            .await;
    }
    async fn wait(&mut self) -> FabricEnd {
        FabricEnd {
            failed: self
                .task
                .wait_for_stop_or_leaf_failure(&self.fabric.terminated)
                .await,
            stage_ended: false,
        }
    }
    async fn stop(&self, _stage_ended: bool) {
        let _ = self
            .task
            .engine
            .dispatch_srt_fabric_command(
                &self.fabric.feed_id,
                EgressCommand::Remove(self.fabric.spec.id.clone()),
            )
            .await;
        self.release().await;
    }
}

pub(super) struct RtmpRun<'a> {
    task: &'a EgressTask,
    fabric: RtmpFabricTask,
}

impl<'a> RtmpRun<'a> {
    pub(super) fn new(task: &'a EgressTask, fabric: RtmpFabricTask) -> Self {
        Self { task, fabric }
    }
}

impl FabricRun for RtmpRun<'_> {
    fn phase(&self) -> EgressPhase {
        EgressPhase::Sending
    }
    fn ensure_phase(&self) -> &'static str {
        "rtmp_fabric_ensure"
    }
    fn leaf_phase(&self) -> &'static str {
        "rtmp_fabric_leaf"
    }
    fn leaf_message(&self) -> &'static str {
        "RTMP fabric leaf terminated unexpectedly (peer closed, protocol failure, or stall recovery)"
    }
    fn feed(&self) -> FeedId {
        self.fabric.feed_id.clone()
    }
    async fn prepare(&self) {
        // Must land before the Add command: the shard thread reads this via
        // RtmpPublishStartupSource::take_startup and never queries anything
        // itself.
        self.task
            .engine
            .set_rtmp_publish_startup(
                &self.fabric.feed_id,
                self.fabric.spec.id.clone(),
                self.fabric.startup.clone(),
            )
            .await;
    }
    async fn ensure(&mut self) -> Result<(), String> {
        self.task
            .engine
            .retain_rtmp_fabric_runtime(self.fabric.feed_id.clone(), self.fabric.feed.as_ref())
            .await
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    }
    async fn add(
        &self,
    ) -> Result<restream_engine::egress::manager::ManagerCommandOutcome, FabricDispatchFailure>
    {
        self.task
            .engine
            .dispatch_rtmp_fabric_command(
                &self.fabric.feed_id,
                EgressCommand::Add(self.fabric.spec.clone()),
            )
            .await
            .map_err(|error| {
                FabricDispatchFailure::new(
                    error.refused().map(ToString::to_string),
                    "rtmp_fabric_dispatch",
                    &error,
                )
            })
    }
    async fn release(&self) {
        let _ = self
            .task
            .engine
            .release_rtmp_fabric_runtime(&self.fabric.feed_id)
            .await;
    }
    async fn wait(&mut self) -> FabricEnd {
        FabricEnd {
            failed: self
                .task
                .wait_for_stop_or_leaf_failure(&self.fabric.terminated)
                .await,
            stage_ended: false,
        }
    }
    async fn stop(&self, _stage_ended: bool) {
        let _ = self
            .task
            .engine
            .dispatch_rtmp_fabric_command(
                &self.fabric.feed_id,
                EgressCommand::Remove(self.fabric.spec.id.clone()),
            )
            .await;
        self.release().await;
    }
}

pub(super) struct HlsPutRun<'a> {
    task: &'a EgressTask,
    fabric: HlsPutFabricTask,
    /// Taken once `ensure` has consumed it, matching the previous local
    /// drop after the retain call.
    store: Option<Arc<restream_engine::hls::HlsStore>>,
    forwarded: ForwardedUploadStatus,
}

impl<'a> HlsPutRun<'a> {
    pub(super) fn new(
        task: &'a EgressTask,
        fabric: HlsPutFabricTask,
        store: Arc<restream_engine::hls::HlsStore>,
    ) -> Self {
        Self {
            task,
            fabric,
            store: Some(store),
            forwarded: ForwardedUploadStatus::default(),
        }
    }
}

impl HlsPutRun<'_> {
    fn feed_id(&self) -> FeedId {
        restream_engine::engine_hls_egress_fabric::hls_put_feed_id(&self.task.pipeline_id)
    }
}

impl FabricRun for HlsPutRun<'_> {
    fn phase(&self) -> EgressPhase {
        EgressPhase::Uploading
    }
    fn ensure_phase(&self) -> &'static str {
        "hls_put_fabric_ensure"
    }
    fn leaf_phase(&self) -> &'static str {
        "hls_put_fabric_leaf"
    }
    fn leaf_message(&self) -> &'static str {
        "HLS ingest rejected the upload or the leaf failed"
    }
    fn feed(&self) -> FeedId {
        self.feed_id()
    }
    async fn prepare(&self) {}
    async fn ensure(&mut self) -> Result<(), String> {
        let store = self
            .store
            .as_ref()
            .expect("HLS store present before ensure");
        let result = self
            .task
            .engine
            .retain_hls_put_fabric_runtime(self.feed_id(), store)
            .await;
        self.store = None;
        result.map(|_| ()).map_err(|error| format!("{error:?}"))
    }
    async fn add(
        &self,
    ) -> Result<restream_engine::egress::manager::ManagerCommandOutcome, FabricDispatchFailure>
    {
        self.task
            .engine
            .dispatch_hls_put_fabric_command(
                &self.feed_id(),
                EgressCommand::Add(self.fabric.spec.clone()),
            )
            .await
            .map_err(|error| {
                FabricDispatchFailure::new(
                    error.refused().map(ToString::to_string),
                    "hls_put_fabric_dispatch",
                    &error,
                )
            })
    }
    async fn release(&self) {
        let _ = self
            .task
            .engine
            .release_hls_put_fabric_runtime(&self.feed_id())
            .await;
    }
    async fn wait(&mut self) -> FabricEnd {
        let task = self.task;
        let fabric = &self.fabric;
        let forwarded = &mut self.forwarded;
        let own_stage = task.own_stage_ended();
        tokio::pin!(own_stage);
        let (failed, stage_ended) = loop {
            task.forward_upload_status(&fabric.upload_status, forwarded)
                .await;
            if fabric.terminated.load(std::sync::atomic::Ordering::Acquire) {
                task.forward_upload_status(&fabric.upload_status, forwarded)
                    .await;
                break (true, false);
            }
            tokio::select! {
                _ = task.registration.cancel_token.cancelled() => break (false, false),
                () = &mut own_stage => break (false, true),
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        };
        FabricEnd {
            // A rejection already reported with its status and object does
            // not also get the generic leaf error.
            failed: failed && forwarded.failures == 0,
            stage_ended,
        }
    }
    async fn stop(&self, stage_ended: bool) {
        // The leaf sends its end playlist after a remove. A stage that ended
        // under the output is no intentional stop: abandon the session, so
        // the destination keeps the stream open for the retry.
        let command = if stage_ended {
            EgressCommand::Abandon(self.fabric.spec.id.clone())
        } else {
            EgressCommand::Remove(self.fabric.spec.id.clone())
        };
        let _ = self
            .task
            .engine
            .dispatch_hls_put_fabric_command(&self.feed_id(), command)
            .await;
        self.release().await;
    }
}
