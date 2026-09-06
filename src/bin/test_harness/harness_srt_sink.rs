//! Tokio-native SRT accept-and-discard listeners for scaling tests.
//!
//! The sink deliberately uses the same `srt-rs` admission and Tokio transport
//! crates as production. It performs no media parsing and records only byte
//! and connection counters, so MediaMTX is not in the scaling critical path.

use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use shiguredo_srt::ConnectionEvent;
use srt_transport::{
    HighResWaiter, IngressTelemetry, ListenerConfig, ListenerTopology, MonotonicDeadline,
    PeerTable, RecvBatch, RecvBudget, RuntimeFlavor, SocketBufferConfig, WorkerCount,
};
use tokio::net::UdpSocket;

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    discarded_bytes: AtomicU64,
    closed: AtomicU64,
}

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

    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        tracing::info!(
            "[harness-srt-sink] stopped {} port(s), accepted={}, discarded={}MB, closed={}",
            self.ports.len(),
            self.counters.accepted.load(Ordering::Relaxed),
            self.counters.discarded_bytes.load(Ordering::Relaxed) / (1024 * 1024),
            self.counters.closed.load(Ordering::Relaxed),
        );
    }
}

fn sink_thread(
    config: srt_transport::PreparedListener,
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
    config: srt_transport::PreparedListener,
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
                }
                ConnectionEvent::Disconnected { .. } => {
                    counters.closed.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

const LISTENER_IDLE: Duration = Duration::from_millis(5);

fn listener_wait_duration(peers: &mut PeerTable, now: shiguredo_srt::Timestamp) -> Duration {
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
    waiter.set_deadline((), MonotonicDeadline::after(wait));
    waiter.wait(due, ready)?;
    Ok(!ready.is_empty())
}

fn drain_woken_listener(
    socket: &UdpSocket,
    recv_batch: &mut RecvBatch,
    budget: RecvBudget,
    on_datagram: impl FnMut(Option<SocketAddr>, &[u8]),
) -> std::io::Result<srt_transport::RecvDrainReport> {
    srt_transport::drain_recv_fd(socket.as_raw_fd(), recv_batch, budget, on_datagram)
}

fn srt_now() -> shiguredo_srt::Timestamp {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    shiguredo_srt::Timestamp::from_micros(
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
