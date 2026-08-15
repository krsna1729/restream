//! Harness-native SRT accept-and-discard listener for `MSR_PEER=sink`.
//!
//! Replaces what used to be `RESTREAM_SINK_MODE=1` on a separate `restream`
//! process: a receiving peer that accepts every connection and discards its
//! data, used purely to give the msr scale harness somewhere real to send
//! to. `verify_msr_sink_checkpoint`
//! (`resource_sweep/msr/verification.rs`) reads its bytes/drop numbers
//! entirely from the *sender's* own engine-health API, never from this
//! peer, so this listener needs no metrics surface or API of its own — its
//! only job is to accept and keep draining.
//!
//! **Why the FFI is hand-written here.** Same reasoning as
//! `srt_raw_sink.rs`: the buffer-tuning surface this needs
//! (`SRTO_UDP_RCVBUF`/`SRTO_FC`/`SRTO_MAXBW`/etc., mirroring the private
//! `srt_set_highbitrate_opts` in `src/media/srt/socket.rs`) is not part of
//! `restream::media::srt`'s public API, and widening a production module's
//! API for a test tool is the wrong trade. The declarations here are
//! checked against `srtcore/srt.h` of the pinned libsrt build; linking is
//! free since the harness binary already links the same static `libsrt.a`.
//!
//! **Threading**: `discard_threads` (default 1, matching the exact
//! single-threaded behavior the production listener this replaces always
//! had) independent OS threads each call `srt_accept()` on the *same*
//! shared listener socket and maintain their own private client list —
//! libsrt's `srt_accept()` is safe to call concurrently from multiple
//! threads (internally serialized), so accepted connections distribute
//! across threads without any explicit dispatcher. Each thread's client
//! list is touched only by that thread, so no lock is needed once a socket
//! has been accepted -- the same thread-confined-ownership principle this
//! session's C-benchmark work converged on independently (see
//! `test/native/srt-scaling/README.md`).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

type SrtSocket = c_int;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

// SAFETY: Category 8 - FFI boundary. Declarations for the libsrt C library,
// verified against the public `srt.h` of the pinned libsrt build. The
// library is linked statically into this binary through the same build
// script directive the library crate uses, and none of these entry points
// carry Rust-side invariants beyond the argument types spelled out here.
unsafe extern "C" {
    fn srt_startup() -> c_int;
    fn srt_setloglevel(level: c_int);
    fn srt_create_socket() -> SrtSocket;
    fn srt_bind(u: SrtSocket, name: *const SockaddrIn, namelen: c_int) -> c_int;
    fn srt_listen(u: SrtSocket, backlog: c_int) -> c_int;
    fn srt_accept(u: SrtSocket, addr: *mut SockaddrIn, addrlen: *mut c_int) -> SrtSocket;
    fn srt_close(u: SrtSocket) -> c_int;
    fn srt_recv(u: SrtSocket, buf: *mut u8, len: c_int) -> c_int;
    fn srt_setsockopt(
        u: SrtSocket,
        level: c_int,
        optname: c_int,
        optval: *const c_void,
        optlen: c_int,
    ) -> c_int;
    fn srt_getlasterror(errno_loc: *mut c_int) -> c_int;
    fn srt_getlasterror_str() -> *const c_char;
}

const SRT_INVALID_SOCK: SrtSocket = -1;
const SRT_EASYNCRCV: c_int = 6003;

/// `LOG_CRIT`. Non-blocking accept/recv report "nothing available yet" as
/// an *error* on every idle poll, which would bury real problems in noise.
const LIBSRT_LOG_LEVEL: c_int = 2;

const SRTO_RCVSYN: c_int = 2;
const SRTO_FC: c_int = 4;
const SRTO_SNDBUF: c_int = 5;
const SRTO_RCVBUF: c_int = 6;
const SRTO_UDP_SNDBUF: c_int = 8;
const SRTO_UDP_RCVBUF: c_int = 9;
const SRTO_REUSEADDR: c_int = 15;
const SRTO_MAXBW: c_int = 16;
const SRTO_LATENCY: c_int = 23;
const SRTO_LOSSMAXTTL: c_int = 42;
const SRTO_TRANSTYPE: c_int = 50;
const SRTT_LIVE: c_int = 0;

