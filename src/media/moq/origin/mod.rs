//! Restream as a MoQ origin: direct subscribers (native QUIC or browser
//! WebTransport) on the ingest listener's thread.
//!
//! The listener accepts a connection and pauses the moq-lite handshake;
//! [`Origin::admit`] checks the viewer token (an HMAC over the pipeline id
//! and an expiry at most [`token::TOKEN_TTL`] ahead, see [`token`]) and
//! takes a viewer slot (`moq.origin_max_viewers`); [`Origin::serve`] then
//! completes the handshake publishing the pipeline's broadcast and drives
//! the session until the viewer leaves.
//!
//! Each served pipeline has one *generation*: a broadcast (named by the
//! pipeline id) on its own moq-net origin, fed from the pipeline's source
//! ring by one local pump task: the ring producer's publication wake reaches
//! it over a one-slot channel, so it reads each unit once whatever the
//! audience. moq-net fans the frames out as shared `Bytes`, and the origin
//! keeps about two GOPs ([`restream_moq::budget`]).
//!
//! A generation's track set is fixed. A watcher on the listener runtime
//! re-resolves the pipeline's current track set when the ring's track
//! metadata changes and on an absolute one-second deadline (a publication-heavy
//! stream cannot push it back). Track presence is the ring's current track set;
//! the selected ingest and its disconnect record only detect total media loss.
//! Codec configuration identity comes from the retained ring/catalog. When the
//! set differs, the origin publishes a fresh broadcast as the next generation,
//! closes the old generation (ending its viewers) and installs the new one as
//! current. Viewers keep the generation they started on; a generation stops
//! with its last viewer, while a replacement with no viewers stays current for
//! the next viewer to join. SRT PMT changes replace the ring's authoritative
//! track metadata in the same session, so removals and reappearance also
//! revalidate generations without a later viewer attach.

use restream_moq::token;
#[cfg(test)]
mod token_tests;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::pin::pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::future::{Either, select};
use moq_net::origin;
use moq_net::server::Handshake;
use tokio_util::sync::CancellationToken;

use crate::media::egress::feed::ReadBudget;
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::engine::MediaEngine;
use restream_core::metadata::AudioMeta;
use restream_sync::{PublishSubscribers, PublishWake};

use super::egress::media::MediaConverter;
use super::egress::publish::{HangBroadcast, Pump, RingPublisher, live_start};
use super::egress::{MoqPublishStartup, publish_startup};
use super::transport::{self, Accepted};
use restream_moq::media::VideoCodec;

pub(crate) use token::TokenError;

/// Units and bytes one pump batch reads before yielding to the sessions.
const PUMP_BATCH_UNITS: usize = 64;
const PUMP_BATCH_BYTES: usize = 1 << 20;
/// Application close code (moq-net `App`) for a full origin.
const ORIGIN_FULL_CODE: u16 = 1;
/// How often a served generation re-resolves its pipeline's track set when
/// nothing wakes it first. Ring metadata changes wake the watcher; a media
/// loss that only the engine knows (a disconnect leaves no ring event) is
/// found by this bounded check.
const TRACK_SET_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Let ring metadata updates that arrive together (codec hint, configuration,
/// first packet) coalesce before one re-resolution.
const TRACK_SET_SETTLE: Duration = Duration::from_millis(50);

/// Why a viewer was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reject {
    MissingToken,
    Token(TokenError),
    /// Every viewer slot is taken.
    Full {
        max_viewers: usize,
    },
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingToken => f.write_str("MoQ viewer presented no token"),
            Self::Token(error) => error.fmt(f),
            Self::Full { max_viewers } => {
                write!(f, "MoQ origin serves its maximum of {max_viewers} viewers")
            }
        }
    }
}

impl Reject {
    /// The error to close the paused handshake with.
    pub(crate) fn error(&self) -> moq_net::Error {
        match self {
            Self::MissingToken | Self::Token(_) => moq_net::Error::Unauthorized,
            Self::Full { .. } => moq_net::Error::App(ORIGIN_FULL_CODE),
        }
    }
}

