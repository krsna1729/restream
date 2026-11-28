//! In-memory HLS segmenter — muxes to MPEG-TS via in-house `TsMuxer`, splits on
//! keyframe boundaries, and stores segments in [`HlsStore`]. No disk I/O, no
//! FFmpeg, no OS threads on the hot path. Segments are served from memory by
//! the Axum API.
//!
//! # Window
//!
//! MPEG-TS live/upload storage evicts when the deque exceeds `HlsConfig.max_segments`
//! (default `MAX_SEGMENTS`, 20) and drops associated `variant_segments` entries
//! for the evicted index. `EXT-X-TARGETDURATION` starts at 6s
//! (`TARGET_DURATION_SECS`) and rises to the **ceiling of** the largest segment
//! duration observed since create/clear (a 7.2s segment yields
//! `EXT-X-TARGETDURATION:8`); eviction does not recompute it. fMP4 preview
//! storage is a separate retention policy: see `hls/fmp4`.
//!
//! # Segment Lifecycle
//!
//! ```text
//! RingBuffer → TsMuxer (inline) → segment accumulator → HlsStore
//! ```

#[forbid(unsafe_code)]
pub mod fmp4;
mod segmenter;
mod store;
#[forbid(unsafe_code)]
pub(crate) mod upload_policy;

use std::sync::Arc;

use crate::events::EventLog;
use crate::ingest_registry::IngestRegistry;
use crate::stage_registry::StageRegistry;
use restream_core::metadata::VideoMeta;
use restream_domain::domain::stage::StageKey;

pub(crate) use segmenter::start_hls_segmenter;
pub use store::{HlsPublished, HlsSegmentSnapshot, HlsSegmentVariant, HlsStore, HlsStoreSnapshot};

const MIN_SEGMENT_SECS: f64 = 1.0;
const SEGMENT_CAPACITY: usize = 8 * 1024 * 1024;
// Keep a longer live window so preview clients can still fetch segments that are
// still referenced by the playlist while the stream is moving forward.
const MAX_SEGMENTS: usize = 20;

#[derive(Debug, Clone, Default)]
pub struct HlsSegmenterStart {
    pub video_meta_override: Option<VideoMeta>,
    pub planned_stage_key: Option<StageKey>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HlsConfig {
    pub min_segment_secs: f64,
    pub segment_capacity: usize,
    pub max_segments: usize,
}

impl Default for HlsConfig {
    fn default() -> Self {
        Self {
            min_segment_secs: MIN_SEGMENT_SECS,
            segment_capacity: SEGMENT_CAPACITY,
            max_segments: MAX_SEGMENTS,
        }
    }
}

impl HlsConfig {
    pub fn from_engine_config(config: &crate::config::EngineConfig) -> Self {
        Self {
            min_segment_secs: config.hls_min_segment_ms,
            segment_capacity: config.hls_segment_capacity_bytes,
            max_segments: config.hls_max_segments,
        }
    }
}

/// What a segmenter reads and reports to: the ingest registry (stream
/// metadata and sequence headers), the stage registry (its stage runtime)
/// and the event log. The engine layer builds it from its own registries.
pub(crate) struct HlsSegmenterContext {
    ingests: Arc<IngestRegistry>,
    stages: Arc<StageRegistry>,
    events: Arc<EventLog>,
}

impl HlsSegmenterContext {
    pub(crate) fn new(
        ingests: Arc<IngestRegistry>,
        stages: Arc<StageRegistry>,
        events: Arc<EventLog>,
    ) -> Self {
        Self {
            ingests,
            stages,
            events,
        }
    }
}

#[cfg(test)]
mod tests;
