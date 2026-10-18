//! One MoQ publisher session on the listener thread (moq-lite + hang).
//!
//! The session accepts the publisher's announcements into a private origin,
//! takes the first announced broadcast, names the stream key from its path
//! ([`stream_key`]), asks Tokio to admit it, reads `catalog.json`, subscribes
//! to the tracks Restream can carry ([`TrackSet`]) and feeds every frame
//! through [`Ingest`] into the pipeline ring. Peer input never panics: a
//! fault ends the session with a [`SessionEnd`] whose reason reaches the
//! logs and the input's disconnect record, and whose error the peer sees in
//! the close.

use std::cell::{Cell, RefCell};
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use moq_net::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::media::{MoqProbe, TrackSet};
use super::publisher::PublisherMedia;
use super::{Admission, Lifecycle, PublisherSession, Rejection, stream_key_from_path};
use crate::media::moq::budget::{self, CacheBudget};
use crate::media::moq::media::from_hang_catalog;
use crate::media::moq::transport;
use crate::media::packet::MediaPacket;
use crate::media::ring_buffer::RingBuffer;

/// Bounds on one publisher session; passing one ends the session.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SessionLimits {
    /// From the handshake to the first broadcast announcement.
    pub(crate) announce_timeout: Duration,
    /// From admission to the first catalog.
    pub(crate) catalog_timeout: Duration,
    /// Without any frame once the tracks are subscribed.
    pub(crate) idle_timeout: Duration,
    /// The largest frame (legacy payload or CMAF fragment) accepted.
    pub(crate) max_frame_bytes: usize,
    /// Malformed frames in a row before the publisher is cut off.
    pub(crate) max_malformed_run: u32,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            announce_timeout: Duration::from_secs(10),
            catalog_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(20),
            max_frame_bytes: 16 << 20,
            max_malformed_run: 64,
        }
    }
}

/// Why a session ended: `reason` for logs and the disconnect record,
/// `error` for the close the peer sees.
#[derive(Debug, Clone)]
pub(crate) struct SessionEnd {
    pub(crate) reason: String,
    pub(crate) error: Error,
}

impl SessionEnd {
    pub(crate) fn new(error: Error, reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            error,
        }
    }

    /// A moq-net failure, with what was being done.
    pub(crate) fn moq(context: &str, error: Error) -> Self {
        let reason = format!("{context}: {error}");
        Self::new(error, reason)
    }

    pub(crate) fn rejected(rejection: &Rejection) -> Self {
        let error = match rejection {
            Rejection::Banned | Rejection::Unauthorized => Error::Unauthorized,
            Rejection::Duplicate => Error::Duplicate,
            Rejection::Unavailable(_) => Error::Unroutable,
        };
        Self::new(error, format!("publisher refused: {}", rejection.reason()))
    }
}

/// The stream key a publisher names: the last segment of the connection
/// path (query dropped) when it has one, else the last segment of the
/// announced broadcast path (`.hang` dropped). So `https://host/<key>` works
/// whatever the broadcast is called, and `https://host/` works with broadcast
/// `<key>` or `<scope>/<key>.hang`.
pub(crate) fn stream_key(connection_path: &str, broadcast: &str) -> Option<String> {
    stream_key_from_path(connection_path)
        .or_else(|| stream_key_from_path(broadcast))
        .map(str::to_string)
}

/// An admitted publisher's media: track conversion, the probe and the ring
/// push. Ends the input registration exactly once, when finished or dropped
/// (a listener torn down mid-session still releases the input).
pub(crate) struct Ingest<L: Lifecycle> {
    lifecycle: L,
    session: Option<PublisherSession>,
    media: PublisherMedia,
    tracks: TrackSet,
    limits: SessionLimits,
    malformed_run: u32,
    probe_sent: bool,
    packets: Vec<MediaPacket>,
}

impl<L: Lifecycle> Ingest<L> {
    pub(crate) fn new(lifecycle: L, admission: Admission, limits: SessionLimits) -> Self {
        Self {
            lifecycle,
            session: Some(admission.session),
            media: admission.media,
            tracks: TrackSet::default(),
            limits,
            malformed_run: 0,
            probe_sent: false,
            packets: Vec::with_capacity(4),
        }
    }

