//! SRT caller/receiver lifecycle stress benchmark.
//!
//! Models the SRT part of Restream A -> Restream B without HTTP, ffmpeg, or
//! pipeline duplication. One receiver epoll loop accepts and drains sockets;
//! client workers exercise blocking raw and non-blocking egress-style setup.
//!
//! Key metric: elapsed time for 1200 connections + 3s send; non-blocking
//! egress-mode connect must not stall the caller (contrast with raw-blocking).
//!
//! `cargo bench --bench srt_lifecycle -- 1200 8 3`

use std::ffi::c_int;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const SRTO_SNDSYN: c_int = 1;
const SRTO_RCVSYN: c_int = 2;
const SRTO_REUSEADDR: c_int = 15;
const SRTO_CONNTIMEO: c_int = 36;
const SRTO_LATENCY: c_int = 23;
const SRTO_TRANSTYPE: c_int = 50;
const SRTT_LIVE: c_int = 0;
const SRT_EPOLL_IN: c_int = 0x1;
const PORT: u16 = 37900;
const PACKET: [u8; 188] = [0x47; 188];
type SrtSocket = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

#[link(name = "srt")]
#[link(name = "mbedtls")]
#[link(name = "mbedx509")]
#[link(name = "mbedcrypto")]
unsafe extern "C" {
    fn srt_startup() -> c_int;
    fn srt_cleanup() -> c_int;
    fn srt_create_socket() -> SrtSocket;
    fn srt_close(socket: SrtSocket) -> c_int;
    fn srt_setsockopt(
        socket: SrtSocket,
        level: c_int,
        option: c_int,
        value: *const u8,
        len: c_int,
    ) -> c_int;
    fn srt_bind(socket: SrtSocket, address: *const SockAddrIn, len: c_int) -> c_int;
    fn srt_listen(socket: SrtSocket, backlog: c_int) -> c_int;
    fn srt_accept(socket: SrtSocket, address: *mut SockAddrIn, len: *mut c_int) -> SrtSocket;
    fn srt_connect(socket: SrtSocket, address: *const SockAddrIn, len: c_int) -> c_int;
    fn srt_recv(socket: SrtSocket, buffer: *mut u8, len: c_int) -> c_int;
    fn srt_send(socket: SrtSocket, buffer: *const u8, len: c_int) -> c_int;
    fn srt_epoll_create() -> c_int;
    fn srt_epoll_add_usock(epoll: c_int, socket: SrtSocket, events: *const c_int) -> c_int;
    fn srt_epoll_remove_usock(epoll: c_int, socket: SrtSocket) -> c_int;
    fn srt_epoll_release(epoll: c_int) -> c_int;
    fn srt_epoll_wait(
        epoll: c_int,
        read_fds: *mut SrtSocket,
        read_count: *mut c_int,
        write_fds: *mut SrtSocket,
        write_count: *mut c_int,
        timeout_ms: i64,
        error_read_fds: *mut c_int,
        error_read_count: *mut c_int,
        error_write_fds: *mut c_int,
        error_write_count: *mut c_int,
    ) -> c_int;
}

fn set_option(socket: SrtSocket, option: c_int, value: c_int) {
    let _ = unsafe {
        srt_setsockopt(
            socket,
            0,
            option,
            &value as *const _ as *const u8,
            std::mem::size_of::<c_int>() as c_int,
        )
    };
}

fn address(port: u16) -> SockAddrIn {
    SockAddrIn {
        sin_family: 2,
        sin_port: port.to_be(),
        sin_addr: u32::from_ne_bytes([127, 0, 0, 1]),
        sin_zero: [0; 8],
    }
}

#[derive(Default)]
struct ReceiverStats {
    accepted: AtomicU64,
    bytes: AtomicU64,
    closed: AtomicU64,
}

