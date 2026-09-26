//! Cheap UDP drain peer for substrate benchmarks.
//!
//! The substrate question ("how many datagrams per CPU-second can the sender
//! submit?") is only answerable when the receiving side is deliberately
//! trivial: one wildcard socket per drain thread, a blocking `recv_from` loop,
//! no per-packet work beyond two counters. Its own counters are what let a run
//! be classified — a sender result below target is only attributable to the
//! sender while this peer keeps up and its drop counters stay clean.
//!
//! A `/16` on the peer's veth side makes a whole destination prefix local, so
//! the sender can address 1000 distinct destinations while a single wildcard
//! socket receives them all.

use std::net::UdpSocket;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::peer_state::{
    bind_state_listener, cpus_allowed_list, host_nic_drop_counters, host_udp_drop_counters,
    pin_to_cpuset, run_id, serve_state_json, softnet_counters, udp_drops_since_start,
};
use super::*;

/// A wildcard UDP socket with `SO_REUSEPORT`, a large receive buffer and a
/// read timeout so the drain loop can observe the stop flag.
fn bind_drain_socket(
    port: u16,
    rcvbuf: usize,
    timeout: Duration,
    reuseport: bool,
) -> Result<UdpSocket, String> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    // Own the fd from here on so every early return closes it.
    let socket = unsafe { UdpSocket::from_raw_fd(fd) };
    let enable: libc::c_int = 1;
    // SO_REUSEPORT only when explicitly asked for: per-socket reuseport spreads
    // by flow hash, and with a handful of destination flows it leaves threads
    // idle while one socket absorbs everything.
    let mut options = vec![(libc::SO_REUSEADDR, enable)];
    if reuseport {
        options.push((libc::SO_REUSEPORT, enable));
    }
    for (name, value) in options {
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                name,
                &value as *const _ as *const libc::c_void,
                std::mem::size_of_val(&value) as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(format!(
                "setsockopt({name}): {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    let rcvbuf = rcvbuf as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &rcvbuf as *const _ as *const libc::c_void,
            std::mem::size_of_val(&rcvbuf) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "setsockopt(SO_RCVBUF): {}",
            std::io::Error::last_os_error()
        ));
    }
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr { s_addr: 0 },
        sin_zero: [0; 8],
    };
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&addr) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "bind(:{port}): {}",
            std::io::Error::last_os_error()
        ));
    }
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    Ok(socket)
}

/// One drain thread: count datagrams and bytes until asked to stop. No parsing,
/// no allocation, no logging inside the loop.
fn drain_loop(socket: UdpSocket, stop: Arc<AtomicBool>, counters: Arc<DrainCounters>) {
    let mut buffer = [0_u8; 65_536];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv(&mut buffer) {
            Ok(len) => {
                counters.datagrams.fetch_add(1, Ordering::Relaxed);
                counters.bytes.fetch_add(len as u64, Ordering::Relaxed);
            }
            Err(error) => match error.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {}
                std::io::ErrorKind::Interrupted => {}
                _ => {
                    counters.errors.fetch_add(1, Ordering::Relaxed);
                }
            },
        }
    }
}

/// The receive buffer the kernel actually granted this socket, read back rather
/// than assumed: a requested size is clamped to `net.core.rmem_max`.
fn granted_rcvbuf(socket: &UdpSocket) -> Option<u64> {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &mut value as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(value as u64)
}

/// One drain thread's counters. Cache-line separated: every datagram touches
/// two of these fields, and sharing one cache line across receiver CPUs would
/// put the apparatus's own coherence traffic into the measurement it exists to
/// keep cheap.
#[repr(align(64))]
#[derive(Default)]
struct DrainCounters {
    datagrams: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
}

impl DrainCounters {
    fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.datagrams.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
        )
    }
}

/// The drain socket's identity, and the per-thread counters that make load skew
/// visible even when every thread shares one socket.
struct DrainSocket {
    index: usize,
    local_port: u16,
    requested_rcvbuf_bytes: usize,
    granted_rcvbuf_bytes: Option<u64>,
}

/// Datagrams per `recvmmsg` call. One syscall per datagram is not a property of
/// the workload; the receiver is apparatus and must not become the ceiling.
fn drain_batch() -> usize {
    env_usize("UDP_DRAIN_BATCH", 32).clamp(1, 256)
}

