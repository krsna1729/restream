//! The pipeline HLS segmenter an HLS egress attempt starts.

use std::sync::Arc;

use crate::media::engine::MediaEngine;
use crate::media::stage_lease::StageLease;

/// Spawns the pipeline's one HLS segmenter on `ring`. The segmenter is
/// shared by every HLS output of the pipeline and outlives the output that
/// started it, so it holds that output's stage leases itself: the stages
/// feeding `ring` stop only after the segmenter ends. A stage that ends on
/// its own leaves `ring` unfed for good, so the segmenter ends with it and
/// the next HLS attempt starts a new one on its new stage.
pub(super) fn spawn_pipeline_hls_segmenter(
    engine: Arc<MediaEngine>,
    pipeline_id: String,
    store: Arc<crate::media::hls::HlsStore>,
    ring: Arc<crate::media::ring_buffer::RingBuffer>,
    cancel: tokio_util::sync::CancellationToken,
    stage_key: crate::domain::stage::StageKey,
    stages: Vec<StageLease>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let segmenter = crate::media::hls::start_hls_segmenter(
            pipeline_id.clone(),
            store,
            ring,
            None,
            engine.clone(),
            cancel.clone(),
            crate::media::hls::HlsSegmenterStart {
                video_meta_override: None,
                planned_stage_key: Some(stage_key),
            },
        );
        tokio::pin!(segmenter);
        tokio::select! {
            () = &mut segmenter => {}
            () = StageLease::any_ended(&stages) => {
                cancel.cancel();
                segmenter.await;
            }
        }
        engine.shutdown_hls_segmenter(&pipeline_id).await;
        drop(stages);
    })
}

#[cfg(test)]
#[path = "hls_segmenter_tests.rs"]
mod tests;
