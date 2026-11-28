//! In-memory HLS preview packager for fragmented MP4 renditions.
//!
//! Retention is not the MPEG-TS `HlsStore` algorithm. Each fMP4 rendition
//! keeps `max_segments + PLAYLIST_RETENTION_GRACE_SEGMENTS` (grace is 6) so a
//! player fetching the oldest advertised segment is not racing immediate
//! eviction; playlists advertise only the newest `max_segments`. Target
//! duration starts from the first media segment and is a nondecreasing
//! high-water mark of `duration.ceil()` until clear; eviction does not shrink it.
//!
//! The served preview path intentionally diverges from remote HLS PUT uploads:
//! preview uses native fMP4 so we can expose one muxer per HLS rendition
//! (video plus alternate audio playlists), while upload keeps MPEG-TS because
//! common ingest targets such as YouTube HLS PUT require `.ts` media segments.
//!
//! The rendition split is also deliberate for the muxer surface we use here:
//! `shiguredo_mp4::mux::Fmp4SegmentMuxer` currently handles one audio track and
//! one video track per muxer. HLS alternate audio already models separate
//! audio-only playlists, so one muxer per rendition keeps the code small and
//! matches the HLS presentation model instead of forcing many audio tracks into
//! one fragmented MP4 stream.

mod codec;
mod rendition;
mod segmenter;
mod store;

pub(crate) use segmenter::start_hls_fmp4_segmenter;
pub use store::Fmp4HlsStore;

pub fn parse_fmp4_segment_name(segment: &str) -> Option<u64> {
    segment
        .strip_prefix("seg")
        .and_then(|segment| segment.strip_suffix(".m4s"))
        .and_then(|segment| segment.parse::<u64>().ok())
}

#[cfg(test)]
mod tests;
