//! Rust equivalent of `udp_sender.c`, with one deliberate architectural
//! change: instead of one `connect()`ed socket per simulated stream, each
//! worker thread owns exactly ONE unconnected UDP socket and batches every
//! wheel-slot's due sends into a single `sendmmsg()` call (one syscall per
//! thread per tick instead of one syscall per stream per tick). This is
//! the specific lever `test/native/srt-scaling/README.md` names as
//! unexplored ("closing it further needs sendmmsg()/GSO batching").
//!
//! Usage: udp_sender_rs <host> <port_base> <port_count> <threads>
//!                       <bitrate_Bps> <c1,c2,...> <hold_secs> [cpu_base]
//!
//! Deliberately NOT argument-compatible with `udp_sender.c`'s trailing
//! `[local_port_count] [local_port_base]`: those exist in the C tool only
//! because each stream has its own socket that can be bound to a distinct
//! local port. With one shared socket per thread there is nothing
//! equivalent to bind per-stream, so those two positions are dropped
//! rather than kept as meaningless no-ops.

use rs_udp_bench::{
    calibrate_tsc_hz, make_udp_socket, parse_ipv4, pin_to_cpu, rdtsc_now, sockaddr_in, SendBatch,
    PAYLOAD_SIZE, WHEEL_SLOTS,
};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

static PAYLOAD: [u8; PAYLOAD_SIZE] = [0x42; PAYLOAD_SIZE];

/// Single-producer (main thread, before publish) / single-consumer (owning
/// worker thread, after publish) shared state, mirroring `udp_sender.c`'s
/// `owned`/`n_owned` discipline: everything in `owned`/`slot_next` up to
/// index `n_owned` (loaded with `Acquire`, published with `Release`) is
/// safe for the worker to read without further synchronization.
struct WorkerShared {
    owned: Box<[UnsafeCell<libc::sockaddr_in>]>,
    slot_next: Box<[UnsafeCell<i32>]>,
    n_owned: AtomicUsize,
    bytes_sent: AtomicU64,
    send_attempts: AtomicU64,
    send_would_block: AtomicU64,
    syscalls: AtomicU64,
}
unsafe impl Sync for WorkerShared {}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 8 {
        eprintln!(
            "usage: {} <host> <port_base> <port_count> <threads> <bitrate_Bps> <c1,c2,...> <hold_secs> [cpu_base] [batch:0|1]",
            args[0]
        );
        std::process::exit(1);
    }
    let host_ip = parse_ipv4(&args[1]);
    let port_base: u16 = args[2].parse().unwrap();
    let port_count: usize = args[3].parse().unwrap();
    let nthreads: usize = args[4].parse().unwrap();
    let bitrate_bps: f64 = args[5].parse().unwrap();
    let checkpoints: Vec<usize> = args[6].split(',').map(|s| s.parse().unwrap()).collect();
    let hold_secs: u64 = args[7].parse().unwrap();
    let cpu_base: usize = args.get(8).map(|s| s.parse().unwrap()).unwrap_or(1);
    // batch=1 (default): one sendmmsg() per due wheel-slot. batch=0: one
    // sendto() per message, same shared-socket-per-thread architecture --
    // isolates the batching variable from the architecture change.
    let batch_mode: bool = args.get(9).map(|s| s != "0").unwrap_or(true);

    let interval_s = PAYLOAD_SIZE as f64 / bitrate_bps;
    let tsc_hz = calibrate_tsc_hz();
    let slot_duration_ticks = (interval_s / WHEEL_SLOTS as f64 * tsc_hz as f64) as u64;
    eprintln!(
        "[udp_sender_rs] tsc_hz={tsc_hz} slot_duration_ticks={slot_duration_ticks} interval_s={interval_s:.6} batch_mode={batch_mode}"
    );

    let dest_addrs: Vec<libc::sockaddr_in> = (0..port_count)
        .map(|p| sockaddr_in(host_ip, port_base + p as u16))
        .collect();

    let max_conns = *checkpoints.iter().max().unwrap();
    let per_thread_cap = max_conns.div_ceil(nthreads) + 1;

    let mut workers: Vec<Arc<WorkerShared>> = Vec::with_capacity(nthreads);
    for _ in 0..nthreads {
        let owned: Box<[UnsafeCell<libc::sockaddr_in>]> = (0..per_thread_cap)
            .map(|_| UnsafeCell::new(sockaddr_in(0, 0)))
            .collect();
        let slot_next: Box<[UnsafeCell<i32>]> = (0..per_thread_cap).map(|_| UnsafeCell::new(-1)).collect();
        workers.push(Arc::new(WorkerShared {
            owned,
            slot_next,
            n_owned: AtomicUsize::new(0),
            bytes_sent: AtomicU64::new(0),
            send_attempts: AtomicU64::new(0),
            send_would_block: AtomicU64::new(0),
            syscalls: AtomicU64::new(0),
        }));
    }

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut handles = Vec::with_capacity(nthreads);
    for (tid, w) in workers.iter().cloned().enumerate() {
        let running = running.clone();
        handles.push(std::thread::spawn(move || {
            worker_loop(tid, cpu_base, w, running, slot_duration_ticks, per_thread_cap, batch_mode);
        }));
    }

    println!(
        "checkpoint,requested,registered,steady_bytes_sent,steady_send_attempts,steady_would_block,steady_syscalls,target_bytes,pct_of_target,elapsed_register_s"
    );

    let mut already_started = 0usize;
    for &target in &checkpoints {
        let ramp_start = Instant::now();
        for idx in already_started..target {
            let owner = idx % nthreads;
            let dest = dest_addrs[idx % port_count];
            let w = &workers[owner];
            let slot = w.n_owned.load(Ordering::Relaxed);
            unsafe {
                *w.owned[slot].get() = dest;
            }
            w.n_owned.store(slot + 1, Ordering::Release);
        }
        let elapsed_register_s = ramp_start.elapsed().as_secs_f64();

        for w in &workers {
            w.bytes_sent.store(0, Ordering::Relaxed);
            w.send_attempts.store(0, Ordering::Relaxed);
            w.send_would_block.store(0, Ordering::Relaxed);
            w.syscalls.store(0, Ordering::Relaxed);
        }
        std::thread::sleep(std::time::Duration::from_secs(hold_secs));

        let mut total_bytes = 0u64;
        let mut total_attempts = 0u64;
        let mut total_wb = 0u64;
        let mut total_syscalls = 0u64;
        for w in &workers {
            total_bytes += w.bytes_sent.load(Ordering::Relaxed);
            total_attempts += w.send_attempts.load(Ordering::Relaxed);
            total_wb += w.send_would_block.load(Ordering::Relaxed);
            total_syscalls += w.syscalls.load(Ordering::Relaxed);
        }

        let n_new = target - already_started;
        let target_bytes = target as f64 * bitrate_bps * hold_secs as f64;
        let pct_of_target = if target_bytes > 0.0 {
            100.0 * total_bytes as f64 / target_bytes
        } else {
            0.0
        };

        println!(
            "{target},{n_new},{target},{total_bytes},{total_attempts},{total_wb},{total_syscalls},{target_bytes:.0},{pct_of_target:.2},{elapsed_register_s:.4}"
        );
        already_started = target;
    }

    running.store(false, Ordering::Relaxed);
    for h in handles {
        let _ = h.join();
    }
}

