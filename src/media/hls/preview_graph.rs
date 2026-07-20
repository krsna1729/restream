//! Runtime HLS preview graph resolution.
//!
//! The pure planner returns a `StageGraphPlan`; this module reconciles that
//! plan with live engine state, ring buffers, and preview stage execution.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKind;
use crate::media::engine::MediaEngine;
use crate::media::metadata::VideoMeta;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_runtime::StageRuntimeManager;
use crate::planner::plan_hls_preview_graph;

/// Resolved HLS preview graph.
pub struct HlsPreviewGraph {
    /// Ring to read video packets from.
    pub video_ring: Arc<RingBuffer>,
    /// Separate ring for audio when video comes from a transcoding stage
    /// (HEVC preview). `None` when audio is in `video_ring`.
    pub audio_ring: Option<Arc<RingBuffer>>,
    /// Override video metadata to use in the HLS master playlist.
    pub video_meta: Option<VideoMeta>,
}

/// Resolve and start the HLS preview runtime graph for a pipeline.
///
/// For HEVC/H.265 ingest, this creates a video-only preview transcoder that
/// scales to 720p H.264 via the `StageRuntimeManager`. Audio is read from
/// the source ring directly.
///
/// For H.264 and other codecs, the source ring is used as-is.
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
                        audio_ring: None,
                        video_meta: None,
                    });
                };
                let preview_stage_key = preview_plan
                    .stages
                    .iter()
                    .find(|stage| matches!(stage.kind, StageKind::Preview { .. }))
                    .map(|stage| stage.key.clone());
                let Some(key) = preview_stage_key else {
                    return Some(HlsPreviewGraph {
                        video_ring: source_ring,
                        audio_ring: None,
                        video_meta: None,
                    });
                };
                let preview_video = build_preview_video_meta(&engine, pipeline_id).await;
                let manager = StageRuntimeManager::new(engine.clone());
                let (handle, created) = manager
                    .ensure_stage(key.clone(), source_ring.clone(), None)
                    .await;

                if created {
                    manager.spawn_preview_stage(handle.clone(), source_ring.clone());
                }

                return Some(HlsPreviewGraph {
                    video_ring: handle.ring,
                    audio_ring: Some(source_ring),
                    video_meta: preview_video,
                });
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
    let preview_stage_key =
        plan_hls_preview_graph(resource_id, codec, &backend_policy).and_then(|plan| {
            plan.stages
                .into_iter()
                .find(|stage| matches!(stage.kind, StageKind::Preview { .. }))
                .map(|stage| stage.key)
        });
    let Some(key) = preview_stage_key else {
        return HlsPreviewGraph {
            video_ring: source_ring,
            audio_ring: None,
            video_meta: source_video,
        };
    };

    let preview_video = build_preview_video_meta_from_source(source_video).await;
    let manager = StageRuntimeManager::new(engine);
    let (handle, created) = manager.ensure_stage(key, source_ring.clone(), None).await;
    if created {
        manager.spawn_preview_stage(handle.clone(), source_ring.clone());
    }
    HlsPreviewGraph {
        video_ring: handle.ring,
        audio_ring: Some(source_ring),
        video_meta: Some(preview_video),
    }
}

async fn build_preview_video_meta(engine: &MediaEngine, pipeline_id: &str) -> Option<VideoMeta> {
    let source_video = {
        let ingests = engine.ingests.active.read().await;
        ingests
            .get(pipeline_id)
            .and_then(|ingest| ingest.metadata().video)
    };
    Some(build_preview_video_meta_from_source(source_video).await)
}

async fn build_preview_video_meta_from_source(source_video: Option<VideoMeta>) -> VideoMeta {
    let profile = crate::media::profiles::get("720p").await;
    let mut preview_video = source_video.unwrap_or_default();
    preview_video.codec = "h264".to_string();
    if profile.width > 0 {
        preview_video.width = profile.width;
    }
    if profile.height > 0 {
        preview_video.height = profile.height;
    }
    preview_video.profile = None;
    preview_video.level = None;
    preview_video.pixel_format = Some("yuv420p".to_string());
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
}
