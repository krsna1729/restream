//! SRT accept-and-discard listeners for scaling tests, on the production
//! receive path: each port gets one `srt_transport::compio::Owner` thread,
//! built exactly like the SRT ingress Owners (forced io_uring runtime, managed
//! multishot RX when the kernel supports it). Parallelism comes from ports the
//! harness spreads outputs over (`HARNESS_SRT_SINK_PORTS_PER_PEER`), never from
//! kernel `SO_REUSEPORT` hashing: Restream sends every SRT output of an egress
//! shard from one UDP socket, so a hash sees one flow per shard. The sink
//! performs no media parsing and records only byte, connection and drop
//! counters, so MediaMTX is not in the scaling critical path.
//!
//! Drops are counted at both layers that can lose a datagram before SRT sees
//! it: each kernel socket (`/proc/net/udp{,6}` `drops`, per local port and
//! inode) and the Owners' managed-RX rings (`dropped`, `buffer_exhaustions`,
//! `truncated`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use restream_engine::srt::srt_owner_runtime::{
    SRT_OWNER_WIRE_CEILING, listener_shape, production_runtime,
};
use srt_proto::{ConnectionEvent, Timestamp};
use srt_transport::advanced::admission::{AdmissionEvent, BondedInputPolicy, LogicalPeerId};
use srt_transport::compio::{
    Owner, OwnerServiceBudget, ProductionRuntimeConfig, RxModePolicy, observe_production_runtime,
};
use srt_transport::{
    ListenerConfig, OwnerListenerPlan, RuntimeFlavor, SocketBufferConfig, owner_plans,
};

use crate::core::PEER_PORT_STRIDE;

/// The sink only replies with protocol control (handshake, ACK/NAK).
const SINK_TX_CAPACITY: usize = 64;
/// Longest park when nothing is due, so `stop` is observed promptly.
const IDLE_PARK: Duration = Duration::from_millis(5);
/// How often a sink thread publishes per-connection bytes and Owner RX stats;
/// far below the delivery sampler's 1 s tick, and off the per-packet path.
const PUBLISH_EVERY: Duration = Duration::from_millis(100);
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);
const DEFAULT_SINK_PORTS_PER_PEER: usize = 2;
/// A peer instance's sink ports stay inside its `PEER_PORT_STRIDE` band.
const MAX_SINK_PORTS_PER_PEER: usize = PEER_PORT_STRIDE as usize;

/// SRT sink ports (one Owner each) per peer instance,
/// `HARNESS_SRT_SINK_PORTS_PER_PEER`.
pub(crate) fn configured_ports_per_peer() -> usize {
    let ports_per_peer = std::env::var("HARNESS_SRT_SINK_PORTS_PER_PEER")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SINK_PORTS_PER_PEER);
    clamp_ports_per_peer(ports_per_peer)
}

fn clamp_ports_per_peer(ports_per_peer: usize) -> usize {
    ports_per_peer.clamp(1, MAX_SINK_PORTS_PER_PEER)
}

/// Where spread slot `slot` (an output-name hash or ordinal) lands among
/// `peer_count` sink instances of `ports_per_peer` ports each:
/// `(instance, port offset)`. The instance is `slot % peer_count`, as the
/// per-instance spread picks it; the offset then takes that instance's ports
/// in turn, one sink Owner each.
pub(crate) fn sink_slot(slot: u64, peer_count: usize, ports_per_peer: usize) -> (usize, u16) {
    let peers = peer_count.max(1) as u64;
    let offset = (slot / peers) % ports_per_peer.max(1) as u64;
    ((slot % peers) as usize, offset as u16)
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
    /// Owners that faulted; a faulted Owner stops serving its port.
    pub owner_faults: u64,
    pub socket: Option<BTreeMap<(u16, u64), u64>>,
}

impl SrtSinkDrops {
    /// Drops accumulated since `start` (a rated window's first reading).
    /// A socket present at `start` but gone by now loses that window's
    /// drops. Owner faults stay cumulative: a faulted Owner never serves its
    /// port again, so every later window must show it.
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
    /// Managed-RX drops per port (one Owner each).
    owner_rx: Mutex<HashMap<u16, OwnerRxDrops>>,
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
    restream_sync::lock::lock(mutex)
}

