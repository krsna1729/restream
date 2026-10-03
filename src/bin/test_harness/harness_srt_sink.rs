//! SRT accept-and-discard listeners for scaling tests, on the production
//! receive path: each port gets one `srt_transport::compio::Owner` on its own
//! thread, built exactly like the SRT ingress Owner (forced io_uring runtime,
//! managed multishot RX when the kernel supports it). The sink performs no
//! media parsing and records only byte, connection and drop counters, so
//! MediaMTX is not in the scaling critical path.
//!
//! Drops are counted at both layers that can lose a datagram before SRT sees
//! it: the kernel socket (`/proc/net/udp{,6}` `drops`, per port) and the
//! Owner's managed-RX ring (`dropped`, `buffer_exhaustions`, `truncated`).

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use restream::media::srt::srt_owner_runtime::{SRT_OWNER_WIRE_CEILING, production_runtime};
use srt_proto::{ConnectionEvent, Timestamp};
use srt_transport::advanced::admission::{AdmissionEvent, LogicalPeerId};
use srt_transport::compio::{
    Owner, OwnerServiceBudget, ProductionRuntimeConfig, RxModePolicy, observe_production_runtime,
};
use srt_transport::{ListenerConfig, ListenerTopology, PromotionPolicy, SocketBufferConfig};

/// The sink only replies with protocol control (handshake, ACK/NAK).
const SINK_TX_CAPACITY: usize = 64;
/// Longest park when nothing is due, so `stop` is observed promptly.
const IDLE_PARK: Duration = Duration::from_millis(5);
/// How often a sink thread publishes per-connection bytes and Owner RX stats;
/// far below the delivery sampler's 1 s tick, and off the per-packet path.
const PUBLISH_EVERY: Duration = Duration::from_millis(100);
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

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

    /// Cumulative drops at the kernel socket and the Owner RX ring.
    pub(crate) fn drops(&self) -> SrtSinkDrops {
        let owner = lock(&self.counters.owner_rx)
            .values()
            .fold(OwnerRxDrops::default(), |sum, rx| sum.plus(*rx));
        SrtSinkDrops {
            owner,
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

/// Drops at both layers. `socket` is `None` when `/proc/net/udp` is
/// unreadable, so a missing sensor never reads as zero drops.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SrtSinkDrops {
    pub owner: OwnerRxDrops,
    /// Kernel socket drops per sink port.
    pub socket: Option<BTreeMap<u16, u64>>,
}

impl SrtSinkDrops {
    /// Drops accumulated since `start` (a rated window's first reading).
    pub(crate) fn since(&self, start: &Self) -> Self {
        let socket = match (&self.socket, &start.socket) {
            (Some(end), Some(begin)) => Some(
                end.iter()
                    .map(|(port, drops)| {
                        let base = begin.get(port).copied().unwrap_or(0);
                        (*port, drops.saturating_sub(base))
                    })
                    .collect(),
            ),
            _ => None,
        };
        Self {
            owner: self.owner.since(start.owner),
            socket,
        }
    }

    /// The one JSON shape for sink drops (state endpoint and sweep records).
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "ownerRxDropped": self.owner.dropped,
            "ownerRxBufferExhaustions": self.owner.buffer_exhaustions,
            "ownerRxTruncated": self.owner.truncated,
            "socketDrops": self.socket,
            "socketDropsTotal": self.socket.as_ref().map(|ports| ports.values().sum::<u64>()),
        })
    }
}

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    discarded_bytes: AtomicU64,
    closed: AtomicU64,
    per_peer: Mutex<HashMap<(u16, u64), u64>>,
    owner_rx: Mutex<HashMap<u16, OwnerRxDrops>>,
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
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) struct HarnessSrtSinkPool {
    ports: Arc<[u16]>,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    threads: Vec<JoinHandle<()>>,
}

