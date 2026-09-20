//! The SRT ingress owner thread: ONE Compio runtime and ONE
//! `srt_transport::compio::Owner` that owns the listener UDP socket and every
//! protocol object behind it (handshake admission, timers, ACK/NAK, listener
//! TX, receive state, per-peer send/disconnect/retire).
//!
//! Tokio owns the control plane and the media application state. The two sides
//! meet only through two bounded bridges:
//!
//! * `IngressCommand` (Tokio -> Owner): `Send`, `Disconnect`, `Shutdown`, all
//!   addressed by `LogicalPeerId`. A `LogicalPeerId` is the sole cross-thread
//!   session handle; no protocol object, table reference, socket id or
//!   `SocketAddr` identifies a session.
//! * `SrtIngressEvent` (Owner -> Tokio): `Connected`, `Media`, `Disconnected`,
//!   plus the terminal `Fault`.
//!
//! Everything here runs on the owner thread. The runtime and the Owner are
//! `!Send`; they are built, driven and dropped on this thread and nothing
//! protocol-shaped crosses to Tokio.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::future::{Either, pending, select};
use srt_proto::{ConnectionEvent, Timestamp};
use srt_transport::advanced::admission::{AdmissionEvent, BondedInputPolicy, LogicalPeerId};
use srt_transport::compio::{
    Owner, OwnerRxMode, OwnerServiceBudget, OwnerServiceReport, ProductionRuntimeConfig,
    RxModePolicy, observe_production_runtime,
};
use srt_transport::{ListenerConfig, ListenerTopology, PromotionPolicy};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::media::egress::backends::srt::owner_set::{
    SRT_OWNER_WIRE_CEILING, production_runtime, substrate_diagnosis,
};
use crate::media::snapshots::ListenerSocketStats;

use super::ingress_admission::{ReceiverGroupId, ingress_resolver};
use super::srt_policy::SrtIngestPolicyStore;

/// Concurrent datagram sends (TX pool slots and lanes) for the ingress Owner.
/// Ingress TX is protocol replies (handshake, ACK/NAK, SHUTDOWN) plus SRT
/// read/play payloads. Unsent output waits in bounded protocol state.
pub(crate) const INGRESS_TX_CAPACITY: usize = 64;

/// Tokio -> Owner command bridge capacity.
pub(crate) const INGRESS_COMMAND_CAPACITY: usize = 256;

/// Owner -> Tokio event bridge capacity.
pub(crate) const INGRESS_EVENT_CAPACITY: usize = 256;

/// Commands applied per owner-loop visit. The Owner is serviced between
/// visits, so a busy reader cannot starve RX, timers, ACK/NAK or the other
/// peers; the loop never drains commands to quiescence.
pub(crate) const COMMANDS_PER_VISIT: usize = 32;

/// Read/play fragments one peer may have waiting for send-window room before
/// that peer is explicitly disconnected as overloaded.
const DEFERRED_PER_PEER: usize = 32;

/// Deferred read/play fragments across all peers. Exceeding it disconnects the
/// peer that would exceed it; it never blocks other peers' commands.
const DEFERRED_TOTAL: usize = 1024;

/// Longest the owner parks when nothing is due. Commands, Owner activity and
/// event-bridge capacity all wake it earlier; protocol deadlines shorten it.
const IDLE_PARK: Duration = Duration::from_secs(1);

/// How long a locally-disconnected peer may keep its SHUTDOWN in flight before
/// it is retired regardless of a terminal event.
const CLOSING_GRACE: Duration = Duration::from_millis(500);

/// Closing peers examined per visit.
const CLOSING_CHECKS_PER_VISIT: usize = 32;

/// Finite bounds for the orderly shutdown.
const SHUTDOWN_FLUSH_VISITS: u32 = 20;
const SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(2);