// Mirrors src/media/srt/socket.rs's DESIRED_* constants for
// srt_set_highbitrate_opts -- kept in sync by hand since that function is
// private to the production srt module (see module doc comment).
const DESIRED_LATENCY_MS: c_int = 250;
const DESIRED_LOSSMAXTTL: c_int = 256;
const DESIRED_FC: c_int = 32768;
const DESIRED_SRT_BUF: c_int = 12 * 1024 * 1024;

fn srt_error() -> String {
    // SAFETY: Category 8 - FFI boundary. `srt_getlasterror_str` returns a
    // pointer to libsrt's thread-local static message buffer, valid until
    // the next libsrt call on this thread; copied out before returning.
    let raw = unsafe { srt_getlasterror_str() };
    if raw.is_null() {
        return "unknown libsrt error".to_string();
    }
    // SAFETY: Category 8 - FFI boundary. Non-null NUL-terminated C string
    // owned by libsrt, read-only and copied immediately.
    unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned()
}

fn srt_startup_once() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        // SAFETY: Category 8 - FFI boundary. `srt_startup` takes no
        // arguments and is required exactly once per process before any
        // other libsrt call; `Once` provides that guarantee.
        unsafe {
            srt_startup();
            srt_setloglevel(LIBSRT_LOG_LEVEL);
        }
    });
}

fn set_int_option(
    socket: SrtSocket,
    option: c_int,
    value: c_int,
    name: &str,
) -> Result<(), String> {
    // SAFETY: Category 8 - FFI boundary. `socket` is a live libsrt socket
    // owned by the caller, and the value pointer/length pair describes a
    // stack `c_int` that outlives the call.
    let result = unsafe {
        srt_setsockopt(
            socket,
            0,
            option,
            std::ptr::from_ref(&value).cast::<c_void>(),
            std::mem::size_of::<c_int>() as c_int,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(format!("set {name}: {}", srt_error()))
    }
}

/// Same preset production restream applies via `srt_set_highbitrate_opts`
/// before its own sink-mode listener binds (now removed from production —
/// see `docs/agent-guidance/quality/srt-scaling-investigation.md`).
/// `udp_buffer` is the one caller-configurable knob (defaults to matching
/// `RESTREAM_SRT_UDP_BUFFER`'s own 8MB production default).
fn apply_highbitrate_opts(socket: SrtSocket, udp_buffer: c_int) -> Result<(), String> {
    set_int_option(socket, SRTO_LATENCY, DESIRED_LATENCY_MS, "SRTO_LATENCY")?;
    set_int_option(
        socket,
        SRTO_LOSSMAXTTL,
        DESIRED_LOSSMAXTTL,
        "SRTO_LOSSMAXTTL",
    )?;
    set_int_option(socket, SRTO_UDP_SNDBUF, udp_buffer, "SRTO_UDP_SNDBUF")?;
    set_int_option(socket, SRTO_UDP_RCVBUF, udp_buffer, "SRTO_UDP_RCVBUF")?;
    // FC before SNDBUF/RCVBUF: libsrt documents that both must not exceed
    // FC in packet-count terms.
    set_int_option(socket, SRTO_FC, DESIRED_FC, "SRTO_FC")?;
    set_int_option(socket, SRTO_SNDBUF, DESIRED_SRT_BUF, "SRTO_SNDBUF")?;
    set_int_option(socket, SRTO_RCVBUF, DESIRED_SRT_BUF, "SRTO_RCVBUF")?;
    let maxbw: i64 = -1;
    // SAFETY: Category 8 - FFI boundary. `socket` is a live libsrt socket;
    // `maxbw` is a stack-local i64 matching SRTO_MAXBW's contract.
    let rc = unsafe {
        srt_setsockopt(
            socket,
            0,
            SRTO_MAXBW,
            std::ptr::from_ref(&maxbw).cast::<c_void>(),
            std::mem::size_of::<i64>() as c_int,
        )
    };
    if rc != 0 {
        return Err(format!("set SRTO_MAXBW: {}", srt_error()));
    }
    Ok(())
}

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    discarded_bytes: AtomicU64,
    closed: AtomicU64,
}