impl HarnessSrtSinkPool {
    /// One Owner thread per port (`Owner::listen` is per-port), so the port
    /// count is the sink's thread count.
    pub(crate) fn start(ports: &[u16], udp_buffer: usize) -> Result<Self, String> {
        if ports.is_empty() {
            return Err("harness SRT sink pool needs at least one port".to_string());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let (ready_tx, ready_rx) = mpsc::sync_channel(ports.len());
        let mut pool = Self {
            ports: ports.into(),
            stop,
            counters,
            threads: Vec::with_capacity(ports.len()),
        };
        for &port in ports {
            let stop = pool.stop.clone();
            let counters = pool.counters.clone();
            let ready_tx = ready_tx.clone();
            match std::thread::Builder::new()
                .name(format!("harness-srt-sink-{port}"))
                .spawn(move || sink_thread(port, udp_buffer, stop, counters, ready_tx))
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
            "[harness-srt-sink] srt-rs Owner sink on {} port(s)",
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
             owner_rx_dropped={}, socket_drops={:?}",
            self.ports.len(),
            counters.accepted,
            counters.discarded_bytes / (1024 * 1024),
            counters.closed,
            drops.owner.dropped,
            drops.socket.map(|ports| ports.values().sum::<u64>()),
        );
    }
}

fn sink_thread(
    port: u16,
    udp_buffer: usize,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    ready_tx: SyncSender<Result<(), String>>,
) {
    let (mut owner, runtime) = match build_owner(port, udp_buffer) {
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
            tracing::error!(port, ?fault, "harness SRT sink Owner faulted");
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
                ConnectionEvent::Disconnected { .. } => {
                    counters.closed.fetch_add(1, Ordering::Relaxed);
                    // A closed connection is no longer a destination: keep
                    // reused sink stacks from reporting it as a stalled one.
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
fn build_owner(port: u16, udp_buffer: usize) -> Result<(Owner, compio::runtime::Runtime), String> {
    let runtime_config =
        ProductionRuntimeConfig::for_owner(SINK_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    let runtime = production_runtime(runtime_config)?;
    let profile = runtime.block_on(observe_production_runtime(
        &runtime,
        SINK_TX_CAPACITY,
        SRT_OWNER_WIRE_CEILING,
    ));
    let config = ListenerConfig::builder(SocketAddr::from(([0, 0, 0, 0], port)))
        .topology(ListenerTopology::PerPort)
        .configure_transport(|transport| {
            transport.socket_buffers = NonZeroUsize::new(udp_buffer)
                .map(SocketBufferConfig::Bytes)
                .unwrap_or(SocketBufferConfig::SystemDefault);
            transport.promotion = PromotionPolicy::Never;
        })
        .build()
        .map_err(|error| error.to_string())?;
    let mut owner = Owner::new_with_ceiling(SINK_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    owner
        .set_rx_substrate(profile.managed_rx_substrate())
        .map_err(|error| error.to_string())?;
    owner.set_rx_mode_policy(RxModePolicy::ManagedPreferred);
    runtime
        .block_on(async { owner.listen(&config) })
        .map_err(|error| error.to_string())?;
    Ok((owner, runtime))
}

/// Kernel drops per sink port from `/proc/net/udp` and `/proc/net/udp6`.
fn socket_drops(ports: &[u16]) -> Option<BTreeMap<u16, u64>> {
    let mut drops = BTreeMap::new();
    let mut readable = false;
    for path in ["/proc/net/udp", "/proc/net/udp6"] {
        if let Ok(table) = std::fs::read_to_string(path) {
            readable = true;
            for (port, count) in parse_udp_socket_drops(&table) {
                if ports.contains(&port) {
                    *drops.entry(port).or_insert(0) += count;
                }
            }
        }
    }
    readable.then(|| {
        for &port in ports {
            drops.entry(port).or_insert(0);
        }
        drops
    })
}

/// `(local port, drops)` for every socket row of a `/proc/net/udp{,6}` table:
/// field 1 is `ADDR:PORT` in hex, field 12 is the socket's `drops`.
fn parse_udp_socket_drops(table: &str) -> impl Iterator<Item = (u16, u64)> + '_ {
    table.lines().skip(1).filter_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let port = fields.get(1)?.rsplit_once(':')?.1;
        let port = u16::from_str_radix(port, 16).ok()?;
        let drops = fields.get(12)?.parse::<u64>().ok()?;
        Some((port, drops))
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
    fn sink_pool_starts_one_owner_per_port_and_stops() {
        let pool = HarnessSrtSinkPool::start(&free_udp_ports(2), 0).expect("start sink pool");
        assert_eq!(pool.threads.len(), 2);
        let drops = pool.counters().drops();
        let socket = drops.socket.expect("/proc/net/udp readable on Linux");
        assert_eq!(socket.len(), 2, "every sink port reports socket drops");
        pool.stop();
    }

    #[test]
    fn sink_pool_rejects_a_port_already_bound() {
        let ports = free_udp_ports(1);
        let pool = HarnessSrtSinkPool::start(&ports, 0).expect("start sink pool");
        assert!(HarnessSrtSinkPool::start(&ports, 0).is_err());
        pool.stop();
    }

    #[test]
    fn udp_table_rows_yield_port_and_drops() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
          1: 00000000:1F90 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 12345 2 0000000000000000 17\n\
          2: 0100007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 23456 2 0000000000000000 0\n";
        let rows: Vec<_> = parse_udp_socket_drops(table).collect();
        assert_eq!(rows, [(8080, 17), (53, 0)]);
        // An IPv6 row parses the same way (address is 32 hex digits).
        let v6 = "header\n  0: 00000000000000000000000000000000:1F91 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000 1000 0 1 2 0000000000000000 5\n";
        assert_eq!(parse_udp_socket_drops(v6).collect::<Vec<_>>(), [(8081, 5)]);
    }

    #[test]
    fn window_drops_subtract_the_start_reading_per_port() {
        let start = SrtSinkDrops {
            owner: OwnerRxDrops {
                dropped: 10,
                buffer_exhaustions: 1,
                truncated: 0,
            },
            socket: Some(BTreeMap::from([(1, 100), (2, 5)])),
        };
        let end = SrtSinkDrops {
            owner: OwnerRxDrops {
                dropped: 25,
                buffer_exhaustions: 1,
                truncated: 2,
            },
            socket: Some(BTreeMap::from([(1, 160), (2, 5), (3, 9)])),
        };
        let window = end.since(&start);
        assert_eq!(window.owner.dropped, 15);
        assert_eq!(window.owner.truncated, 2);
        assert_eq!(
            window.socket,
            Some(BTreeMap::from([(1, 60), (2, 0), (3, 9)]))
        );
        // A missing reading at either end stays missing, never zero.
        let unknown = SrtSinkDrops {
            socket: None,
            ..end.clone()
        };
        assert_eq!(unknown.since(&start).socket, None);
    }

    proptest::proptest! {
        /// Any well-formed row round-trips its port and drop count.
        #[test]
        fn udp_rows_round_trip(port in proptest::num::u16::ANY, drops in proptest::num::u64::ANY, addr in "[0-9A-F]{8}") {
            let table = format!(
                "header\n  0: {addr}:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 1 2 0000000000000000 {drops}\n"
            );
            let rows: Vec<_> = parse_udp_socket_drops(&table).collect();
            proptest::prop_assert_eq!(rows, vec![(port, drops)]);
        }
    }
}