pub(crate) enum IngressCommand {
    /// Send one SRT message payload to a connected reader peer.
    Send {
        logical_peer: LogicalPeerId,
        payload: Bytes,
    },
    /// Begin an orderly protocol disconnect of one peer. The owner retires the
    /// peer when its terminal event arrives (or after a short grace).
    Disconnect { logical_peer: LogicalPeerId },
    /// Disconnect nothing further; flush, drain the Owner and exit.
    Shutdown,
}

/// Narrow application vocabulary from the Owner to Tokio.
pub(crate) enum SrtIngressEvent {
    Connected {
        peer: SocketAddr,
        logical_peer: LogicalPeerId,
        stream_id: String,
    },
    Media {
        logical_peer: LogicalPeerId,
        payload: Bytes,
    },
    Disconnected {
        peer: SocketAddr,
        logical_peer: LogicalPeerId,
        reason: String,
    },
    /// The Owner developed an OWNER-FATAL fault; the listener is gone.
    Fault { detail: String },
}

/// What the owner thread reports when it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngressExit {
    /// `shutdown_and_drain` reached quiescence (or there was nothing to drain).
    pub(crate) quiescent: bool,
    pub(crate) fault: Option<String>,
}

/// Why the owner thread could not start.
#[derive(Debug)]
pub(crate) struct IngressStartError(pub(crate) String);

impl std::fmt::Display for IngressStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything the owner thread is built from.
pub(crate) struct IngressConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) policy_store: Arc<SrtIngestPolicyStore>,
    pub(crate) receiver_group: ReceiverGroupId,
    pub(crate) stats: Arc<ListenerSocketStats>,
    pub(crate) command_capacity: usize,
    pub(crate) event_capacity: usize,
}

/// Tokio's handle to the owner thread.
pub(crate) struct SrtIngressHandle {
    pub(crate) events: mpsc::Receiver<SrtIngressEvent>,
    commands: flume::Sender<IngressCommand>,
    thread: Option<std::thread::JoinHandle<IngressExit>>,
    local_addr: SocketAddr,
}

