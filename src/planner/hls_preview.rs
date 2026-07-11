//! HLS preview graph planning.
//!
//! Decides whether a browser-preview HLS stream needs a transcoding stage
//! (HEVC→H.264 video-only) or can read the source ring directly.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKind;
use crate::media::engine::{MediaEngine, VideoMeta};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_runtime::StageRuntimeManager;
use crate::planner::graph_plan::plan_hls_preview_graph;

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

/// Plan and start the HLS preview graph for a pipeline.
///
/// For HEVC/H.265 ingest, this creates a video-only preview transcoder that
/// scales to 720p H.264 via the `StageRuntimeManager`. Audio is read from
/// the source ring directly.
///
/// For H.264 and other codecs, the source ring is used as-is.
///
/// Returns `None` when the ingest codec cannot be determined within the
/// deadline (3 seconds).
pub async fn plan_hls_preview(
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

async fn build_preview_video_meta(engine: &MediaEngine, pipeline_id: &str) -> Option<VideoMeta> {
    let source_video = {
        let ingests = engine.ingests.active.read().await;
        ingests
            .get(pipeline_id)
            .and_then(|ingest| ingest.video.clone())
    };
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
    Some(preview_video)
}