/// A multi-threaded SRT listener that accepts connections and discards
/// everything read from them.
pub(crate) struct HarnessSrtSink {
    port: u16,
    listener: SrtSocket,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    threads: Vec<JoinHandle<()>>,
}

impl HarnessSrtSink {
    pub(crate) fn start(
        port: u16,
        udp_buffer: i32,
        discard_threads: usize,
    ) -> Result<Self, String> {
        srt_startup_once();
        let discard_threads = discard_threads.max(1);

        // SAFETY: Category 8 - FFI boundary. No arguments; returns a
        // socket id or SRT_INVALID_SOCK, checked below.
        let listener = unsafe { srt_create_socket() };
        if listener == SRT_INVALID_SOCK {
            return Err(format!("create harness SRT sink socket: {}", srt_error()));
        }

        let configured = configure_and_bind(listener, port, udp_buffer as c_int);
        if let Err(error) = configured {
            // SAFETY: Category 8 - FFI boundary. Closing the socket
            // created immediately above, which no other thread has
            // observed yet.
            unsafe {
                srt_close(listener);
            }
            return Err(error);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let mut threads = Vec::with_capacity(discard_threads);
        for worker_idx in 0..discard_threads {
            let thread_stop = stop.clone();
            let thread_counters = counters.clone();
            let handle = std::thread::Builder::new()
                .name(format!("harness-srt-sink-{port}-{worker_idx}"))
                .spawn(move || discard_loop(listener, &thread_stop, &thread_counters))
                .map_err(|error| format!("spawn harness SRT sink thread: {error}"))?;
            threads.push(handle);
        }

        tracing::info!(
            "[harness-srt-sink] listening on {port} ({discard_threads} discard thread(s), udp_buffer={udp_buffer})"
        );

        Ok(Self {
            port,
            listener,
            stop,
            counters,
            threads,
        })
    }

    /// Stop every discard thread, join them, then close the shared
    /// listener socket exactly once (no thread touches it directly).
    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
        tracing::info!(
            "[harness-srt-sink] stopped {} (accepted={} discarded={}MB closed={})",
            self.port,
            self.counters.accepted.load(Ordering::Relaxed),
            self.counters.discarded_bytes.load(Ordering::Relaxed) / (1024 * 1024),
            self.counters.closed.load(Ordering::Relaxed),
        );
        // SAFETY: Category 8 - FFI boundary. Every thread that accepted
        // connections off this listener has now joined and stopped
        // touching it; closing here is the sole close of this socket.
        unsafe {
            srt_close(self.listener);
        }
    }
}

fn configure_and_bind(listener: SrtSocket, port: u16, udp_buffer: c_int) -> Result<(), String> {
    // SAFETY: Category 8 - FFI boundary. `listener` is a live socket owned
    // by this function; the option value is a stack-allocated c_int.
    unsafe {
        let live: c_int = SRTT_LIVE;
        srt_setsockopt(
            listener,
            0,
            SRTO_TRANSTYPE,
            std::ptr::from_ref(&live).cast::<c_void>(),
            std::mem::size_of::<c_int>() as c_int,
        );
    }
    apply_highbitrate_opts(listener, udp_buffer)?;
    set_int_option(listener, SRTO_REUSEADDR, 1, "SRTO_REUSEADDR")?;
    // Non-blocking accept, so the discard loop can service existing
    // clients and honour the stop flag without blocking on a new one.
    set_int_option(listener, SRTO_RCVSYN, 0, "SRTO_RCVSYN")?;

    let addr = SockaddrIn {
        sin_family: libc::AF_INET as u16,
        sin_port: port.to_be(),
        sin_addr: 0, // INADDR_ANY
        sin_zero: [0; 8],
    };
    // SAFETY: Category 8 - FFI boundary. `addr` is a fully initialised
    // `sockaddr_in` living on this stack frame for the call's duration,
    // and its declared length matches the struct.
    let bound = unsafe { srt_bind(listener, &addr, std::mem::size_of::<SockaddrIn>() as c_int) };
    if bound != 0 {
        return Err(format!("bind harness SRT sink on {port}: {}", srt_error()));
    }
    // SAFETY: Category 8 - FFI boundary. `listener` is bound above.
    if unsafe { srt_listen(listener, 1024) } != 0 {
        return Err(format!(
            "listen harness SRT sink on {port}: {}",
            srt_error()
        ));
    }
    Ok(())
}

