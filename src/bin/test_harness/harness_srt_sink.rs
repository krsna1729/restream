//! Tokio-native SRT accept-and-discard listeners for scaling tests.
//!
//! The sink deliberately uses the same `srt-rs` admission and Tokio transport
//! crates as production. It performs no media parsing and records only byte
//! and connection counters, so MediaMTX is not in the scaling critical path.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use srt_proto::ConnectionEvent;
use srt_transport::advanced::admission::PeerTable;
use srt_transport::advanced::driver::RecvBudget;
use srt_transport::advanced::native_io::{HighResWaiter, MonotonicDeadline, RecvBatch};
use srt_transport::advanced::telemetry::IngressTelemetry;
use srt_transport::{
    ListenerConfig, ListenerTopology, RuntimeFlavor, SocketBufferConfig, WorkerCount,
};
use tokio::net::UdpSocket;

/// Cloneable handle to a running pool's counters.
#[derive(Clone)]
pub(crate) struct SrtSinkCountersHandle {
    counters: Arc<SinkCounters>,
}

impl SrtSinkCountersHandle {
    /// Cumulative payload bytes per logical connection, keyed by (sink port,
    /// pool-wide connection sequence for each srt-rs `LogicalPeerId`) — not by
    /// address: Restream's SRT callers share
    /// UDP sockets, so many connections arrive from one address. Published by
    /// each sink thread every `PER_PEER_PUBLISH`.
    pub(crate) fn per_peer_bytes(&self) -> Vec<((u16, u64), u64)> {
        self.counters
            .per_peer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(peer, bytes)| (*peer, *bytes))
            .collect()
    }

    pub(crate) fn snapshot(&self) -> SrtSinkCounters {
        SrtSinkCounters {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            discarded_bytes: self.counters.discarded_bytes.load(Ordering::Relaxed),
            closed: self.counters.closed.load(Ordering::Relaxed),
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

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    discarded_bytes: AtomicU64,
    closed: AtomicU64,
    per_peer: Mutex<HashMap<(u16, u64), u64>>,
    /// Pool-wide connection sequence: sink threads can share a port, so a
    /// per-thread index would collide.
    next_connection: AtomicU64,
}

/// How often a sink thread publishes its per-connection byte counts; far
/// below the delivery sampler's 1 s tick, and off the per-packet path.
const PER_PEER_PUBLISH: Duration = Duration::from_millis(100);

pub(crate) struct HarnessSrtSinkPool {
    ports: Vec<u16>,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    threads: Vec<JoinHandle<()>>,
}

impl HarnessSrtSinkPool {
    pub(crate) fn start(
        ports: &[u16],
        udp_buffer: usize,
        thread_count: usize,
    ) -> Result<Self, String> {
        if ports.is_empty() {
            return Err("harness SRT sink pool needs at least one port".to_string());
        }
        let thread_count = thread_count.max(ports.len());
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let mut workers = Vec::with_capacity(thread_count);
        let base = thread_count / ports.len();
        let remainder = thread_count % ports.len();
        for (index, port) in ports.iter().copied().enumerate() {
            let socket_count = base + usize::from(index < remainder);
            let config = ListenerConfig::builder(SocketAddr::from(([0, 0, 0, 0], port)))
                .topology(if socket_count == 1 {
                    ListenerTopology::PerPort
                } else {
                    ListenerTopology::ReusePortMulti {
                        acceptors: WorkerCount::Count(
                            NonZeroUsize::new(socket_count).expect("sink worker count is non-zero"),
                        ),
                    }
                })
                .configure_transport(|transport| {
                    transport.socket_buffers = NonZeroUsize::new(udp_buffer)
                        .map(SocketBufferConfig::Bytes)
                        .unwrap_or(SocketBufferConfig::SystemDefault);
                })
                .build()
                .map_err(|error| format!("bind SRT sink port {port}: {error}"))?
                .prepare(RuntimeFlavor::Tokio)
                .map_err(|error| format!("bind SRT sink port {port}: {error}"))?;
            for socket in config
                .bind_sockets()
                .map_err(|error| format!("bind SRT sink port {port}: {error}"))?
            {
                workers.push((config.clone(), socket));
            }
        }
        let (ready_tx, ready_rx) = mpsc::sync_channel(workers.len());
        let mut threads = Vec::with_capacity(thread_count);
        for (worker, (config, socket)) in workers.into_iter().enumerate() {
            let thread_stop = stop.clone();
            let counters = counters.clone();
            let ready_tx = ready_tx.clone();
            match std::thread::Builder::new()
                .name(format!("harness-srt-rs-sink-{worker}"))
                .spawn(move || sink_thread(config, socket, thread_stop, counters, ready_tx))
            {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    stop.store(true, Ordering::Release);
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(format!("spawn harness SRT sink thread: {error}"));
                }
            }
        }
        drop(ready_tx);
        for _ in 0..threads.len() {
            match ready_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    stop.store(true, Ordering::Release);
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(error);
                }
                Err(error) => {
                    stop.store(true, Ordering::Release);
                    for thread in threads {
                        let _ = thread.join();
                    }
                    return Err(format!(
                        "timed out waiting for harness SRT sink bind: {error}"
                    ));
                }
            }
        }
        tracing::info!(
            "[harness-srt-sink] srt-rs Tokio sink on {} port(s), {} thread(s)",
            ports.len(),
            thread_count
        );
        Ok(Self {
            ports: ports.to_vec(),
            stop,
            counters,
            threads,
        })
    }

    /// A cloneable handle to the pool's counters, so a state endpoint can
    /// report them while the pool keeps owning its threads.
    pub(crate) fn counters(&self) -> SrtSinkCountersHandle {
        SrtSinkCountersHandle {
            counters: self.counters.clone(),
        }
    }

    /// Cumulative counters so far (connections accepted, payload bytes
    /// discarded, connections closed). Readable while the pool is running.
    pub(crate) fn snapshot(&self) -> SrtSinkCounters {
        SrtSinkCounters {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            discarded_bytes: self.counters.discarded_bytes.load(Ordering::Relaxed),
            closed: self.counters.closed.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let counters = SrtSinkCounters {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            discarded_bytes: self.counters.discarded_bytes.load(Ordering::Relaxed),
            closed: self.counters.closed.load(Ordering::Relaxed),
        };
        tracing::info!(
            "[harness-srt-sink] stopped {} port(s), accepted={}, discarded={}MB, closed={}",
            self.ports.len(),
            counters.accepted,
            counters.discarded_bytes / (1024 * 1024),
            counters.closed,
        );
    }
}

