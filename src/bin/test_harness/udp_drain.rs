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
use std::os::fd::FromRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::peer_state::{
    bind_state_listener, cpus_allowed_list, host_nic_drop_counters, host_udp_drop_counters,
    pin_to_cpuset, run_id, serve_state_json, udp_drops_since_start,
};
use super::*;

/// A wildcard UDP socket with `SO_REUSEPORT`, a large receive buffer and a
/// read timeout so the drain loop can observe the stop flag.
fn bind_drain_socket(port: u16, rcvbuf: usize, timeout: Duration) -> Result<UdpSocket, String> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    // Own the fd from here on so every early return closes it.
    let socket = unsafe { UdpSocket::from_raw_fd(fd) };
    let enable: libc::c_int = 1;
    for (name, value) in [(libc::SO_REUSEADDR, enable), (libc::SO_REUSEPORT, enable)] {
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
        match socket.recv_from(&mut buffer) {
            Ok((len, _)) => {
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

#[derive(Default)]
struct DrainCounters {
    datagrams: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
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
    let counters = Arc::new(DrainCounters::default());
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::with_capacity(threads);
    for index in 0..threads {
        let socket = bind_drain_socket(port, rcvbuf, Duration::from_millis(200))?;
        let counters = Arc::clone(&counters);
        let stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name(format!("udp-drain-{index}"))
            .spawn(move || drain_loop(socket, stop, counters))
            .map_err(|e| format!("spawn drain thread {index}: {e}"))?;
        handles.push(handle);
    }

    let listener = bind_state_listener(state_port).await?;
    let state_task = tokio::spawn(serve_state_json(listener, {
        let run = run.clone();
        let counters = Arc::clone(&counters);
        Arc::new(move || {
            let (nic_rx_dropped, nic_tx_dropped) = host_nic_drop_counters();
            json!({
                "runId": run,
                "startedAtMs": started_ms,
                "cpusAllowedList": cpus_allowed_list(),
                "port": port,
                "threads": threads,
                "rcvbufBytes": rcvbuf,
                "datagrams": counters.datagrams.load(Ordering::Relaxed),
                "bytes": counters.bytes.load(Ordering::Relaxed),
                "receiveErrors": counters.errors.load(Ordering::Relaxed),
                "udpInErrors": host_udp_drop_counters().map(|drops| drops.0),
                "udpRcvbufErrors": host_udp_drop_counters().map(|drops| drops.1),
                "udpSndbufErrors": host_udp_drop_counters().map(|drops| drops.2),
                "nicRxDropped": nic_rx_dropped,
                "nicTxDropped": nic_tx_dropped,
                "uptimeSecs": (SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis())
                    .unwrap_or(started_ms)
                    .saturating_sub(started_ms)) / 1000,
            })
        })
    }));
    println!(
        "[udp-drain] run {run} draining :{port} on {threads} thread(s) with {rcvbuf} B rcvbuf; \
         state endpoint on {state_port}/state; send SIGINT/SIGTERM to stop"
    );

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("install SIGTERM handler: {e}"))?;
    let mut last = (0_u64, 0_u64);
    let stopped_by = loop {
        tokio::select! {
            _ = ticker.tick() => {
                let now = (
                    counters.datagrams.load(Ordering::Relaxed),
                    counters.bytes.load(Ordering::Relaxed),
                );
                let secs = started.elapsed().as_secs_f64();
                let drops = udp_drops_since_start(start_drops);
                println!(
                    "[udp-drain] {}",
                    json!({
                        "runId": run,
                        "uptimeSecs": started.elapsed().as_secs(),
                        "datagrams": now.0,
                        "datagramsPerSec": (now.0.saturating_sub(last.0)) as f64 / secs.max(1e-9),
                        "bytes": now.1,
                        "receiveErrors": counters.errors.load(Ordering::Relaxed),
                        "udpRcvbufErrorsSinceStart": drops.map(|drops| drops.1),
                        "udpInErrorsSinceStart": drops.map(|drops| drops.0),
                    })
                );
                last = now;
            }
            _ = tokio::signal::ctrl_c() => break "ctrl_c",
            _ = sigterm.recv() => break "sigterm",
        }
    };

    let datagrams = counters.datagrams.load(Ordering::Relaxed);
    let bytes = counters.bytes.load(Ordering::Relaxed);
    let errors = counters.errors.load(Ordering::Relaxed);
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
        "rcvbufBytes": rcvbuf,
        "uptimeSecs": started.elapsed().as_secs(),
        "stoppedBy": stopped_by,
        "datagrams": datagrams,
        "bytes": bytes,
        "receiveErrors": errors,
        "udpInErrorsSinceStart": drops.map(|drops| drops.0),
        "udpRcvbufErrorsSinceStart": drops.map(|drops| drops.1),
        "udpSndbufErrorsSinceStart": drops.map(|drops| drops.2),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuseport_sockets_can_share_one_port() {
        let first = bind_drain_socket(0, 1 << 20, Duration::from_millis(50)).expect("first");
        let port = first.local_addr().expect("addr").port();
        let second = bind_drain_socket(port, 1 << 20, Duration::from_millis(50))
            .expect("second shares port");
        // Both bound to the same wildcard port, so the sender's 1000
        // destinations can be spread across drain threads.
        assert_eq!(second.local_addr().expect("addr").port(), port);
    }

    #[test]
    fn drain_loop_counts_datagrams_and_bytes_until_stopped() {
        let socket = bind_drain_socket(0, 1 << 20, Duration::from_millis(50)).expect("socket");
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