/// The viewer token a request presents as `token=`: in the WebTransport
/// CONNECT query, else in the query part of the moq-lite SETUP path.
pub(crate) fn request_token<'a>(
    accepted: &'a Accepted,
    handshake_path: &'a str,
) -> Option<&'a str> {
    token_in(accepted.query.as_deref(), handshake_path)
}

fn token_in<'a>(connect_query: Option<&'a str>, handshake_path: &'a str) -> Option<&'a str> {
    let token = |query: &'a str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .filter(|token| !token.is_empty())
    };
    connect_query.and_then(token).or_else(|| {
        handshake_path
            .split_once('?')
            .and_then(|(_, query)| token(query))
    })
}

/// The process-wide viewer cap. Atomic so listener threads may share it.
#[derive(Debug, Clone)]
pub(crate) struct ViewerSlots {
    in_use: Arc<AtomicUsize>,
    max: usize,
}

/// One taken viewer slot; dropping it frees the slot.
#[derive(Debug)]
pub(crate) struct ViewerSlot {
    in_use: Arc<AtomicUsize>,
}

impl ViewerSlots {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            in_use: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }

    /// Take a slot unless all `max` are in use. Never overshoots, whatever
    /// the concurrency (the loom model `tests/moq_origin_loom.rs` checks
    /// this algorithm).
    pub(crate) fn try_acquire(&self) -> Option<ViewerSlot> {
        let mut current = self.in_use.load(Ordering::Acquire);
        loop {
            if current >= self.max {
                return None;
            }
            match self.in_use.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ViewerSlot {
                        in_use: self.in_use.clone(),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Acquire)
    }
}

impl Drop for ViewerSlot {
    fn drop(&mut self) {
        self.in_use.fetch_sub(1, Ordering::AcqRel);
    }
}

/// An admitted viewer: its pipeline and its slot.
#[derive(Debug)]
pub(crate) struct OriginGrant {
    pipeline_id: String,
    _slot: ViewerSlot,
}

impl OriginGrant {
    #[cfg(test)]
    pub(crate) fn pipeline_id(&self) -> &str {
        &self.pipeline_id
    }
}

