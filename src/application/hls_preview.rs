//! Application-layer HLS preview orchestration.
//!
//! Owns application policy for HLS preview requests. The media runtime owns
//! preview graph planning, fMP4 store creation, cancellation, and segmenter task
//! lifecycle.

use std::sync::Arc;

use crate::media::engine::MediaEngine;
use crate::media::hls_fmp4::Fmp4HlsStore;

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
/// If an active ingest exists, this asks the media runtime to create or reuse
/// the preview runtime. The runtime handles graph planning and segmenter task
/// ownership.
///
/// If no active ingest exists, returns the existing store if one is available,
/// or an error if no preview has been created yet.
pub async fn ensure_hls_preview(
    engine: Arc<MediaEngine>,
    pipeline_id: &str,
) -> Result<Arc<Fmp4HlsStore>, HlsPreviewError> {
    let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);

    if has_ingest {
        return Ok(engine.ensure_hls_preview_runtime(pipeline_id).await);
    }

    let Some(store) = engine.get_hls_preview_store(pipeline_id).await else {
        return Err(HlsPreviewError::NoStream);
    };
    engine.touch_hls_preview(pipeline_id).await;
    Ok(store)
}
