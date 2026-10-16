//! Test doubles for HLS push ingest: one accepted stream key, a real engine.

use std::sync::Arc;

use super::{HlsIngest, HlsIngestLimits};
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use crate::media::security::IngestSecurityService;

pub(crate) const STREAM_KEY: &str = "hls-key";
pub(crate) const PIPELINE_ID: &str = "pipe-hls";
pub(crate) const INPUT_ID: &str = "input-hls";

/// Accepts only [`STREAM_KEY`], for HLS publish only.
pub(crate) struct OneKey;

impl PipelineAccessAuthenticator for OneKey {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            if mode == PipelineAccessMode::HlsPublish && stream_key == STREAM_KEY {
                Ok(AuthenticatedPipeline {
                    id: PIPELINE_ID.to_string(),
                    input_id: INPUT_ID.to_string(),
                    selected: true,
                })
            } else {
                Err(PipelineAccessError::InvalidStreamKey)
            }
        })
    }
}

pub(crate) fn test_ingest(limits: HlsIngestLimits) -> (Arc<MediaEngine>, Arc<HlsIngest>) {
    let engine = Arc::new(MediaEngine::new());
    let ingest = HlsIngest::new(
        engine.clone(),
        Arc::new(OneKey),
        Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
        limits,
    );
    (engine, ingest)
}

pub(crate) fn authenticated() -> AuthenticatedPipeline {
    AuthenticatedPipeline {
        id: PIPELINE_ID.to_string(),
        input_id: INPUT_ID.to_string(),
        selected: true,
    }
}