/// One worker's private accept-and-discard loop. `listener` is shared
/// read-only across all workers (only `srt_accept`/`srt_getlasterror` are
/// called on it here, both safe for concurrent use); each worker's
/// `clients` list is touched only by that worker.
fn discard_loop(listener: SrtSocket, stop: &AtomicBool, counters: &SinkCounters) {
    let mut clients: Vec<SrtSocket> = Vec::with_capacity(1024);
    let mut idx = 0usize;
    let mut buf = [0u8; 1316];
    let mut empty_streak: usize = 0;

    while !stop.load(Ordering::Relaxed) {
        let mut peer = SockaddrIn {
            sin_family: 0,
            sin_port: 0,
            sin_addr: 0,
            sin_zero: [0; 8],
        };
        let mut peer_len = std::mem::size_of::<SockaddrIn>() as c_int;
        // SAFETY: Category 8 - FFI boundary. Non-blocking accept on a
        // listener socket shared (read-only, concurrency-safe per
        // libsrt) across workers; out-parameters are stack locals sized
        // by `peer_len`.
        let accepted = unsafe { srt_accept(listener, &mut peer, &mut peer_len) };
        if accepted != SRT_INVALID_SOCK {
            clients.push(accepted);
            counters.accepted.fetch_add(1, Ordering::Relaxed);
            empty_streak = 0;
            continue;
        }

        if clients.is_empty() {
            empty_streak = 0;
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        idx %= clients.len();
        let sock = clients[idx];
        // SAFETY: Category 8 - FFI boundary. `sock` was accepted by this
        // thread and is never touched by any other thread; `buf` is a
        // correctly-sized stack buffer.
        let n = unsafe { srt_recv(sock, buf.as_mut_ptr(), buf.len() as c_int) };
        if n > 0 {
            counters
                .discarded_bytes
                .fetch_add(n as u64, Ordering::Relaxed);
            idx += 1;
            empty_streak = 0;
        } else {
            let mut sys_errno: c_int = 0;
            // SAFETY: Category 8 - FFI boundary. `sys_errno` is a valid
            // stack-local out-parameter.
            let err = unsafe { srt_getlasterror(&mut sys_errno) };
            if err == SRT_EASYNCRCV {
                // Nothing buffered yet -- not a close. A fresh accept
                // commonly has no data on its first poll.
                idx += 1;
                empty_streak += 1;
                if empty_streak >= clients.len() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    empty_streak = 0;
                }
            } else {
                // SAFETY: Category 8 - FFI boundary. `sock` is owned
                // exclusively by this thread's client list.
                unsafe {
                    srt_close(sock);
                }
                clients.swap_remove(idx);
                counters.closed.fetch_add(1, Ordering::Relaxed);
                if idx >= clients.len() {
                    idx = 0;
                }
                empty_streak = 0;
            }
        }
    }

    for sock in clients {
        // SAFETY: Category 8 - FFI boundary. Every socket in this list was
        // accepted by this thread and is closed exactly once, by it.
        unsafe {
            srt_close(sock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind probe socket");
        socket.local_addr().expect("probe socket addr").port()
    }

    #[test]
    fn harness_srt_sink_starts_and_stops_without_connections() {
        let port = free_udp_port();
        let sink = HarnessSrtSink::start(port, 8 * 1024 * 1024, 1).expect("start harness SRT sink");
        sink.stop();
    }

    #[test]
    fn harness_srt_sink_rejects_a_port_already_bound() {
        let port = free_udp_port();
        let sink = HarnessSrtSink::start(port, 8 * 1024 * 1024, 1).expect("start harness SRT sink");
        let conflict = HarnessSrtSink::start(port, 8 * 1024 * 1024, 1);
        assert!(
            conflict.is_err(),
            "second sink on port {port} unexpectedly bound"
        );
        sink.stop();
    }

    #[test]
    fn harness_srt_sink_supports_multiple_discard_threads() {
        let port = free_udp_port();
        let sink = HarnessSrtSink::start(port, 8 * 1024 * 1024, 4).expect("start harness SRT sink");
        assert_eq!(sink.threads.len(), 4);
        sink.stop();
    }
}