pub(crate) struct HarnessSrtSinkPool {
    ports: Arc<[u16]>,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    threads: Vec<JoinHandle<()>>,
}

impl HarnessSrtSinkPool {
    /// Start one Owner thread per port, each with its own runtime and
    /// socket. The sockets do not set `SO_REUSEPORT`, so a port that already
    /// has a UDP socket (a leftover sink) fails to bind instead of sharing it.
    pub(crate) fn start(ports: &[u16], udp_buffer: usize) -> Result<Self, String> {
        if ports.is_empty() {
            return Err("harness SRT sink pool needs at least one port".to_string());
        }
        let mut unique_ports = HashSet::with_capacity(ports.len());
        for &port in ports {
            if !unique_ports.insert(port) {
                return Err(format!("harness SRT sink pool has duplicate port {port}"));
            }
        }
        let plans = ports
            .iter()
            .map(|&port| Ok((port, build_listener_plan(port, udp_buffer)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let (ready_tx, ready_rx) = mpsc::sync_channel(plans.len());
        let mut pool = Self {
            ports: ports.into(),
            stop: Arc::new(AtomicBool::new(false)),
            counters: Arc::new(SinkCounters::default()),
            threads: Vec::with_capacity(plans.len()),
        };
        for (port, plan) in plans {
            let stop = pool.stop.clone();
            let counters = pool.counters.clone();
            let ready_tx = ready_tx.clone();
            match std::thread::Builder::new()
                .name(format!("srt-s-{port}"))
                .spawn(move || sink_thread(port, plan, stop, counters, ready_tx))
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
            "[harness-srt-sink] srt-rs Owner sink on {} port(s), one Owner per port",
            ports.len()
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
            "[harness-srt-sink] stopped {} port(s), accepted={}, discarded={}MB, closed={}, \
             owner_rx_dropped={}, owner_faults={}, socket_drops={:?}",
            self.ports.len(),
            counters.accepted,
            counters.discarded_bytes / (1024 * 1024),
            counters.closed,
            drops.owner.dropped,
            drops.owner_faults,
            drops.socket.map(|ports| ports.values().sum::<u64>()),
        );
    }
}

/// The single-Owner (`PerPort`) listener plan for `port`, shaped as the
/// ingress shapes a one-Owner listener.
fn build_listener_plan(port: u16, udp_buffer: usize) -> Result<OwnerListenerPlan, String> {
    let (topology, promotion) = listener_shape(NonZeroUsize::MIN);
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
    let plans = owner_plans(&config, RuntimeFlavor::Compio).map_err(|error| error.to_string())?;
    let [plan] = <[OwnerListenerPlan; 1]>::try_from(plans).map_err(|plans| {
        format!(
            "SRT sink port {port} planned {} Owners, expected 1",
            plans.len()
        )
    })?;
    Ok(plan)
}

fn sink_thread(
    port: u16,
    plan: OwnerListenerPlan,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    ready_tx: SyncSender<Result<(), String>>,
) {
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
    // Connection index and bytes per logical peer (its raw id is private).
    let mut per_peer: HashMap<LogicalPeerId, (u64, u64)> = HashMap::new();
    let mut published_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        // One non-blocking driver poll per visit reaps TX completions and RX.
        runtime.enter(|| {
            runtime.poll_with(Some(Duration::ZERO));
            runtime.run();
        });
        let at = now();
        let report = runtime.block_on(owner.service(at, OwnerServiceBudget::default()));
        if let Some(fault) = owner.fault() {
            tracing::error!(
                port,
                ?fault,
                "harness SRT sink Owner faulted; its port stops"
            );
            counters.owner_faults.fetch_add(1, Ordering::AcqRel);
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
                    port,
                    OwnerRxDrops {
                        dropped: rx.dropped,
                        buffer_exhaustions: rx.buffer_exhaustions,
                        truncated: rx.truncated,
                    },
                );
            }
        }
        if !report.work_remaining {
            let wait =
                Duration::from_micros(owner.time_until_next_deadline(at, idle_us)).min(IDLE_PARK);
            runtime.block_on(owner.wait_for_activity(wait));
        }
    }
    if !runtime.block_on(owner.shutdown_and_drain(SHUTDOWN_DRAIN)) {
        tracing::warn!(port, "harness SRT sink Owner did not reach quiescence");
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
#[path = "harness_srt_sink_tests.rs"]
mod tests;
