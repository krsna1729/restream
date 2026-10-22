//! SRT accept-and-discard listeners for scaling tests, on the production
//! receive path: each port gets a configurable number of `srt_transport::compio::Owner`
//! threads in one `SO_REUSEPORT` listener group, built exactly like the SRT ingress Owners
//! (forced io_uring runtime, managed multishot RX when the kernel supports it). The sink
//! performs no media parsing and records only byte, connection and drop counters, so
//! MediaMTX is not in the scaling critical path.
//!
//! Drops are counted at both layers that can lose a datagram before SRT sees
//! it: each kernel socket (`/proc/net/udp{,6}` `drops`, per local port and
//! inode) and the Owners' managed-RX rings (`dropped`, `buffer_exhaustions`,
//! `truncated`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use futures_util::future::{Either, select};
use restream::media::srt::srt_owner_runtime::{
    SRT_OWNER_WIRE_CEILING, listener_shape, production_runtime,
};
use srt_proto::{ConnectionEvent, Timestamp};
use srt_transport::advanced::admission::{AdmissionEvent, BondedInputPolicy, LogicalPeerId};
use srt_transport::compio::{
    Owner, OwnerServiceBudget, ProductionRuntimeConfig, RxModePolicy, observe_production_runtime,
};
use srt_transport::{
    ListenerConfig, ListenerTransfer, OwnerListenerPlan, RuntimeFlavor, SocketBufferConfig,
    owner_plans,
};

/// The sink only replies with protocol control (handshake, ACK/NAK).
const SINK_TX_CAPACITY: usize = 64;
/// Longest park when nothing is due, so `stop` is observed promptly.
const IDLE_PARK: Duration = Duration::from_millis(5);
/// How often a sink thread publishes per-connection bytes and Owner RX stats;
/// far below the delivery sampler's 1 s tick, and off the per-packet path.
const PUBLISH_EVERY: Duration = Duration::from_millis(100);
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);
/// Listener transfers accepted per visit, as in the ingress Owner; a
/// non-empty inbox keeps the Owner from parking.
const TRANSFERS_PER_VISIT: usize = 32;
pub(crate) const DEFAULT_SINK_OWNERS_PER_PORT: usize = 2;
const MAX_SINK_OWNERS_PER_PORT: usize = 4;
pub(crate) fn configured_owners_per_port() -> usize {
    let owners_per_port = std::env::var("HARNESS_SRT_SINK_OWNERS_PER_PORT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SINK_OWNERS_PER_PORT);
    clamp_owners_per_port(owners_per_port)
}

fn clamp_owners_per_port(owners_per_port: usize) -> usize {
    owners_per_port.clamp(1, MAX_SINK_OWNERS_PER_PORT)
}

/// Cloneable handle to a running pool's counters.
#[derive(Clone)]
pub(crate) struct SrtSinkCountersHandle {
    counters: Arc<SinkCounters>,
    ports: Arc<[u16]>,
}

impl SrtSinkCountersHandle {
    /// Cumulative payload bytes per logical connection, keyed by (sink port,
    /// pool-wide connection sequence for each srt-rs `LogicalPeerId`) — not by
    /// address: Restream's SRT callers share UDP sockets, so many connections
    /// arrive from one address.
    pub(crate) fn per_peer_bytes(&self) -> Vec<((u16, u64), u64)> {
        lock(&self.counters.per_peer)
            .iter()
            .map(|(peer, bytes)| (*peer, *bytes))
            .collect()
    }

    pub(crate) fn snapshot(&self) -> SrtSinkCounters {
        self.counters.snapshot()
    }

    /// Cumulative drops at the kernel sockets and every Owner RX ring, and
    /// the Owner faults that stopped the pool.
    pub(crate) fn drops(&self) -> SrtSinkDrops {
        let owner = lock(&self.counters.owner_rx)
            .values()
            .fold(OwnerRxDrops::default(), |sum, rx| sum.plus(*rx));
        SrtSinkDrops {
            owner,
            owner_faults: self.counters.owner_faults.load(Ordering::Acquire),
            socket: socket_drops(&self.ports),
        }
    }
}

/// One reading of a sink pool's cumulative counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SrtSinkCounters {
    pub accepted: u64,
    pub discarded_bytes: u64,
    pub closed: u64,
}