fn sink_thread(
    config: srt_transport::advanced::prepared::PreparedListener,
    socket: std::net::UdpSocket,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    ready_tx: SyncSender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "failed to build harness SRT Tokio runtime");
            let _ = ready_tx.send(Err(format!("build harness SRT Tokio runtime: {error}")));
            return;
        }
    };
    runtime.block_on(async move {
        let port = config.bind.port();
        if let Err(error) = sink_port(config, socket, stop, counters, ready_tx).await {
            tracing::error!(port, %error, "harness srt-rs sink stopped");
        }
    });
}

async fn sink_port(
    config: srt_transport::advanced::prepared::PreparedListener,
    socket: std::net::UdpSocket,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    ready_tx: SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let socket = UdpSocket::from_std(socket).map_err(|error| error.to_string())?;
    let _ = ready_tx.send(Ok(()));
    let options = config.admission_options();
    let mut peers = config.peer_table();
    let telemetry = IngressTelemetry::default();
    let mut recv_batch = RecvBatch::with_capacity(64, 2048);
    let mut outputs = Vec::with_capacity(64);
    let mut events = Vec::with_capacity(64);
    let mut waiter = HighResWaiter::<()>::new().map_err(|error| error.to_string())?;
    waiter
        .register((), socket.as_raw_fd())
        .map_err(|error| error.to_string())?;
    let mut due = Vec::new();
    let mut ready = Vec::new();
    let port = socket
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    // Connection index per logical peer (its raw id is private to srt-rs).
    let mut per_peer: HashMap<srt_transport::advanced::admission::LogicalPeerId, (u64, u64)> =
        HashMap::new();
    let mut published_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let wait = listener_wait_duration(&mut peers, srt_now());
        // Dedicated current-thread runtime: park directly. `block_in_place`
        // panics on this flavor.
        let socket_ready = park_listener(&mut waiter, &mut due, &mut ready, wait)
            .map_err(|error| error.to_string())?;
        if socket_ready {
            // HighResWaiter observed the raw fd. Do not use
            // `drain_readable` here: Tokio READABLE is unset after a
            // waiter park, so handshake datagrams become WouldBlock.
            let _ = drain_woken_listener(
                &socket,
                &mut recv_batch,
                restream::media::srt::srt_knobs::recv_budget_or(RecvBudget::new(8, 512)),
                |addr, data| {
                    let Some(peer) = addr else {
                        return;
                    };
                    let _ = peers.admit(peer, data, srt_now(), &options, 0, 1, &telemetry);
                },
            );
        }

        peers.poll_outbound(srt_now(), &mut outputs);
        for (peer, packet) in outputs.drain(..) {
            let _ = socket.send_to(&packet, peer).await;
        }
        peers.poll_events(&mut events);
        for event in events.drain(..) {
            match event.event {
                ConnectionEvent::Connected => {
                    counters.accepted.fetch_add(1, Ordering::Relaxed);
                }
                ConnectionEvent::DataReceived { payload, .. } => {
                    counters
                        .discarded_bytes
                        .fetch_add(payload.len() as u64, Ordering::Relaxed);
                    per_peer
                        .entry(event.logical_peer)
                        .or_insert_with(|| {
                            (counters.next_connection.fetch_add(1, Ordering::Relaxed), 0)
                        })
                        .1 += payload.len() as u64;
                }
                ConnectionEvent::Disconnected { .. } => {
                    counters.closed.fetch_add(1, Ordering::Relaxed);
                    // A closed connection is no longer a destination: keep
                    // reused sink stacks from reporting it as a stalled one.
                    if let Some((index, _)) = per_peer.remove(&event.logical_peer) {
                        counters
                            .per_peer
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&(port, index));
                    }
                }
                _ => {}
            }
        }
        if published_at.elapsed() >= PER_PEER_PUBLISH {
            published_at = Instant::now();
            counters
                .per_peer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend(
                    per_peer
                        .values()
                        .map(|(index, bytes)| ((port, *index), *bytes)),
                );
        }
    }
    Ok(())
}

