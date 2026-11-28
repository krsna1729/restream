//! Browser HLS preview runtimes: the fMP4 preview segmenter and the runtime
//! graph feeding it.
//!
//! The pure planner returns a `StageGraphPlan`; this module reconciles that
//! plan with live engine state, ring buffers, and shared-stage execution.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::engine::MediaEngine;
use crate::hls::HlsSegmenterStart;
use crate::hls_fmp4::Fmp4HlsStore;
use crate::stage_registry::StageLease;
use restream_core::metadata::VideoMeta;
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_domain::planner::plan_hls_preview_graph;
use restream_domain::runtime::graph::StageGraphPlan;
use restream_sync::RingBuffer;

/// Resolved HLS preview graph.
struct HlsPreviewGraph {
    /// Ring to read video and audio packets from.
    video_ring: Arc<RingBuffer>,
    /// Override video metadata to use in the HLS master playlist.
    video_meta: Option<VideoMeta>,
    /// Lease on the shared transcode stage, when the preview reads one; the
    /// segmenter holds it for its lifetime.
    stage: Option<StageLease>,
}

impl MediaEngine {
    /// Ensure a browser-preview HLS runtime exists and spawn its segmenter if
    /// this call created the preview consumer. The runtime layer owns the store,
    /// cancellation token, graph planning, and segmenter task lifecycle.
    pub async fn ensure_hls_preview_runtime(
        self: &Arc<Self>,
        pipeline_id: &str,
    ) -> Arc<Fmp4HlsStore> {
        let (store, already_running, cancel_token) =
            self.ensure_hls_preview_segmenter(pipeline_id).await;
        if !already_running {
            let engine = self.clone();
            let pid = pipeline_id.to_string();
            let graph = match self
                .resolve_hls_preview_graph(pipeline_id, cancel_token.clone())
                .await
            {
                Some(graph) => graph,
                None => HlsPreviewGraph {
                    video_ring: self
                        .ingests
                        .get_or_create_pipeline(crate::preview_ids::preview_pipeline(pipeline_id).0)
                        .await,
                    video_meta: None,
                    stage: None,
                },
            };
            let store_for_task = store.clone();
            tokio::spawn(async move {
                engine
                    .start_hls_fmp4_segmenter(
                        pid.clone(),
                        store_for_task,
                        graph.video_ring,
                        cancel_token,
                        HlsSegmenterStart {
                            video_meta_override: graph.video_meta,
                            planned_stage_key: None,
                        },
                    )
                    .await;
                engine.shutdown_hls_preview_segmenter(&pid).await;
                drop(graph.stage);
            });
        }

        self.touch_hls_preview(pipeline_id).await;
        store
    }

    /// `preview_input_id` is an input id, or `<input>.hevc` for the H.265
    /// source as sent.
    pub async fn ensure_input_hls_preview_runtime(
        self: &Arc<Self>,
        preview_input_id: &str,
    ) -> Option<(String, Arc<Fmp4HlsStore>)> {
        let input_id = crate::preview_ids::preview_pipeline(preview_input_id).0;
        let source_ring = self.ingests.ensure_input_preview_ring(input_id).await?;
        let resource_id = crate::preview_ids::input_hls_preview_resource_id(preview_input_id);
        let (store, already_running, cancel_token) =
            self.ensure_hls_preview_segmenter(&resource_id).await;
        if !already_running {
            let graph = self
                .resolve_input_hls_preview_graph(
                    &resource_id,
                    input_id,
                    source_ring,
                    cancel_token.clone(),
                )
                .await;
            let engine = self.clone();
            let segmenter_id = resource_id.clone();
            let store_for_task = store.clone();
            tokio::spawn(async move {
                engine
                    .start_hls_fmp4_segmenter(
                        segmenter_id.clone(),
                        store_for_task,
                        graph.video_ring,
                        cancel_token,
                        HlsSegmenterStart {
                            video_meta_override: graph.video_meta,
                            planned_stage_key: None,
                        },
                    )
                    .await;
                engine.shutdown_hls_preview_segmenter(&segmenter_id).await;
                drop(graph.stage);
            });
        }
        self.touch_hls_preview(&resource_id).await;
        Some((resource_id, store))
    }