/// Datagrams the Owner's managed-RX ring lost before SRT saw them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OwnerRxDrops {
    pub dropped: u64,
    pub buffer_exhaustions: u64,
    pub truncated: u64,
}

impl OwnerRxDrops {
    fn plus(self, other: Self) -> Self {
        Self {
            dropped: self.dropped + other.dropped,
            buffer_exhaustions: self.buffer_exhaustions + other.buffer_exhaustions,
            truncated: self.truncated + other.truncated,
        }
    }

    fn since(self, start: Self) -> Self {
        Self {
            dropped: self.dropped.saturating_sub(start.dropped),
            buffer_exhaustions: self
                .buffer_exhaustions
                .saturating_sub(start.buffer_exhaustions),
            truncated: self.truncated.saturating_sub(start.truncated),
        }
    }
}

/// Drops at both layers. `socket` is `None` when `/proc/net/udp{,6}` is
/// unreadable, so a missing sensor never reads as zero. Keys identify each
/// local port and socket inode separately.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SrtSinkDrops {
    pub owner: OwnerRxDrops,
    /// Owners that faulted. Any fault stops the whole pool: a reuseport
    /// member that exits rehashes its sessions onto members that drop them.
    pub owner_faults: u64,
    pub socket: Option<BTreeMap<(u16, u64), u64>>,
}

