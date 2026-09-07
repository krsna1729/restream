//! Runtime HLS preview graph resolution.
//!
//! The pure planner returns a `StageGraphPlan`; this module reconciles that
//! plan with live engine state, ring buffers, and shared-stage execution.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::metadata::VideoMeta;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_runtime::StageRuntimeManager;
use crate::planner::plan_hls_preview_graph;
use crate::runtime::graph::StageGraphPlan;

/// Resolved HLS preview graph.
pub struct HlsPreviewGraph {
    /// Ring to read video and audio packets from.
    pub video_ring: Arc<RingBuffer>,
    /// Override video metadata to use in the HLS master playlist.
    pub video_meta: Option<VideoMeta>,
}

/// Resolve and start the HLS preview runtime graph for a pipeline.
///
/// For HEVC/H.265 ingest, this reuses the shared `hevc_to_h264:from:source`
/// codec edge (same key as legacy RTMP) so audio and video stay on one ring at
/// source resolution. For H.264 and other codecs, the source ring is used as-is.
///
/// Returns `None` when the ingest codec cannot be determined within the
/// deadline (3 seconds).
pub async fn resolve_hls_preview_graph(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
    cancel: CancellationToken,
) -> Option<HlsPreviewGraph> {
    let source_ring = engine.get_or_create_pipeline(pipeline_id).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);

    loop {
        let ingest_codec = engine.ingest_video_codec(pipeline_id).await;
        let codec_hint = source_ring.codec_hint_str();
        let resolved_codec = ingest_codec
            .as_deref()
            .or((!codec_hint.is_empty()).then_some(codec_hint));

        if cancel.is_cancelled() {
            return None;
        }

        match resolved_codec {
            Some(codec) => {
                let backend_policy = engine.backend_policy();
                let preview_plan =
                    plan_hls_preview_graph(pipeline_id, Some(codec), &backend_policy);
                let Some(preview_plan) = preview_plan else {
                    return Some(HlsPreviewGraph {
                        video_ring: source_ring,
                        video_meta: None,
                    });
                };
                let Some(key) = preview_transcode_key(&preview_plan) else {
                    return Some(HlsPreviewGraph {
                        video_ring: source_ring,
                        video_meta: None,
                    });
                };
                let source_video = ingest_video_meta(&engine, pipeline_id).await;
                return Some(
                    ensure_shared_bridge_graph(engine, key, source_ring, source_video).await,
                );
            }
            None if tokio::time::Instant::now() >= deadline => {
                return None;
            }
            None => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
}

pub async fn resolve_input_hls_preview_graph(
    engine: Arc<MediaEngine>,
    resource_id: &str,
    input_id: &str,
    source_ring: Arc<RingBuffer>,
    cancel: CancellationToken,
) -> HlsPreviewGraph {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    let source_video = loop {
        let video = engine
            .ingests
            .sessions
            .read()
            .await
            .get(input_id)
            .and_then(|ingest| ingest.metadata().video);
        if video.is_some() || cancel.is_cancelled() || tokio::time::Instant::now() >= deadline {
            break video;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let codec_hint = source_ring.codec_hint_str();
    let codec = source_video
        .as_ref()
        .map(|video| video.codec.as_str())
        .or((!codec_hint.is_empty()).then_some(codec_hint));
    let backend_policy = engine.backend_policy();
    let preview_plan = plan_hls_preview_graph(resource_id, codec, &backend_policy);
    let Some(key) = preview_plan.as_ref().and_then(preview_transcode_key) else {
        return HlsPreviewGraph {
            video_ring: source_ring,
            video_meta: source_video,
        };
    };

    ensure_shared_bridge_graph(engine, key, source_ring, source_video).await
}

/// Media stage feeding the HLS segmenter, when that upstream is not Source.
fn preview_transcode_key(plan: &StageGraphPlan) -> Option<StageKey> {
    let StageKind::HlsSegmenter { upstream } = &plan.terminal_stage.kind else {
        return None;
    };
    if matches!(upstream.as_ref(), StageKind::Source) {
        return None;
    }
    Some(StageKey::new(
        plan.terminal_stage.pipeline.clone(),
        upstream.as_ref().clone(),
    ))
}

async fn ensure_shared_bridge_graph(
    engine: Arc<MediaEngine>,
    key: StageKey,
    source_ring: Arc<RingBuffer>,
    source_video: Option<VideoMeta>,
) -> HlsPreviewGraph {
    let manager = StageRuntimeManager::new(engine);
    let (handle, created) = manager.ensure_stage(key, source_ring.clone(), None).await;
    if created {
        manager.spawn_codec_edge_stage(handle.clone(), source_ring);
    }
    HlsPreviewGraph {
        video_ring: handle.ring,
        video_meta: Some(h264_preview_video_meta(source_video)),
    }
}

async fn ingest_video_meta(engine: &MediaEngine, pipeline_id: &str) -> Option<VideoMeta> {
    let ingests = engine.ingests.active.read().await;
    ingests
        .get(pipeline_id)
        .and_then(|ingest| ingest.metadata().video)
}

fn h264_preview_video_meta(source_video: Option<VideoMeta>) -> VideoMeta {
    let mut preview_video = source_video.unwrap_or_default();
    preview_video.codec = "h264".to_string();
    preview_video.profile = None;
    preview_video.level = None;
    preview_video.pixel_format = None;
    preview_video
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn engine_with_pipeline(pipeline_id: &str) -> Arc<MediaEngine> {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest(pipeline_id, "stream-key", "rtmp")
            .await
            .unwrap();
        let _ = engine.get_or_create_pipeline(pipeline_id).await;
        engine
    }

    // Regression/documentation test: `resolve_hls_preview_graph` checks
    // `cancel.is_cancelled()` once per loop iteration, before doing any
    // codec-resolution wait. A pre-cancelled token on a pipeline with no
    // resolvable codec must short-circuit to `None` on the very first
    // iteration rather than falling through to the 100ms poll sleep or the
    // 3s deadline.
    #[tokio::test(start_paused = true)]
    async fn returns_none_immediately_when_cancelled_before_codec_resolves() {
        let engine = engine_with_pipeline("pipe-preview-graph-cancel").await;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let start = tokio::time::Instant::now();
        let graph = resolve_hls_preview_graph(engine, "pipe-preview-graph-cancel", cancel).await;

        assert!(graph.is_none(), "cancelled resolution must return None");
        assert_eq!(
            tokio::time::Instant::now(),
            start,
            "a pre-cancelled token must short-circuit before any deadline sleep"
        );
    }

    // A pipeline whose codec never resolves (no ingest video codec and no
    // ring codec hint) must give up after the 3s deadline rather than
    // polling forever.
    #[tokio::test(start_paused = true)]
    async fn returns_none_after_deadline_when_codec_never_resolves() {
        let engine = engine_with_pipeline("pipe-preview-graph-deadline").await;
        let cancel = CancellationToken::new();

        let start = tokio::time::Instant::now();
        let graph = resolve_hls_preview_graph(engine, "pipe-preview-graph-deadline", cancel).await;

        assert!(graph.is_none(), "an unresolved codec must time out to None");
        assert!(
            tokio::time::Instant::now() >= start + std::time::Duration::from_secs(3),
            "must not return before the 3s resolution deadline"
        );
    }

    #[test]
    fn hevc_preview_meta_keeps_source_resolution() {
        let source = VideoMeta {
            codec: "hevc".to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            profile: Some("Main".to_string()),
            level: Some("4.0".to_string()),
            pixel_format: Some("yuv420p10le".to_string()),
            ..Default::default()
        };
        let preview = h264_preview_video_meta(Some(source));
        assert_eq!(preview.codec, "h264");
        assert_eq!(preview.width, 1920);
        assert_eq!(preview.height, 1080);
        assert!(preview.profile.is_none());
        assert!(preview.level.is_none());
        assert!(preview.pixel_format.is_none());
    }
}