/// What a served broadcast publishes: the tracks present and the codec
/// configuration identity the existing media model uses (a video record's
/// description, an audio track's AudioSpecificConfig), so a change in any of
/// them starts a new generation rather than mutating the served catalog's
/// track list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackSet {
    video: Option<VideoIdentity>,
    audio: Option<AudioIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoIdentity {
    codec: VideoCodec,
    description: Option<bytes::Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioIdentity {
    track_index: u32,
    sample_rate: u32,
    asc: Option<bytes::Bytes>,
}

impl TrackSet {
    fn of(startup: &MoqPublishStartup) -> Self {
        Self {
            video: startup.video.as_ref().map(|video| VideoIdentity {
                codec: video.codec,
                description: video
                    .config
                    .as_ref()
                    .and_then(|config| config.description.clone()),
            }),
            audio: startup.audio.as_ref().map(|audio| AudioIdentity {
                track_index: audio.track_index,
                sample_rate: audio.config.sample_rate,
                asc: audio.config.asc.clone(),
            }),
        }
    }

    fn is_empty(&self) -> bool {
        self.video.is_none() && self.audio.is_none()
    }
}

/// One served generation: a moq-net origin and the pump task that feed a
/// broadcast with one fixed track set. A track-set change replaces it with a
/// new generation; viewers stay on theirs and are closed with it.
struct Served {
    origin: origin::Producer,
    viewers: Cell<usize>,
    generation: u64,
    track_set: TrackSet,
    /// Wakes every viewer of this generation when it closes.
    closed: CancellationToken,
    /// Pump + origin driver; taking it stops the broadcast.
    task: RefCell<Option<compio::runtime::JoinHandle<()>>>,
    /// A handle to the published broadcast so a test can read the catalog
    /// and media of a generation with no attached viewer.
    #[cfg(test)]
    probe: moq_net::broadcast::Producer,
}

impl Served {
    /// Close the generation: end every viewer session and drop the broadcast.
    /// Idempotent, and also run when the last handle drops. The watcher exits
    /// on `closed`.
    fn close(&self) {
        self.closed.cancel();
        drop(self.task.borrow_mut().take());
    }

    fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
            || self
                .task
                .borrow()
                .as_ref()
                .is_none_or(compio::runtime::JoinHandle::is_finished)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) struct Origin {
    engine: Arc<MediaEngine>,
    slots: ViewerSlots,
    /// The current generation of each served pipeline. A replaced generation
    /// stays alive only through its viewers' `Rc`s, and closes with them.
    served: RefCell<HashMap<String, Rc<Served>>>,
    /// Serializes attaches and track-set re-resolutions, whose pipeline
    /// lookup yields: two racing viewers must not replace each other with
    /// stale startup snapshots.
    attach_lock: tokio::sync::Mutex<()>,
    /// Generation numbers handed out, never reused.
    generations: Cell<u64>,
}

impl Origin {
    pub(crate) fn new(engine: Arc<MediaEngine>, max_viewers: usize) -> Rc<Self> {
        Rc::new(Self {
            engine,
            slots: ViewerSlots::new(max_viewers),
            served: RefCell::default(),
            attach_lock: tokio::sync::Mutex::new(()),
            generations: Cell::new(0),
        })
    }

    /// Verify the viewer's token and take a viewer slot.
    pub(crate) fn admit(
        &self,
        accepted: &Accepted,
        token: Option<&str>,
    ) -> Result<OriginGrant, Reject> {
        let grant = self.admit_token(token, unix_now())?;
        tracing::debug!(pipeline_id = grant.pipeline_id.as_str(), mode = ?accepted.mode, "MoQ viewer admitted");
        Ok(grant)
    }

    fn admit_token(&self, token: Option<&str>, now_secs: u64) -> Result<OriginGrant, Reject> {
        let token = token.ok_or(Reject::MissingToken)?;
        let pipeline_id =
            token::verify(token::process_key(), token, now_secs).map_err(Reject::Token)?;
        let slot = self.slots.try_acquire().ok_or(Reject::Full {
            max_viewers: self.slots.max,
        })?;
        Ok(OriginGrant {
            pipeline_id,
            _slot: slot,
        })
    }

    /// Complete the handshake publishing the grant's pipeline and drive the
    /// session until the viewer leaves. Never panics on peer input; every
    /// end is logged.
    pub(crate) async fn serve<S: moq_net::transport::poll::Session>(
        self: Rc<Self>,
        grant: OriginGrant,
        handshake: Handshake<S>,
    ) {
        let pipeline_id = grant.pipeline_id.clone();
        let served = match self.clone().attach(&pipeline_id).await {
            Ok(served) => served,
            Err(reason) => {
                tracing::warn!(pipeline_id = pipeline_id.as_str(), %reason, "MoQ origin cannot serve the pipeline");
                handshake.close(moq_net::Error::NotFound);
                return;
            }
        };
        // Detaches on every exit, including cancellation at shutdown, from
        // this generation only.
        let _viewer = Viewer {
            origin: self.clone(),
            pipeline_id: pipeline_id.clone(),
            served: served.clone(),
        };
        let Some(accepted) = Self::await_handshake(
            handshake.with_publisher(&served.origin).ok(),
            &served.closed,
        )
        .await
        else {
            tracing::debug!(
                pipeline_id = pipeline_id.as_str(),
                generation = served.generation,
                "MoQ viewer generation closed during the handshake"
            );
            drop(grant);
            return;
        };
        let (_session, driver) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::debug!(pipeline_id = pipeline_id.as_str(), %error, "MoQ viewer handshake failed");
                return;
            }
        };
        // A replaced generation closes its viewers; a viewer leaving first
        // is the ordinary end.
        let error = {
            let driver = pin!(transport::run_driver(driver));
            let closed = pin!(served.closed.cancelled());
            match select(driver, closed).await {
                Either::Left((error, _)) => error,
                Either::Right(((), _driver)) => {
                    tracing::debug!(
                        pipeline_id = pipeline_id.as_str(),
                        generation = served.generation,
                        "MoQ viewer generation closed"
                    );
                    drop(grant);
                    return;
                }
            }
        };
        tracing::debug!(pipeline_id = pipeline_id.as_str(), %error, "MoQ viewer left");
        drop(grant);
    }

    /// Complete the paused handshake, but abandon it when the generation
    /// closes. A legacy SETUP write can stay blocked on flow control, so a
    /// replacement must release the viewer and its admission slot without
    /// waiting for `ok()` to finish.
    async fn await_handshake<T>(
        handshake: impl Future<Output = T>,
        closed: &CancellationToken,
    ) -> Option<T> {
        let handshake = pin!(handshake);
        let closed = pin!(closed.cancelled());
        match select(handshake, closed).await {
            Either::Left((value, _)) => Some(value),
            Either::Right(((), _handshake)) => None,
        }
    }

    /// The pipeline's served generation, attaching this viewer to it. Every
    /// attach re-resolves the pipeline's track set; when it differs from the
    /// current generation's, the origin starts a new generation with a fresh
    /// broadcast and closes the old one. A pipeline the engine does not know
    /// (deleted after the token was minted) is refused rather than given a
    /// new ring.
    async fn attach(self: Rc<Self>, pipeline_id: &str) -> Result<Rc<Served>, String> {
        let _guard = self.attach_lock.lock().await;
        let (ring, startup) = resolve_pipeline(&self.engine, pipeline_id)
            .await
            .ok_or("no such pipeline")?;
        let track_set = TrackSet::of(&startup);
        let current = self.served.borrow().get(pipeline_id).cloned();
        if track_set.is_empty() {
            // The pipeline lost all media: refuse, and never keep the stale
            // broadcast serving.
            if let Some(old) = self.served.borrow_mut().remove(pipeline_id) {
                old.close();
            }
            return Err("the pipeline has no media yet".to_string());
        }
        if let Some(current) = current
            && current.track_set == track_set
            && !current.is_closed()
        {
            current.viewers.set(current.viewers.get() + 1);
            return Ok(current);
        }
        self.install(pipeline_id, ring, &startup, track_set, 1)
    }

    /// Re-resolve a generation's track set after its ring or the pipeline's
    /// media changed. A difference installs the next generation and closes
    /// this one; losing every track closes it without a replacement.
    async fn revalidate(self: Rc<Self>, pipeline_id: &str, generation: u64) {
        let _guard = self.attach_lock.lock().await;
        let Some(current) = self.served.borrow().get(pipeline_id).cloned() else {
            return;
        };
        if current.generation != generation || current.is_closed() {
            return;
        }
        let Some((ring, startup)) = resolve_pipeline(&self.engine, pipeline_id).await else {
            // The pipeline was deleted while served.
            if self.served.borrow_mut().remove(pipeline_id).is_some() {
                current.close();
            }
            return;
        };
        let track_set = TrackSet::of(&startup);
        if track_set.is_empty() {
            if self.served.borrow_mut().remove(pipeline_id).is_some() {
                current.close();
                tracing::info!(
                    pipeline_id,
                    generation,
                    "MoQ origin closed the generation: the pipeline lost all media"
                );
            }
            return;
        }
        if track_set == current.track_set {
            return;
        }
        match self.install(pipeline_id, ring, &startup, track_set, 0) {
            Ok(served) => tracing::info!(
                pipeline_id,
                from = generation,
                to = served.generation,
                "MoQ origin replaced the served generation"
            ),
            Err(reason) => tracing::warn!(
                pipeline_id,
                generation,
                %reason,
                "MoQ origin could not replace the served generation"
            ),
        }
    }

    /// Publish `startup`'s broadcast as the next generation and install it as
    /// current, closing the generation it replaces. `viewers` is the number
    /// the caller already attaches (0 for a watcher-triggered replacement).
    fn install(
        self: &Rc<Self>,
        pipeline_id: &str,
        ring: Arc<restream_sync::RingBuffer>,
        startup: &MoqPublishStartup,
        track_set: TrackSet,
        viewers: usize,
    ) -> Result<Rc<Served>, String> {
        let generation = self.generations.get() + 1;
        let served = match start(
            pipeline_id,
            ring,
            startup,
            track_set,
            generation,
            viewers,
            Rc::downgrade(self),
        ) {
            Ok(served) => Rc::new(served),
            Err(reason) => {
                // The old generation no longer matches the pipeline; never
                // leave it served as if it did.
                if let Some(old) = self.served.borrow_mut().remove(pipeline_id) {
                    old.close();
                }
                return Err(reason);
            }
        };
        self.generations.set(generation);
        let replaced = self
            .served
            .borrow_mut()
            .insert(pipeline_id.to_string(), served.clone());
        if let Some(replaced) = replaced {
            // Viewers of the old generation detach from it and its broadcast
            // closes; the new generation is current for the next attach.
            replaced.close();
        }
        Ok(served)
    }

    #[cfg(test)]
    pub(crate) fn viewers(&self, pipeline_id: &str) -> usize {
        self.served
            .borrow()
            .get(pipeline_id)
            .map_or(0, |served| served.viewers.get())
    }

    #[cfg(test)]
    pub(crate) fn served_generation(&self, pipeline_id: &str) -> Option<u64> {
        self.served
            .borrow()
            .get(pipeline_id)
            .map(|served| served.generation)
    }

    #[cfg(test)]
    pub(crate) fn slots_in_use(&self) -> usize {
        self.slots.in_use()
    }
}