impl SrtSinkDrops {
    /// Drops accumulated since `start` (a rated window's first reading).
    /// A socket present at `start` but gone by now (a closed relocated-leg
    /// socket) loses that window's drops. Owner faults stay cumulative: a
    /// fault stops the pool, so every later window must show it.
    pub(crate) fn since(&self, start: &Self) -> Self {
        let socket = match (&self.socket, &start.socket) {
            (Some(end), Some(begin)) => Some(
                end.iter()
                    .map(|(socket, drops)| {
                        (
                            *socket,
                            drops.saturating_sub(begin.get(socket).copied().unwrap_or(0)),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        };
        Self {
            owner: self.owner.since(start.owner),
            owner_faults: self.owner_faults,
            socket,
        }
    }

    /// The one JSON shape for sink drops (state endpoint and sweep records).
    pub(crate) fn json(&self) -> serde_json::Value {
        let socket_drops = self.socket.as_ref().map(|sockets| {
            sockets
                .iter()
                .map(|((port, inode), drops)| {
                    serde_json::json!({
                        "port": port,
                        "inode": inode,
                        "drops": drops,
                    })
                })
                .collect::<Vec<_>>()
        });
        serde_json::json!({
            "ownerRxDropped": self.owner.dropped,
            "ownerRxBufferExhaustions": self.owner.buffer_exhaustions,
            "ownerRxTruncated": self.owner.truncated,
            "ownerFaults": self.owner_faults,
            "socketDrops": socket_drops,
            "socketDropsTotal": self.socket.as_ref().map(|sockets| sockets.values().sum::<u64>()),
        })
    }
}

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    discarded_bytes: AtomicU64,
    closed: AtomicU64,
    per_peer: Mutex<HashMap<(u16, u64), u64>>,
    owner_rx: Mutex<HashMap<(u16, u16), OwnerRxDrops>>,
    owner_faults: AtomicU64,
    /// Pool-wide connection sequence, so indexes never collide across ports.
    next_connection: AtomicU64,
}

impl SinkCounters {
    fn snapshot(&self) -> SrtSinkCounters {
        SrtSinkCounters {
            accepted: self.accepted.load(Ordering::Relaxed),
            discarded_bytes: self.discarded_bytes.load(Ordering::Relaxed),
            closed: self.closed.load(Ordering::Relaxed),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    restream::sync::lock(mutex)
}

pub(crate) struct HarnessSrtSinkPool {
    ports: Arc<[u16]>,
    owners_per_port: usize,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    threads: Vec<JoinHandle<()>>,
}

impl HarnessSrtSinkPool {
    /// Start one listener group per port. Every group member owns its own
    /// runtime and socket; `SO_REUSEPORT` distributes independent source flows.
    pub(crate) fn start(
        ports: &[u16],
        udp_buffer: usize,
        owners_per_port: usize,
    ) -> Result<Self, String> {
        if ports.is_empty() {
            return Err("harness SRT sink pool needs at least one port".to_string());
        }
        let mut unique_ports = HashSet::with_capacity(ports.len());
        for &port in ports {
            if !unique_ports.insert(port) {
                return Err(format!("harness SRT sink pool has duplicate port {port}"));
            }
        }
        // `SO_REUSEPORT` would let a leftover same-user sink silently join a
        // port's group (srt-rs's registry guards one process only), so a sink
        // port that already has any UDP socket refuses the pool.
        if let Some(sockets) = socket_drops(ports) {
            let taken: BTreeSet<u16> = sockets.keys().map(|(port, _)| *port).collect();
            if !taken.is_empty() {
                return Err(format!(
                    "harness SRT sink port(s) {taken:?} already have a UDP socket"
                ));
            }
        }
        let owners_per_port = clamp_owners_per_port(owners_per_port);
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let mut planned = Vec::with_capacity(ports.len() * owners_per_port);
        for &port in ports {
            let plans = build_listener_plans(port, udp_buffer, owners_per_port)?;
            if plans.len() != owners_per_port {
                return Err(format!(
                    "SRT sink port {port} planned {} Owners, expected {owners_per_port}",
                    plans.len()
                ));
            }
            let (members, inboxes): (Vec<_>, Vec<_>) = plans
                .iter()
                .map(|_| flume::unbounded::<ListenerTransfer>())
                .unzip();
            for (owner_index, (plan, inbox)) in plans.into_iter().zip(inboxes).enumerate() {
                let owner_index = u16::try_from(owner_index)
                    .map_err(|_| "too many SRT sink Owners".to_string())?;
                planned.push(SinkOwnerSeat {
                    port,
                    owner_index,
                    plan,
                    inbox,
                    members: members.clone(),
                });
            }
        }
        let thread_count = planned.len();
        let (ready_tx, ready_rx) = mpsc::sync_channel(thread_count);
        let mut pool = Self {
            ports: ports.into(),
            owners_per_port,
            stop,
            counters,
            threads: Vec::with_capacity(thread_count),
        };
        for seat in planned {
            let stop = pool.stop.clone();
            let counters = pool.counters.clone();
            let ready_tx = ready_tx.clone();
            let thread_name = format!("srt-s-{}-{}", seat.port, seat.owner_index);
            match std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || sink_thread(seat, stop, counters, ready_tx))
            {
                Ok(thread) => pool.threads.push(thread),
                Err(error) => {
                    pool.halt();
                    return Err(format!("spawn harness SRT sink thread: {error}"));
                }
            }
        }
        drop(ready_tx);
        for _ in 0..pool.threads.len() {
            let outcome = match ready_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(outcome) => outcome,
                Err(error) => Err(format!(
                    "timed out waiting for harness SRT sink bind: {error}"
                )),
            };
            if let Err(error) = outcome {
                pool.halt();
                return Err(error);
            }
        }
        tracing::info!(
            "[harness-srt-sink] srt-rs Owner sink on {} port(s), {} Owner(s) per port",
            ports.len(),
            pool.owners_per_port
        );
        Ok(pool)
    }

    /// A cloneable handle to the pool's counters, so a state endpoint can
    /// report them while the pool keeps owning its threads.
    pub(crate) fn counters(&self) -> SrtSinkCountersHandle {
        SrtSinkCountersHandle {
            counters: self.counters.clone(),
            ports: self.ports.clone(),
        }
    }

    /// Cumulative counters so far (connections accepted, payload bytes
    /// discarded, connections closed). Readable while the pool is running.
    pub(crate) fn snapshot(&self) -> SrtSinkCounters {
        self.counters.snapshot()
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }

    pub(crate) fn stop(mut self) {
        self.halt();
        let counters = self.counters.snapshot();
        let drops = self.counters().drops();
        tracing::info!(
            "[harness-srt-sink] stopped {} port(s), {} Owner(s) per port, accepted={}, discarded={}MB, closed={}, \
             owner_rx_dropped={}, owner_faults={}, socket_drops={:?}",
            self.ports.len(),
            self.owners_per_port,
            counters.accepted,
            counters.discarded_bytes / (1024 * 1024),
            counters.closed,
            drops.owner.dropped,
            drops.owner_faults,
            drops.socket.map(|ports| ports.values().sum::<u64>()),
        );
    }
}

fn build_listener_plans(
    port: u16,
    udp_buffer: usize,
    owners_per_port: usize,
) -> Result<Vec<OwnerListenerPlan>, String> {
    let owners = NonZeroUsize::new(owners_per_port).expect("owner count is clamped above zero");
    let (topology, promotion) = listener_shape(owners);
    let config = ListenerConfig::builder(SocketAddr::from(([0, 0, 0, 0], port)))
        .topology(topology)
        .bonded_inputs(BondedInputPolicy::Accept)
        .configure_transport(|transport| {
            transport.socket_buffers = NonZeroUsize::new(udp_buffer)
                .map(SocketBufferConfig::Bytes)
                .unwrap_or(SocketBufferConfig::SystemDefault);
            transport.promotion = promotion;
        })
        .build()
        .map_err(|error| error.to_string())?;
    owner_plans(&config, RuntimeFlavor::Compio).map_err(|error| error.to_string())
}
struct SinkOwnerSeat {
    port: u16,
    owner_index: u16,
    plan: OwnerListenerPlan,
    inbox: flume::Receiver<ListenerTransfer>,
    members: Vec<flume::Sender<ListenerTransfer>>,
}

fn sink_thread(
    seat: SinkOwnerSeat,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    ready_tx: SyncSender<Result<(), String>>,
) {
    let SinkOwnerSeat {
        port,
        owner_index,
        plan,
        inbox,
        members,
    } = seat;
    let (mut owner, runtime) = match build_owner(&plan) {
        Ok(built) => {
            let _ = ready_tx.send(Ok(()));
            built
        }
        Err(error) => {
            let _ = ready_tx.send(Err(format!("bind SRT sink port {port}: {error}")));
            return;
        }
    };
    let epoch = Instant::now();
    let now =
        || Timestamp::from_micros(epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
    let idle_us = u64::try_from(IDLE_PARK.as_micros()).unwrap_or(u64::MAX);
    let mut events: Vec<AdmissionEvent> = Vec::with_capacity(64);
    let mut transfers: Vec<ListenerTransfer> = Vec::with_capacity(TRANSFERS_PER_VISIT);
    // Connection index and bytes per logical peer (its raw id is private).
    let mut per_peer: HashMap<LogicalPeerId, (u64, u64)> = HashMap::new();
    let mut published_at = Instant::now();
    // A transfer received while parked, accepted first on the next visit.
    let mut transfer_stash: Option<ListenerTransfer> = None;
    while !stop.load(Ordering::Acquire) {
        // One non-blocking driver poll per visit reaps TX completions and RX.
        runtime.enter(|| {
            runtime.poll_with(Some(Duration::ZERO));
            runtime.run();
        });
        let at = now();
        let mut budget = TRANSFERS_PER_VISIT;
        if let Some(transfer) = transfer_stash.take() {
            owner.accept_listener_transfer(transfer, at);
            budget -= 1;
        }
        for _ in 0..budget {
            match inbox.try_recv() {
                Ok(transfer) => owner.accept_listener_transfer(transfer, at),
                Err(flume::TryRecvError::Empty | flume::TryRecvError::Disconnected) => break,
            }
        }
        let report = runtime.block_on(owner.service(at, OwnerServiceBudget::default()));
        owner.poll_listener_transfers(&mut transfers);
        for transfer in transfers.drain(..) {
            if let Some(member) = members.get(transfer.to) {
                let _ = member.send(transfer);
            }
        }
        if let Some(fault) = owner.fault() {
            tracing::error!(
                port,
                owner_index,
                ?fault,
                "harness SRT sink Owner faulted; stopping the pool"
            );
            counters.owner_faults.fetch_add(1, Ordering::AcqRel);
            // srt-rs requires a reuseport group to stop together: a member
            // that exits rehashes its sessions onto members that drop them.
            stop.store(true, Ordering::Release);
            break;
        }
        owner.poll_listener_events(&mut events);
        for AdmissionEvent {
            logical_peer,
            event,
            ..
        } in events.drain(..)
        {
            match event {
                ConnectionEvent::Connected => {
                    counters.accepted.fetch_add(1, Ordering::Relaxed);
                }
                ConnectionEvent::DataReceived { payload, .. } => {
                    let len = payload.len() as u64;
                    counters.discarded_bytes.fetch_add(len, Ordering::Relaxed);
                    per_peer
                        .entry(logical_peer)
                        .or_insert_with(|| {
                            (counters.next_connection.fetch_add(1, Ordering::Relaxed), 0)
                        })
                        .1 += len;
                }
                // A closed connection is no longer a destination: keep
                // reused sink stacks from reporting it as a stalled one.
                ConnectionEvent::Disconnected { .. } => {
                    counters.closed.fetch_add(1, Ordering::Relaxed);
                    if let Some((index, _)) = per_peer.remove(&logical_peer) {
                        lock(&counters.per_peer).remove(&(port, index));
                    }
                }
                _ => {}
            }
        }
        if published_at.elapsed() >= PUBLISH_EVERY {
            published_at = Instant::now();
            lock(&counters.per_peer).extend(
                per_peer
                    .values()
                    .map(|(index, bytes)| ((port, *index), *bytes)),
            );
            if let Some(rx) = owner.rx_stats().listener {
                lock(&counters.owner_rx).insert(
                    (port, owner_index),
                    OwnerRxDrops {
                        dropped: rx.dropped,
                        buffer_exhaustions: rx.buffer_exhaustions,
                        truncated: rx.truncated,
                    },
                );
            }
        }
        // Like the ingress Owner: queued transfers run the next visit without
        // parking, and one arriving while parked wakes the Owner.
        if !report.work_remaining && inbox.is_empty() {
            let wait =
                Duration::from_micros(owner.time_until_next_deadline(at, idle_us)).min(IDLE_PARK);
            transfer_stash = runtime.block_on(async {
                // Every Owner holds a sender to its own inbox, so it never
                // closes while this Owner runs.
                let transfer = pin!(inbox.recv_async());
                let activity = pin!(owner.wait_for_activity(wait));
                match select(transfer, activity).await {
                    Either::Left((transfer, _)) => transfer.ok(),
                    Either::Right(((), _)) => None,
                }
            });
        }
    }
    if !runtime.block_on(owner.shutdown_and_drain(SHUTDOWN_DRAIN)) {
        tracing::warn!(
            port,
            owner_index,
            "harness SRT sink Owner did not reach quiescence"
        );
    }
}

/// The SRT ingress Owner's construction (`media::srt::ingress_owner::build`)
/// minus the admission resolver: the sink accepts every caller.
fn build_owner(plan: &OwnerListenerPlan) -> Result<(Owner, compio::runtime::Runtime), String> {
    let mut runtime_config =
        ProductionRuntimeConfig::for_owner(SINK_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    // A/B knob: managed-RX provided buffers (power of two; production 256).
    if let Some(entries) = std::env::var("HARNESS_SRT_SINK_RX_RING")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|entries| entries.is_power_of_two())
    {
        runtime_config.rx_ring_entries = entries;
    }
    let runtime = production_runtime(runtime_config)?;
    let profile = runtime.block_on(observe_production_runtime(
        &runtime,
        SINK_TX_CAPACITY,
        SRT_OWNER_WIRE_CEILING,
    ));
    let mut owner = Owner::new_with_ceiling(SINK_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    owner
        .set_rx_substrate(profile.managed_rx_substrate())
        .map_err(|error| error.to_string())?;
    owner.set_rx_mode_policy(RxModePolicy::ManagedPreferred);
    runtime
        .block_on(async { owner.listen_planned(plan, None) })
        .map_err(|error| error.to_string())?;
    Ok((owner, runtime))
}

/// Kernel drops per sink socket from `/proc/net/udp` and `/proc/net/udp6`.
fn socket_drops(ports: &[u16]) -> Option<BTreeMap<(u16, u64), u64>> {
    let mut drops = BTreeMap::new();
    let mut readable = false;
    for path in ["/proc/net/udp", "/proc/net/udp6"] {
        if let Ok(table) = std::fs::read_to_string(path) {
            readable = true;
            for (port, inode, count) in parse_udp_socket_drops(&table) {
                if ports.contains(&port) {
                    *drops.entry((port, inode)).or_insert(0) += count;
                }
            }
        }
    }
    readable.then_some(drops)
}

/// `(local port, inode, drops)` for each `/proc/net/udp{,6}` socket row:
/// field 1 is `ADDR:PORT` in hex, field 9 is the inode, field 12 is `drops`.
fn parse_udp_socket_drops(table: &str) -> impl Iterator<Item = (u16, u64, u64)> + '_ {
    table.lines().skip(1).filter_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let port = fields.get(1)?.rsplit_once(':')?.1;
        let port = u16::from_str_radix(port, 16).ok()?;
        let inode = fields.get(9)?.parse::<u64>().ok()?;
        let drops = fields.get(12)?.parse::<u64>().ok()?;
        Some((port, inode, drops))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_udp_ports(count: usize) -> Vec<u16> {
        (0..count)
            .map(|_| {
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe socket");
                socket.local_addr().expect("probe address").port()
            })
            .collect()
    }

    #[test]
    fn owners_per_port_is_bounded() {
        assert_eq!(clamp_owners_per_port(0), 1);
        assert_eq!(clamp_owners_per_port(2), 2);
        assert_eq!(clamp_owners_per_port(usize::MAX), 4);
    }

    #[test]
    fn sink_pool_starts_two_owners_per_port_and_stops() {
        let ports = free_udp_ports(2);
        let pool = HarnessSrtSinkPool::start(&ports, 0, 2).expect("start multi-owner sink pool");
        assert_eq!(pool.threads.len(), 4);
        let drops = pool.counters().drops();
        let sockets = drops.socket.expect("/proc/net/udp readable on Linux");
        for port in &ports {
            assert!(
                sockets.keys().any(|(socket_port, _)| socket_port == port),
                "no per-socket drop rows for port {port}: {sockets:?}"
            );
        }
        pool.stop();
    }

    const CALLER_TX: usize = 64;
    const CALLER_PAYLOAD: usize = 1316;

    /// One srt-rs caller on its own Owner (so its own UDP source port),
    /// driven like the ingress tests' `run_caller`: connect to `sink`, send
    /// `payloads` datagrams, then keep servicing until `done`. Returns whether
    /// it connected and how many payloads it sent.
    fn run_caller(sink: SocketAddr, payloads: usize, done: Arc<AtomicBool>) -> (bool, usize) {
        use srt_transport::advanced::caller::{LogicalCallerState, PoolOutcome};
        use srt_transport::{CallerConfig, SocketOwnership};

        let runtime = production_runtime(ProductionRuntimeConfig::for_owner(
            CALLER_TX,
            SRT_OWNER_WIRE_CEILING,
        ))
        .expect("io_uring runtime for the test caller");
        runtime.block_on(async {
            let mut owner = Owner::new_with_ceiling(CALLER_TX, SRT_OWNER_WIRE_CEILING);
            let epoch = Instant::now();
            let now = || Timestamp::from_micros(epoch.elapsed().as_micros() as u64);
            let config = CallerConfig::builder(sink)
                .ownership(SocketOwnership::Shared)
                .build()
                .expect("caller config");
            let outcome = owner.connect(&config, now()).expect("caller attaches");
            let PoolOutcome::Admitted(id) = outcome else {
                panic!("the caller was not admitted immediately: {outcome:?}");
            };
            let payload = bytes::Bytes::from(vec![0x47; CALLER_PAYLOAD]);
            let (mut connected, mut sent) = (false, 0);
            let mut events = Vec::new();
            while !done.load(Ordering::Acquire) && epoch.elapsed() < Duration::from_secs(20) {
                let at = now();
                let _ = owner.service(at, OwnerServiceBudget::default()).await;
                owner.poll_caller_events(&mut events);
                events.clear();
                if let Some(mut caller) = owner.logical_caller_mut(&id) {
                    connected |= caller.state() == Some(LogicalCallerState::Connected);
                    while connected
                        && sent < payloads
                        && caller.can_send()
                        && matches!(caller.send_shared(payload.clone(), at), Ok(legs) if legs > 0)
                    {
                        sent += 1;
                    }
                }
                owner.wait_for_activity(Duration::from_millis(2)).await;
            }
            if let Some(mut caller) = owner.logical_caller_mut(&id) {
                caller.disconnect(now());
            }
            let _ = owner.shutdown_and_drain(Duration::from_secs(2)).await;
            (connected, sent)
        })
    }

    /// Real callers from distinct source ports reach a 2-Owner reuseport
    /// group: every one is accepted and its bytes counted, and both Owners
    /// publish their own RX counters.
    #[test]
    fn multi_owner_sink_accepts_and_counts_real_callers() {
        const CALLERS: usize = 8;
        const PAYLOADS: usize = 50;
        let port = free_udp_ports(1)[0];
        let pool = HarnessSrtSinkPool::start(&[port], 0, 2).expect("start multi-owner sink pool");
        let sink = SocketAddr::from(([127, 0, 0, 1], port));
        let done = Arc::new(AtomicBool::new(false));
        let callers: Vec<_> = (0..CALLERS)
            .map(|_| {
                let done = done.clone();
                std::thread::spawn(move || run_caller(sink, PAYLOADS, done))
            })
            .collect();
        let handle = pool.counters();
        let expected = (PAYLOADS * CALLER_PAYLOAD) as u64;
        let delivered = |handle: &SrtSinkCountersHandle| {
            let peers = handle.per_peer_bytes();
            handle.snapshot().accepted == CALLERS as u64
                && peers.len() == CALLERS
                && peers.iter().all(|(_, bytes)| *bytes == expected)
        };
        let owner_rx_entries = || {
            lock(&pool.counters.owner_rx)
                .keys()
                .filter(|(owner_port, _)| *owner_port == port)
                .count()
        };
        // Checked while the callers are connected: a disconnect retires a
        // peer's byte row.
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline && !delivered(&handle) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let peers = handle.per_peer_bytes();
        let all_delivered = delivered(&handle);
        done.store(true, Ordering::Release);
        let reports: Vec<_> = callers
            .into_iter()
            .map(|caller| caller.join().expect("caller thread"))
            .collect();
        assert!(
            reports.iter().all(|&report| report == (true, PAYLOADS)),
            "{reports:?}"
        );
        let snapshot = handle.snapshot();
        assert!(
            all_delivered,
            "every caller accepted with all its bytes: {snapshot:?} {peers:?}"
        );
        assert_eq!(snapshot.accepted, CALLERS as u64, "{snapshot:?}");
        assert_eq!(snapshot.discarded_bytes, CALLERS as u64 * expected);
        // Both Owners publish within `PUBLISH_EVERY`, traffic or not.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && owner_rx_entries() < 2 {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(owner_rx_entries(), 2, "each Owner publishes its own RX row");
        assert_eq!(handle.drops().owner_faults, 0);
        pool.stop();
    }

    /// A same-user socket already in a port's reuseport group (a leftover
    /// sink) refuses the pool instead of being joined.
    #[test]
    fn sink_pool_refuses_to_join_a_foreign_reuseport_socket() {
        let port = free_udp_ports(1)[0];
        let foreign =
            crate::udp_drain::bind_drain_socket(port, 1 << 20, Duration::from_millis(100), true)
                .expect("foreign SO_REUSEPORT socket");
        match HarnessSrtSinkPool::start(&[port], 0, 2) {
            Err(error) => assert!(error.contains("already have a UDP socket"), "{error}"),
            Ok(pool) => {
                pool.stop();
                panic!("the sink joined a foreign reuseport group on port {port}");
            }
        }
        drop(foreign);
    }

    #[test]
    fn sink_pool_rejects_duplicate_ports() {
        let port = free_udp_ports(1)[0];
        match HarnessSrtSinkPool::start(&[port, port], 0, 2) {
            Err(error) => assert!(error.contains("duplicate port"), "{error}"),
            Ok(pool) => {
                pool.stop();
                panic!("duplicate sink ports unexpectedly started");
            }
        }
    }

    #[test]
    fn sink_owner_rx_drops_aggregate_per_owner() {
        let counters = Arc::new(SinkCounters::default());
        lock(&counters.owner_rx).extend([
            (
                (9000, 0),
                OwnerRxDrops {
                    dropped: 3,
                    buffer_exhaustions: 2,
                    truncated: 1,
                },
            ),
            (
                (9000, 1),
                OwnerRxDrops {
                    dropped: 5,
                    buffer_exhaustions: 4,
                    truncated: 2,
                },
            ),
        ]);
        counters.owner_faults.store(1, Ordering::Release);
        let handle = SrtSinkCountersHandle {
            counters,
            ports: Arc::from([9000]),
        };
        let drops = handle.drops();
        assert_eq!(
            drops.owner,
            OwnerRxDrops {
                dropped: 8,
                buffer_exhaustions: 6,
                truncated: 3,
            }
        );
        assert_eq!(drops.owner_faults, 1);
        assert_eq!(drops.json()["ownerFaults"], 1);
        assert_eq!(
            drops.since(&drops).owner_faults,
            1,
            "a fault stays visible in every later window"
        );
    }

    #[test]
    fn sink_pool_rejects_a_port_already_bound() {
        let ports = free_udp_ports(1);
        let pool = HarnessSrtSinkPool::start(&ports, 0, 1).expect("start sink pool");
        assert!(HarnessSrtSinkPool::start(&ports, 0, 1).is_err());
        pool.stop();
    }

    #[test]
    fn udp_table_rows_yield_port_inode_and_drops() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
          1: 00000000:1F90 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 12345 2 0000000000000000 17\n\
          2: 0100007F:1F90 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 12346 2 0000000000000000 3\n\
          3: 0100007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 23456 2 0000000000000000 0\n";
        let rows: Vec<_> = parse_udp_socket_drops(table).collect();
        assert_eq!(rows, [(8080, 12345, 17), (8080, 12346, 3), (53, 23456, 0)]);
        let v6 = "header\n  0: 00000000000000000000000000000000:1F91 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000 1000 0 1 2 0000000000000000 5\n";
        assert_eq!(
            parse_udp_socket_drops(v6).collect::<Vec<_>>(),
            [(8081, 1, 5)]
        );
    }

    #[test]
    fn window_drops_subtract_the_start_reading_per_socket() {
        let start = SrtSinkDrops {
            owner: OwnerRxDrops {
                dropped: 10,
                buffer_exhaustions: 1,
                truncated: 0,
            },
            owner_faults: 0,
            socket: Some(BTreeMap::from([((1, 11), 100), ((1, 12), 5), ((2, 21), 5)])),
        };
        let end = SrtSinkDrops {
            owner: OwnerRxDrops {
                dropped: 25,
                buffer_exhaustions: 1,
                truncated: 2,
            },
            owner_faults: 0,
            socket: Some(BTreeMap::from([
                ((1, 11), 160),
                ((1, 12), 5),
                ((2, 21), 5),
                ((3, 31), 9),
            ])),
        };
        let window = end.since(&start);
        assert_eq!(window.owner.dropped, 15);
        assert_eq!(window.owner.truncated, 2);
        assert_eq!(
            window.socket,
            Some(BTreeMap::from([
                ((1, 11), 60),
                ((1, 12), 0),
                ((2, 21), 0),
                ((3, 31), 9),
            ]))
        );
        let unknown = SrtSinkDrops {
            socket: None,
            ..end.clone()
        };
        assert_eq!(unknown.since(&start).socket, None);
    }

    #[test]
    fn sink_drop_json_identifies_each_socket() {
        let drops = SrtSinkDrops {
            socket: Some(BTreeMap::from([((9000, 11), 3), ((9000, 12), 5)])),
            ..SrtSinkDrops::default()
        }
        .json();
        assert_eq!(
            drops["socketDrops"],
            serde_json::json!([
                {"port": 9000, "inode": 11, "drops": 3},
                {"port": 9000, "inode": 12, "drops": 5}
            ])
        );
        assert_eq!(drops["socketDropsTotal"], 8);
    }

    proptest::proptest! {
        /// Any well-formed row retains its port, inode and drop count.
        #[test]
        fn udp_rows_round_trip(port in proptest::num::u16::ANY, drops in proptest::num::u64::ANY, addr in "[0-9A-F]{8}") {
            let table = format!(
                "header\n  0: {addr}:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 1 2 0000000000000000 {drops}\n"
            );
            let rows: Vec<_> = parse_udp_socket_drops(&table).collect();
            proptest::prop_assert_eq!(rows, vec![(port, 1, drops)]);
        }
    }
}
