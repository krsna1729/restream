//! One MoQ egress leaf: a QUIC connection to a relay over which a moq-net
//! client (moq-lite or IETF MOQT, whichever the relay negotiates) publishes
//! one broadcast.
//!
//! The connection lives in a local Compio task on the shard thread
//! ([`spawn_link`]): resolve → QUIC (+TLS) → `Client::connect` →
//! drive the session until it ends. The task reports its state through a
//! [`LeafLink`]; the shard's visits ([`MoqEngine`]) read the ring and write
//! the broadcast, which moq-net sends from that task. Writes never block:
//! a relay that reads slower than live loses whole old groups inside
//! moq-net (no group outlives the next), so the ring cursor never lags.
//! A dead relay ends the QUIC connection (idle timeout), which ends the
//! leaf as an unexpected termination; an explicit Remove finishes the
//! broadcast, drains the session (bounded) and closes with code 0.

use std::cell::{Cell, RefCell};
use std::future::{Future, pending, poll_fn};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::pin::pin;
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use futures_util::future::{Either, LocalBoxFuture, select};
use moq_net::{Client, origin};
use web_transport_trait::poll::Session as _;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability, WaitCondition,
};
use crate::media::egress::feed::{FeedCursor, ReadBudget};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::leaf_arena::LeafKey;
use crate::media::egress::policy::WorkBudget;
use crate::media::moq::transport;
use restream_moq::tls;
use restream_srt::address_family::AddressFamily;

use super::publish::{Pump, RingPublisher};
use restream_moq::url::MoqUrl;

/// Opens the transport session a leaf publishes over. Production is
/// [`QuicConnector`]; tests inject sessions of their own.
pub(crate) trait MoqConnector: 'static {
    type Session: moq_net::transport::poll::Boxable;

    fn connect(
        self: Rc<Self>,
        url: MoqUrl,
        extra_ca_pem: Option<Vec<u8>>,
    ) -> LocalBoxFuture<'static, Result<Self::Session, String>>;
}

/// Native-QUIC connections from one shard: one client endpoint per address
/// family, created on first use.
#[derive(Default)]
pub(crate) struct QuicConnector {
    endpoints: RefCell<[Option<compio::quic::Endpoint>; 2]>,
}

impl QuicConnector {
    async fn endpoint(&self, family: AddressFamily) -> Result<compio::quic::Endpoint, String> {
        let slot = usize::from(family == AddressFamily::V6);
        if let Some(endpoint) = self.endpoints.borrow()[slot].clone() {
            return Ok(endpoint);
        }
        let endpoint = transport::client_endpoint(family)
            .await
            .map_err(|error| format!("MoQ client endpoint: {error}"))?;
        Ok(self.endpoints.borrow_mut()[slot]
            .get_or_insert(endpoint)
            .clone())
    }
}

impl MoqConnector for QuicConnector {
    type Session = transport::MoqSession;