    /// Take the catalog's tracks; returns their names to subscribe to, in
    /// [`Self::frame`]'s index order. A catalog with nothing Restream can
    /// carry ends the session.
    pub(crate) fn set_tracks(&mut self, tracks: TrackSet) -> Result<Vec<String>, SessionEnd> {
        for (name, reason) in tracks.rejected() {
            info!(
                event_class = "media",
                event_type = "restream.moq.ingest.track_rejected",
                track = %name,
                reason = %reason,
                "MoQ publisher track not ingested",
            );
        }
        if tracks.is_empty() {
            return Err(SessionEnd::new(
                Error::Unsupported,
                "the catalog has no H.264, H.265 or AAC track Restream can ingest",
            ));
        }
        self.tracks = tracks;
        Ok(self
            .tracks
            .tracks()
            .iter()
            .map(|track| track.name.clone())
            .collect())
    }

    /// The probe to apply, the first time the tracks know their metadata;
    /// packets are held from now until [`Self::probe_applied`].
    pub(crate) fn ready_probe(&mut self) -> Option<(PublisherSession, MoqProbe)> {
        if self.probe_sent {
            return None;
        }
        let probe = self.tracks.probe()?;
        self.probe_sent = true;
        self.media.begin_probe();
        Some((self.session.clone()?, probe))
    }

    pub(crate) fn probe_applied(&mut self, ring: Option<Arc<RingBuffer>>) {
        self.media.probe_applied(ring);
    }

    /// One frame of track `index` into the ring; returns the probe to apply
    /// once it is complete. An oversized frame, or too many malformed ones
    /// in a row, ends the session; a single malformed frame is dropped.
    pub(crate) fn frame(
        &mut self,
        index: usize,
        frame: Bytes,
    ) -> Result<Option<(PublisherSession, MoqProbe)>, SessionEnd> {
        let len = frame.len();
        if len > self.limits.max_frame_bytes {
            return Err(SessionEnd::new(
                Error::FrameTooLarge,
                format!(
                    "a {len}-byte frame exceeds the {}-byte bound",
                    self.limits.max_frame_bytes
                ),
            ));
        }
        self.media.note_received(len);
        let Some(track) = self.tracks.track_mut(index) else {
            return Ok(None);
        };
        if let Err(error) = track.push(frame, &mut self.packets) {
            self.malformed_run += 1;
            if self.malformed_run >= self.limits.max_malformed_run {
                return Err(SessionEnd::new(
                    Error::ProtocolViolation,
                    format!(
                        "{} malformed frames in a row; last: {error}",
                        self.malformed_run
                    ),
                ));
            }
            debug!(track = %track.name, %error, "dropped a malformed MoQ frame");
            return Ok(None);
        }
        self.malformed_run = 0;
        let probe = self.ready_probe();
        for packet in self.packets.drain(..) {
            self.media.publish(packet);
        }
        Ok(probe)
    }

    /// Flush the media and end the input registration with `reason`.
    pub(crate) fn finish(mut self, reason: String) {
        self.release(reason);
    }

    fn release(&mut self, reason: String) {
        if let Some(session) = self.session.take() {
            self.media.flush();
            self.lifecycle.finish(session, reason);
        }
    }
}

impl<L: Lifecycle> Drop for Ingest<L> {
    fn drop(&mut self) {
        self.release("the MoQ ingest listener stopped".to_string());
    }
}

/// How long a closing session's driver keeps running so the close reaches
/// the peer.
const CLOSE_FLUSH: Duration = Duration::from_millis(500);

