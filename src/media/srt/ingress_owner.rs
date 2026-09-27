//! The SRT ingress owner thread: ONE Compio runtime and ONE
//! `srt_transport::compio::Owner` that owns the listener UDP socket and every
//! protocol object behind it (handshake admission, timers, ACK/NAK, listener
//! TX, receive state, per-peer send/disconnect/retire).
//!
//! Media runs to completion here too: every received payload goes through TS
//! demux, input gating, timestamp mapping, standby GOP and ring publication on
//! this thread (`ingress_media`), and direct SRT read/play is driven here.
//! Tokio owns session lifecycle only. The two sides meet through two lossless
//! bounded bridges that carry lifecycle, never media (plus one lossy telemetry
//! bridge, below):
//!
//! * `IngressCommand` (Tokio -> Owner): `AttachPublisher`, `AttachReader`,
//!   `ProbeApplied`, `Disconnect`, `Shutdown`, all addressed by
//!   `LogicalPeerId`. A `LogicalPeerId` is the sole cross-thread session
//!   handle; no protocol object, table reference, socket id or `SocketAddr`
//!   identifies a session.
//! * `SrtIngressEvent` (Owner -> Tokio): `Connected`, `Probe`, `Disconnected`,
//!   plus the terminal `Fault`.
//! * `QualitySample` (Owner -> Tokio, LOSSY): per-peer receive-quality
//!   observations stamped with the time the Owner took them. Sent with
//!   `try_send`; a full bridge drops the sample and counts it, so telemetry can
//!   never delay protocol service. Tokio owns everything derived from it.
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

use super::ingress_admission::ingress_resolver;
pub(crate) use super::ingress_bridge::{
    INGRESS_COMMAND_CAPACITY, INGRESS_EVENT_CAPACITY, INGRESS_TELEMETRY_CAPACITY, IngressCommand,
    IngressConfig, IngressExit, SrtIngressEvent, SrtIngressHandle,
};
use super::ingress_media::IngressMedia;

#[path = "ingress_owner_send.rs"]
mod send;
use super::ingress_quality::{Observation, QualitySample, sample_from_stats};
use send::DeferredSends;

/// Concurrent datagram sends (TX pool slots and lanes) for the ingress Owner.
/// Ingress TX is protocol replies (handshake, ACK/NAK, SHUTDOWN) plus SRT
/// read/play payloads. Unsent output waits in bounded protocol state.
pub(crate) const INGRESS_TX_CAPACITY: usize = 64;

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

/// Park bound while direct-play readers are attached, so their pulls keep
/// pace with the feed.
const READER_POLL: Duration = Duration::from_millis(5);

/// Fragments one reader may send per visit, so one busy reader cannot
/// monopolize the Owner.
const READER_SENDS_PER_VISIT: usize = 32;

/// Lifecycle events waiting for Tokio. Media no longer waits on this bridge;
/// only past this many undelivered lifecycle events does the Owner stop
/// draining protocol events (backpressure through SRT flow control).
const PENDING_EVENTS_MAX: usize = 1024;

/// Longest the owner parks when nothing is due. Commands, Owner activity and
/// event-bridge capacity all wake it earlier; protocol deadlines shorten it.
const IDLE_PARK: Duration = Duration::from_secs(1);

/// How long a locally-disconnected peer may keep its SHUTDOWN in flight before
/// it is retired regardless of a terminal event.
const CLOSING_GRACE: Duration = Duration::from_millis(500);

/// Closing peers examined per visit.
const CLOSING_CHECKS_PER_VISIT: usize = 32;

/// Receive-quality sampling: each live peer is sampled about once per interval,
/// in slices, so sampling cost per visit is bounded and independent of the
/// peer count.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const SAMPLES_PER_VISIT: usize = 16;

/// Finite bounds for the orderly shutdown.
const SHUTDOWN_FLUSH_VISITS: u32 = 20;
const SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(2);

struct ClosingPeer {
    peer: LogicalPeerId,
    since: Instant,
}

