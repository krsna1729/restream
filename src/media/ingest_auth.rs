//! Protocol-facing ingest authentication contract.
//!
//! RTMP and SRT protocol loops need only to resolve an accepted stream key to a
//! pipeline identifier. Application code owns the backing catalog lookup and
//! rate-limit policy behind this small runtime contract.

use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineAccessMode {
    RtmpPublish,
    RtmpPlay,
    SrtPublish,
    SrtRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPipeline {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineAccessError {
    InvalidStreamKey,
    LookupFailed(String),
}

pub type PipelineAccessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AuthenticatedPipeline, PipelineAccessError>> + Send + 'a>>;

pub trait PipelineAccessAuthenticator: Send + Sync {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        client_ip: &'a str,
    ) -> PipelineAccessFuture<'a>;
}
