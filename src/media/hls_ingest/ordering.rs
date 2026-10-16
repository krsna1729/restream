//! Segment ordering for one HLS push session. Pure state: objects and time
//! go in, ordered releases come out; no I/O, no clock reads.
//!
//! Encoders upload a segment and only then the playlist that lists it, so a
//! segment usually arrives before its sequence number is known. It is held
//! (bounded in count, bytes and age) until a playlist of the followed variant
//! names it; segments then leave in `EXT-X-MEDIA-SEQUENCE` order, each
//! (epoch, sequence) at most once.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::OwnedSemaphorePermit;

use super::playlist::{MasterPlaylist, MediaPlaylist, Playlist};

/// Production idle floor: also the hold age and idle timeout before any
/// playlist names a target duration.
pub(super) const IDLE_FLOOR: Duration = Duration::from_secs(10);
/// Other variants whose listings are remembered to drop their segments.
const MAX_OTHER_VARIANTS: usize = 16;

/// Segment bytes and the share of the global ingest budget they occupy; the
/// budget is returned when the payload drops (released, evicted, discarded).
pub(super) struct Payload {
    pub(super) bytes: Bytes,
    _budget: Option<OwnedSemaphorePermit>,
}

impl Payload {
    pub(super) fn new(bytes: Bytes, budget: Option<OwnedSemaphorePermit>) -> Self {
        Self {
            bytes,
            _budget: budget,
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionLimits {
    pub(super) hold_segments: usize,
    pub(super) hold_bytes: usize,
    pub(super) idle_floor: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SegmentOutcome {
    /// Not listed yet: held until a playlist names it.
    Held,
    /// A listed sequence was waiting for these bytes.
    Attached,
    /// Its sequence already has bytes.
    Duplicate,
    /// Listed by a variant this session does not follow.
    OtherVariant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistOutcome {
    Accepted,
    OtherVariant,
}

pub(super) struct Release {
    pub(super) sequence: u64,
    pub(super) epoch: u64,
    /// The bytes start an unrelated TS stream (EXT-X-DISCONTINUITY, a new
    /// epoch, or a variant switch).
    pub(super) discontinuity: bool,
    pub(super) payload: Payload,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Counters {
    pub(super) skipped: u64,
    pub(super) duplicates: u64,
    pub(super) evicted: u64,
    pub(super) epochs: u64,
}

enum Lock {
    Unlocked,
    Media(String),
    Master(String),
}

impl Lock {
    fn follows(&self, playlist_path: &str) -> Option<bool> {
        match self {
            Self::Unlocked => None,
            Self::Media(path) | Self::Master(path) => Some(path == playlist_path),
        }
    }
}

struct Listed {
    path: String,
    discontinuity: bool,
    listed_at: Instant,
    payload: Option<Payload>,
}

struct Held {
    path: String,
    payload: Payload,
    at: Instant,
}

/// The shape of the last followed playlist, to recognise a restarted encoder.
struct Window {
    media_sequence: u64,
    last: u64,
    by_sequence: BTreeMap<u64, String>,
}

pub(super) struct Session {
    limits: SessionLimits,
    lock: Lock,
    target: Option<Duration>,
    /// Followed-variant sequences at or after `next_seq`; nothing below.
    listed: BTreeMap<u64, Listed>,
    window: Option<Window>,
    /// Path to sequence in the last followed playlist.
    latest_listing: HashMap<String, u64>,
    other_variants: HashMap<String, HashSet<String>>,
    held: VecDeque<Held>,
    held_bytes: usize,
    next_seq: Option<u64>,
    epoch: u64,
    discontinuity_pending: bool,
    ended: bool,
    counters: Counters,
}

impl Session {
    pub(super) fn new(limits: SessionLimits) -> Self {
        Self {
            limits,
            lock: Lock::Unlocked,
            target: None,
            listed: BTreeMap::new(),
            window: None,
            latest_listing: HashMap::new(),
            other_variants: HashMap::new(),
            held: VecDeque::new(),
            held_bytes: 0,
            next_seq: None,
            epoch: 0,
            discontinuity_pending: false,
            ended: false,
            counters: Counters::default(),
        }
    }

    pub(super) fn counters(&self) -> Counters {
        self.counters
    }

    #[cfg(test)]
    pub(super) fn held_segments(&self) -> usize {
        self.held.len()
    }

    #[cfg(test)]
    pub(super) fn held_bytes(&self) -> usize {
        self.held_bytes
    }

    /// No object for this long ends the session; it is also the hold age.
    pub(super) fn idle_timeout(&self) -> Duration {
        let floor = self.limits.idle_floor;
        self.target
            .map_or(floor, |target| target.saturating_mul(3).max(floor))
    }

    /// `EXT-X-ENDLIST` was seen and every listed segment has left.
    pub(super) fn finished(&self) -> bool {
        self.ended && self.listed.is_empty()
    }

    pub(super) fn on_segment(
        &mut self,
        path: String,
        payload: Payload,
        now: Instant,
    ) -> SegmentOutcome {
        if let Some(&sequence) = self.latest_listing.get(&path)
            && let Some(listed) = self.listed.get_mut(&sequence)
            && listed.path == path
        {
            if listed.payload.is_some() {
                self.counters.duplicates += 1;
                return SegmentOutcome::Duplicate;
            }
            listed.payload = Some(payload);
            return SegmentOutcome::Attached;
        }
        if !self.latest_listing.contains_key(&path)
            && self
                .other_variants
                .values()
                .any(|paths| paths.contains(&path))
        {
            return SegmentOutcome::OtherVariant;
        }
        // Either never listed, or listed below `next_seq`: a re-upload of a
        // released sequence or the first segment of a restarted encoder. The
        // next playlist tells them apart, so hold it; the newest upload wins.
        self.remove_held(&path);
        self.held_bytes += payload.len();
        self.held.push_back(Held {
            path,
            payload,
            at: now,
        });
        while self.held.len() > self.limits.hold_segments
            || self.held_bytes > self.limits.hold_bytes
        {
            self.pop_oldest_held();
            self.counters.evicted += 1;
        }
        SegmentOutcome::Held
    }

    pub(super) fn on_playlist(
        &mut self,
        playlist_path: &str,
        playlist: Playlist,
        now: Instant,
    ) -> PlaylistOutcome {
        match playlist {
            Playlist::Master(master) => {
                self.on_master(&master);
                PlaylistOutcome::Accepted
            }
            Playlist::Media(media) => match self.lock.follows(playlist_path) {
                Some(false) => {
                    self.on_other_variant(playlist_path, &media);
                    PlaylistOutcome::OtherVariant
                }
                follows => {
                    if follows.is_none() {
                        self.lock = Lock::Media(playlist_path.to_string());
                    }
                    self.on_followed(media, now);
                    PlaylistOutcome::Accepted
                }
            },
        }
    }

    fn on_master(&mut self, master: &MasterPlaylist) {
        let Some(chosen) = master.highest_bandwidth() else {
            return;
        };
        let follows = self.lock.follows(&chosen.path);
        self.lock = Lock::Master(chosen.path.clone());
        self.other_variants.remove(&chosen.path);
        if follows != Some(false) {
            return;
        }
        // Switch variants: the old one's listing is void. Held segments stay:
        // the new variant's are attached when its playlist arrives; the old
        // one's are purged by its next playlist or age out.
        self.listed.clear();
        self.window = None;
        self.latest_listing.clear();
        self.next_seq = None;
        self.ended = false;
        self.discontinuity_pending = true;
    }

    fn on_other_variant(&mut self, playlist_path: &str, media: &MediaPlaylist) {
        let paths: HashSet<String> = media
            .segments
            .iter()
            .map(|segment| segment.path.clone())
            .collect();
        for path in &paths {
            self.remove_held(path);
        }
        if self.other_variants.contains_key(playlist_path)
            || self.other_variants.len() < MAX_OTHER_VARIANTS
        {
            self.other_variants.insert(playlist_path.to_string(), paths);
        }
    }

    fn starts_new_epoch(&self, media: &MediaPlaylist, last: u64) -> bool {
        let Some(window) = &self.window else {
            return false;
        };
        media.media_sequence < window.media_sequence
            || last < window.last
            || media.segments.iter().any(|segment| {
                window
                    .by_sequence
                    .get(&segment.sequence)
                    .is_some_and(|path| *path != segment.path)
            })
    }

    fn on_followed(&mut self, media: MediaPlaylist, now: Instant) {
        self.target = Some(media.target_duration);
        let (Some(first), Some(last)) = (media.segments.first(), media.segments.last()) else {
            // An empty listing says nothing about order or epochs.
            self.ended |= media.ended;
            return;
        };
        let (first, last) = (first.sequence, last.sequence);
        if self.starts_new_epoch(&media, last) {
            // Order matters: forget the old epoch's listing and move
            // `next_seq` to the new playlist's lowest sequence BEFORE the
            // merge, so the same-epoch dedup below cannot discard the
            // restarted encoder's sequence-0 segment, which is held now and
            // is exactly what this epoch must release.
            self.listed.clear();
            self.latest_listing.clear();
            self.next_seq = Some(first);
            self.epoch += 1;
            self.counters.epochs += 1;
            self.discontinuity_pending = true;
            self.ended = false;
        }
        if self.next_seq.is_none() {
            self.next_seq = Some(first);
        }

        let mut latest_listing = HashMap::with_capacity(media.segments.len());
        let mut by_sequence = BTreeMap::new();
        for segment in media.segments {
            latest_listing.insert(segment.path.clone(), segment.sequence);
            by_sequence.insert(segment.sequence, segment.path.clone());
            if self.next_seq.is_some_and(|next| segment.sequence < next) {
                // Same epoch, already released or skipped: a held copy is a
                // duplicate upload.
                if self.remove_held(&segment.path) {
                    self.counters.duplicates += 1;
                }
                continue;
            }
            if let Some(listed) = self.listed.get(&segment.sequence) {
                if listed.payload.is_none()
                    && listed.path == segment.path
                    && let Some(payload) = self.take_held(&segment.path)
                    && let Some(listed) = self.listed.get_mut(&segment.sequence)
                {
                    listed.payload = Some(payload);
                }
                continue;
            }
            let payload = self.take_held(&segment.path);
            self.listed.insert(
                segment.sequence,
                Listed {
                    path: segment.path,
                    discontinuity: segment.discontinuity,
                    listed_at: now,
                    payload,
                },
            );
        }
        self.window = Some(Window {
            media_sequence: media.media_sequence,
            last,
            by_sequence,
        });
        self.latest_listing = latest_listing;
        self.ended |= media.ended;
    }

    /// Move every segment that may leave now into `out`, in order; skip a
    /// gap that has waited a target duration while later bytes are ready.
    pub(super) fn poll(&mut self, now: Instant, out: &mut Vec<Release>) {
        while let Some(next) = self.next_seq
            && let Some((&sequence, head)) = self.listed.first_key_value()
        {
            if sequence > next {
                // The window slid past sequences that were never listed here.
                self.counters.skipped += sequence - next;
                self.next_seq = Some(sequence);
                continue;
            }
            if head.payload.is_none() && !self.gap_expired(head, now) {
                break;
            }
            let Some(listed) = self.listed.remove(&sequence) else {
                break;
            };
            match listed.payload {
                Some(payload) => {
                    out.push(Release {
                        sequence,
                        epoch: self.epoch,
                        discontinuity: listed.discontinuity || self.discontinuity_pending,
                        payload,
                    });
                    self.discontinuity_pending = false;
                }
                None => self.counters.skipped += 1,
            }
            self.next_seq = sequence.checked_add(1);
        }
        let hold_age = self.idle_timeout();
        while self
            .held
            .front()
            .is_some_and(|held| now.saturating_duration_since(held.at) >= hold_age)
        {
            self.pop_oldest_held();
            self.counters.evicted += 1;
        }
    }

    /// When `poll` may next make progress without a new object.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let gap = self.listed.first_key_value().and_then(|(_, head)| {
            (head.payload.is_none() && self.later_bytes_ready())
                .then(|| self.target.map(|target| head.listed_at + target))
                .flatten()
        });
        let hold = self.held.front().map(|held| held.at + self.idle_timeout());
        match (gap, hold) {
            (Some(gap), Some(hold)) => Some(gap.min(hold)),
            (gap, hold) => gap.or(hold),
        }
    }

    fn gap_expired(&self, head: &Listed, now: Instant) -> bool {
        self.target
            .is_some_and(|target| now.saturating_duration_since(head.listed_at) >= target)
            && self.later_bytes_ready()
    }

    fn later_bytes_ready(&self) -> bool {
        self.listed
            .values()
            .skip(1)
            .any(|listed| listed.payload.is_some())
    }

    fn take_held(&mut self, path: &str) -> Option<Payload> {
        let index = self.held.iter().position(|held| held.path == path)?;
        let held = self.held.remove(index)?;
        self.held_bytes -= held.payload.len();
        Some(held.payload)
    }

    fn remove_held(&mut self, path: &str) -> bool {
        self.take_held(path).is_some()
    }

    fn pop_oldest_held(&mut self) {
        if let Some(held) = self.held.pop_front() {
            self.held_bytes -= held.payload.len();
        }
    }
}

#[cfg(test)]
#[path = "ordering_tests.rs"]
mod tests;