/// The pipeline's current ring and publish startup, read on the listener
/// task; the engine's async state is runtime-agnostic and does not need a
/// Tokio task. The selection lock is held across the read so startup and
/// presence describe one selected input, never a mixture of two.
///
/// Per-track presence comes from the ring's current track set (what the
/// broadcast serves, including a recirculation input whose engine metadata is
/// empty). The selected-ingest state only answers total media loss: no active
/// ingest but a session or disconnect record means nothing is feeding the
/// ring, so no track survives. Same-session SRT PMT changes replace the ring's
/// track metadata and clear removed decoder configurations.
async fn resolve_pipeline(
    engine: &MediaEngine,
    pipeline_id: &str,
) -> Option<(Arc<restream_sync::RingBuffer>, MoqPublishStartup)> {
    let _selection = engine.ingests.selection_lock.lock().await;
    let ring = engine
        .ingests
        .pipelines
        .read()
        .await
        .get(pipeline_id)
        .cloned()?;
    let mut startup = publish_startup(engine, pipeline_id, &ring).await;
    if total_media_loss(engine, pipeline_id).await {
        startup.video = None;
        startup.audio = None;
    }
    Some((ring, startup))
}

/// Whether the pipeline has no selected source at all: no active ingest, but
/// a registered session or a disconnect record. `false` when an ingest is
/// active (even unprobed, as a recirculation publisher) or when the pipeline
/// never had one (a ring-only stage or a test).
async fn total_media_loss(engine: &MediaEngine, pipeline_id: &str) -> bool {
    if engine.ingests.active.read().await.contains_key(pipeline_id) {
        return false;
    }
    let has_session = engine
        .ingests
        .sessions
        .read()
        .await
        .values()
        .any(|ingest| ingest.pipeline_id == pipeline_id);
    let has_recent = engine.ingests.recent.read().await.contains_key(pipeline_id);
    has_session || has_recent
}

