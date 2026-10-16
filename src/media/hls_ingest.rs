//! HLS push ingest: encoders PUT or POST an HLS stream (playlists and
//! MPEG-TS or fMP4 segments) under `/ingest/hls/{stream_key}/...`, as they
//! do to YouTube HLS ingest or Akamai MSL.
//!
//! The HTTP handler only authorizes the key and reads the bounded body; it
//! hands each object to the stream key's session task on the MEDIA executor,
//! which parses playlists, orders segments (`ordering`) and publishes them
//! through the shared ingest publisher.

pub mod playlist;

mod ordering;
mod pacing;
mod registry;
mod session;
#[cfg(test)]
pub(crate) mod test_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tokio::sync::{OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tracing::warn;

pub use registry::{AuthorizeError, DispatchError, HlsIngest};

/// Every bound on what HLS push ingest buffers. `Default` is production;
/// tests inject smaller values through `HlsIngest::new`.
#[derive(Debug, Clone, Copy)]
pub struct HlsIngestLimits {
    pub max_playlist_bytes: usize,
    /// 64 MiB: a 10 s segment at 51 Mbps, YouTube's top 4K HLS bitrate.
    pub max_segment_bytes: usize,
    /// 4 MiB: an fMP4 init segment (`ftyp` + `moov`) is a few KiB; its own
    /// bound keeps it off the segment budget.
    pub max_init_bytes: usize,
    /// 8 MiB: every stream key's last applied init, remembered across
    /// sessions. A normal init is about 1 KiB, so this is thousands of them;
    /// the keys are bounded too, and the oldest is evicted first.
    pub init_cache_bytes: usize,
    /// Uploads one stream key may have in flight (read, queued, awaiting
    /// reply); encoders upload sequentially, so 2 only absorbs overlap.
    pub uploads_per_key: usize,
    /// Segments held before a playlist lists them; normally one per variant.
    pub hold_segments: usize,
    /// Two maximum-size segments.
    pub hold_bytes: usize,
    /// All HLS ingest bytes in flight, queued, held or publishing, across
    /// every stream key: >100 keys of typical 2 MB segments, and far below
    /// the host memory media work needs. Exhausted, an upload gets `503`,
    /// which FFmpeg never reads: that segment is lost.
    pub total_buffer_bytes: usize,
    /// One stream key's share for segments (a quarter of the total). It
    /// bounds starvation without preventing it: four keys at their share
    /// fill the total, and other keys' segments then get `503` (four 4K
    /// publishers with 10 s segments reach that legitimately). A segment
    /// waits for its share: the encoder's blocked write is backpressure it
    /// respects.
    pub key_buffer_bytes: usize,
    /// Idle timeout floor, the hold age before any playlist, the time each
    /// request body frame and an upload slot may take (`408` after it), and
    /// a quarter of the whole-body deadline before any playlist.
    pub idle_floor: Duration,
}

impl Default for HlsIngestLimits {
    fn default() -> Self {
        Self {
            max_playlist_bytes: 1024 * 1024,
            max_segment_bytes: 64 * 1024 * 1024,
            max_init_bytes: 4 * 1024 * 1024,
            init_cache_bytes: 8 * 1024 * 1024,
            uploads_per_key: 2,
            hold_segments: 8,
            hold_bytes: 128 * 1024 * 1024,
            total_buffer_bytes: 512 * 1024 * 1024,
            key_buffer_bytes: 128 * 1024 * 1024,
            idle_floor: ordering::IDLE_FLOOR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Playlist,
    /// An MPEG-TS segment.
    TsSegment,
    /// An fMP4 object: an init segment or a media segment. Which one is
    /// known from the playlist's `EXT-X-MAP` and the object's boxes, not
    /// from its extension.
    Fmp4,
}

impl ObjectKind {
    /// The object an upload path names: `.m3u8`, `.ts`, or `.mp4`/`.m4s`
    /// (encoders name inits `.mp4` and media segments either way).
    pub fn of(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1;
        if extension.eq_ignore_ascii_case("m3u8") {
            Some(Self::Playlist)
        } else if extension.eq_ignore_ascii_case("ts") {
            Some(Self::TsSegment)
        } else if extension.eq_ignore_ascii_case("mp4") || extension.eq_ignore_ascii_case("m4s") {
            Some(Self::Fmp4)
        } else {
            None
        }
    }

    /// An fMP4 object's role is unknown until its body is read, so it gets
    /// the segment bound; an init is held to `max_init_bytes` afterwards.
    pub fn max_bytes(self, limits: &HlsIngestLimits) -> usize {
        match self {
            Self::Playlist => limits.max_playlist_bytes,
            Self::TsSegment | Self::Fmp4 => limits.max_segment_bytes,
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
    /// The playlist or fMP4 object does not parse; the session continues.
    Malformed(String),
    /// The object's codec or container is not ingested (a mixed TS/fMP4
    /// stream, an HEVC fMP4 init).
    Unsupported(String),
    /// An fMP4 init over its bound.
    TooLarge(String),
    /// The session ended with the object still queued; upload it again.
    Retry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyError {
    TooLarge,
    /// The global ingest buffer budget is exhausted.
    BufferFull,
}

/// Bytes a stream key has outstanding (read, queued, held or publishing);
/// counted for observability, never enforced.
pub(crate) struct KeyCharge {
    outstanding: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for KeyCharge {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Claim step for a segment body sent without `Content-Length`.
const SHARE_STEP: usize = 4 * 1024 * 1024;

/// A segment's claim on its key's buffer share. Claims are made under the
/// key's lock, and a chunked body keeps the lock until it is complete, so
/// two bodies of one key never each hold a fragment and wait for more.
pub(crate) struct SegmentShare {
    pub(crate) share: Arc<Semaphore>,
    pub(crate) lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) growing: Option<OwnedMutexGuard<()>>,
}

/// One object's body, read frame by frame against its size bound. A
/// segment claims its key's share before its bytes are read (awaited, so
/// the encoder's write blocks instead of failing; the caller bounds the
/// wait with the whole-body deadline). Every frame then takes the global
/// budget, never awaited (`BufferFull` drops the body and its claim). A
/// playlist, which is small and is what releases held segments, takes only
/// the global budget.
pub struct ObjectBody {
    budget: Arc<Semaphore>,
    /// `Some` for segments.
    segment: Option<SegmentShare>,
    max_bytes: usize,
    bytes: BytesMut,
    claimed: usize,
    permit: Option<OwnedSemaphorePermit>,
    share_permit: Option<OwnedSemaphorePermit>,
    input_id: String,
    fair_share: usize,
    charge: KeyCharge,
}

impl ObjectBody {
    /// Claim a segment's declared length from its key's share up front, in
    /// one acquire under the key's claim lock.
    pub(crate) async fn claim_declared(&mut self, declared: usize) -> Result<(), BodyError> {
        let Some(segment) = &self.segment else {
            return Ok(());
        };
        if declared == 0 {
            return Ok(());
        }
        let _claiming = segment.lock.clone().lock_owned().await;
        self.claim_share(declared).await
    }

    pub async fn push(&mut self, frame: &[u8]) -> Result<(), BodyError> {
        if frame.is_empty() {
            return Ok(());
        }
        let length = self.bytes.len().saturating_add(frame.len());
        if length > self.max_bytes {
            return Err(BodyError::TooLarge);
        }
        if let Some(segment) = &mut self.segment
            && length > self.claimed
        {
            if segment.growing.is_none() {
                segment.growing = Some(segment.lock.clone().lock_owned().await);
            }
            let step = (length - self.claimed)
                .max(SHARE_STEP)
                .min(self.max_bytes - self.claimed);
            self.claim_share(step).await?;
        }
        let permits = u32::try_from(frame.len()).map_err(|_| BodyError::TooLarge)?;
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(permits)
            .map_err(|_| BodyError::BufferFull)?;
        merge(&mut self.permit, permit);
        self.bytes.extend_from_slice(frame);
        self.charge.bytes += frame.len();
        let before = self
            .charge
            .outstanding
            .fetch_add(frame.len(), Ordering::Relaxed);
        if before <= self.fair_share && before + frame.len() > self.fair_share {
            warn!(
                input = %self.input_id,
                outstanding = before + frame.len(),
                fair_share = self.fair_share,
                "hls ingest key holds more than its fair share of the buffer"
            );
        }
        Ok(())
    }

    async fn claim_share(&mut self, bytes: usize) -> Result<(), BodyError> {
        let Some(segment) = &self.segment else {
            return Ok(());
        };
        let permits = u32::try_from(bytes).map_err(|_| BodyError::TooLarge)?;
        let permit = segment
            .share
            .clone()
            .acquire_many_owned(permits)
            .await
            .map_err(|_| BodyError::BufferFull)?;
        merge(&mut self.share_permit, permit);
        self.claimed += bytes;
        Ok(())
    }

    fn into_payload(mut self) -> ordering::Payload {
        // Return what a claim took beyond the body's real length.
        let excess = self.claimed.saturating_sub(self.bytes.len());
        if excess > 0
            && let Some(permit) = &mut self.share_permit
        {
            drop(permit.split(excess));
        }
        ordering::Payload::new(
            Bytes::from(self.bytes),
            self.permit,
            self.share_permit,
            Some(self.charge),
        )
    }
}

fn merge(held: &mut Option<OwnedSemaphorePermit>, permit: OwnedSemaphorePermit) {
    match held {
        Some(held) => held.merge(permit),
        None => *held = Some(permit),
    }
}
