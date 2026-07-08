//! Application-layer HLS preview orchestration.
//!
//! Owns the lifecycle of HLS preview segmenters: creating the fMP4 store,
//! planning the preview graph (including HEVC→H.264 transcode when needed),
//! and spawning the segmenter task. API handlers call this service rather than
//! directly manipulating media internals.

use std::sync::Arc;

use crate::media::engine::MediaEngine;
use crate::media::hls_fmp4::Fmp4HlsStore;
use crate::planner::hls_preview::{HlsPreviewGraph, plan_hls_preview};

#[derive(Debug)]
pub enum HlsPreviewError {
    NoStream,
}

impl std::fmt::Display for HlsPreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoStream => f.write_str("No HLS stream"),
        }
    }
}

/// Ensure the HLS preview segmenter is running for the given pipeline.
///
/// If an active ingest exists, this creates the fMP4 store (if not already
/// running), plans and starts the preview graph (including HEVC→H.264
/// transcoding when the ingest codec is HEVC), and spawns the segmenter task.
///
/// If no active ingest exists, returns the existing store if one is available,
/// or an error if no preview has been created yet.
pub async fn ensure_hls_preview(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<Arc<Fmp4HlsStore>, HlsPreviewError> {
    let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);

    if has_ingest {
        let (store, already_running) = engine.ensure_hls_preview_segmenter(pipeline_id).await;
        if !already_running {
            let engine_c = engine.clone();
            let pid = pipeline_id.to_string();
            let cancel_token = engine
                .get_hls_preview_cancel_token(pipeline_id)
                .await
                .unwrap();
            let graph =
                match plan_hls_preview(engine.clone(), pipeline_id, cancel_token.clone()).await {
                    Some(g) => g,
                    None => HlsPreviewGraph {
                        video_ring: engine.get_or_create_pipeline(pipeline_id).await,
                        audio_ring: None,
                        video_meta: None,
                    },
                };
            let store_c = store.clone();
            tokio::spawn(async move {
                crate::media::hls_fmp4::start_hls_fmp4_segmenter(
                    pid.clone(),
                    store_c,
                    graph.video_ring,
                    graph.audio_ring,
                    engine_c.clone(),
                    cancel_token,
                    graph.video_meta,
                )
                .await;
                engine_c.shutdown_hls_preview_segmenter(&pid).await;
            });
        }
        engine.touch_hls_preview(pipeline_id).await;
        return Ok(store);
    }

    let Some(store) = engine.get_hls_preview_store(pipeline_id).await else {
        return Err(HlsPreviewError::NoStream);
    };
    engine.touch_hls_preview(pipeline_id).await;
    Ok(store)
}