/// A viewer attached to one served generation; detaches from it on drop.
struct Viewer {
    origin: Rc<Origin>,
    pipeline_id: String,
    served: Rc<Served>,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let remaining = self.served.viewers.get().saturating_sub(1);
        self.served.viewers.set(remaining);
        if remaining == 0 {
            let mut served = self.origin.served.borrow_mut();
            let current = served
                .get(&self.pipeline_id)
                .is_some_and(|current| Rc::ptr_eq(current, &self.served));
            if current {
                served.remove(&self.pipeline_id);
            }
        }
    }
}

/// Start serving `ring` as broadcast `pipeline_id` with `track_set`, with
/// `viewers` already attached (0 for a watcher-triggered replacement). A
/// pipeline with no media yet is refused: its broadcast's track set is fixed,
/// so a trackless one would never play.
fn start(
    pipeline_id: &str,
    ring: Arc<restream_sync::RingBuffer>,
    startup: &MoqPublishStartup,
    track_set: TrackSet,
    generation: u64,
    viewers: usize,
    origin: Weak<Origin>,
) -> Result<Served, String> {
    let converter = MediaConverter::new(startup);
    if !converter.has_video() && !converter.has_audio() {
        return Err("the pipeline has no media yet".to_string());
    }
    let (origin_producer, driver, budget) = restream_moq::budget::origin();
    let broadcast = HangBroadcast::create(
        &origin_producer,
        budget,
        pipeline_id,
        converter.has_video(),
        converter.has_audio(),
    )
    .map_err(|error| format!("MoQ broadcast: {error}"))?;
    #[cfg(test)]
    let probe = broadcast.producer();
    let publisher = RingPublisher::new(converter, broadcast);
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    let monitor_feed = feed.clone_reader();
    let (wake_tx, wake_rx) = flume::bounded(1);
    let wake = Subscription::new(feed.publication_ring().publication_subscribers(), wake_tx);
    let (monitor_tx, monitor_rx) = flume::bounded(1);
    let monitor_wake =
        Subscription::new(feed.publication_ring().metadata_subscribers(), monitor_tx);
    let closed = CancellationToken::new();
    let task = compio::runtime::spawn(async move {
        let _wake = wake;
        let pump = pin!(pump(feed, publisher, wake_rx));
        let origin_run = pin!(transport::run_driver(driver));
        let _ = select(pump, origin_run).await;
    });
    let monitor = compio::runtime::spawn({
        let closed = closed.clone();
        let pipeline_id = pipeline_id.to_string();
        async move {
            let _wake = monitor_wake;
            watch_track_set(
                origin,
                pipeline_id,
                generation,
                monitor_feed,
                monitor_rx,
                closed,
            )
            .await;
        }
    });
    // The watcher is cancelled by `closed`, never by dropping a handle: it
    // may be the task that replaces or closes its own generation.
    monitor.detach();
    Ok(Served {
        origin: origin_producer,
        viewers: Cell::new(viewers),
        generation,
        track_set,
        closed,
        task: RefCell::new(Some(task)),
        #[cfg(test)]
        probe,
    })
}

