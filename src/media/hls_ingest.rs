//! HLS push ingest: encoders PUT or POST an MPEG-TS HLS stream (playlists
//! and segments) under `/ingest/hls/{stream_key}/...`, as they do to YouTube
//! HLS ingest or Akamai MSL.
//!
//! The HTTP handler only authorizes the key and reads the bounded body; it
//! hands each object to the stream key's session task on the MEDIA executor,
//! which parses playlists, orders segments (`ordering`) and publishes them
//! through the shared TS publisher.

pub mod playlist;

mod ordering;
mod registry;
mod session;
#[cfg(test)]
pub(crate) mod test_support;

use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub use registry::{AuthorizeError, DispatchError, HlsIngest};

/// Every bound on what HLS push ingest buffers. `Default` is production;
/// tests inject smaller values through `HlsIngest::new`.
#[derive(Debug, Clone, Copy)]
pub struct HlsIngestLimits {
    pub max_playlist_bytes: usize,
    /// 64 MiB: a 10 s segment at 51 Mbps, YouTube's top 4K HLS bitrate.
    pub max_segment_bytes: usize,
    /// Uploads one stream key may have in flight (read, queued, awaiting
    /// reply); encoders upload sequentially, so 2 only absorbs overlap.
    pub uploads_per_key: usize,
    /// Segments held before a playlist lists them; normally one per variant.
    pub hold_segments: usize,
    /// Two maximum-size segments.
    pub hold_bytes: usize,
    /// All HLS ingest bytes in flight, queued, held or publishing, across
    /// every stream key: >100 keys of typical 2 MB segments, or 2 keys at
    /// their worst case, and far below the host memory media work needs.
    pub total_buffer_bytes: usize,
    /// Idle timeout floor (and the hold age before any playlist).
    pub idle_floor: Duration,
}

impl Default for HlsIngestLimits {
    fn default() -> Self {
        Self {
            max_playlist_bytes: 1024 * 1024,
            max_segment_bytes: 64 * 1024 * 1024,
            uploads_per_key: 2,
            hold_segments: 8,
            hold_bytes: 128 * 1024 * 1024,
            total_buffer_bytes: 512 * 1024 * 1024,
            idle_floor: ordering::IDLE_FLOOR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Playlist,
    Segment,
}

impl ObjectKind {
    /// The object an upload path names; fMP4 segments are not ingested yet.
    pub fn of(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1;
        if extension.eq_ignore_ascii_case("m3u8") {
            Some(Self::Playlist)
        } else if extension.eq_ignore_ascii_case("ts") {
            Some(Self::Segment)
        } else {
            None
        }
    }

    pub fn max_bytes(self, limits: &HlsIngestLimits) -> usize {
        match self {
            Self::Playlist => limits.max_playlist_bytes,
            Self::Segment => limits.max_segment_bytes,
        }
    }
}

/// What a session did with an accepted object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectOutcome {
    /// A playlist was applied, or a segment completed a listed sequence.
    Stored,
    /// The segment waits for a playlist to list it.
    Held,
    /// A duplicate, or an object of a variant this session does not follow.
    Dropped,
    /// The playlist does not parse; the session continues.
    Malformed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    TooLarge,
    /// The global ingest buffer budget is exhausted.
    BufferFull,
}

/// One object's body, read frame by frame against its size bound and the
/// global budget; never waits for budget, so partial readers cannot
/// deadlock each other.
pub struct ObjectBody {
    budget: Arc<Semaphore>,
    max_bytes: usize,
    bytes: BytesMut,
    permit: Option<OwnedSemaphorePermit>,
}

impl ObjectBody {
    pub fn push(&mut self, frame: &[u8]) -> Result<(), BodyError> {
        if frame.is_empty() {
            return Ok(());
        }
        if self.bytes.len().saturating_add(frame.len()) > self.max_bytes {
            return Err(BodyError::TooLarge);
        }
        let permits = u32::try_from(frame.len()).map_err(|_| BodyError::TooLarge)?;
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(permits)
            .map_err(|_| BodyError::BufferFull)?;
        match &mut self.permit {
            Some(held) => held.merge(permit),
            None => self.permit = Some(permit),
        }
        self.bytes.extend_from_slice(frame);
        Ok(())
    }

    fn into_payload(self) -> ordering::Payload {
        ordering::Payload::new(Bytes::from(self.bytes), self.permit)
    }
}
