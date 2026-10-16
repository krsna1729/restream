//! One stream key's HLS push session on the MEDIA executor: applies objects
//! to the ordering state and replies at once; publishes what it releases,
//! paced to media time, between objects.
//!
//! The first media object fixes the session's container (MPEG-TS or fMP4);
//! an object of the other container is refused. fMP4 objects are checked
//! here, before their reply: an init against its bound and the codec scope,
//! a media segment against the box walk.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::ordering::{
    Counters, Payload, PlaylistOutcome, Release, SegmentOutcome, Session, SessionLimits,
};
use super::pacing::Pacer;
use super::playlist;
use super::{ObjectKind, ObjectOutcome};
use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ingest_publisher::fmp4::{Fmp4Demux, boxes};
use crate::media::ingest_publisher::{
    Container, IngestTimestamps, MediaRejection, PUBLISH_CHUNK_BYTES, PublishTarget,
    SegmentPublisher,
};
use crate::media::ring_buffer::RingBuffer;

/// One uploaded object handed from the HTTP handler to its session.
pub(super) struct HlsObject {
    pub(super) path: String,
    pub(super) kind: ObjectKind,
    pub(super) payload: Payload,
    pub(super) reply: oneshot::Sender<ObjectOutcome>,
}

pub(super) struct SessionContext {
    pub(super) engine: Arc<MediaEngine>,
    pub(super) pipeline_id: String,
    pub(super) registration: IngestRegistration,
    pub(super) ring: Arc<RingBuffer>,
    /// Where the session publishes its pacing lead for the body deadline.
    pub(super) lead_ms: Arc<AtomicU64>,
    pub(super) limits: SessionLimits,
    /// Bound on an fMP4 init segment.
    pub(super) max_init_bytes: usize,
    /// The init each stream key applied last: FFmpeg uploads its init once
    /// per run, so a session that starts after an idle timeout, a retry or
    /// a failure still needs it.
    pub(super) inits: Arc<Mutex<InitCache>>,
}

/// The last init a stream key applied, and the `EXT-X-MAP` it belongs to.
#[derive(Clone)]
pub(super) struct AppliedInit {
    pub(super) uri: String,
    pub(super) bytes: Bytes,
}

/// Most stream keys' inits one ingest keeps, besides the byte bound.
const CACHED_INITS: usize = 64;

/// The init each stream key applied last, kept across sessions: FFmpeg
/// uploads its `EXT-X-MAP` init once per run, so a session that starts
/// after an idle timeout, a retry or a failure must apply the same init
/// again. Bounded by bytes and by key count, oldest first out, so a hostile
/// or broken encoder cannot hold memory outside the ingest budget; a single
/// init larger than the whole byte bound is kept alone.
pub(super) struct InitCache {
    max_bytes: usize,
    bytes: usize,
    by_key: HashMap<String, AppliedInit>,
    order: VecDeque<String>,
}

impl InitCache {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes: 0,
            by_key: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<AppliedInit> {
        self.by_key.get(key).cloned()
    }

    /// Forget `key`'s init. A refused init upload and an ended run leave
    /// nothing that a later session may apply.
    pub(super) fn remove(&mut self, key: &str) {
        if let Some(removed) = self.by_key.remove(key) {
            self.bytes = self.bytes.saturating_sub(removed.bytes.len());
            if let Some(index) = self.order.iter().position(|cached| cached == key) {
                self.order.remove(index);
            }
        }
    }