/// Run one publisher session to its end: complete the paused handshake with
/// a private origin for its announcements, then ingest the first announced
/// broadcast. Returns why it ended; the peer has been told. The origin's
/// cache is budgeted like a published broadcast's ([`budget`]).
pub(crate) async fn publish<L, S>(
    lifecycle: &L,
    peer: SocketAddr,
    connection_path: &str,
    handshake: moq_net::server::Handshake<S>,
    limits: SessionLimits,
    shutdown: &CancellationToken,
) -> SessionEnd
where
    L: Lifecycle + Clone,
    S: moq_net::transport::poll::Session,
{
    let (origin, origin_driver, budget) = budget::origin();
    let budget = RefCell::new(budget);
    let (session, driver) = match handshake.with_subscriber(origin.clone()).ok().await {
        Ok(started) => started,
        Err(error) => return SessionEnd::moq("MoQ handshake failed", error),
    };
    let slot = RefCell::new(None);
    let mut session_driver = pin!(transport::run_driver(driver));
    let mut origin_driver = pin!(transport::run_driver(origin_driver));
    let mut driver_done = false;
    let end = tokio::select! {
        biased;
        () = shutdown.cancelled() => SessionEnd::new(Error::Cancel, "Restream is shutting down"),
        error = &mut session_driver => {
            driver_done = true;
            SessionEnd::moq("MoQ session closed", error)
        }
        error = &mut origin_driver => SessionEnd::moq("MoQ origin stopped", error),
        end = ingest(lifecycle, peer, connection_path, &origin, &slot, &budget, limits) => end,
    };
    if let Some(ingest) = slot.take() {
        ingest.finish(end.reason.clone());
    }
    session.abort(end.error.clone());
    if !driver_done {
        let _ = compio::time::timeout(CLOSE_FLUSH, &mut session_driver).await;
    }
    end
}

/// Announcement → key → admission → catalog → tracks → frames.
async fn ingest<L: Lifecycle + Clone>(
    lifecycle: &L,
    peer: SocketAddr,
    connection_path: &str,
    origin: &moq_net::origin::Producer,
    slot: &RefCell<Option<Ingest<L>>>,
    budget: &RefCell<CacheBudget>,
    limits: SessionLimits,
) -> SessionEnd {
    let consumer = origin.consume();
    let mut announced = consumer.announced();
    let broadcast = loop {
        match compio::time::timeout(limits.announce_timeout, announced.next()).await {
            Err(_) => return SessionEnd::new(Error::Timeout, "no broadcast was announced"),
            Ok(None) => {
                return SessionEnd::new(Error::Closed, "the publisher left before announcing");
            }
            Ok(Some(update)) if update.kind.is_active() => break update.prefix,
            Ok(Some(_)) => {}
        }
    };
    let Some(key) = stream_key(connection_path, broadcast.as_str()) else {
        return SessionEnd::new(
            Error::Unauthorized,
            format!("broadcast {:?} names no stream key", broadcast.as_str()),
        );
    };
    let admission = match lifecycle.admit(peer, &key).await {
        Ok(admission) => admission,
        Err(rejection) => return SessionEnd::rejected(&rejection),
    };
    *slot.borrow_mut() = Some(Ingest::new(lifecycle.clone(), admission, limits));

    let broadcast = match compio::time::timeout(
        limits.catalog_timeout,
        consumer.routed_broadcast(broadcast.as_str()),
    )
    .await
    {
        Err(_) => return SessionEnd::new(Error::Timeout, "the announced broadcast never resolved"),
        Ok(Err(error)) => return SessionEnd::moq("broadcast", error),
        Ok(Ok(broadcast)) => broadcast,
    };
    let catalog = match first_catalog(
        async {
            hang::Catalog::<()>::subscribe(&broadcast)
                .await?
                .next()
                .await
        },
        limits.catalog_timeout,
    )
    .await
    {
        Ok(catalog) => catalog,
        Err(end) => return end,
    };
    let tracks = TrackSet::new(from_hang_catalog(&catalog));
    let (names, probe) = {
        let mut slot = slot.borrow_mut();
        let Some(ingest) = slot.as_mut() else {
            return SessionEnd::new(Error::Cancel, "the session was released");
        };
        match ingest.set_tracks(tracks) {
            Ok(names) => (names, ingest.ready_probe()),
            Err(end) => return end,
        }
    };
    if let Some((session, probe)) = probe {
        apply_probe(lifecycle, slot, &session, probe).await;
    }

    let last_frame = Cell::new(Instant::now());
    let mut pumps = FuturesUnordered::new();
    for (index, name) in names.into_iter().enumerate() {
        let subscriber = match broadcast.track(&name) {
            Ok(track) => track.subscribe(None::<moq_net::track::Subscription>).await,
            Err(error) => Err(error),
        };
        match subscriber {
            Ok(subscriber) => pumps.push(pump(
                index,
                subscriber.ordered(),
                lifecycle,
                slot,
                budget,
                &last_frame,
            )),
            Err(error) => return SessionEnd::moq(&format!("subscribe to track {name:?}"), error),
        }
    }
    let mut idle = pin!(idle_watch(&last_frame, limits.idle_timeout));
    loop {
        tokio::select! {
            end = &mut idle => return end,
            next = pumps.next() => match next {
                Some(Some(end)) => return end,
                Some(None) => {}
                None => return SessionEnd::new(Error::Closed, "the publisher finished every track"),
            },
        }
    }
}