/// Blocking `recvmmsg` drain: up to `batch` datagrams per syscall, no
/// per-datagram allocation, and a receive timeout so the stop flag is observed
/// promptly when traffic stops.
fn drain_loop_batched(
    socket: UdpSocket,
    stop: Arc<AtomicBool>,
    counters: Arc<DrainCounters>,
    batch: usize,
) {
    const SLOT_BYTES: usize = 2048;
    let mut arena = vec![0_u8; SLOT_BYTES * batch];
    let mut iovecs: Vec<libc::iovec> = (0..batch)
        .map(|slot| libc::iovec {
            iov_base: arena[slot * SLOT_BYTES..].as_mut_ptr() as *mut libc::c_void,
            iov_len: SLOT_BYTES,
        })
        .collect();
    let mut messages: Vec<libc::mmsghdr> = (0..batch)
        .map(|slot| {
            let mut header: libc::mmsghdr = unsafe { std::mem::zeroed() };
            header.msg_hdr.msg_iov = &mut iovecs[slot] as *mut libc::iovec;
            header.msg_hdr.msg_iovlen = 1;
            header
        })
        .collect();
    let fd = socket.as_raw_fd();
    while !stop.load(Ordering::Relaxed) {
        let received = unsafe {
            libc::recvmmsg(
                fd,
                messages.as_mut_ptr(),
                batch as u32,
                0,
                std::ptr::null_mut(),
            )
        };
        if received <= 0 {
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted => {}
                _ => {
                    counters.errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            continue;
        }
        let mut bytes = 0_u64;
        for message in messages.iter_mut().take(received as usize) {
            bytes += u64::from(message.msg_len);
            // Reset the slot length: recvmmsg overwrites it per call.
            message.msg_hdr.msg_iovlen = 1;
        }
        counters
            .datagrams
            .fetch_add(received as u64, Ordering::Relaxed);
        counters.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// The peer-side half of a substrate run: bind the drain sockets, serve the
/// state endpoint the measuring host differences per window, print per-interval
/// counters, and stop on SIGINT/SIGTERM with a final record.
pub(crate) async fn udp_drain_mode() -> Result<Value, String> {
    let port = env_usize("UDP_DRAIN_PORT", 9000);
    let port = u16::try_from(port).map_err(|_| "UDP_DRAIN_PORT out of range")?;
    let state_port = env_usize(
        "UDP_DRAIN_STATE_PORT",
        harness_port_defaults().mtx_api as usize,
    );
    let state_port = u16::try_from(state_port).map_err(|_| "UDP_DRAIN_STATE_PORT out of range")?;
    let threads = env_usize("UDP_DRAIN_THREADS", 2).max(1);
    let rcvbuf = env_usize("UDP_DRAIN_SOCKET_BUFFER", 64 * 1024 * 1024);
    let interval_secs = env_secs("UDP_DRAIN_REPORT_SECS", 5).max(1);
    if let Ok(mask) = std::env::var("UDP_DRAIN_CPUSET")
        && !mask.trim().is_empty()
    {
        // Before the drain threads spawn: they inherit this mask.
        pin_to_cpuset(mask.trim())?;
        println!("[udp-drain] pinned to cpus {mask}");
    }

    let started = Instant::now();
    let started_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let run = run_id();
    let start_drops = host_udp_drop_counters();
    let stop = Arc::new(AtomicBool::new(false));
    let batch = drain_batch();

    // Per-thread sockets with SO_REUSEPORT: the hash spreads by flow, and a
    // single recvmmsg thread already absorbs ~175 000 datagrams/s here, so hash
    // skew (now visible in `perThread`) is tolerated rather than contended. A
    // single shared socket across threads measured *slower* — socket-lock
    // contention cost more than the skew did.
    let mut handles = Vec::with_capacity(threads);
    let mut counters = Vec::with_capacity(threads);
    let mut socket_info = Vec::with_capacity(threads);
    for index in 0..threads {
        let socket = bind_drain_socket(port, rcvbuf, Duration::from_millis(200), true)?;
        socket_info.push(DrainSocket {
            index,
            local_port: socket.local_addr().map_err(|e| e.to_string())?.port(),
            requested_rcvbuf_bytes: rcvbuf,
            granted_rcvbuf_bytes: granted_rcvbuf(&socket),
        });
        let thread_counters = Arc::new(DrainCounters::default());
        counters.push(Arc::clone(&thread_counters));
        let stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name(format!("udp-drain-{index}"))
            .spawn(move || {
                if batch > 1 {
                    drain_loop_batched(socket, stop, thread_counters, batch);
                } else {
                    drain_loop(socket, stop, thread_counters);
                }
            })
            .map_err(|e| format!("spawn drain thread {index}: {e}"))?;
        handles.push(handle);
    }
    let sockets = Arc::new(socket_info);
    let counters = Arc::new(counters);
    let listener = bind_state_listener(state_port).await?;
    let state_task = tokio::spawn(serve_state_json(listener, {
        let run = run.clone();
        let sockets = Arc::clone(&sockets);
        let counters = Arc::clone(&counters);
        Arc::new(move || {
            let (nic_rx_dropped, nic_tx_dropped) = host_nic_drop_counters();
            let per_thread: Vec<Value> = counters
                .iter()
                .enumerate()
                .map(|(index, counters)| {
                    let (datagrams, bytes, errors) = counters.snapshot();
                    json!({
                        "index": index,
                        "datagrams": datagrams,
                        "bytes": bytes,
                        "receiveErrors": errors,
                    })
                })
                .collect();
            let total: u64 = counters
                .iter()
                .map(|c| c.datagrams.load(Ordering::Relaxed))
                .sum();
            let total_bytes: u64 = counters
                .iter()
                .map(|c| c.bytes.load(Ordering::Relaxed))
                .sum();
            let total_errors: u64 = counters
                .iter()
                .map(|c| c.errors.load(Ordering::Relaxed))
                .sum();
            json!({
                "runId": run,
                "startedAtMs": started_ms,
                "cpusAllowedList": cpus_allowed_list(),
                "port": port,
                "threads": threads,
                "batch": batch,
                "sockets": sockets
                    .iter()
                    .map(|socket| json!({
                        "index": socket.index,
                        "localPort": socket.local_port,
                        "requestedRcvbufBytes": socket.requested_rcvbuf_bytes,
                        "grantedRcvbufBytes": socket.granted_rcvbuf_bytes,
                    }))
                    .collect::<Vec<_>>(),
                "datagrams": total,
                "bytes": total_bytes,
                "receiveErrors": total_errors,
                "perThread": per_thread,
                "udpInErrors": host_udp_drop_counters().map(|drops| drops.0),
                "udpRcvbufErrors": host_udp_drop_counters().map(|drops| drops.1),
                "udpSndbufErrors": host_udp_drop_counters().map(|drops| drops.2),
                "nicRxDropped": nic_rx_dropped,
                "nicTxDropped": nic_tx_dropped,
                "softnet": softnet_counters(),
                "uptimeSecs": (SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis())
                    .unwrap_or(started_ms)
                    .saturating_sub(started_ms)) / 1000,
            })
        })
    }));
    println!(
        "[udp-drain] run {run} draining :{port} on {threads} thread(s), recvmmsg batch {batch}, \
         {rcvbuf} B rcvbuf requested; state endpoint on {state_port}/state; SIGINT/SIGTERM to stop"
    );

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("install SIGTERM handler: {e}"))?;
    let mut last = (0_u64, 0_u64);
    let mut last_at = Instant::now();
    let stopped_by = loop {
        tokio::select! {
            _ = ticker.tick() => {
                let now = (
                    counters.iter().map(|c| c.datagrams.load(Ordering::Relaxed)).sum::<u64>(),
                    counters.iter().map(|c| c.bytes.load(Ordering::Relaxed)).sum::<u64>(),
                );
                // Per-interval rate: the delta since the previous tick over the
                // time since the previous tick, not over total uptime.
                let now_at = Instant::now();
                let interval = now_at.duration_since(last_at).as_secs_f64().max(1e-9);
                let drops = udp_drops_since_start(start_drops);
                println!(
                    "[udp-drain] {}",
                    json!({
                        "runId": run,
                        "uptimeSecs": started.elapsed().as_secs(),
                        "datagrams": now.0,
                        "datagramsPerSec": (now.0.saturating_sub(last.0)) as f64 / interval,
                        "bytes": now.1,
                        "receiveErrors": counters
                            .iter()
                            .map(|c| c.errors.load(Ordering::Relaxed))
                            .sum::<u64>(),
                        "udpRcvbufErrorsSinceStart": drops.map(|drops| drops.1),
                        "udpInErrorsSinceStart": drops.map(|drops| drops.0),
                        "softnetDroppedSinceStart": softnet_counters().and_then(|s| s["dropped"].as_u64()),
                    })
                );
                last = now;
                last_at = now_at;
            }
            _ = tokio::signal::ctrl_c() => break "ctrl_c",
            _ = sigterm.recv() => break "sigterm",
        }
    };

    let datagrams: u64 = counters
        .iter()
        .map(|c| c.datagrams.load(Ordering::Relaxed))
        .sum();
    let bytes: u64 = counters
        .iter()
        .map(|c| c.bytes.load(Ordering::Relaxed))
        .sum();
    let errors: u64 = counters
        .iter()
        .map(|c| c.errors.load(Ordering::Relaxed))
        .sum();
    let drops = udp_drops_since_start(start_drops);
    state_task.abort();
    stop.store(true, Ordering::Relaxed);
    for handle in handles {
        let _ = handle.join();
    }
    Ok(json!({
        "mode": "udp-drain",
        "runId": run,
        "cpusAllowedList": cpus_allowed_list(),
        "port": port,
        "statePort": state_port,
        "threads": threads,
        "batch": batch,
        "grantedRcvbufBytes": sockets.first().and_then(|socket| socket.granted_rcvbuf_bytes),
        "perThread": counters
            .iter()
            .enumerate()
            .map(|(index, counters)| {
                let (datagrams, bytes, errors) = counters.snapshot();
                json!({"index": index, "datagrams": datagrams, "bytes": bytes, "receiveErrors": errors})
            })
            .collect::<Vec<_>>(),
        "uptimeSecs": started.elapsed().as_secs(),
        "stoppedBy": stopped_by,
        "datagrams": datagrams,
        "bytes": bytes,
        "receiveErrors": errors,
        "udpInErrorsSinceStart": drops.map(|drops| drops.0),
        "udpRcvbufErrorsSinceStart": drops.map(|drops| drops.1),
        "udpSndbufErrorsSinceStart": drops.map(|drops| drops.2),
        "softnet": softnet_counters(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_reuseport_sockets_can_share_one_port() {
        // Production topology: one SO_REUSEPORT socket per drain thread on the
        // same wildcard port. (A single shared socket across threads was tried
        // and measured slower — socket-lock contention cost more than the hash
        // skew it removed — so it is not the production shape.)
        let first = bind_drain_socket(0, 1 << 20, Duration::from_millis(50), true).expect("first");
        let port = first.local_addr().expect("addr").port();
        let second =
            bind_drain_socket(port, 1 << 20, Duration::from_millis(50), true).expect("second");
        assert_eq!(second.local_addr().expect("addr").port(), port);
        assert!(granted_rcvbuf(&second).is_some());
    }

    #[test]
    fn drain_loop_counts_datagrams_and_bytes_until_stopped() {
        let socket =
            bind_drain_socket(0, 1 << 20, Duration::from_millis(50), true).expect("socket");
        let port = socket.local_addr().expect("addr").port();
        let counters = Arc::new(DrainCounters::default());
        let stop = Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn({
            let counters = Arc::clone(&counters);
            let stop = Arc::clone(&stop);
            move || drain_loop(socket, stop, counters)
        });

        let sender = UdpSocket::bind("127.0.0.1:0").expect("sender");
        let target = format!("127.0.0.1:{port}");
        for _ in 0..8 {
            sender.send_to(&[7_u8; 1316], &target).expect("send");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while counters.datagrams.load(Ordering::Relaxed) < 8 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
        assert_eq!(counters.datagrams.load(Ordering::Relaxed), 8);
        assert_eq!(counters.bytes.load(Ordering::Relaxed), 8 * 1316);
        assert_eq!(counters.errors.load(Ordering::Relaxed), 0);
    }
}