    /// Remember `key`'s init, evicting other keys oldest first until both
    /// bounds hold. The entry just inserted is never evicted.
    pub(super) fn put(&mut self, key: &str, init: AppliedInit) {
        let size = init.bytes.len();
        if let Some(previous) = self.by_key.insert(key.to_string(), init) {
            self.bytes = self.bytes.saturating_sub(previous.bytes.len());
        } else {
            self.order.push_back(key.to_string());
        }
        self.bytes = self.bytes.saturating_add(size);
        while (self.bytes > self.max_bytes || self.order.len() > CACHED_INITS)
            && self.order.len() > 1
        {
            let Some(index) = self.order.iter().position(|cached| cached != key) else {
                break;
            };
            let Some(victim) = self.order.remove(index) else {
                break;
            };
            if let Some(removed) = self.by_key.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(removed.bytes.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init(uri: &str, len: usize) -> AppliedInit {
        AppliedInit {
            uri: uri.to_string(),
            bytes: Bytes::from(vec![0u8; len]),
        }
    }

    /// Over the byte bound: the oldest entries go, the newest stays, and the
    /// total never exceeds the bound.
    #[test]
    fn the_init_cache_evicts_oldest_first_inside_its_byte_bound() {
        let mut cache = InitCache::new(1000);
        cache.put("a", init("init.mp4", 400));
        cache.put("b", init("init.mp4", 400));
        assert_eq!(cache.bytes, 800);
        cache.put("c", init("init.mp4", 400));
        assert!(cache.get("a").is_none(), "the oldest went first");
        assert!(cache.get("b").is_some() && cache.get("c").is_some());
        assert!(cache.bytes <= 1000, "{} bytes cached", cache.bytes);

        // Replacing a key's init counts once, not twice.
        cache.put("b", init("init.mp4", 400));
        assert_eq!(cache.bytes, 800);
        assert!(cache.bytes <= 1000);
    }

    /// A single init larger than the whole bound is kept alone, and the next
    /// init still evicts it oldest-first rather than looping.
    #[test]
    fn an_init_over_the_whole_bound_is_kept_alone_then_evicted() {
        let mut cache = InitCache::new(100);
        cache.put("a", init("init.mp4", 50));
        cache.put("b", init("init.mp4", 5000));
        assert!(cache.get("b").is_some(), "the newest init is never evicted");
        assert!(cache.get("a").is_none(), "it is kept alone");
        assert_eq!(cache.order.len(), 1);

        cache.put("c", init("init.mp4", 10));
        assert!(cache.get("c").is_some());
        assert!(cache.get("b").is_none());
        assert!(cache.bytes <= 100);
        assert_eq!(cache.order, VecDeque::from(["c".to_string()]));
    }

    /// The key bound holds on its own, however small the inits are.
    #[test]
    fn the_init_cache_evicts_by_key_count_too() {
        let mut cache = InitCache::new(usize::MAX);
        for key in 0..CACHED_INITS + 3 {
            cache.put(&format!("key{key}"), init("init.mp4", 1));
        }
        assert_eq!(cache.order.len(), CACHED_INITS);
        assert_eq!(cache.bytes, CACHED_INITS);
        assert!(cache.get("key0").is_none());
        assert!(cache.get(&format!("key{}", CACHED_INITS + 2)).is_some());
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionEnd {
    /// `EXT-X-ENDLIST` and every listed segment published.
    Ended,
    Idle,
    Cancelled,
    Failed(String),
}

pub(super) async fn run(
    context: SessionContext,
    mut objects: mpsc::Receiver<HlsObject>,
) -> (SessionEnd, Counters) {
    let cancel = context.registration.cancel_token.clone();
    let outbox_limit = context.limits.hold_bytes;
    let max_init_bytes = context.max_init_bytes;
    let mut session = Session::new(context.limits);
    let key = context.registration.input_id.clone();
    if let Some(applied) = crate::sync::lock(&context.inits).get(&key) {
        session.seed_init(&applied.uri, applied.bytes);
    }
    let end = match PublishTarget::open(
        context.engine,
        context.pipeline_id,
        context.registration,
        context.ring,
    )
    .await
    {
        Ok(target) => {
            let mut state = Publishing {
                outbox: Outbox::default(),
                outbox_limit,
                max_init_bytes,
                inits: context.inits.clone(),
                key: key.clone(),
                failed_logged: false,
                container: None,
                publisher: None,
                pacer: Pacer::default(),
            };
            drive(
                &mut session,
                &target,
                &cancel,
                &mut objects,
                &mut state,
                &context.lead_ms,
            )
            .await
        }
        Err(error) => SessionEnd::Failed(error),
    };
    if end == SessionEnd::Ended {
        // The stream is over: the next run uploads its own init.
        crate::sync::lock(&context.inits).remove(&key);
    }
    // End the attempt before the queue closes: a handler whose send then
    // fails must find the attempt cancelled, so its one restart registers
    // a new attempt instead of being refused as a second publisher. Objects
    // already queued are answered `Retry`; an encoder that does not read
    // replies (FFmpeg) loses them.
    cancel.cancel();
    objects.close();
    while let Ok(object) = objects.try_recv() {
        let _ = object.reply.send(ObjectOutcome::Retry);
    }
    (end, session.counters())
}

/// Released segments not yet fully published, and where the first one is;
/// sized in bytes and in listed (`EXTINF`) media time.
#[derive(Default)]
struct Outbox {
    releases: VecDeque<Release>,
    offset: usize,
    bytes: usize,
    media: Duration,
}

impl Outbox {
    fn push(&mut self, release: Release) {
        self.bytes += release.payload.bytes.len();
        self.media = self.media.saturating_add(release.duration);
        self.releases.push_back(release);
    }

    fn is_empty(&self) -> bool {
        self.releases.is_empty()
    }
}

/// What publishing needs besides the ordering state.
struct Publishing {
    outbox: Outbox,
    outbox_limit: usize,
    max_init_bytes: usize,
    /// The stream key's init slot: an applied init outlives the session.
    inits: Arc<Mutex<InitCache>>,
    /// The stream key this session publishes for.
    key: String,
    /// One log line per session for dropped segments.
    failed_logged: bool,
    /// Fixed by the first accepted media object.
    container: Option<Container>,
    /// Built with the container.
    publisher: Option<SegmentPublisher>,
    pacer: Pacer,
}

async fn drive(
    session: &mut Session,
    target: &PublishTarget,
    cancel: &CancellationToken,
    objects: &mut mpsc::Receiver<HlsObject>,
    state: &mut Publishing,
    lead_ms: &AtomicU64,
) -> SessionEnd {
    let mut releases: Vec<Release> = Vec::new();
    let mut last_activity = Instant::now();

    loop {
        let now = Instant::now();
        let publish_at = state
            .publisher
            .as_ref()
            .filter(|_| !state.outbox.is_empty())
            .map(|publisher| {
                now + state
                    .pacer
                    .delay(now, publisher.media_time_ms(), session.pacing_lead())
            });
        // Idle counts only once everything released has been published.
        let idle_at = state
            .outbox
            .is_empty()
            .then(|| last_activity + session.idle_timeout());
        let wake_at = match (session.next_deadline(), idle_at) {
            (Some(deadline), Some(idle)) => Some(deadline.min(idle)),
            (deadline, idle) => deadline.or(idle),
        };
        // An encoder faster than real time waits for its replies (and,
        // through its upload slots, its next uploads) once two target
        // durations of media, or a hold's worth of bytes, wait to publish.
        let lead = session.pacing_lead();
        let accepting =
            state.outbox.media < lead.saturating_mul(2) && state.outbox.bytes <= state.outbox_limit;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return SessionEnd::Cancelled,
            object = objects.recv(), if accepting => {
                // The registry keeps a sender while the session is
                // registered, so a closed queue means it was forgotten.
                let Some(object) = object else { return SessionEnd::Cancelled };
                last_activity = Instant::now();
                let outcome = apply(session, state, object.kind, object.path, object.payload, last_activity);
                let _ = object.reply.send(outcome);
                let lead = u64::try_from(session.pacing_lead().as_millis()).unwrap_or(u64::MAX);
                lead_ms.store(lead, Ordering::Relaxed);
            }
            _ = sleep_until(publish_at) => {
                // A segment whose bytes cannot be demuxed is dropped, not
                // fatal: the encoder keeps pushing and the next segment
                // publishes. Its object was answered when it arrived.
                if let Err(failure) = publish_next_chunk(state, target).await {
                    session.note_failed_segment();
                    state.drop_failed_release(failure.sequence, &failure.rejection);
                }
                last_activity = Instant::now();
            }
            _ = sleep_until(wake_at) => {
                if idle_at.is_some_and(|idle| Instant::now() >= idle) {
                    if let Some(publisher) = state.publisher.as_mut() {
                        publisher.flush(target);
                    }
                    return SessionEnd::Idle;
                }
            }
        }

        session.poll(Instant::now(), &mut releases);
        for release in releases.drain(..) {
            tracing::trace!(
                sequence = release.sequence,
                epoch = release.epoch,
                discontinuity = release.discontinuity,
                init = release.init.is_some(),
                "hls ingest segment released"
            );
            state.outbox.push(release);
        }
        if session.finished() && state.outbox.is_empty() {
            if let Some(publisher) = state.publisher.as_mut() {
                publisher.flush(target);
            }
            return SessionEnd::Ended;
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// A segment dropped because its bytes could not be demuxed.
struct ReleaseFailure {
    sequence: u64,
    rejection: MediaRejection,
}

/// Publish at most one chunk of the outbox's first release. A release that
/// starts applies its discontinuity and its init first; one that ends
/// closes its object. A rejection names the release it failed on: the
/// caller drops that segment and keeps the session.
async fn publish_next_chunk(
    state: &mut Publishing,
    target: &PublishTarget,
) -> Result<(), ReleaseFailure> {
    let Publishing {
        outbox,
        publisher,
        pacer,
        max_init_bytes,
        inits,
        key,
        ..
    } = state;
    let (Some(release), Some(publisher)) = (outbox.releases.front(), publisher.as_mut()) else {
        return Ok(());
    };
    let failed = |rejection| ReleaseFailure {
        sequence: release.sequence,
        rejection,
    };
    let bytes = &release.payload.bytes;
    if outbox.offset == 0 {
        if release.discontinuity {
            publisher.discontinuity(target);
            pacer.reset();
        }
        if let Some(init) = &release.init {
            publisher
                .apply_init(target, &init.payload.bytes, *max_init_bytes)
                .await
                .map_err(failed)?;
            // The init outlives this session: an encoder uploads it once.
            crate::sync::lock(inits).put(
                key,
                AppliedInit {
                    uri: init.uri.clone(),
                    bytes: init.payload.bytes.clone(),
                },
            );
        }
        publisher.begin_object(bytes).map_err(failed)?;
    }
    let end = bytes.len().min(outbox.offset + PUBLISH_CHUNK_BYTES);
    publisher
        .publish(target, &bytes[outbox.offset..end])
        .await
        .map_err(failed)?;
    if end < bytes.len() {
        outbox.offset = end;
        return Ok(());
    }
    publisher.end_object(target).map_err(failed)?;
    outbox.offset = 0;
    outbox.bytes -= bytes.len();
    outbox.media = outbox.media.saturating_sub(release.duration);
    // Dropping the release returns its bytes to the global budget.
    outbox.releases.pop_front();
    Ok(())
}

impl Publishing {
    /// Drop the release whose bytes could not be demuxed: log the reason
    /// once per session, discard what it produced (nothing from it may be
    /// published unpaced) so the next segment re-anchors its timestamps, and
    /// return the bytes to the budget.
    fn drop_failed_release(&mut self, sequence: u64, rejection: &MediaRejection) {
        if !self.failed_logged {
            self.failed_logged = true;
            tracing::warn!(
                sequence,
                reason = %rejection,
                "hls ingest dropped a segment that could not be demuxed"
            );
        }
        if let Some(publisher) = self.publisher.as_mut() {
            publisher.discard_failed_object();
        }
        self.pacer.reset();
        self.outbox.offset = 0;
        if let Some(release) = self.outbox.releases.pop_front() {
            self.outbox.bytes -= release.payload.bytes.len();
            self.outbox.media = self.outbox.media.saturating_sub(release.duration);
        }
    }
}

fn apply(
    session: &mut Session,
    state: &mut Publishing,
    kind: ObjectKind,
    path: String,
    payload: Payload,
    now: Instant,
) -> ObjectOutcome {
    let container = match kind {
        ObjectKind::Playlist => {
            return match playlist::parse(&payload.bytes, &path) {
                Ok(parsed) => match session.on_playlist(&path, parsed, now) {
                    PlaylistOutcome::Accepted => ObjectOutcome::Stored,
                    PlaylistOutcome::OtherVariant => ObjectOutcome::Dropped,
                },
                Err(error) => ObjectOutcome::Malformed(error.to_string()),
            };
        }
        ObjectKind::TsSegment => Container::Ts,
        ObjectKind::Fmp4 => Container::Fmp4,
    };
    if let Some(current) = state.container
        && current != container
    {
        return ObjectOutcome::Unsupported(format!(
            "this HLS ingest publishes {}; {} objects cannot join it",
            container_name(current),
            container_name(container)
        ));
    }
    if container == Container::Fmp4 {
        let is_map_object = session.knows_map(&path);
        if let Err(rejection) = check_fmp4(
            &payload.bytes,
            is_map_object,
            session.map_required(),
            state.max_init_bytes,
        ) {
            // A refused init leaves nothing behind: its map keeps no init
            // (so its segments wait and are skipped) and the key's cached
            // init goes too. FFmpeg never reads the refusal, so a stale
            // init would silently demux the new run's segments — an HEVC
            // stream as H.264, another codec's frames as AAC.
            if is_map_object || boxes::is_init_segment(&payload.bytes) {
                session.void_map(&path);
                crate::sync::lock(&state.inits).remove(&state.key);
            }
            return rejection.into();
        }
    }
    if state.container.is_none() {
        state.container = Some(container);
        state.publisher = Some(SegmentPublisher::new(
            container.demux(),
            IngestTimestamps::default(),
        ));
    }
    match session.on_segment(path, payload, now) {
        SegmentOutcome::Held => ObjectOutcome::Held,
        SegmentOutcome::Attached | SegmentOutcome::Init => ObjectOutcome::Stored,
        SegmentOutcome::Duplicate | SegmentOutcome::OtherVariant => ObjectOutcome::Dropped,
    }
}

fn container_name(container: Container) -> &'static str {
    match container {
        Container::Ts => "MPEG-TS",
        Container::Fmp4 => "fMP4",
    }
}

/// An fMP4 object before it is held: an init (a top-level `moov`) is
/// checked against its bound and the codec scope, anything else must walk
/// as a media segment. The `EXT-X-MAP` object must be an init, and a
/// playlist that names no map can carry no fMP4 media at all.
fn check_fmp4(
    bytes: &[u8],
    is_map_object: bool,
    map_required: bool,
    max_init_bytes: usize,
) -> Result<(), MediaRejection> {
    if boxes::is_init_segment(bytes) {
        return Fmp4Demux::check_init(bytes, max_init_bytes);
    }
    if is_map_object {
        return Err(MediaRejection::Malformed(
            "the EXT-X-MAP object is not an fMP4 init segment".to_string(),
        ));
    }
    if map_required {
        return Err(MediaRejection::Unsupported(
            "fMP4 segments need an EXT-X-MAP init segment; this playlist names none".to_string(),
        ));
    }
    boxes::media_fragments(bytes, &mut Vec::new())
        .map(|_| ())
        .map_err(|error| MediaRejection::Malformed(error.to_string()))
}

impl From<MediaRejection> for ObjectOutcome {
    fn from(rejection: MediaRejection) -> Self {
        match rejection {
            MediaRejection::Unsupported(reason) => Self::Unsupported(reason),
            MediaRejection::Malformed(reason) => Self::Malformed(reason),
            MediaRejection::TooLarge(reason) => Self::TooLarge(reason),
        }
    }
}