fn worker_loop(
    tid: usize,
    cpu_base: usize,
    w: Arc<WorkerShared>,
    running: Arc<std::sync::atomic::AtomicBool>,
    slot_duration_ticks: u64,
    cap: usize,
    batch_mode: bool,
) {
    pin_to_cpu(cpu_base + tid);
    let fd = make_udp_socket();
    assert!(fd >= 0, "socket() failed");

    let mut slot_head = [-1i32; WHEEL_SLOTS];
    let mut cur_slot = 0usize;
    let mut last_seen_n = 0usize;
    let mut next_slot_tick = rdtsc_now();
    let mut batch = SendBatch::new(cap, &PAYLOAD);

    while running.load(Ordering::Relaxed) {
        let n = w.n_owned.load(Ordering::Acquire);
        for i in last_seen_n..n {
            unsafe {
                *w.slot_next[i].get() = slot_head[cur_slot];
            }
            slot_head[cur_slot] = i as i32;
        }
        last_seen_n = n;

        let now = rdtsc_now();
        while now >= next_slot_tick {
            let mut idx = slot_head[cur_slot];
            slot_head[cur_slot] = -1;
            batch.clear();
            // Collect this slot's due streams, requeuing each into the
            // just-vacated slot for the next lap (one wheel rotation later),
            // same as the C sender.
            let mut requeue_head = -1i32;
            while idx != -1 {
                let next = unsafe { *w.slot_next[idx as usize].get() };
                let dest = unsafe { *w.owned[idx as usize].get() };
                if !batch.is_full() {
                    batch.push(dest);
                }
                unsafe {
                    *w.slot_next[idx as usize].get() = requeue_head;
                }
                requeue_head = idx;
                idx = next;
            }
            slot_head[cur_slot] = requeue_head;

            if !batch.is_empty() {
                let attempted = batch.len();
                let (sent, bytes) = if batch_mode {
                    batch.send(fd)
                } else {
                    batch.send_unbatched(fd)
                };
                let syscalls_this_tick = if batch_mode { 1 } else { attempted as u64 };
                w.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
                w.send_attempts.fetch_add(attempted as u64, Ordering::Relaxed);
                w.syscalls.fetch_add(syscalls_this_tick, Ordering::Relaxed);
                if sent < attempted {
                    w.send_would_block.fetch_add((attempted - sent) as u64, Ordering::Relaxed);
                }
            }

            cur_slot = (cur_slot + 1) % WHEEL_SLOTS;
            next_slot_tick += slot_duration_ticks;
        }
    }
    unsafe {
        libc::close(fd);
    }
}