impl SrtIngressHandle {
    /// Build the runtime and the Owner on a new OS thread and wait for its
    /// verdict. A runtime or listener that cannot be built is a typed error;
    /// there is no fallback transport.
    pub(crate) async fn start(config: IngressConfig) -> Result<Self, IngressStartError> {
        let (commands_tx, commands_rx) = flume::bounded(config.command_capacity.max(1));
        let (events_tx, events_rx) = mpsc::channel(config.event_capacity.max(1));
        let (ready_tx, ready_rx) = flume::bounded::<Result<SocketAddr, String>>(1);
        let thread = std::thread::Builder::new()
            // `srt-in-<port>`: unique per listener and short enough that the
            // kernel's 15-byte thread name keeps the whole port.
            .name(format!("srt-in-{}", config.bind.port()))
            .spawn(move || run_owner_thread(config, commands_rx, events_tx, ready_tx))
            .map_err(|error| IngressStartError(format!("spawn SRT ingress thread: {error}")))?;
        match ready_rx.recv_async().await {
            Ok(Ok(local_addr)) => Ok(Self {
                events: events_rx,
                commands: commands_tx,
                thread: Some(thread),
                local_addr,
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(IngressStartError(message))
            }
            Err(_) => {
                let _ = thread.join();
                Err(IngressStartError(
                    "SRT ingress thread exited before reporting readiness".to_string(),
                ))
            }
        }
    }

    /// The address the Owner's listener socket is bound to.
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Offer a command without blocking. `Err(command)` hands it back when the
    /// bounded bridge is full (or the thread is gone), so the caller keeps its
    /// media/session state and retries instead of losing it.
    pub(crate) fn try_send(&self, command: IngressCommand) -> Result<(), IngressCommand> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                flume::TrySendError::Full(command) | flume::TrySendError::Disconnected(command) => {
                    command
                }
            })
    }

    /// Whether the owner thread's command receiver is gone (thread exited).
    pub(crate) fn is_closed(&self) -> bool {
        self.commands.is_disconnected()
    }

    /// Orderly stop: deliver `Shutdown` (bounded wait for bridge room), then
    /// join the owner thread and report its truthful verdict.
    pub(crate) async fn shutdown(mut self) -> IngressExit {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut command = IngressCommand::Shutdown;
        loop {
            match self.commands.try_send(command) {
                Ok(()) => break,
                Err(flume::TrySendError::Disconnected(_)) => break,
                Err(flume::TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    command = back;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
        // Dropping the sender also stops the owner thread if the explicit
        // command could not be queued.
        drop(self.commands);
        let Some(thread) = self.thread.take() else {
            return IngressExit {
                quiescent: true,
                fault: None,
            };
        };
        match tokio::task::spawn_blocking(move || thread.join()).await {
            Ok(Ok(exit)) => exit,
            _ => IngressExit {
                quiescent: false,
                fault: Some("SRT ingress owner thread panicked".to_string()),
            },
        }
    }
}

/// Deferred read/play fragments, per peer, in arrival order.
#[derive(Default)]
struct DeferredSends {
    per_peer: HashMap<LogicalPeerId, VecDeque<Bytes>>,
    total: usize,
}

impl DeferredSends {
    fn len_for(&self, peer: &LogicalPeerId) -> usize {
        self.per_peer.get(peer).map_or(0, VecDeque::len)
    }

    fn push(&mut self, peer: LogicalPeerId, payload: Bytes) {
        self.per_peer.entry(peer).or_default().push_back(payload);
        self.total += 1;
    }

    fn forget(&mut self, peer: &LogicalPeerId) {
        if let Some(queue) = self.per_peer.remove(peer) {
            self.total -= queue.len();
        }
    }
}

struct ClosingPeer {
    peer: LogicalPeerId,
    since: Instant,
}

struct OwnerLoop {
    owner: Owner,
    runtime: compio::runtime::Runtime,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
    stats: Arc<ListenerSocketStats>,
    epoch: Instant,
    /// Translated events waiting for bridge room. Only refilled when empty,
    /// so it is bounded by one Owner drain and accepted media is never lost.
    pending_events: VecDeque<SrtIngressEvent>,
    scratch: Vec<AdmissionEvent>,
    deferred: DeferredSends,
    closing: VecDeque<ClosingPeer>,
    /// Peers the owner has disconnected as overloaded; further sends to them
    /// are discarded (counted) until their terminal event retires them.
    overloaded: std::collections::HashSet<LogicalPeerId>,
    peers: u64,
    /// A command received while parked, applied first on the next visit.
    stash: Option<IngressCommand>,
    shutting_down: bool,
    home_thread: std::thread::ThreadId,
}

fn run_owner_thread(
    config: IngressConfig,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
    ready: flume::Sender<Result<SocketAddr, String>>,
) -> IngressExit {
    match build(config, commands, events) {
        Ok((mut owner_loop, local_addr)) => {
            let _ = ready.send(Ok(local_addr));
            owner_loop.serve()
        }
        Err(message) => {
            error!(error = %message, "SRT ingress Owner failed to start");
            let _ = ready.send(Err(message));
            IngressExit {
                quiescent: true,
                fault: None,
            }
        }
    }
}

fn build(
    config: IngressConfig,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
) -> Result<(OwnerLoop, SocketAddr), String> {
    // The runtime and the Owner are born on this thread and never leave it.
    let runtime_config =
        ProductionRuntimeConfig::for_owner(INGRESS_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    let runtime = production_runtime(runtime_config)?;
    let profile = runtime.block_on(observe_production_runtime(
        &runtime,
        INGRESS_TX_CAPACITY,
        SRT_OWNER_WIRE_CEILING,
    ));
    let substrate = profile.managed_rx_substrate();
    let rx_policy = RxModePolicy::ManagedPreferred;
    let listener_config = ListenerConfig::builder(config.bind)
        .topology(ListenerTopology::PerPort)
        .bonded_inputs(BondedInputPolicy::Accept)
        .configure_transport(|transport| {
            super::apply_optional_udp_buf(transport);
            // The Owner has no relocation target.
            transport.promotion = PromotionPolicy::Never;
        })
        .build()
        .map_err(|error| format!("failed to build srt-rs listener config: {error}"))?;
    let mut owner = Owner::new_with_ceiling(INGRESS_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    owner
        .set_rx_substrate(substrate)
        .map_err(|error| format!("failed to declare RX substrate: {error}"))?;
    owner.set_rx_mode_policy(rx_policy);
    let resolver = ingress_resolver(config.policy_store, config.receiver_group);
    runtime
        .block_on(async { owner.listen_with_resolver(&listener_config, resolver) })
        .map_err(|error| format!("failed to attach srt-rs listener: {error}"))?;
    let local_addr = owner
        .listener_local_addr()
        .ok_or_else(|| "srt-rs listener has no local address".to_string())?;
    let rx_mode = owner.rx_mode();
    info!(
        driver = %profile.driver_type,
        compio = %profile.compio_version,
        io_uring = profile.is_io_uring,
        managed_rx = ?substrate,
        substrate_diagnosis = substrate_diagnosis(substrate),
        rx_mode = ?rx_mode,
        rx_policy = ?rx_policy,
        bind = %local_addr,
        receiver_group_id = format_args!("{:#010x}", config.receiver_group.wire_id()),
        tx_capacity = INGRESS_TX_CAPACITY,
        wire_ceiling = SRT_OWNER_WIRE_CEILING,
        command_capacity = config.command_capacity,
        event_capacity = config.event_capacity,
        commands_per_visit = COMMANDS_PER_VISIT,
        "srt ingress owner attached"
    );
    let stats = config.stats;
    stats.ingress_owner.tx_capacity.store(
        u64::try_from(INGRESS_TX_CAPACITY).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
    stats.ingress_owner.managed_rx.store(
        rx_mode == Some(OwnerRxMode::ManagedMultishot),
        Ordering::Relaxed,
    );
    Ok((
        OwnerLoop {
            owner,
            runtime,
            commands,
            events,
            stats,
            epoch: Instant::now(),
            pending_events: VecDeque::with_capacity(64),
            scratch: Vec::with_capacity(64),
            deferred: DeferredSends::default(),
            closing: VecDeque::new(),
            overloaded: std::collections::HashSet::new(),
            peers: 0,
            stash: None,
            shutting_down: false,
            home_thread: std::thread::current().id(),
        },
        local_addr,
    ))
}

impl OwnerLoop {
    fn timestamp(&self) -> Timestamp {
        Timestamp::from_micros(self.epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
    }

    fn assert_home_thread(&self) {
        debug_assert_eq!(
            std::thread::current().id(),
            self.home_thread,
            "SRT ingress Owner used off its owner thread"
        );
    }

    fn serve(&mut self) -> IngressExit {
        let mut fault = None;
        loop {
            self.assert_home_thread();
            // `block_on` returns as soon as its future is ready without
            // touching the driver, so a loop that never parks would never reap
            // TX completions or receive datagrams. One non-blocking driver
            // poll per visit keeps both flowing (the egress shard's contract).
            self.runtime.poll_with(Some(Duration::ZERO));
            self.runtime.run();

            let now = self.timestamp();
            self.retry_deferred(now);
            let mut inbox_open = true;
            let mut budget = COMMANDS_PER_VISIT;
            if let Some(command) = self.stash.take() {
                self.apply(command, now);
                budget -= 1;
            }
            for _ in 0..budget {
                match self.commands.try_recv() {
                    Ok(command) => self.apply(command, now),
                    Err(flume::TryRecvError::Empty) => break,
                    Err(flume::TryRecvError::Disconnected) => {
                        inbox_open = false;
                        break;
                    }
                }
            }
            if !inbox_open {
                self.shutting_down = true;
            }
            if self.shutting_down {
                break;
            }

            let report = {
                let owner = &mut self.owner;
                self.runtime
                    .block_on(owner.service(now, OwnerServiceBudget::default()))
            };
            self.account_service(&report);
            self.reap_closing();
            if let Some(detail) = self.owner.fault().map(|fault| format!("{fault:?}")) {
                error!(fault = %detail, "SRT ingress Owner faulted; the listener is stopping");
                self.stats
                    .ingress_owner
                    .faulted
                    .store(true, Ordering::Relaxed);
                fault = Some(detail);
                break;
            }
            self.drain_owner_events();
            self.flush_events();

            let busy = report.work_remaining
                || self.stash.is_some()
                || !self.commands.is_empty()
                || (!self.pending_events.is_empty() && self.events_has_room());
            if !busy {
                self.park(now);
            }
        }
        self.finish(fault)
    }

    /// Apply one command to Owner-owned state.
    fn apply(&mut self, command: IngressCommand, now: Timestamp) {
        match command {
            IngressCommand::Send {
                logical_peer,
                payload,
            } => self.send(logical_peer, payload, now),
            IngressCommand::Disconnect { logical_peer } => self.disconnect(logical_peer, now),
            IngressCommand::Shutdown => self.shutting_down = true,
        }
    }

    fn send(&mut self, peer: LogicalPeerId, payload: Bytes, now: Timestamp) {
        if self.overloaded.contains(&peer) {
            // Already being disconnected as overloaded.
            self.stats
                .ingress_owner
                .stale_commands
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Per-peer order: anything already waiting goes first.
        if self.deferred.len_for(&peer) > 0 {
            self.defer(peer, payload, now);
            return;
        }
        match self.try_send_now(&peer, &payload, now) {
            SendOutcome::Sent | SendOutcome::Dropped => {}
            SendOutcome::NoWindow => self.defer(peer, payload, now),
        }
    }

    fn try_send_now(
        &mut self,
        peer: &LogicalPeerId,
        payload: &Bytes,
        now: Timestamp,
    ) -> SendOutcome {
        let Some(mut entry) = self.owner.listener_peer_mut(*peer) else {
            // Retired (terminal event, overload, closing): harmless and counted.
            self.stats
                .ingress_owner
                .stale_commands
                .fetch_add(1, Ordering::Relaxed);
            return SendOutcome::Dropped;
        };
        if !entry.can_send() {
            return SendOutcome::NoWindow;
        }
        match entry.send_shared(payload.clone(), now) {
            Ok(_) => SendOutcome::Sent,
            Err(error) if error.kind == srt_proto::ErrorKind::InvalidState => {
                self.stats
                    .ingress_owner
                    .stale_commands
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Dropped
            }
            Err(error) => {
                warn!(peer = ?peer, %error, payload_len = payload.len(), "SRT reader send failed");
                self.stats
                    .ingress_owner
                    .send_failures
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Dropped
            }
        }
    }

    /// The peer's send window is closed: keep the fragment, bounded, or fail
    /// the overloaded peer explicitly. Never a silent drop.
    fn defer(&mut self, peer: LogicalPeerId, payload: Bytes, now: Timestamp) {
        if self.deferred.len_for(&peer) >= DEFERRED_PER_PEER
            || self.deferred.total >= DEFERRED_TOTAL
        {
            warn!(peer = ?peer, "SRT reader is not draining; disconnecting the overloaded peer");
            self.stats
                .ingress_owner
                .overload_disconnects
                .fetch_add(1, Ordering::Relaxed);
            self.overloaded.insert(peer);
            self.deferred.forget(&peer);
            self.disconnect(peer, now);
            return;
        }
        self.deferred.push(peer, payload);
        let stats = &self.stats.ingress_owner;
        stats
            .deferred_sends
            .store(self.deferred.total as u64, Ordering::Relaxed);
        stats
            .deferred_sends_hwm
            .fetch_max(self.deferred.total as u64, Ordering::Relaxed);
    }

    /// Flush deferred fragments for peers whose window has reopened. One pass
    /// over the (bounded) deferred set per visit.
    fn retry_deferred(&mut self, now: Timestamp) {
        if self.deferred.total == 0 {
            return;
        }
        let peers: Vec<LogicalPeerId> = self.deferred.per_peer.keys().copied().collect();
        for peer in peers {
            while let Some(payload) = self
                .deferred
                .per_peer
                .get(&peer)
                .and_then(|queue| queue.front())
                .cloned()
            {
                match self.try_send_now(&peer, &payload, now) {
                    SendOutcome::Sent => {
                        if let Some(queue) = self.deferred.per_peer.get_mut(&peer) {
                            queue.pop_front();
                            self.deferred.total -= 1;
                        }
                    }
                    SendOutcome::Dropped => {
                        // The peer is gone or the fragment failed: nothing more
                        // can be delivered to it.
                        self.deferred.forget(&peer);
                        break;
                    }
                    SendOutcome::NoWindow => break,
                }
            }
            if self
                .deferred
                .per_peer
                .get(&peer)
                .is_some_and(VecDeque::is_empty)
            {
                self.deferred.per_peer.remove(&peer);
            }
        }
        self.stats
            .ingress_owner
            .deferred_sends
            .store(self.deferred.total as u64, Ordering::Relaxed);
    }

    fn disconnect(&mut self, peer: LogicalPeerId, now: Timestamp) {
        let Some(mut entry) = self.owner.listener_peer_mut(peer) else {
            self.stats
                .ingress_owner
                .stale_commands
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        entry.disconnect(now);
        self.deferred.forget(&peer);
        self.closing.push_back(ClosingPeer {
            peer,
            since: Instant::now(),
        });
    }

    /// Retire locally-disconnected peers whose terminal event never came.
    fn reap_closing(&mut self) {
        let checks = CLOSING_CHECKS_PER_VISIT.min(self.closing.len());
        for _ in 0..checks {
            let Some(entry) = self.closing.pop_front() else {
                break;
            };
            if self.owner.listener_peer_mut(entry.peer).is_none() {
                continue;
            }
            if entry.since.elapsed() >= CLOSING_GRACE {
                self.retire(entry.peer);
            } else {
                self.closing.push_back(entry);
            }
        }
    }

    /// Remove a logical peer from the Owner. Idempotent: a peer already retired
    /// (or never present) makes this a no-op that cannot touch a later peer,
    /// because `LogicalPeerId`s are never reused.
    fn retire(&mut self, peer: LogicalPeerId) {
        if self.owner.remove_listener_peer(peer).is_some() {
            self.peers = self.peers.saturating_sub(1);
        }
        self.deferred.forget(&peer);
        self.overloaded.remove(&peer);
    }

    /// Translate Owner listener events into the application vocabulary. Only
    /// runs when the previous batch has been handed to Tokio, so accepted media
    /// is never discarded: protocol flow control absorbs the backpressure.
    fn drain_owner_events(&mut self) {
        if !self.pending_events.is_empty() {
            return;
        }
        self.owner.poll_listener_events(&mut self.scratch);
        let mut scratch = std::mem::take(&mut self.scratch);
        for event in scratch.drain(..) {
            let AdmissionEvent {
                representative_peer: peer,
                logical_peer,
                event,
            } = event;
            match event {
                ConnectionEvent::Connected => {
                    let stream_id = self
                        .owner
                        .listener_peer_mut(logical_peer)
                        .and_then(|entry| entry.stream_id().map(str::to_owned))
                        .unwrap_or_default();
                    self.peers += 1;
                    self.pending_events.push_back(SrtIngressEvent::Connected {
                        peer,
                        logical_peer,
                        stream_id,
                    });
                }
                ConnectionEvent::DataReceived { payload, .. } => {
                    self.pending_events.push_back(SrtIngressEvent::Media {
                        logical_peer,
                        payload,
                    });
                }
                ConnectionEvent::Disconnected { reason } => {
                    self.pending_events
                        .push_back(SrtIngressEvent::Disconnected {
                            peer,
                            logical_peer,
                            reason: reason.to_string(),
                        });
                    // Forward, then retire: the terminal peer never stays
                    // resident, and the id cannot be reused.
                    self.retire(logical_peer);
                }
                ConnectionEvent::StateChanged(_)
                | ConnectionEvent::Error(_)
                | ConnectionEvent::KeyRefreshNeeded { .. } => {}
            }
        }
        self.scratch = scratch;
    }

    fn events_has_room(&self) -> bool {
        self.events.capacity() > 0
    }

    /// Hand pending events to Tokio without blocking. Stops at the first full
    /// bridge; the rest wait (bounded) and no further Owner events are drained
    /// until they are delivered.
    fn flush_events(&mut self) {
        while let Some(event) = self.pending_events.pop_front() {
            match self.events.try_send(event) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(event)) => {
                    self.pending_events.push_front(event);
                    self.stats
                        .ingress_owner
                        .event_bridge_full_visits
                        .fetch_add(1, Ordering::Relaxed);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Tokio is gone: nothing can consume events; stop.
                    self.pending_events.clear();
                    self.shutting_down = true;
                    break;
                }
            }
        }
        let depth = (self.events.max_capacity() - self.events.capacity()) as u64;
        self.stats
            .ingress_owner
            .event_depth_hwm
            .fetch_max(depth, Ordering::Relaxed);
        let commands = self.commands.len() as u64;
        self.stats
            .ingress_owner
            .command_depth_hwm
            .fetch_max(commands, Ordering::Relaxed);
    }

    fn account_service(&mut self, report: &OwnerServiceReport) {
        let stats = &self.stats.ingress_owner;
        stats.service_visits.fetch_add(1, Ordering::Relaxed);
        stats
            .service_actions
            .fetch_add(report.actions as u64, Ordering::Relaxed);
        stats
            .maintenance_actions
            .fetch_add(report.maintenance_actions as u64, Ordering::Relaxed);
        if report.budget_exhausted {
            stats.budget_exhausted.fetch_add(1, Ordering::Relaxed);
        }
        stats
            .rx_packets
            .fetch_add(report.rx_packets as u64, Ordering::Relaxed);
        stats
            .rx_bytes
            .fetch_add(report.rx_bytes as u64, Ordering::Relaxed);
        stats
            .tx_packets
            .fetch_add(report.tx_packets_submitted as u64, Ordering::Relaxed);
        stats
            .tx_completed_ok
            .fetch_add(report.tx_completed_ok as u64, Ordering::Relaxed);
        stats.tx_failed.fetch_add(
            (report.tx_failed_sends + report.tx_short_sends) as u64,
            Ordering::Relaxed,
        );
        let tx = self.owner.tx_pool_snapshot();
        stats
            .tx_in_flight
            .store(self.owner.tx_in_flight() as u64, Ordering::Relaxed);
        stats
            .tx_high_water
            .store(tx.high_water as u64, Ordering::Relaxed);
        stats
            .tx_exhaustions
            .store(tx.exhaustions, Ordering::Relaxed);
        if let Some(rx) = self.owner.rx_stats().listener {
            stats
                .rx_ring_depth
                .store(rx.depth as u64, Ordering::Relaxed);
            stats.rx_ring_dropped.store(rx.dropped, Ordering::Relaxed);
            stats.rx_truncated.store(rx.truncated, Ordering::Relaxed);
        }
        if let Some(telemetry) = self.owner.listener_telemetry() {
            stats
                .policy_requests
                .store(telemetry.policy_requests, Ordering::Relaxed);
            stats
                .policy_rejections
                .store(telemetry.policy_rejections, Ordering::Relaxed);
            stats
                .policy_deferred
                .store(telemetry.policy_deferred, Ordering::Relaxed);
            stats
                .credential_failures
                .store(telemetry.credential_failures, Ordering::Relaxed);
        }
        stats.peers.store(self.peers, Ordering::Relaxed);
    }

    /// Park inside the Compio runtime until a command arrives, the Owner has
    /// network/completion activity, bridge room reappears (only when events are
    /// waiting), or the next protocol deadline. No fixed-frequency polling.
    fn park(&mut self, now: Timestamp) {
        let default_us = u64::try_from(IDLE_PARK.as_micros()).unwrap_or(u64::MAX);
        let wait = Duration::from_micros(self.owner.time_until_next_deadline(now, default_us))
            .min(IDLE_PARK);
        let Self {
            owner,
            runtime,
            commands,
            events,
            stash,
            pending_events,
            shutting_down,
            ..
        } = self;
        let waiting_for_room = !pending_events.is_empty();
        runtime.block_on(async {
            let command = pin!(async { Wake::Command(commands.recv_async().await.ok()) });
            let activity = pin!(async {
                owner.wait_for_activity(wait).await;
                Wake::Activity
            });
            let room = pin!(async {
                if !waiting_for_room {
                    pending::<Wake<'_>>().await
                } else {
                    match events.reserve().await {
                        Ok(permit) => Wake::Room(permit),
                        Err(_) => Wake::Closed,
                    }
                }
            });
            // Commands are polled first so they win ties.
            match first_ready(command, pin!(first_ready(activity, room))).await {
                Wake::Command(Some(command)) => *stash = Some(command),
                // Every Tokio sender is gone: the application is shutting down.
                Wake::Command(None) | Wake::Closed => *shutting_down = true,
                Wake::Room(permit) => {
                    if let Some(event) = pending_events.pop_front() {
                        permit.send(event);
                    }
                }
                Wake::Activity => {}
            }
        });
    }

    /// Orderly exit: flush queued protocol output (SHUTDOWN datagrams), report
    /// a fault to Tokio, then run the canonical Owner teardown and report its
    /// verdict truthfully. A live managed-RX Owner is never just dropped.
    fn finish(&mut self, fault: Option<String>) -> IngressExit {
        self.assert_home_thread();
        if let Some(detail) = &fault {
            self.report_fault(detail);
        } else {
            for _ in 0..SHUTDOWN_FLUSH_VISITS {
                self.runtime.poll_with(Some(Duration::ZERO));
                self.runtime.run();
                let now = self.timestamp();
                let owner = &mut self.owner;
                let report = self.runtime.block_on(async {
                    let report = owner.service(now, OwnerServiceBudget::default()).await;
                    if !report.work_remaining && owner.tx_in_flight() > 0 {
                        owner.wait_for_activity(Duration::from_millis(5)).await;
                    }
                    report
                });
                if !report.work_remaining && self.owner.tx_in_flight() == 0 {
                    break;
                }
            }
        }
        let owner = &mut self.owner;
        let quiescent = self
            .runtime
            .block_on(owner.shutdown_and_drain(SHUTDOWN_DRAIN_DEADLINE));
        if !quiescent {
            warn!(
                fault = ?self.owner.fault(),
                in_flight = self.owner.tx_in_flight(),
                "srt ingress Owner did not reach quiescence within its shutdown bound"
            );
        }
        IngressExit { quiescent, fault }
    }

    /// Tell Tokio the listener died. Bounded: a stalled consumer cannot hold
    /// the owner thread past one second.
    fn report_fault(&mut self, detail: &str) {
        let mut event = SrtIngressEvent::Fault {
            detail: detail.to_string(),
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.events.try_send(event) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => return,
                Err(mpsc::error::TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return;
                    }
                    event = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
}

enum Wake<'a> {
    Command(Option<IngressCommand>),
    Activity,
    Room(mpsc::Permit<'a, SrtIngressEvent>),
    Closed,
}

/// The output of whichever future finishes first (the left one on a tie).
async fn first_ready<T>(
    a: impl std::future::Future<Output = T> + Unpin,
    b: impl std::future::Future<Output = T> + Unpin,
) -> T {
    match select(a, b).await {
        Either::Left((value, _)) | Either::Right((value, _)) => value,
    }
}

enum SendOutcome {
    Sent,
    /// The peer's send window is closed right now.
    NoWindow,
    /// Nothing more can or should be delivered for this fragment.
    Dropped,
}
