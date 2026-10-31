//! Segment ordering for one HLS push session. Pure state: objects and time
//! go in, ordered releases come out; no I/O, no clock reads.
//!
//! Encoders upload a segment and only then the playlist that lists it, so a
//! segment usually arrives before its sequence number is known. It is held
//! (bounded in count, bytes and age) until a playlist of a followed rendition
//! names it; segments then leave in `EXT-X-MEDIA-SEQUENCE` order, each
//! (rendition, epoch, sequence) at most once.
//!
//! A session follows one variant's media playlist and, when the variant's
//! audio is a separate `EXT-X-MEDIA` rendition, that rendition's playlist too:
//! each has its own sequence space, epochs and `EXT-X-MAP` inits, and they
//! share the hold and its bounds.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::OwnedSemaphorePermit;

use super::KeyCharge;
use super::playlist::{MasterPlaylist, MediaPlaylist, Playlist};

mod release;
pub(super) use release::{Init, Reanchor, Release};

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
    _share: Option<OwnedSemaphorePermit>,
    _charge: Option<KeyCharge>,
}

impl Payload {
    pub(super) fn new(
        bytes: Bytes,
        budget: Option<OwnedSemaphorePermit>,
        share: Option<OwnedSemaphorePermit>,
        charge: Option<KeyCharge>,
    ) -> Self {
        Self {
            bytes,
            _budget: budget,
            _share: share,
            _charge: charge,
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
    /// The object is the followed playlist's current `EXT-X-MAP` init: it is
    /// delivered ahead of the next segment, never released as one.
    Init,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaylistOutcome {
    Accepted,
    OtherVariant,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Counters {
    pub(super) skipped: u64,
    pub(super) duplicates: u64,
    pub(super) evicted: u64,
    pub(super) epochs: u64,
    /// Segments dropped because their bytes could not be demuxed.
    pub(super) failed: u64,
}

/// How many `EXT-X-MAP` inits one session tracks; a playlist that changes
/// its map more often than this keeps only the newest (oldest dropped).
const MAX_MAPS: usize = 4;

/// One `EXT-X-MAP` init of the followed playlist and its object.
struct MapState {
    uri: String,
    /// Init bytes uploaded for `uri`, not yet delivered with a release.
    pending: Option<Payload>,
    /// The init this map has: the hash of its bytes, delivered or already held
    /// by the demuxer when a re-upload or a second map URI made a delivery
    /// unnecessary. `init_ready` reads it, a return to this map re-delivers
    /// `last` behind it, and whether a delivery is *needed* is decided by
    /// `applied` — so record it before that decision, never after.
    delivered: Option<u64>,
    /// The payload this map last delivered, so a return to it can deliver its
    /// bytes again without a re-upload: the demuxer holds one init, so a stream
    /// that goes content map → ad map → content map must re-deliver the content
    /// map's init or its segments are demuxed with the ad map's tracks. The
    /// payload — not bare bytes — so the cache stays inside the byte budget.
    last: Option<Payload>,
}

/// The renditions one session follows: the chosen variant's media playlist
/// and, when its master names one, the variant's separate audio rendition.
pub(super) const MAX_RENDITIONS: usize = 2;

/// Which followed rendition a playlist path is, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follows {
    /// Nothing is followed yet: the first media playlist becomes rendition 0.
    Unlocked,
    Rendition(usize),
    /// Another variant, or a rendition the master does not select.
    Other,
}

struct Listed {
    path: String,
    discontinuity: bool,
    duration: Duration,
    listed_at: Instant,
    /// The `EXT-X-MAP` this segment was listed under.
    map_uri: Option<String>,
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

/// What every followed rendition shares: the bounds, the early-upload hold,
/// the counters and the inits seeded from an earlier session.
struct Pool {
    limits: SessionLimits,
    held: VecDeque<Held>,
    held_bytes: usize,
    counters: Counters,
    /// The inits the stream key applied last, from before this session, by
    /// map URI. Each is delivered to its rendition's demuxer like a fresh
    /// upload, because the demuxer starts empty.
    seeded: Vec<(String, Bytes)>,
}

impl Pool {
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

/// One followed media playlist: its own sequence space, epochs, maps and the
/// init its demuxer holds.
struct Track {
    /// The playlist path; `None` until a playlist or a master names it.
    path: Option<String>,
    target: Option<Duration>,
    /// Sequences at or after `next_seq`; nothing below.
    listed: BTreeMap<u64, Listed>,
    window: Option<Window>,
    /// Path to sequence in the last playlist.
    latest_listing: HashMap<String, u64>,
    next_seq: Option<u64>,
    epoch: u64,
    /// A new epoch counted but not yet carried by a release.
    timeline_pending: Option<u64>,
    clock_pending: bool,
    /// A segment was skipped since the last release.
    gap_pending: bool,
    generation: u64,
    ended: bool,
    /// The maps the playlist names, newest last (bounded).
    maps: Vec<MapState>,
    /// The playlist's own `EXT-X-MAP`: the map its newest segments use.
    current_map: Option<String>,
    /// A media playlist was applied, so its (missing) map is known.
    playlist_seen: bool,
    /// The hash of the init this rendition's demuxer currently holds. One
    /// init is held at a time, and the demuxer holds bytes rather than URIs,
    /// so this — not each map's own last delivery — decides whether a
    /// delivery is needed: the same bytes listed under a second map need
    /// none. A later, different init starts an unrelated timeline (a
    /// discontinuity).
    applied: Option<u64>,
}

impl Track {
    fn new(path: Option<String>) -> Self {
        Self {
            path,
            target: None,
            listed: BTreeMap::new(),
            window: None,
            latest_listing: HashMap::new(),
            next_seq: None,
            epoch: 0,
            timeline_pending: None,
            clock_pending: false,
            gap_pending: false,
            generation: 0,
            ended: false,
            maps: Vec::new(),
            current_map: None,
            playlist_seen: false,
            applied: None,
        }
    }

    fn knows_map(&self, uri: &str) -> bool {
        self.maps.iter().any(|map| map.uri == uri)
    }

    /// `EXT-X-ENDLIST` was seen and every listed segment has left.
    fn finished(&self) -> bool {
        self.ended && self.listed.is_empty()
    }

    /// Follow another playlist in this rendition's place: the old listing is
    /// void. Held segments stay: the new playlist's are attached when it
    /// arrives; the old one's are purged by its next playlist or age out.
    fn switch_to(&mut self, path: String) {
        self.path = Some(path);
        self.listed.clear();
        self.window = None;
        self.latest_listing.clear();
        self.next_seq = None;
        self.ended = false;
        self.clock_pending = true;
        // The new playlist names its own `EXT-X-MAP`, and its first fMP4
        // segment arrives before that playlist: the old map says nothing
        // about it.
        self.maps.clear();
        self.current_map = None;
        self.playlist_seen = false;
    }

    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    /// A skipped segment still crosses its break: carry the generation on.
    /// The next release follows a gap.
    fn skip_discontinuity(&mut self, discontinuity: bool) {
        self.gap_pending = true;
        if discontinuity && self.timeline_pending.is_none() {
            self.timeline_pending = Some(self.next_generation());
        }
    }

    /// A pending epoch or the segment's own discontinuity starts a
    /// generation; a pending switch or replaced init keeps the clock.
    fn take_reanchor(&mut self, discontinuity: bool, replaces_init: bool) -> Reanchor {
        let pending = self.timeline_pending.take();
        let clock = std::mem::take(&mut self.clock_pending) || replaces_init;
        match pending.or_else(|| discontinuity.then(|| self.next_generation())) {
            Some(generation) => Reanchor::Timeline(generation),
            None if clock => Reanchor::Clock,
            None => Reanchor::None,
        }
    }

    /// The playlist is now another rendition, demuxed by another demuxer that
    /// holds none of its inits: its current init is delivered again with its
    /// next segment.
    fn moved(&mut self) {
        self.applied = None;
    }

    /// Track `uri` as one of the playlist's maps, taking an init already
    /// uploaded for it from the hold (or from the seed).
    fn ensure_map(&mut self, uri: &str, pool: &mut Pool) {
        if self.knows_map(uri) {
            return;
        }
        let pending = pool.take_held(uri).or_else(|| {
            pool.seeded
                .iter()
                .find(|(seeded, _)| seeded == uri)
                .map(|(_, bytes)| Payload::new(bytes.clone(), None, None, None))
        });
        if self.maps.len() >= MAX_MAPS {
            self.maps.remove(0);
        }
        self.maps.push(MapState {
            uri: uri.to_string(),
            pending,
            delivered: None,
            last: None,
        });
    }

    /// A restart is recognised only when the new listing contradicts the
    /// last one. One case is not: the old window was exactly
    /// `[0: index0.ts]` and the restarted encoder lists the same single
    /// entry. That playlist is identical, so no epoch starts and the new
    /// `index0.ts` is discarded as a duplicate of released sequence 0; the
    /// next segment (`index1.ts`, sequence 1) continues normally. One
    /// segment is lost, never order.
    ///
    /// A stale playlist that arrives after a newer one (reordered uploads)
    /// moves the window back and so starts a spurious epoch: it costs one
    /// discontinuity and up to a target duration's stall while the next
    /// listing realigns, and never duplicates media (released sequences are
    /// gone from the listing).
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

    /// Whether a segment listed under `uri` may leave: it names no map, or
    /// that map's init is here (uploaded, seeded, or delivered already).
    fn init_ready(&self, uri: Option<&str>) -> bool {
        let Some(uri) = uri else {
            return true;
        };
        self.maps
            .iter()
            .find(|map| map.uri == uri)
            .is_some_and(|map| map.pending.is_some() || map.delivered.is_some())
    }

    /// The init to deliver with the next release, if it differs from the one
    /// already delivered for its map, and whether it replaces an earlier init
    /// (an unrelated timeline).
    fn take_init(&mut self, uri: Option<&str>) -> (Option<Init>, bool) {
        let Some(uri) = uri else {
            return (None, false);
        };
        let Some(map) = self.maps.iter_mut().find(|map| map.uri == uri) else {
            return (None, false);
        };
        // Fresh bytes for this map win; otherwise the bytes it last delivered,
        // so a return to an earlier map re-delivers its init instead of
        // leaving the other map's tracks in the demuxer.
        let (bytes, hash) = match map.pending.take() {
            Some(payload) => {
                let mut hasher = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&payload.bytes[..], &mut hasher);
                let hash = std::hash::Hasher::finish(&hasher);
                let bytes = payload.bytes.clone();
                // The map keeps the charged payload; the delivered handle below
                // shares its bytes without a second charge.
                map.last = Some(payload);
                (bytes, hash)
            }
            None => {
                let (Some(hash), Some(kept)) = (map.delivered, map.last.as_ref()) else {
                    return (None, false);
                };
                (kept.bytes.clone(), hash)
            }
        };
        // The map has an init now, whether or not it is delivered: record it
        // first, or a map whose bytes were already in the demuxer would be left
        // waiting for an init it never delivered, and every later segment under
        // it would be held.
        map.delivered = Some(hash);
        // The demuxer holds one init, and it holds bytes rather than URIs: if it
        // already holds exactly these bytes — a re-upload of the same init (an
        // encoder's `resend`), a return to the map it is on, or the same bytes
        // listed under another map URI — nothing is delivered, and nothing is a
        // discontinuity.
        if self.applied == Some(hash) {
            return (None, false);
        }
        let replaces = self.applied.replace(hash).is_some();
        (
            Some(Init {
                uri: uri.to_string(),
                payload: Payload::new(bytes, None, None, None),
            }),
            replaces,
        )
    }

    fn on_followed(&mut self, mut media: MediaPlaylist, now: Instant, pool: &mut Pool) {
        // Every map the playlist names is tracked (with the init already
        // uploaded for it); `current_map` is the one its newest segments use.
        self.playlist_seen = true;
        self.current_map = media.map_uri.take();
        if let Some(uri) = self.current_map.clone() {
            self.ensure_map(&uri, pool);
        }
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
            self.epoch = self.epoch.saturating_add(1);
            pool.counters.epochs = pool.counters.epochs.saturating_add(1);
            self.timeline_pending = Some(self.next_generation());
            self.clock_pending = false;
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
                if pool.remove_held(&segment.path) {
                    pool.counters.duplicates = pool.counters.duplicates.saturating_add(1);
                }
                continue;
            }
            if let Some(listed) = self.listed.get(&segment.sequence) {
                if listed.payload.is_none()
                    && listed.path == segment.path
                    && let Some(payload) = pool.take_held(&segment.path)
                    && let Some(listed) = self.listed.get_mut(&segment.sequence)
                {
                    listed.payload = Some(payload);
                }
                continue;
            }
            if let Some(uri) = &segment.map_uri
                && !self.knows_map(uri)
            {
                let uri = uri.clone();
                self.ensure_map(&uri, pool);
            }
            let payload = pool.take_held(&segment.path);
            self.listed.insert(
                segment.sequence,
                Listed {
                    path: segment.path,
                    discontinuity: segment.discontinuity,
                    duration: segment.duration,
                    listed_at: now,
                    map_uri: segment.map_uri,
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

    /// Move every segment of this rendition that may leave now into `out`, in
    /// order; skip a gap that has waited a target duration while later bytes
    /// are ready.
    fn poll(&mut self, rendition: usize, now: Instant, out: &mut Vec<Release>, pool: &mut Pool) {
        while let Some(next) = self.next_seq
            && let Some((&sequence, head)) = self.listed.first_key_value()
        {
            if sequence > next {
                // The window slid past sequences that were never listed here.
                pool.counters.skipped = pool.counters.skipped.saturating_add(sequence - next);
                self.gap_pending = true;
                self.next_seq = Some(sequence);
                continue;
            }
            let waiting_since = head.listed_at;
            if head.payload.is_none() && !self.gap_expired(head, now) {
                break;
            }
            let map_uri = head.map_uri.clone();
            if head.payload.is_some() && !self.init_ready(map_uri.as_deref()) {
                // Its init has not arrived: wait a target duration, then
                // skip it, so segments of a map without an init cannot pile up.
                if self
                    .target
                    .is_none_or(|target| now.saturating_duration_since(waiting_since) < target)
                {
                    break;
                }
                self.skip_discontinuity(head.discontinuity);
                self.listed.remove(&sequence);
                pool.counters.skipped = pool.counters.skipped.saturating_add(1);
                self.next_seq = sequence.checked_add(1);
                continue;
            }
            let Some(listed) = self.listed.remove(&sequence) else {
                break;
            };
            match listed.payload {
                Some(payload) => {
                    let (init, replaces) = self.take_init(map_uri.as_deref());
                    let reanchor = self.take_reanchor(listed.discontinuity, replaces);
                    out.push(Release {
                        rendition,
                        sequence,
                        epoch: self.epoch,
                        reanchor,
                        after_gap: std::mem::take(&mut self.gap_pending),
                        duration: listed.duration,
                        init,
                        payload,
                    });
                }
                None => {
                    self.skip_discontinuity(listed.discontinuity);
                    pool.counters.skipped = pool.counters.skipped.saturating_add(1);
                }
            }
            self.next_seq = sequence.checked_add(1);
        }
    }

    /// When this rendition's head may next make progress without a new object.
    fn next_deadline(&self) -> Option<Instant> {
        self.listed.first_key_value().and_then(|(_, head)| {
            let waits_for_bytes = head.payload.is_none() && self.later_bytes_ready();
            let waits_for_init =
                head.payload.is_some() && !self.init_ready(head.map_uri.as_deref());
            (waits_for_bytes || waits_for_init)
                .then(|| self.target.map(|target| head.listed_at + target))
                .flatten()
        })
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
}

pub(super) struct Session {
    pool: Pool,
    /// Rendition 0 is the followed variant (or the first media playlist when
    /// no master came); rendition 1, when present, its separate audio. Never
    /// empty, and at most `MAX_RENDITIONS`.
    tracks: Vec<Track>,
    other_variants: HashMap<String, HashSet<String>>,
}

impl Session {
    pub(super) fn new(limits: SessionLimits) -> Self {
        Self {
            pool: Pool {
                limits,
                held: VecDeque::new(),
                held_bytes: 0,
                counters: Counters::default(),
                seeded: Vec::new(),
            },
            tracks: vec![Track::new(None)],
            other_variants: HashMap::new(),
        }
    }

    pub(super) fn counters(&self) -> Counters {
        self.pool.counters
    }

    /// How many renditions this session follows: a followed variant's media
    /// playlist, plus its separate audio rendition when its master names one.
    pub(super) fn renditions(&self) -> usize {
        self.tracks
            .iter()
            .filter(|track| track.path.is_some())
            .count()
            .max(1)
    }

    /// The first followed playlist's current `EXT-X-MAP` URI.
    #[cfg(test)]
    pub(super) fn map_uri(&self) -> Option<&str> {
        self.tracks
            .first()
            .and_then(|track| track.current_map.as_deref())
    }

    /// Every followed playlist seen so far named no `EXT-X-MAP`, so an fMP4
    /// object can never be demuxed.
    pub(super) fn map_required(&self) -> bool {
        let mut seen = self
            .tracks
            .iter()
            .filter(|track| track.playlist_seen)
            .peekable();
        seen.peek().is_some() && seen.all(|track| track.current_map.is_none())
    }

    /// Whether `path` (or `uri`) is an `EXT-X-MAP` init object of a followed
    /// playlist rather than a segment.
    pub(super) fn knows_map(&self, uri: &str) -> bool {
        self.tracks.iter().any(|track| track.knows_map(uri))
    }

    /// Seed the map `rendition` applied in an earlier session, so an encoder
    /// that uploads its init once keeps publishing after the session that
    /// first saw it ended.
    pub(super) fn seed_init(&mut self, rendition: usize, uri: &str, bytes: Bytes) {
        if rendition >= MAX_RENDITIONS {
            return;
        }
        self.pool.seeded.retain(|(seeded, _)| seeded != uri);
        self.pool.seeded.push((uri.to_string(), bytes));
        while self.tracks.len() <= rendition {
            self.tracks.push(Track::new(None));
        }
        if let Some(track) = self.tracks.get_mut(rendition) {
            track.ensure_map(uri, &mut self.pool);
        }
    }

    /// Forget the init a map holds. A *refused* init upload must leave
    /// nothing behind: otherwise the map keeps the previous run's init and
    /// the encoder's next segments are demuxed with it (an encoder never
    /// reads the refusal). The seed goes too: a map recreated later (a
    /// variant switch, an evicted entry) must not take the stale init again.
    pub(super) fn void_map(&mut self, uri: &str) {
        for track in &mut self.tracks {
            if let Some(map) = track.maps.iter_mut().find(|map| map.uri == uri) {
                map.pending = None;
                map.delivered = None;
                map.last = None;
            }
        }
        self.pool.seeded.retain(|(seeded, _)| seeded != uri);
    }

    /// A segment was dropped: its bytes could not be demuxed.
    pub(super) fn note_failed_segment(&mut self) {
        self.pool.counters.failed = self.pool.counters.failed.saturating_add(1);
    }

    /// Whether a segment listed under `uri` in a followed rendition may leave.
    #[cfg(test)]
    pub(super) fn init_ready(&self, uri: Option<&str>) -> bool {
        self.tracks.iter().any(|track| track.init_ready(uri))
    }

    #[cfg(test)]
    pub(super) fn held_segments(&self) -> usize {
        self.pool.held.len()
    }

    #[cfg(test)]
    pub(super) fn held_bytes(&self) -> usize {
        self.pool.held_bytes
    }

    /// The followed renditions' target duration (the longest).
    fn target(&self) -> Option<Duration> {
        self.tracks.iter().filter_map(|track| track.target).max()
    }

    /// No object for this long ends the session; it is also the hold age.
    pub(super) fn idle_timeout(&self) -> Duration {
        let floor = self.pool.limits.idle_floor;
        self.target()
            .map_or(floor, |target| target.saturating_mul(3).max(floor))
    }

    /// How far published media may run from the wall clock: one target
    /// duration, or the idle floor before any playlist names one.
    pub(super) fn pacing_lead(&self) -> Duration {
        self.target().unwrap_or(self.pool.limits.idle_floor)
    }

    /// `EXT-X-ENDLIST` was seen and every listed segment has left, in the
    /// followed variant and in every rendition whose playlist arrived.
    pub(super) fn finished(&self) -> bool {
        self.tracks.first().is_some_and(Track::finished)
            && self
                .tracks
                .iter()
                .filter(|track| track.playlist_seen)
                .all(Track::finished)
    }

    fn follows(&self, playlist_path: &str) -> Follows {
        if self.tracks.first().is_none_or(|track| track.path.is_none()) {
            return Follows::Unlocked;
        }
        self.tracks
            .iter()
            .position(|track| track.path.as_deref() == Some(playlist_path))
            .map_or(Follows::Other, Follows::Rendition)
    }

    pub(super) fn on_segment(
        &mut self,
        path: String,
        payload: Payload,
        now: Instant,
    ) -> SegmentOutcome {
        let pool = &mut self.pool;
        if let Some(map) = self
            .tracks
            .iter_mut()
            .find_map(|track| track.maps.iter_mut().find(|map| map.uri == path))
        {
            // The newest upload of the init wins, like a held segment's.
            map.pending = Some(payload);
            return SegmentOutcome::Init;
        }
        for track in &mut self.tracks {
            if let Some(&sequence) = track.latest_listing.get(&path)
                && let Some(listed) = track.listed.get_mut(&sequence)
                && listed.path == path
            {
                if listed.payload.is_some() {
                    pool.counters.duplicates = pool.counters.duplicates.saturating_add(1);
                    return SegmentOutcome::Duplicate;
                }
                listed.payload = Some(payload);
                return SegmentOutcome::Attached;
            }
        }
        if !self
            .tracks
            .iter()
            .any(|track| track.latest_listing.contains_key(&path))
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
        pool.remove_held(&path);
        pool.held_bytes += payload.len();
        pool.held.push_back(Held {
            path,
            payload,
            at: now,
        });
        while pool.held.len() > pool.limits.hold_segments
            || pool.held_bytes > pool.limits.hold_bytes
        {
            pool.pop_oldest_held();
            pool.counters.evicted = pool.counters.evicted.saturating_add(1);
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
            Playlist::Media(media) => {
                let rendition = match self.follows(playlist_path) {
                    Follows::Other => {
                        self.on_other_variant(playlist_path, &media);
                        return PlaylistOutcome::OtherVariant;
                    }
                    Follows::Unlocked => {
                        if let Some(track) = self.tracks.first_mut() {
                            track.path = Some(playlist_path.to_string());
                        }
                        0
                    }
                    Follows::Rendition(rendition) => rendition,
                };
                if let Some(track) = self.tracks.get_mut(rendition) {
                    track.on_followed(media, now, &mut self.pool);
                }
                PlaylistOutcome::Accepted
            }
        }
    }

    /// Follow the master's highest-bandwidth variant and, when that variant's
    /// audio is a separate rendition, that rendition too (`DEFAULT`, else
    /// `AUTOSELECT`, else the first of its group).
    fn on_master(&mut self, master: &MasterPlaylist) {
        let Some(chosen) = master.highest_bandwidth() else {
            return;
        };
        let audio = master
            .audio_rendition(chosen)
            .and_then(|audio| audio.path.clone())
            .filter(|path| *path != chosen.path);
        let wanted: Vec<String> = std::iter::once(chosen.path.clone()).chain(audio).collect();
        // A rendition's playlist can arrive before the master that names it
        // (FFmpeg writes its master after every variant's first playlist), so
        // the first media playlist may be followed as rendition 0 and then
        // named the audio rendition. It moves to its index whole: listing,
        // maps and the init the encoder uploaded once.
        let mut old: Vec<Option<Track>> = std::mem::take(&mut self.tracks)
            .into_iter()
            .map(Some)
            .collect();
        let mut tracks: Vec<Option<Track>> = wanted
            .iter()
            .enumerate()
            .map(|(rendition, path)| {
                let index = old.iter().position(|track| {
                    track
                        .as_ref()
                        .is_some_and(|track| track.path.as_deref() == Some(path.as_str()))
                })?;
                let mut track = old.get_mut(index)?.take()?;
                if index != rendition {
                    track.moved();
                }
                Some(track)
            })
            .collect();
        for (rendition, (slot, path)) in tracks.iter_mut().zip(&wanted).enumerate() {
            self.other_variants.remove(path);
            if slot.is_some() {
                continue;
            }
            // The rendition's previous playlist, if any: a variant switch. A
            // rendition the master no longer selects is dropped with its
            // listing; its held segments age out.
            let track = match old.get_mut(rendition).and_then(Option::take) {
                Some(mut track) if track.path.is_some() => {
                    track.switch_to(path.clone());
                    track
                }
                Some(mut seeded) => {
                    seeded.path = Some(path.clone());
                    seeded
                }
                None => Track::new(Some(path.clone())),
            };
            *slot = Some(track);
        }
        self.tracks = tracks.into_iter().flatten().collect();
        if self.tracks.is_empty() {
            self.tracks.push(Track::new(None));
        }
    }

    fn on_other_variant(&mut self, playlist_path: &str, media: &MediaPlaylist) {
        let paths: HashSet<String> = media
            .segments
            .iter()
            .map(|segment| segment.path.clone())
            .collect();
        for path in &paths {
            self.pool.remove_held(path);
        }
        if self.other_variants.contains_key(playlist_path)
            || self.other_variants.len() < MAX_OTHER_VARIANTS
        {
            self.other_variants.insert(playlist_path.to_string(), paths);
        }
    }

    /// Move every segment that may leave now into `out`, each rendition in
    /// its own order; age out the hold.
    pub(super) fn poll(&mut self, now: Instant, out: &mut Vec<Release>) {
        for (rendition, track) in self.tracks.iter_mut().enumerate() {
            track.poll(rendition, now, out, &mut self.pool);
        }
        let hold_age = self.idle_timeout();
        let pool = &mut self.pool;
        while pool
            .held
            .front()
            .is_some_and(|held| now.saturating_duration_since(held.at) >= hold_age)
        {
            pool.pop_oldest_held();
            pool.counters.evicted = pool.counters.evicted.saturating_add(1);
        }
    }

    /// When `poll` may next make progress without a new object.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let gap = self.tracks.iter().filter_map(Track::next_deadline).min();
        let hold = self
            .pool
            .held
            .front()
            .map(|held| held.at + self.idle_timeout());
        match (gap, hold) {
            (Some(gap), Some(hold)) => Some(gap.min(hold)),
            (gap, hold) => gap.or(hold),
        }
    }
}

#[cfg(test)]
mod tests;