/// The publisher's first `catalog.json`, as `read` yields it within
/// `timeout`. hang parses it from peer bytes, so this is the session's panic
/// boundary for that parser (hang 0.21.16 sliced a non-ASCII H.264 codec
/// string off a char boundary): a panic ends the session as a protocol
/// violation, like a catalog hang refuses, and the caller releases the input
/// with that reason.
async fn first_catalog(
    read: impl Future<Output = hang::Result<Option<hang::Catalog>>>,
    timeout: Duration,
) -> Result<hang::Catalog, SessionEnd> {
    let read = futures_util::FutureExt::catch_unwind(AssertUnwindSafe(read));
    match compio::time::timeout(timeout, read).await {
        Err(_) => Err(SessionEnd::new(Error::Timeout, "no catalog.json arrived")),
        Ok(Err(payload)) => {
            let panic = crate::panic_boundary::record_contained(payload.as_ref());
            Err(SessionEnd::new(
                Error::ProtocolViolation,
                format!("catalog.json: the parser panicked: {panic}"),
            ))
        }
        Ok(Ok(Err(error))) => Err(SessionEnd::new(
            Error::ProtocolViolation,
            format!("catalog.json: {error}"),
        )),
        Ok(Ok(Ok(None))) => Err(SessionEnd::new(Error::Closed, "catalog.json ended empty")),
        Ok(Ok(Ok(Some(catalog)))) => Ok(catalog),
    }
}

async fn apply_probe<L: Lifecycle>(
    lifecycle: &L,
    slot: &RefCell<Option<Ingest<L>>>,
    session: &PublisherSession,
    probe: MoqProbe,
) {
    let ring = lifecycle.apply_probe(session, probe).await;
    if let Some(ingest) = slot.borrow_mut().as_mut() {
        ingest.probe_applied(ring);
    }
}

/// One track's groups and frames into [`Ingest::frame`]. `None` when the
/// publisher finished the track; a group the network cut short is skipped.
async fn pump<L: Lifecycle>(
    index: usize,
    mut track: moq_net::track::Ordered,
    lifecycle: &L,
    slot: &RefCell<Option<Ingest<L>>>,
    budget: &RefCell<CacheBudget>,
    last_frame: &Cell<Instant>,
) -> Option<SessionEnd> {
    loop {
        let mut group = match track.next_group().await {
            Ok(Some(group)) => {
                budget.borrow_mut().group(index);
                group
            }
            Ok(None) => return None,
            Err(error) => return Some(SessionEnd::moq("track", error)),
        };
        loop {
            let frame = match group.read_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    debug!(%error, "MoQ group ended early");
                    break;
                }
            };
            last_frame.set(Instant::now());
            budget.borrow_mut().frame(index, frame.payload.len());
            let step = slot
                .borrow_mut()
                .as_mut()
                .map(|ingest| ingest.frame(index, frame.payload));
            match step {
                None => return Some(SessionEnd::new(Error::Cancel, "the session was released")),
                Some(Err(end)) => return Some(end),
                Some(Ok(None)) => {}
                Some(Ok(Some((session, probe)))) => {
                    apply_probe(lifecycle, slot, &session, probe).await;
                }
            }
        }
    }
}

async fn idle_watch(last_frame: &Cell<Instant>, idle: Duration) -> SessionEnd {
    loop {
        let deadline = last_frame.get() + idle;
        if Instant::now() >= deadline {
            return SessionEnd::new(Error::Timeout, format!("no media for {} s", idle.as_secs()));
        }
        compio::time::sleep_until(deadline).await;
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
