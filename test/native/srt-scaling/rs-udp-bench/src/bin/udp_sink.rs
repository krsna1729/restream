//! Rust equivalent of `udp_sink.c`'s "shared" mode: `port_count` plain UDP
//! listener sockets, busy-polled epoll across `total_worker_threads`
//! worker threads. The one architectural change from the C tool: each
//! ready socket is drained with `recvmmsg()` (batched) instead of a
//! `recv()`-per-message loop — see `udp_sender.rs`'s module doc for why.
//!
//! `connected` mode (per-peer `connect()`-isolated sockets) is not
//! reimplemented here; this first prototype is scoped to the batching
//! question specifically. See the top-level README.md.
//!
//! Usage: udp_sink_rs <port_base> <port_count> <total_worker_threads> <rcvbuf_bytes> [cpu_base]

use rs_udp_bench::{
    bind_v4, epoll_add_readable, epoll_create, make_udp_socket, pin_to_cpu, set_reuseaddr,
    set_rcvbuf, RecvBatch,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MAX_EPOLL_EVENTS: usize = 4096;
const RECV_BATCH_CAP: usize = 64;
const RECV_BUF_STRIDE: usize = 2048; // > PAYLOAD_SIZE, headroom for stray oversize datagrams

struct WorkerCounters {
    bytes_received: AtomicU64,
    messages_received: AtomicU64,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: {} <port_base> <port_count> <total_worker_threads> <rcvbuf_bytes> [cpu_base] [batch:0|1]",
            args[0]
        );
        std::process::exit(1);
    }
    let port_base: u16 = args[1].parse().unwrap();
    let port_count: usize = args[2].parse().unwrap();
    let total_worker_threads: usize = args[3].parse().unwrap();
    let rcvbuf: i32 = args[4].parse().unwrap();
    let cpu_base: usize = args.get(5).map(|s| s.parse().unwrap()).unwrap_or(2);
    let batch_mode: bool = args.get(6).map(|s| s != "0").unwrap_or(true);

    let epfds: Vec<i32> = (0..total_worker_threads).map(|_| epoll_create()).collect();
    let counters: Vec<Arc<WorkerCounters>> = (0..total_worker_threads)
        .map(|_| {
            Arc::new(WorkerCounters {
                bytes_received: AtomicU64::new(0),
                messages_received: AtomicU64::new(0),
            })
        })
        .collect();

    for p in 0..port_count {
        let fd = make_udp_socket();
        assert!(fd >= 0, "socket() failed");
        set_reuseaddr(fd);
        set_rcvbuf(fd, rcvbuf);
        let ok = bind_v4(fd, port_base + p as u16);
        assert!(ok, "bind() failed on port {}", port_base as usize + p);
        let w = p % total_worker_threads;
        epoll_add_readable(epfds[w], fd);
    }

    eprintln!(
        "[udp_sink_rs] port_base={port_base} port_count={port_count} threads={total_worker_threads} rcvbuf={rcvbuf} mode=shared batch_mode={batch_mode} listening"
    );

    // Runs until killed (SIGINT/SIGTERM/SIGKILL from the driving script),
    // same lifecycle as udp_sink.c -- no graceful shutdown path needed for
    // a benchmark receiver.
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut _handles = Vec::with_capacity(total_worker_threads);
    for (tid, (epfd, ctr)) in epfds.iter().copied().zip(counters.iter().cloned()).enumerate() {
        let running = running.clone();
        _handles.push(std::thread::spawn(move || {
            worker_loop(tid, cpu_base, epfd, ctr, running, batch_mode);
        }));
    }

    let total_connections = port_count as i64;
    let mut last_report = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if last_report.elapsed().as_secs() >= 1 {
            let mut total_bytes = 0u64;
            let mut total_msgs = 0u64;
            for c in &counters {
                total_bytes += c.bytes_received.load(Ordering::Relaxed);
                total_msgs += c.messages_received.load(Ordering::Relaxed);
            }
            eprintln!("[udp_sink_rs] listeners={total_connections} bytes={total_bytes} msgs={total_msgs}");
            last_report = std::time::Instant::now();
        }
    }
}

fn worker_loop(
    tid: usize,
    cpu_base: usize,
    epfd: i32,
    ctr: Arc<WorkerCounters>,
    running: Arc<std::sync::atomic::AtomicBool>,
    batch_mode: bool,
) {
    pin_to_cpu(cpu_base + tid);
    let mut events: Vec<libc::epoll_event> = vec![unsafe { std::mem::zeroed() }; MAX_EPOLL_EVENTS];
    let mut batch = RecvBatch::new(RECV_BATCH_CAP, RECV_BUF_STRIDE);

    while running.load(Ordering::Relaxed) {
        // Busy-poll (timeout=0): same rationale as udp_sink.c -- removes
        // blocking-wakeup latency even though SO_BUSY_POLL's NAPI-polling
        // benefit doesn't apply on loopback.
        let n = unsafe { libc::epoll_wait(epfd, events.as_mut_ptr(), MAX_EPOLL_EVENTS as i32, 0) };
        if n <= 0 {
            continue;
        }
        for ev in &events[..n as usize] {
            let fd = ev.u64 as i32;
            let (msgs, bytes) = if batch_mode {
                batch.drain(fd)
            } else {
                batch.drain_unbatched(fd)
            };
            ctr.messages_received.fetch_add(msgs, Ordering::Relaxed);
            ctr.bytes_received.fetch_add(bytes, Ordering::Relaxed);
        }
    }
}
