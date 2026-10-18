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
//! Each served pipeline has one broadcast (named by the pipeline id) on its
//! own moq-net origin, fed from the pipeline's source ring by one local
//! pump task: the ring producer's publication wake reaches it over a
//! one-slot channel, so it reads each unit once whatever the audience.
//! moq-net fans the frames out as shared `Bytes`, and the origin keeps
//! about two GOPs ([`super::budget`]). The broadcast stops with its last
//! viewer.

pub(crate) mod token;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::future::select;
use moq_net::origin;
use moq_net::server::Handshake;

use crate::media::egress::feed::ReadBudget;
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::{PublishSubscribers, PublishWake};

use super::egress::media::MediaConverter;
use super::egress::publish::{HangBroadcast, Pump, RingPublisher, live_start};
use super::egress::{MoqPublishStartup, publish_startup};
use super::transport::{self, Accepted};

pub(crate) use token::TokenError;

/// Units and bytes one pump batch reads before yielding to the sessions.
const PUMP_BATCH_UNITS: usize = 64;
const PUMP_BATCH_BYTES: usize = 1 << 20;
/// Application close code (moq-net `App`) for a full origin.
const ORIGIN_FULL_CODE: u16 = 1;

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

/// One served pipeline: its moq-net origin and the pump task.
struct Served {
    origin: origin::Producer,
    viewers: Cell<usize>,
    /// Pump + origin driver; dropping it stops the broadcast.
    _task: compio::runtime::JoinHandle<()>,
}

pub(crate) struct Origin {
    engine: Arc<MediaEngine>,
    tokio: tokio::runtime::Handle,
    slots: ViewerSlots,
    served: RefCell<HashMap<String, Rc<Served>>>,
}

impl Origin {
    pub(crate) fn new(
        engine: Arc<MediaEngine>,
        tokio: tokio::runtime::Handle,
        max_viewers: usize,
    ) -> Rc<Self> {
        Rc::new(Self {
            engine,
            tokio,
            slots: ViewerSlots::new(max_viewers),
            served: RefCell::default(),
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
        // Detaches on every exit, including cancellation at shutdown.
        let _viewer = Viewer {
            origin: self.clone(),
            pipeline_id: pipeline_id.clone(),
        };
        let (_session, driver) = match handshake.with_publisher(&served.origin).ok().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::debug!(pipeline_id = pipeline_id.as_str(), %error, "MoQ viewer handshake failed");
                return;
            }
        };
        let error = transport::run_driver(driver).await;
        tracing::debug!(pipeline_id = pipeline_id.as_str(), %error, "MoQ viewer left");
        drop(grant);
    }

    /// The pipeline's served broadcast, started for its first viewer. A
    /// pipeline the engine does not know (deleted after the token was
    /// minted) is refused rather than given a new ring.
    async fn attach(self: Rc<Self>, pipeline_id: &str) -> Result<Rc<Served>, String> {
        if let Some(served) = self.attached(pipeline_id) {
            return Ok(served);
        }
        let engine = self.engine.clone();
        let id = pipeline_id.to_string();
        let found = self
            .tokio
            .spawn(async move {
                let ring = engine.ingests.pipelines.read().await.get(&id).cloned()?;
                let startup = publish_startup(&engine, &id, &ring).await;
                Some((ring, startup))
            })
            .await
            .map_err(|error| format!("pipeline lookup failed: {error}"))?;
        let (ring, startup) = found.ok_or("no such pipeline")?;
        // Another viewer may have started it while this one waited.
        if let Some(served) = self.attached(pipeline_id) {
            return Ok(served);
        }
        let served = Rc::new(start(pipeline_id, ring, &startup)?);
        self.served
            .borrow_mut()
            .insert(pipeline_id.to_string(), served.clone());
        Ok(served)
    }

    fn attached(&self, pipeline_id: &str) -> Option<Rc<Served>> {
        let served = self.served.borrow().get(pipeline_id).cloned()?;
        served.viewers.set(served.viewers.get() + 1);
        Some(served)
    }

    fn detach(&self, pipeline_id: &str) {
        let mut served = self.served.borrow_mut();
        let last = served.get(pipeline_id).is_some_and(|served| {
            served.viewers.set(served.viewers.get().saturating_sub(1));
            served.viewers.get() == 0
        });
        if last {
            served.remove(pipeline_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn viewers(&self, pipeline_id: &str) -> usize {
        self.served
            .borrow()
            .get(pipeline_id)
            .map_or(0, |served| served.viewers.get())
    }

    #[cfg(test)]
    pub(crate) fn slots_in_use(&self) -> usize {
        self.slots.in_use()
    }
}

/// A viewer attached to a served pipeline; detaches on drop.
struct Viewer {
    origin: Rc<Origin>,
    pipeline_id: String,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.origin.detach(&self.pipeline_id);
    }
}

/// Start serving `ring` as broadcast `pipeline_id`, with one viewer. A
/// pipeline with no media yet is refused: its broadcast's track set is fixed
/// while it is served, so a trackless one would never play.
fn start(
    pipeline_id: &str,
    ring: Arc<crate::media::ring_buffer::RingBuffer>,
    startup: &MoqPublishStartup,
) -> Result<Served, String> {
    let converter = MediaConverter::new(startup);
    if !converter.has_video() && !converter.has_audio() {
        return Err("the pipeline has no media yet".to_string());
    }
    let (origin, driver, budget) = super::budget::origin();
    let broadcast = HangBroadcast::create(
        &origin,
        budget,
        pipeline_id,
        converter.has_video(),
        converter.has_audio(),
    )
    .map_err(|error| format!("MoQ broadcast: {error}"))?;
    let publisher = RingPublisher::new(converter, broadcast);
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    let (wake_tx, wake_rx) = flume::bounded(1);
    let wake = Subscription::new(feed.publication_ring().publication_subscribers(), wake_tx);
    let task = compio::runtime::spawn(async move {
        let _wake = wake;
        let pump = pin!(pump(feed, publisher, wake_rx));
        let origin_run = pin!(transport::run_driver(driver));
        let _ = select(pump, origin_run).await;
    });
    Ok(Served {
        origin,
        viewers: Cell::new(1),
        _task: task,
    })
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