fn receiver(port: u16, stop: Arc<AtomicBool>, stats: Arc<ReceiverStats>) {
    let listener = unsafe { srt_create_socket() };
    if listener < 0 {
        return;
    }
    set_option(listener, SRTO_TRANSTYPE, SRTT_LIVE);
    set_option(listener, SRTO_LATENCY, 250);
    set_option(listener, SRTO_RCVSYN, 0);
    set_option(listener, SRTO_REUSEADDR, 1);
    let addr = address(port);
    if unsafe { srt_bind(listener, &addr, 16) } < 0 || unsafe { srt_listen(listener, 2048) } < 0 {
        unsafe {
            srt_close(listener);
        }
        return;
    }

    let epoll = unsafe { srt_epoll_create() };
    if epoll < 0 {
        unsafe {
            srt_close(listener);
        }
        return;
    }
    let interest = SRT_EPOLL_IN;
    unsafe {
        srt_epoll_add_usock(epoll, listener, &interest);
    }
    let mut event_buf = [0i32; 4096];
    let mut recv_buf = [0u8; 1316];
    while !stop.load(Ordering::Acquire) {
        let mut read_count = event_buf.len() as c_int;
        let mut error_count = event_buf.len() as c_int;
        let result = unsafe {
            srt_epoll_wait(
                epoll,
                event_buf.as_mut_ptr(),
                &mut read_count,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                50,
                std::ptr::null_mut(),
                &mut error_count,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result < 0 {
            continue;
        }
        let count = read_count.max(0) as usize;
        if count == 0 {
            continue;
        }
        for &socket in &event_buf[..count] {
            if socket == listener {
                loop {
                    let mut peer = address(0);
                    let mut peer_len = 16i32;
                    let accepted = unsafe { srt_accept(listener, &mut peer, &mut peer_len) };
                    if accepted < 0 {
                        break;
                    }
                    unsafe {
                        srt_epoll_add_usock(epoll, accepted, &interest);
                    }
                    stats.accepted.fetch_add(1, Ordering::Relaxed);
                }
                continue;
            }
            let received =
                unsafe { srt_recv(socket, recv_buf.as_mut_ptr(), recv_buf.len() as c_int) };
            if received > 0 {
                stats.bytes.fetch_add(received as u64, Ordering::Relaxed);
            } else {
                unsafe {
                    srt_epoll_remove_usock(epoll, socket);
                }
                stats.closed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    unsafe {
        srt_epoll_release(epoll);
        srt_close(listener);
    }
}

#[derive(Clone, Copy)]
enum CallerMode {
    RawBlocking,
    EgressNonblocking,
}

/// Creates `count` sockets, connects (non-blocking for egress mode),
/// sends for `duration`, then returns the open sockets for the caller to
/// drain and close. The sockets stay open so the receiver can read data.
fn caller_batch(
    port: u16,
    count: usize,
    mode: CallerMode,
    duration: Duration,
) -> (u64, u64, u64, Vec<SrtSocket>) {
    let mut sockets = Vec::with_capacity(count);
    let mut opened = 0u64;
    let mut failed = 0u64;
    let addr = address(port);

    for _ in 0..count {
        let socket = unsafe { srt_create_socket() };
        if socket < 0 {
            failed += 1;
            continue;
        }
        set_option(socket, SRTO_TRANSTYPE, SRTT_LIVE);
        set_option(socket, SRTO_LATENCY, 200);
        set_option(socket, SRTO_REUSEADDR, 1);
        set_option(socket, SRTO_CONNTIMEO, 5_000);
        match mode {
            CallerMode::RawBlocking => {
                set_option(socket, SRTO_RCVSYN, 1);
                set_option(socket, SRTO_SNDSYN, 1);
            }
            CallerMode::EgressNonblocking => {
                // Production fabric egress makes connect asynchronous.
                set_option(socket, SRTO_RCVSYN, 0);
            }
        }
        if unsafe { srt_connect(socket, &addr, 16) } >= 0 {
            if matches!(mode, CallerMode::EgressNonblocking) {
                set_option(socket, SRTO_SNDSYN, 0);
            }
            sockets.push(socket);
            opened += 1;
        } else {
            unsafe {
                srt_close(socket);
            }
            failed += 1;
        }
    }

    let deadline = Instant::now() + duration;
    let mut bytes = 0u64;
    while Instant::now() < deadline {
        let mut progressed = false;
        for &socket in &sockets {
            let sent = unsafe { srt_send(socket, PACKET.as_ptr(), PACKET.len() as c_int) };
            if sent > 0 {
                bytes += sent as u64;
                progressed = true;
            }
        }
        if !progressed {
            thread::yield_now();
        }
    }
    // Sockets returned OPEN — caller must drain receiver then close.
    (opened, bytes, failed, sockets)
}

fn run_case(
    label: &str,
    mode: CallerMode,
    count: usize,
    workers: usize,
    duration: Duration,
    port: u16,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(ReceiverStats::default());
    let receiver_handle = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || receiver(port, stop, stats))
    };
    thread::sleep(Duration::from_millis(200));

    let per_worker = count / workers;
    let extra = count % workers;
    let start = Instant::now();
    let handles: Vec<_> = (0..workers)
        .map(|worker| {
            let n = per_worker + usize::from(worker < extra);
            thread::spawn(move || caller_batch(port, n, mode, duration))
        })
        .collect();
    let mut opened = 0u64;
    let mut bytes = 0u64;
    let mut failed = 0u64;
    let mut all_sockets: Vec<SrtSocket> = Vec::with_capacity(count);
    for handle in handles {
        let (ok, sent, fail, sockets) = handle.join().expect("caller worker panicked");
        opened += ok;
        bytes += sent;
        failed += fail;
        all_sockets.extend(sockets);
    }
    let elapsed = start.elapsed();

    // Drain: let the receiver read pending data before closing sender sockets.
    thread::sleep(Duration::from_millis(500));
    for socket in all_sockets {
        unsafe {
            srt_close(socket);
        }
    }
    // One more drain cycle so the receiver processes close events.
    thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Release);
    receiver_handle.join().expect("receiver panicked");

    let accepted = stats.accepted.load(Ordering::Relaxed);
    let received = stats.bytes.load(Ordering::Relaxed);
    println!(
        "{label}: opened={opened}/{count} accepted={accepted}/{count} failed={failed} sent={}KB received={}KB elapsed={:.2}s",
        bytes / 1024,
        received / 1024,
        elapsed.as_secs_f64(),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let count = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(1200);
    let workers = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(8);
    let duration_secs = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(3);
    unsafe {
        srt_startup();
    }
    run_case(
        "raw-blocking",
        CallerMode::RawBlocking,
        count,
        workers,
        Duration::from_secs(duration_secs),
        PORT,
    );
    run_case(
        "egress-nonblocking",
        CallerMode::EgressNonblocking,
        count,
        workers,
        Duration::from_secs(duration_secs),
        PORT + 1,
    );
    unsafe {
        srt_cleanup();
    }
}