/// What the ring currently advertises about its tracks. Comparing this on
/// each wake keeps the watcher off the engine lookup for ordinary packet
/// flow; the interval covers changes no ring metadata event reports.
struct MediaShape {
    codec_hint: String,
    video_generation: u32,
    audio: Option<Arc<Vec<AudioMeta>>>,
}

impl MediaShape {
    fn capture(feed: &RingFeed) -> Self {
        let ring = feed.current_ring();
        Self {
            codec_hint: ring.codec_hint_str().to_string(),
            video_generation: ring.video_config().map_or(0, |config| config.generation),
            audio: ring.audio_tracks_snapshot(),
        }
    }

    fn matches(&self, feed: &RingFeed) -> bool {
        let ring = feed.current_ring();
        self.codec_hint == ring.codec_hint_str()
            && self.video_generation == ring.video_config().map_or(0, |config| config.generation)
            && match (&self.audio, &ring.audio_tracks_snapshot()) {
                (None, None) => true,
                (Some(mine), Some(theirs)) => Arc::ptr_eq(mine, theirs),
                _ => false,
            }
    }
}

/// Watch a generation's track identity on the listener runtime. Ring
/// metadata wakes and a bounded interval both end in the same re-resolution;
/// the ring's own publications and metadata changes wake it, and the interval
/// catches a media loss only the engine knows (for example a publisher
/// disconnect that leaves the ring unchanged). Exits with the generation.
async fn watch_track_set(
    origin: Weak<Origin>,
    pipeline_id: String,
    generation: u64,
    feed: RingFeed,
    wake: flume::Receiver<()>,
    closed: CancellationToken,
) {
    let mut shape = MediaShape::capture(&feed);
    // Absolute, not idle: a publication-only stream never resets this, so a
    // media loss the ring cannot see is still checked every interval.
    let mut next_check = Instant::now() + TRACK_SET_CHECK_INTERVAL;
    loop {
        let periodic = {
            let waited = async {
                let wake = pin!(wake.recv_async());
                let deadline = pin!(compio::time::sleep_until(next_check));
                match select(wake, deadline).await {
                    // A ring track metadata change.
                    Either::Left((Ok(()), _)) => false,
                    // The subscription is gone; fall back to the deadline.
                    Either::Left((Err(_), _)) => {
                        compio::time::sleep_until(next_check).await;
                        true
                    }
                    Either::Right(((), _)) => true,
                }
            };
            let waited = pin!(waited);
            let cancelled = pin!(closed.cancelled());
            match select(waited, cancelled).await {
                Either::Left((periodic, _)) => periodic,
                Either::Right(((), _)) => return,
            }
        };
        if !periodic {
            if shape.matches(&feed) {
                continue;
            }
            // Codec hint, configuration and the first packet often arrive
            // together: let them settle so one generation replaces one.
            compio::time::sleep(TRACK_SET_SETTLE).await;
            if closed.is_cancelled() {
                return;
            }
            if shape.matches(&feed) {
                continue;
            }
        }
        shape = MediaShape::capture(&feed);
        let Some(origin) = origin.upgrade() else {
            return;
        };
        origin.revalidate(&pipeline_id, generation).await;
        // This check was at least as fresh as the bound; start the next
        // absolute window (metadata wakes cannot accumulate to starve it).
        next_check = Instant::now() + TRACK_SET_CHECK_INTERVAL;
        if closed.is_cancelled() {
            return;
        }
    }
}

