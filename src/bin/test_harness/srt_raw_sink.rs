//! Tokio/srt-rs receiver used by the SRT transport fault proof.
//!
//! The sink completes the SRT handshake, lets srt-rs generate protocol
//! acknowledgements, and discards application payloads after delivery to the
//! protocol core. The transport bounds queued unread delivery, so its receive
//! window remains a real source of sender backpressure.

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use shiguredo_srt::{ConnectionEvent, Timestamp};
use srt_transport::{
    HighResWaiter, IngressTelemetry, ListenerConfig, ListenerTopology, MonotonicDeadline,
    PeerTable, RecvBatch, RuntimeFlavor,
};
use tokio::net::UdpSocket;

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    connected_now: AtomicU64,
    peak_connected: AtomicU64,
    data_events: AtomicU64,
}

/// A sample of what the sink has seen, safe to fold into a result artifact.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawSrtSinkObservation {
    pub(crate) accepted: u64,
    pub(crate) connected_now: u64,
    pub(crate) peak_connected: u64,
    /// Number of application payload events delivered by srt-rs and discarded
    /// by the harness sink.
    pub(crate) data_events: u64,
}

impl RawSrtSinkObservation {
    pub(crate) fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "accepted": self.accepted,
            "connectedNow": self.connected_now,
            "peakConnected": self.peak_connected,
            "dataEvents": self.data_events,
        })
    }
}

/// An SRT listener that keeps the protocol alive and discards delivered media.
pub(crate) struct RawSrtSink {
    port: u16,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    thread: Option<JoinHandle<()>>,
}

impl RawSrtSink {
    pub(crate) fn start(port: u16) -> Result<Self, String> {
        let bind: SocketAddr = format!("127.0.0.1:{port}")
            .parse()
            .map_err(|error| format!("parse raw SRT sink address: {error}"))?;
        let prepared = ListenerConfig::builder(bind)
            .topology(ListenerTopology::PerPort)
            .build()
            .map_err(|error| format!("build raw SRT sink: {error}"))?
            .prepare(RuntimeFlavor::Tokio)
            .map_err(|error| format!("prepare raw SRT sink: {error}"))?;
        let mut sockets = prepared
            .bind_sockets()
            .map_err(|error| format!("bind raw SRT sink on {port}: {error}"))?;
        let socket = sockets
            .pop()
            .ok_or_else(|| "raw SRT sink produced no UDP socket".to_string())?;
        let admission = prepared.admission_options();
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let thread_stop = stop.clone();
        let thread_counters = counters.clone();
        let thread = std::thread::Builder::new()
            .name(format!("srt-rs-stall-sink-{port}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => return,
                };
                runtime.block_on(run_sink(socket, admission, thread_stop, thread_counters));
            })
            .map_err(|error| format!("spawn raw SRT sink: {error}"))?;
        Ok(Self {
            port,
            stop,
            counters,
            thread: Some(thread),
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn observe(&self) -> RawSrtSinkObservation {
        RawSrtSinkObservation {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            connected_now: self.counters.connected_now.load(Ordering::Relaxed),
            peak_connected: self.counters.peak_connected.load(Ordering::Relaxed),
            data_events: self.counters.data_events.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RawSrtSink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn run_sink(
    std_socket: std::net::UdpSocket,
    admission: srt_transport::AdmissionOptions,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
) {
    let Ok(socket) = UdpSocket::from_std(std_socket) else {
        return;
    };
    let mut peers = srt_transport::PeerTable::new();
    let telemetry = IngressTelemetry::default();
    let mut recv_batch = RecvBatch::new();
    let mut outbound = Vec::new();
    let mut events = Vec::new();
    let Ok(mut waiter) = HighResWaiter::<()>::new() else {
        return;
    };
    if waiter.register((), socket.as_raw_fd()).is_err() {
        return;
    }
    let mut due = Vec::new();
    let mut ready = Vec::new();
    let started = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let now = sink_timestamp(started);
        let wait = listener_wait_duration(&mut peers, now);
        // Dedicated current-thread runtime: park directly. `block_in_place`
        // panics on this flavor.
        let socket_ready = match park_listener(&mut waiter, &mut due, &mut ready, wait) {
            Ok(ready) => ready,
            Err(_) => continue,
        };
        if socket_ready {
            let now = sink_timestamp(started);
            // HighResWaiter observed the raw fd. `drain_readable` needs
            // Tokio READABLE, which is still unset after a waiter park.
            let _ = drain_woken_listener(
                &socket,
                &mut recv_batch,
                restream::media::srt::tokio_egress::recv_budget(),
                |addr, data| {
                    let Some(peer) = addr else {
                        return;
                    };
                    let _ = peers.admit(peer, data, now, &admission, 0, 1, &telemetry);
                },
            );
        }
        let now = sink_timestamp(started);
        peers.poll_outbound(now, &mut outbound);
        for (peer, packet) in outbound.drain(..) {
            let _ = socket.send_to(&packet, peer).await;
        }
        peers.poll_events(&mut events);
        for event in events.drain(..) {
            match event.event {
                ConnectionEvent::Connected => {
                    let connected = counters.connected_now.fetch_add(1, Ordering::Relaxed) + 1;
                    counters.accepted.fetch_add(1, Ordering::Relaxed);
                    counters
                        .peak_connected
                        .fetch_max(connected, Ordering::Relaxed);
                }
                ConnectionEvent::DataReceived { .. } => {
                    counters.data_events.fetch_add(1, Ordering::Relaxed);
                }
                ConnectionEvent::Disconnected { .. } => {
                    counters.connected_now.fetch_sub(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }
}

const LISTENER_IDLE: Duration = Duration::from_millis(5);

fn listener_wait_duration(peers: &mut PeerTable, now: Timestamp) -> Duration {
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

fn sink_timestamp(started: Instant) -> Timestamp {
    Timestamp::from_micros(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind probe socket");
        socket.local_addr().expect("probe socket addr").port()
    }

    #[test]
    fn raw_srt_sink_starts_and_stops_without_connections() {
        let port = free_udp_port();
        let sink = RawSrtSink::start(port).expect("start raw SRT sink");
        assert_eq!(sink.port(), port);
        assert_eq!(sink.observe().accepted, 0);
        sink.stop();
    }

    #[test]
    fn raw_srt_sink_rejects_a_port_already_bound() {
        let port = free_udp_port();
        let sink = RawSrtSink::start(port).expect("start raw SRT sink");
        let conflict = RawSrtSink::start(port);
        assert!(
            conflict.is_err(),
            "second sink on port {port} unexpectedly bound"
        );
        sink.stop();
    }

    #[test]
    fn woken_raw_sink_drains_without_tokio_readable() {
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
                drain_woken_listener(&sock, &mut batch, RecvBudget::default(), |_, data| {
                    got.push(data.to_vec())
                })
                .expect("drain after waiter");
            assert_eq!(report.datagrams, 1);
            assert_eq!(got, [b"ping".to_vec()]);
        });
    }
}