    fn connect(
        self: Rc<Self>,
        url: MoqUrl,
        extra_ca_pem: Option<Vec<u8>>,
    ) -> LocalBoxFuture<'static, Result<Self::Session, String>> {
        Box::pin(async move {
            let alpns = match &url.version {
                Some(version) => std::slice::from_ref(version),
                None => transport::MOQ_ALPNS,
            };
            let tls = tls::client_config(alpns, extra_ca_pem.as_deref(), url.insecure)?;
            let addr = resolve(&url.host, url.port).await?;
            let family = if addr.is_ipv4() {
                AddressFamily::V4
            } else {
                AddressFamily::V6
            };
            let endpoint = self.endpoint(family).await?;
            transport::connect(&endpoint, addr, &url.host, tls)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// The relay address: an IP literal, else the first resolved address (on
/// Compio's blocking pool, never the shard thread).
async fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let owned = host.to_string();
    compio::runtime::spawn_blocking(move || {
        (owned.as_str(), port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
    })
    .await
    .ok()
    .flatten()
    .ok_or_else(|| format!("MoQ relay host {host} did not resolve"))
}

/// Leaves whose link changed state, and the waker of the shard's idle
/// wait. Shard-thread only.
#[derive(Default)]
pub(crate) struct Activity {
    changed: RefCell<Vec<LeafKey>>,
    waker: RefCell<Option<Waker>>,
}

impl Activity {
    fn notify(&self, key: LeafKey) {
        self.changed.borrow_mut().push(key);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }

    /// Move the changed leaves into `out`.
    pub(crate) fn take(&self, out: &mut Vec<LeafKey>) {
        out.append(&mut self.changed.borrow_mut());
    }

    pub(crate) fn is_pending(&self) -> bool {
        !self.changed.borrow().is_empty()
    }

    /// Resolves once some leaf changed state.
    pub(crate) fn wait(&self) -> impl Future<Output = ()> + '_ {
        poll_fn(|cx| {
            if self.is_pending() {
                Poll::Ready(())
            } else {
                *self.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkState {
    Connecting,
    Active,
    Closed(String),
}

/// What the connection task and the shard share about one leaf.
pub(crate) struct LeafLink {
    key: LeafKey,
    state: RefCell<LinkState>,
    activity: Rc<Activity>,
    removed: Cell<bool>,
    removed_waker: RefCell<Option<Waker>>,
}

impl LeafLink {
    pub(crate) fn new(key: LeafKey, activity: Rc<Activity>) -> Rc<Self> {
        Rc::new(Self {
            key,
            state: RefCell::new(LinkState::Connecting),
            activity,
            removed: Cell::new(false),
            removed_waker: RefCell::new(None),
        })
    }

    pub(crate) fn state(&self) -> LinkState {
        self.state.borrow().clone()
    }

    fn set(&self, state: LinkState) {
        let mut current = self.state.borrow_mut();
        if matches!(*current, LinkState::Closed(_)) {
            return;
        }
        *current = state;
        drop(current);
        self.activity.notify(self.key);
    }

    /// The application removed the output: drain and close.
    pub(crate) fn remove(&self) {
        self.removed.set(true);
        if let Some(waker) = self.removed_waker.borrow_mut().take() {
            waker.wake();
        }
    }

    fn removed(&self) -> impl Future<Output = ()> + '_ {
        poll_fn(|cx| {
            if self.removed.get() {
                Poll::Ready(())
            } else {
                *self.removed_waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
    }
}

/// What the connection task needs.
pub(crate) struct LinkPlan<C> {
    pub(crate) connector: Rc<C>,
    pub(crate) url: MoqUrl,
    pub(crate) extra_ca_pem: Option<Vec<u8>>,
    pub(crate) origin: origin::Producer,
    pub(crate) origin_driver: origin::Driver,
    /// Bound on the drain after an explicit Remove.
    pub(crate) drain_timeout: Duration,
}

/// Spawn the leaf's connection task on the current Compio runtime. Dropping
/// the handle cancels it (and with it the session); a removed leaf's task
/// is detached so its drain can finish.
pub(crate) fn spawn_link<C: MoqConnector>(
    plan: LinkPlan<C>,
    link: Rc<LeafLink>,
) -> compio::runtime::JoinHandle<()> {
    compio::runtime::spawn(async move {
        let LinkPlan {
            connector,
            url,
            extra_ca_pem,
            origin,
            origin_driver,
            drain_timeout,
        } = plan;
        let session = pin!(publish(
            connector,
            url,
            extra_ca_pem,
            &origin,
            &link,
            drain_timeout
        ));
        let origin_run = pin!(transport::run_driver(origin_driver));
        let reason = match select(session, origin_run).await {
            Either::Left((reason, _)) => reason,
            Either::Right((error, _)) => format!("MoQ broadcast origin ended: {error}"),
        };
        link.set(LinkState::Closed(reason));
    })
}

/// Connect, run the MoQ handshake (the version the ALPN negotiated), and
/// drive the session; returns why it ended.
async fn publish<C: MoqConnector>(
    connector: Rc<C>,
    url: MoqUrl,
    extra_ca_pem: Option<Vec<u8>>,
    origin: &origin::Producer,
    link: &LeafLink,
    drain_timeout: Duration,
) -> String {
    let path = url.path.clone();
    let session = match connector.connect(url, extra_ca_pem).await {
        Ok(session) => session,
        Err(reason) => return reason,
    };
    let mut closer = session.clone();
    let client = Client::new().with_publisher(origin).with_path(path);
    let (moq, driver) = match client.connect(Instant::now(), session).await {
        Ok(connected) => connected,
        Err(error) => return format!("MoQ handshake failed: {error}"),
    };
    link.set(LinkState::Active);
    let run = pin!(transport::run_driver(driver));
    // Holds the moq-net session handle for the connection's life: its last
    // handle dropping would cancel the session.
    let removal = pin!(async move {
        link.removed().await;
        let _ = compio::time::timeout(drain_timeout, moq.close()).await;
        closer.close(0, "output removed");
        pending::<()>().await;
    });
    match select(run, removal).await {
        Either::Left((error, _)) => format!("MoQ session ended: {error}"),
        Either::Right(((), _)) => "MoQ session removed".to_string(),
    }
}

/// The per-visit side of a leaf: ring → broadcast once the link is up.
pub(crate) struct MoqEngine {
    publisher: RingPublisher,
    active_reported: bool,
}

impl MoqEngine {
    pub(crate) fn new(publisher: RingPublisher) -> Self {
        Self {
            publisher,
            active_reported: false,
        }
    }
}

impl ProtocolEngine for MoqEngine {
    type Feed = RingFeed;
    type Transport = Rc<LeafLink>;

    fn advance(
        &mut self,
        link: &mut Rc<LeafLink>,
        _readiness: Readiness,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        match link.state() {
            LinkState::Connecting => {
                return EngineProgress::Needs(WaitCondition::Io(Interest::WRITE));
            }
            LinkState::Closed(detail) => {
                return EngineProgress::Failed(ProtocolFailure {
                    reason: "moq_session_closed",
                    detail,
                    retryable: true,
                });
            }
            LinkState::Active => {}
        }
        if !self.active_reported {
            // Re-anchors the cursor at the latest sync point: the
            // broadcast starts on a keyframe.
            self.active_reported = true;
            return EngineProgress::HandshakeComplete;
        }
        match self.publisher.pump(
            feed,
            cursor,
            ReadBudget::new(budget.max_units(), budget.max_bytes()),
        ) {
            Pump::Sent { bytes, units } => EngineProgress::Progress {
                bytes,
                units,
                wait: WaitCondition::Feed,
            },
            Pump::Empty => EngineProgress::Needs(WaitCondition::Feed),
            Pump::Overrun => EngineProgress::FeedOverrun,
            Pump::Failed(detail) => EngineProgress::Failed(ProtocolFailure {
                reason: "moq_broadcast_closed",
                detail,
                retryable: true,
            }),
        }
    }

    fn close(&mut self, link: &mut Rc<LeafLink>, _reason: CloseReason) {
        self.publisher.broadcast_mut().finish();
        link.remove();
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::InPlaceResync
    }
}
