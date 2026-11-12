//! Protocol-facing ingest authentication contract.
//!
//! RTMP, SRT, HLS push and MoQ protocol loops need only to resolve an accepted
//! stream key to a pipeline identifier. Application code owns the catalog and
//! rate-limit policy behind this small runtime contract. The production
//! implementation answers from an in-memory index and never waits on a
//! dependency, so it has no lookup-failure outcome; the future shape lets the
//! protocol loops keep their shutdown and deadline guards around it.

use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineAccessMode {
    RtmpPublish,
    RtmpPlay,
    SrtPublish,
    SrtRead,
    HlsPublish,
    MoqPublish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPipeline {
    pub id: String,
    pub input_id: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineAccessError {
    /// Unknown or disabled key, or a push to an input that takes none.
    InvalidStreamKey,
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