    /// Resolve and start the HLS preview runtime graph for a pipeline.
    ///
    /// For HEVC/H.265 ingest, this reuses the shared `hevc_to_h264:from:source`
    /// codec edge (the same key H.264-only outputs use) so audio and video stay on
    /// one ring at source resolution. For H.264 and other codecs, the source ring
    /// is used as-is.
    ///
    /// Returns `None` when the ingest codec cannot be determined within the
    /// deadline (3 seconds).
    ///
    /// A passthrough preview id (`<pipeline>.hevc`) reads the source ring as it
    /// is, H.265 included: the browser asked for it because it decodes H.265.
    async fn resolve_hls_preview_graph(
        &self,
        preview_id: &str,
        cancel: CancellationToken,
    ) -> Option<HlsPreviewGraph> {
        let (pipeline_id, passthrough) = crate::preview_ids::preview_pipeline(preview_id);
        let source_ring = self.ingests.get_or_create_pipeline(pipeline_id).await;
        if passthrough {
            return Some(HlsPreviewGraph {
                video_ring: source_ring,
                video_meta: None,
                stage: None,
            });
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);

        loop {
            let ingest_codec = self.ingest_video_codec(pipeline_id).await;
            let codec_hint = source_ring.codec_hint_str();
            let resolved_codec = ingest_codec
                .as_deref()
                .or((!codec_hint.is_empty()).then_some(codec_hint));

            if cancel.is_cancelled() {
                return None;
            }

            match resolved_codec {
                Some(codec) => {
                    let preview_plan = plan_hls_preview_graph(pipeline_id, Some(codec));
                    let Some(preview_plan) = preview_plan else {
                        return Some(HlsPreviewGraph {
                            video_ring: source_ring,
                            video_meta: None,
                            stage: None,
                        });
                    };
                    let Some(key) = preview_transcode_key(&preview_plan) else {
                        return Some(HlsPreviewGraph {
                            video_ring: source_ring,
                            video_meta: None,
                            stage: None,
                        });
                    };
                    let source_video = self.ingest_video_meta(pipeline_id).await;
                    return Some(
                        self.ensure_shared_bridge_graph(key, source_ring, source_video)
                            .await,
                    );
                }
                None if tokio::time::Instant::now() >= deadline => {
                    return None;
                }
                None => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    }

    async fn resolve_input_hls_preview_graph(
        &self,
        resource_id: &str,
        input_id: &str,
        source_ring: Arc<RingBuffer>,
        cancel: CancellationToken,
    ) -> HlsPreviewGraph {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let source_video = loop {
            let video = self
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
        // A passthrough input preview reads the input's ring as it is.
        let passthrough = crate::preview_ids::preview_pipeline(resource_id).1;
        if passthrough {
            return HlsPreviewGraph {
                video_ring: source_ring,
                video_meta: source_video,
                stage: None,
            };
        }
        let codec_hint = source_ring.codec_hint_str();
        let codec = source_video
            .as_ref()
            .map(|video| video.codec.as_str())
            .or((!codec_hint.is_empty()).then_some(codec_hint));
        let preview_plan = plan_hls_preview_graph(resource_id, codec);
        let Some(key) = preview_plan.as_ref().and_then(preview_transcode_key) else {
            return HlsPreviewGraph {
                video_ring: source_ring,
                video_meta: source_video,
                stage: None,
            };
        };

        self.ensure_shared_bridge_graph(key, source_ring, source_video)
            .await
    }

    async fn ensure_shared_bridge_graph(
        &self,
        key: StageKey,
        source_ring: Arc<RingBuffer>,
        source_video: Option<VideoMeta>,
    ) -> HlsPreviewGraph {
        let manager = self.stage_runtime();
        let (handle, lease, created) = manager.ensure_stage(key, source_ring.clone(), None).await;
        if created {
            manager.spawn_codec_edge_stage(handle.clone(), source_ring);
        }
        HlsPreviewGraph {
            video_ring: handle.ring,
            video_meta: Some(h264_preview_video_meta(source_video)),
            stage: Some(lease),
        }
    }

    async fn ingest_video_meta(&self, pipeline_id: &str) -> Option<VideoMeta> {
        let ingests = self.ingests.active.read().await;
        ingests
            .get(pipeline_id)
            .and_then(|ingest| ingest.metadata().video)
    }
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

fn h264_preview_video_meta(source_video: Option<VideoMeta>) -> VideoMeta {
    let mut preview_video = source_video.unwrap_or_default();
    preview_video.codec = "h264".to_string();
    preview_video.profile = None;
    preview_video.level = None;
    preview_video.pixel_format = None;
    preview_video
}

#[cfg(test)]
mod tests;