const LISTENER_IDLE: Duration = Duration::from_millis(5);

fn listener_wait_duration(peers: &mut PeerTable, now: srt_proto::Timestamp) -> Duration {
    Duration::from_micros(
        peers
            .time_until_next_deadline(now, listener_idle_micros())
            .min(listener_idle_micros()),
    )
}

fn listener_idle_micros() -> u64 {
    u64::try_from(LISTENER_IDLE.as_micros()).unwrap_or(u64::MAX)
}

fn park_listener(
    waiter: &mut HighResWaiter<()>,
    due: &mut Vec<()>,
    ready: &mut Vec<()>,
    wait: Duration,
) -> std::io::Result<bool> {
    waiter.set_deadline((), MonotonicDeadline::after(wait))?;
    waiter.wait(due, ready)?;
    Ok(!ready.is_empty())
}

fn drain_woken_listener(
    socket: &UdpSocket,
    recv_batch: &mut RecvBatch,
    budget: RecvBudget,
    on_datagram: impl FnMut(Option<SocketAddr>, &[u8]),
) -> std::io::Result<srt_transport::advanced::native_io::RecvDrainReport> {
    srt_transport::advanced::native_io::drain_recv_fd(
        socket.as_raw_fd(),
        recv_batch,
        budget,
        on_datagram,
    )
}

fn srt_now() -> srt_proto::Timestamp {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    srt_proto::Timestamp::from_micros(
        START
            .get_or_init(Instant::now)
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64,
    )
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
    fn sink_pool_starts_and_stops_without_connections() {
        HarnessSrtSinkPool::start(&free_udp_ports(1), 0, 1)
            .expect("start sink pool")
            .stop();
    }

    #[test]
    fn sink_pool_rejects_a_port_already_bound() {
        let ports = free_udp_ports(1);
        let pool = HarnessSrtSinkPool::start(&ports, 0, 1).expect("start sink pool");
        assert!(HarnessSrtSinkPool::start(&ports, 0, 1).is_err());
        pool.stop();
    }

    #[test]
    fn sink_pool_clamps_threads_to_ports() {
        let pool = HarnessSrtSinkPool::start(&free_udp_ports(2), 0, 8).expect("start sink pool");
        assert_eq!(pool.threads.len(), 8);
        pool.stop();
    }

    #[test]
    fn woken_sink_drains_without_tokio_readable() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("Tokio runtime builds");
        runtime.block_on(async {
            let receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("receiver binds");
            receiver
                .set_nonblocking(true)
                .expect("receiver is nonblocking");
            let dest = receiver.local_addr().expect("receiver address");
            let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("sender binds");
            sender.send_to(b"ping", dest).expect("send datagram");
            let sock = UdpSocket::from_std(receiver).expect("tokio adopts the socket");

            let mut waiter = HighResWaiter::<()>::new().expect("waiter");
            waiter
                .register((), sock.as_raw_fd())
                .expect("register listener fd");
            let mut due = Vec::new();
            let mut ready = Vec::new();
            assert!(park_listener(&mut waiter, &mut due, &mut ready, LISTENER_IDLE).expect("wait"));

            let mut batch = RecvBatch::new();
            let mut got = Vec::new();
            let report =
                drain_woken_listener(&sock, &mut batch, RecvBudget::new(8, 512), |_, data| {
                    got.push(data.to_vec())
                })
                .expect("drain after waiter");
            assert_eq!(report.datagrams, 1);
            assert_eq!(got, [b"ping".to_vec()]);
        });
    }
}