struct OwnerLoop {
    owner: Owner,
    runtime: compio::runtime::Runtime,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
    telemetry: mpsc::Sender<QualitySample>,
    stats: Arc<ListenerSocketStats>,
    /// Live admitted peers, and the slice of them still to sample this round.
    live: std::collections::HashSet<LogicalPeerId>,
    sample_queue: VecDeque<LogicalPeerId>,
    next_sample_round: Instant,
    epoch: Instant,
    /// Translated events waiting for bridge room. Only refilled when empty,
    /// so it is bounded by one Owner drain and accepted media is never lost.
    pending_events: VecDeque<SrtIngressEvent>,
    scratch: Vec<AdmissionEvent>,
    /// Every publisher's and reader's media state (run to completion here).
    media: IngressMedia,
    /// Representative address of each admitted peer, for lifecycle events.
    addrs: HashMap<LogicalPeerId, SocketAddr>,
    reader_scratch: Vec<Bytes>,
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

pub(super) fn run_owner_thread(
    config: IngressConfig,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
    telemetry: mpsc::Sender<QualitySample>,
    ready: flume::Sender<Result<SocketAddr, String>>,
) -> IngressExit {
    match build(config, commands, events, telemetry) {
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
    telemetry: mpsc::Sender<QualitySample>,
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
            // Same receive buffer as egress Owner sockets (`desired_udp_buf`,
            // 8 MiB unless RESTREAM_SRT_UDP_BUF_BYTES overrides). With the
            // kernel default (208 KB) the listener dropped 5,876 publisher
            // datagrams in one SRT x150 run (`ss -uam`), which ARQ then had to
            // recover.
            if let Some(bytes) = std::num::NonZeroUsize::new(super::desired_udp_buf()) {
                transport.socket_buffers = srt_transport::SocketBufferConfig::Bytes(bytes);
            }
            // The Owner has no relocation target.
            transport.promotion = PromotionPolicy::Never;
        })
        .build()
        .map_err(|error| format!("failed to build srt-rs listener config: {error}"))?;
    let mut owner = Owner::new_with_ceiling(INGRESS_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    // In this pinned srt-rs/Compio combination, transient managed-receive
    // ENOBUFS ends the RX stream and faults the listener. Until that error is
    // retryable, leave the substrate uninstalled so ManagedPreferred selects
    // the documented raw-readiness fallback for ingress. Keep the observation
    // for diagnostics; egress retains its independently qualified managed RX.
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
        rx_ring_entries = runtime_config.rx_ring_entries,
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
            telemetry,
            stats,
            live: std::collections::HashSet::new(),
            sample_queue: VecDeque::new(),
            next_sample_round: Instant::now() + SAMPLE_INTERVAL,
            epoch: Instant::now(),
            pending_events: VecDeque::with_capacity(64),
            scratch: Vec::with_capacity(64),
            media: IngressMedia::default(),
            addrs: HashMap::new(),
            reader_scratch: Vec::with_capacity(READER_SENDS_PER_VISIT),
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
            // Compio tasks (notably TX workers) require Runtime::current while polled.
            self.runtime.enter(|| {
                self.runtime.poll_with(Some(Duration::ZERO));
                self.runtime.run();
            });

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
            self.sample_peers();
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
            self.drive_readers(now);
            self.flush_events();

            let busy = report.work_remaining
                || self.stash.is_some()
                || !self.commands.is_empty()
                || (!self.pending_events.is_empty() && self.events_has_room())
                // A sampling round in progress finishes promptly.
                || !self.sample_queue.is_empty();
            if !busy {
                self.park(now);
            }
        }
        self.finish(fault)
    }

    /// Apply one command to Owner-owned state.
    fn apply(&mut self, command: IngressCommand, now: Timestamp) {
        match command {
            IngressCommand::AttachPublisher {
                logical_peer,
                media,
            } => {
                if !self.live.contains(&logical_peer) {
                    // Retired while Tokio admitted it: nothing to run.
                    return;
                }
                if let Some(probe) =
                    self.media
                        .attach_publisher(logical_peer, media, &self.stats.ingress_owner)
                {
                    self.pending_events.push_back(SrtIngressEvent::Probe {
                        logical_peer,
                        probe,
                    });
                }
            }
            IngressCommand::AttachReader {
                logical_peer,
                reader,
            } => {
                if self.live.contains(&logical_peer) {
                    self.media.attach_reader(logical_peer, reader);
                }
            }
            IngressCommand::ProbeApplied { logical_peer, ring } => {
                self.media.probe_applied(logical_peer, ring);
            }
            IngressCommand::Disconnect { logical_peer } => self.disconnect(logical_peer, now),
            IngressCommand::Shutdown => self.shutting_down = true,
        }
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

    /// Sample receive quality for a bounded slice of live peers. A new round
    /// starts at most once per [`SAMPLE_INTERVAL`]; peers retired meanwhile are
    /// skipped when reached.
    fn sample_peers(&mut self) {
        if self.sample_queue.is_empty() {
            if self.live.is_empty() || Instant::now() < self.next_sample_round {
                return;
            }
            self.sample_queue.extend(self.live.iter().copied());
            self.next_sample_round = Instant::now() + SAMPLE_INTERVAL;
        }
        for _ in 0..SAMPLES_PER_VISIT {
            let Some(peer) = self.sample_queue.pop_front() else {
                break;
            };
            if !self.live.contains(&peer) {
                continue;
            }
            let sample = self
                .owner
                .listener_peer_mut(peer)
                .and_then(|entry| entry.stats())
                .and_then(|stats| sample_from_stats(&stats));
            let Some(sample) = sample else {
                continue;
            };
            let observation = Observation {
                observed_at: Instant::now(),
                sample,
            };
            // Lossy by design: never wait for Tokio.
            if let Err(mpsc::error::TrySendError::Full(_)) =
                self.telemetry.try_send(QualitySample { peer, observation })
            {
                self.stats
                    .ingress_owner
                    .telemetry_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
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
                // No terminal event came: report the end ourselves so Tokio's
                // session bookkeeping always runs.
                if let Some(peer) = self.addrs.get(&entry.peer).copied() {
                    self.pending_events
                        .push_back(SrtIngressEvent::Disconnected {
                            peer,
                            logical_peer: entry.peer,
                            reason: "closed locally".to_string(),
                        });
                }
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
        self.live.remove(&peer);
        self.addrs.remove(&peer);
        // Flushes a publisher's last media into the ring and drops any reader
        // or held state.
        self.media.detach(peer);
    }

    /// Run received media to completion and translate lifecycle events for
    /// Tokio. Stops draining only if Tokio has fallen [`PENDING_EVENTS_MAX`]
    /// lifecycle events behind, so protocol flow control absorbs that
    /// backpressure; media itself never waits on Tokio.
    fn drain_owner_events(&mut self) {
        if self.pending_events.len() >= PENDING_EVENTS_MAX {
            self.stats
                .ingress_owner
                .event_bridge_full_visits
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
        let started = Instant::now();
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
                    self.live.insert(logical_peer);
                    self.addrs.insert(logical_peer, peer);
                    self.pending_events.push_back(SrtIngressEvent::Connected {
                        peer,
                        logical_peer,
                        stream_id,
                    });
                }
                ConnectionEvent::DataReceived { payload, .. } => {
                    if let Some(probe) =
                        self.media
                            .on_payload(logical_peer, payload, &self.stats.ingress_owner)
                    {
                        self.pending_events.push_back(SrtIngressEvent::Probe {
                            logical_peer,
                            probe,
                        });
                    }
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
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let stats = &self.stats.ingress_owner;
        stats.media_work_us.fetch_add(elapsed_us, Ordering::Relaxed);
        stats
            .media_pass_max_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        if elapsed_us > 5_000 {
            stats.media_slow_passes_5ms.fetch_add(1, Ordering::Relaxed);
            if elapsed_us > 20_000 {
                stats.media_slow_passes_20ms.fetch_add(1, Ordering::Relaxed);
            }
        }
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
        let mut wait = Duration::from_micros(self.owner.time_until_next_deadline(now, default_us))
            .min(IDLE_PARK);
        if self.media.has_readers() {
            wait = wait.min(READER_POLL);
        }
        // The next sampling round is a deadline too, while peers are live.
        if !self.live.is_empty() {
            wait = wait.min(
                self.next_sample_round
                    .saturating_duration_since(Instant::now()),
            );
        }
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
        // Flush every publisher's last media into its ring.
        for peer in self.media.publisher_peers() {
            self.media.detach(peer);
        }
        if let Some(detail) = &fault {
            self.report_fault(detail);
        } else {
            for _ in 0..SHUTDOWN_FLUSH_VISITS {
                self.runtime.enter(|| {
                    self.runtime.poll_with(Some(Duration::ZERO));
                    self.runtime.run();
                });
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