/// Feed the broadcast from the ring until the broadcast closes.
async fn pump(feed: RingFeed, mut publisher: RingPublisher, wake: flume::Receiver<()>) {
    let mut cursor = live_start(&feed);
    let budget = ReadBudget::new(PUMP_BATCH_UNITS, PUMP_BATCH_BYTES);
    loop {
        match publisher.pump(&feed, &mut cursor, budget) {
            // Let the sessions send before the next batch.
            Pump::Sent { .. } => yield_now().await,
            // A publication after this read leaves a token in the channel,
            // so the wait cannot miss it.
            Pump::Empty => {
                if wake.recv_async().await.is_err() {
                    return;
                }
            }
            Pump::Overrun => cursor = live_start(&feed),
            Pump::Failed(reason) => {
                tracing::warn!(%reason, "MoQ origin broadcast ended");
                return;
            }
        }
    }
}

async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}

/// The ring's publication wake, forwarded into a one-slot channel (a full
/// channel already holds a pending wake). Unsubscribes on drop.
struct Subscription {
    subscribers: Arc<PublishSubscribers>,
    wake: Arc<dyn PublishWake>,
}

struct ChannelWake(flume::Sender<()>);

impl PublishWake for ChannelWake {
    fn wake(&self) {
        let _ = self.0.try_send(());
    }
}

impl Subscription {
    fn new(subscribers: Arc<PublishSubscribers>, sender: flume::Sender<()>) -> Self {
        let wake: Arc<dyn PublishWake> = Arc::new(ChannelWake(sender));
        subscribers.subscribe(wake.clone());
        Self { subscribers, wake }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.subscribers.unsubscribe(&self.wake);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// A viewer token for `pipeline_id` and its expiry (Unix seconds), for the
/// authenticated mint endpoint.
pub(crate) fn mint_token(pipeline_id: &str) -> (String, u64) {
    token::mint(token::process_key(), pipeline_id, unix_now())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
