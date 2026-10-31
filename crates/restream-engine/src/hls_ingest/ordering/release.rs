//! What the ordering hands the publisher: one segment, in order, with the
//! init and timestamp re-anchoring it needs.

use std::time::Duration;

use super::Payload;

/// An init object delivered with the release that needs it.
pub(in crate::hls_ingest) struct Init {
    pub(in crate::hls_ingest) uri: String,
    pub(in crate::hls_ingest) payload: Payload,
}

/// Why a release re-anchors; the publisher decides whose offset it may adopt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hls_ingest) enum Reanchor {
    None,
    /// The same clock (a variant switch, a changed `EXT-X-MAP` init).
    Clock,
    /// A break every rendition crosses (`EXT-X-DISCONTINUITY`, new epoch).
    Timeline(u64),
}

pub(in crate::hls_ingest) struct Release {
    /// The followed rendition this segment belongs to (see `MAX_RENDITIONS`):
    /// each has its own demuxer.
    pub(in crate::hls_ingest) rendition: usize,
    pub(in crate::hls_ingest) sequence: u64,
    pub(in crate::hls_ingest) epoch: u64,
    /// Whether the bytes re-anchor this rendition's timestamps, and why.
    pub(in crate::hls_ingest) reanchor: Reanchor,
    /// A segment of this rendition was skipped since the previous release:
    /// this one's frames up to its first keyframe may reference media that
    /// never arrives.
    pub(in crate::hls_ingest) after_gap: bool,
    /// The playlist's `EXTINF` for this segment.
    pub(in crate::hls_ingest) duration: Duration,
    /// The `EXT-X-MAP` init to apply before this segment.
    pub(in crate::hls_ingest) init: Option<Init>,
    pub(in crate::hls_ingest) payload: Payload,
}

impl Release {
    /// The media time this release adds: its `EXTINF`, or none for a
    /// separate audio rendition's segment, which covers time the variant's
    /// segments already count.
    pub(in crate::hls_ingest) fn media(&self) -> Duration {
        if self.rendition == 0 {
            self.duration
        } else {
            Duration::ZERO
        }
    }
}
