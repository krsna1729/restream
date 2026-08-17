//! Harness-native SRT accept-and-discard listener pool for `MSR_PEER=sink`.
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
//! **Threading: exclusive port ownership, not shared-multiplexer pooling.**
//! Earlier revisions let every discard thread call `srt_accept()`/`srt_recv()`
//! concurrently against the *same* listener socket. That was measured to be
//! a severe regression (see
//! `docs/agent-guidance/quality/srt-scaling-investigation.md`'s tunable
//! sweep): a listening port in unpatched libsrt has one shared multiplexer
//! (`CSndQueue`/`CRcvQueue`), and multiple external threads hammering it
//! concurrently contends against that structure's internal locking rather
//! than adding capacity. `HarnessSrtSinkPool` instead binds every requested
//! port up front, then partitions the port list into contiguous chunks
//! across `thread_count` threads (`ports.len() / thread_count`, remainder
//! spread one-per-thread to the first few) — each thread owns its chunk of
//! listeners *exclusively* and never touches a socket outside it, so no two
//! threads ever share a multiplexer. `thread_count` must therefore be `<=`
//! the port count for this to have any effect; the pool clamps it to
//! `[1, ports.len()]`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use super::HarnessSrtCrypto;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarnessSrtSinkBackend {
    Libsrt,
    Rust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RustSinkScaling {
    Ports,
    PerStreamPort,
    ReusePort,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RustConnectedRouting {
    RoundRobin,
    LeastTuples,
}

impl RustConnectedRouting {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "round-robin" | "round_robin" | "rr" => Ok(Self::RoundRobin),
            "least-tuples" | "least_tuples" | "least-loaded" | "least_loaded" => {
                Ok(Self::LeastTuples)
            }
            other => Err(format!(
                "HARNESS_SRT_SINK_CONNECTED_ROUTING must be round-robin or least-tuples (got {other})"
            )),
        }
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        let value = std::env::var("HARNESS_SRT_SINK_CONNECTED_ROUTING")
            .unwrap_or_else(|_| "round-robin".to_string());
        Self::parse(&value)
    }
}

impl RustSinkScaling {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ports" | "distinct-ports" => Ok(Self::Ports),
            "per-stream" | "per-stream-port" | "per-stream-ports" | "one-port-per-stream" => {
                Ok(Self::PerStreamPort)
            }
            "reuseport" | "reuse-port" => Ok(Self::ReusePort),
            "connected" | "connected-dgram" => Ok(Self::Connected),
            other => Err(format!(
                "HARNESS_SRT_SINK_SCALING must be ports, per-stream-port, reuseport, or connected (got {other})"
            )),
        }
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        let value =
            std::env::var("HARNESS_SRT_SINK_SCALING").unwrap_or_else(|_| "ports".to_string());
        Self::parse(&value)
    }
}

impl HarnessSrtSinkBackend {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "libsrt" | "native" => Ok(Self::Libsrt),
            "rust" | "srt-rust" => Ok(Self::Rust),
            other => Err(format!(
                "HARNESS_SRT_SINK_BACKEND must be libsrt or rust (got {other})"
            )),
        }
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        let value = std::env::var("HARNESS_SRT_SINK_BACKEND")
            .or_else(|_| std::env::var("RESTREAM_SRT_BACKEND"))
            .unwrap_or_else(|_| "libsrt".to_string());
        Self::parse(&value)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Libsrt => "libsrt",
            Self::Rust => "rust",
        }
    }
}

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
const SRT_EASYNCRCV: c_int = 6002;
const SRT_ETIMEOUT: c_int = 6003;

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
const SRTO_PASSPHRASE: c_int = 26;
const SRTO_PBKEYLEN: c_int = 27;
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

fn set_string_option(
    socket: SrtSocket,
    option: c_int,
    value: &str,
    name: &str,
) -> Result<(), String> {
    let value = CString::new(value).map_err(|_| format!("{name} contains an interior NUL"))?;
    let result = unsafe {
        srt_setsockopt(
            socket,
            0,
            option,
            value.as_ptr().cast::<c_void>(),
            value.as_bytes().len() as c_int,
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

fn create_and_bind_listener(
    port: u16,
    udp_buffer: c_int,
    crypto: &HarnessSrtCrypto,
) -> Result<SrtSocket, String> {
    // SAFETY: Category 8 - FFI boundary. No arguments; returns a socket id
    // or SRT_INVALID_SOCK, checked below.
    let listener = unsafe { srt_create_socket() };
    if listener == SRT_INVALID_SOCK {
        return Err(format!("create harness SRT sink socket: {}", srt_error()));
    }
    if let Err(error) = configure_and_bind(listener, port, udp_buffer, crypto) {
        // SAFETY: Category 8 - FFI boundary. Closing the socket created
        // immediately above, which no other thread has observed yet.
        unsafe {
            srt_close(listener);
        }
        return Err(error);
    }
    Ok(listener)
}

fn configure_and_bind(
    listener: SrtSocket,
    port: u16,
    udp_buffer: c_int,
    crypto: &HarnessSrtCrypto,
) -> Result<(), String> {
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
    if let Some(passphrase) = crypto.passphrase.as_deref() {
        set_string_option(listener, SRTO_PASSPHRASE, passphrase, "SRTO_PASSPHRASE")?;
        let pbkeylen = crypto
            .pbkeylen
            .as_deref()
            .unwrap_or("16")
            .parse::<c_int>()
            .map_err(|error| format!("parse SRTO_PBKEYLEN: {error}"))?;
        set_int_option(listener, SRTO_PBKEYLEN, pbkeylen, "SRTO_PBKEYLEN")?;
    }
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

/// A pool of SRT accept-and-discard listeners spread across a bounded set
/// of threads, each owning a disjoint subset of ports exclusively (see
/// module doc comment for why exclusivity, not shared-multiplexer pooling).
pub(crate) struct HarnessSrtSinkPool {
    listeners: Vec<SrtSocket>,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    threads: Vec<JoinHandle<()>>,
}

impl HarnessSrtSinkPool {
    /// `thread_count` is clamped to `[1, ports.len()]`. Ports are chunked
    /// contiguously across threads (`ports.len() / thread_count` each, the
    /// first `ports.len() % thread_count` threads getting one extra) so
    /// every thread owns its chunk exclusively and no multiplexer is ever
    /// touched by more than one thread.
    pub(crate) fn start(
        ports: &[u16],
        udp_buffer: i32,
        thread_count: usize,
        crypto: &HarnessSrtCrypto,
    ) -> Result<Self, String> {
        srt_startup_once();
        if ports.is_empty() {
            return Err("harness SRT sink pool needs at least one port".to_string());
        }
        let thread_count = thread_count.clamp(1, ports.len());

        let mut listeners = Vec::with_capacity(ports.len());
        for &port in ports {
            match create_and_bind_listener(port, udp_buffer as c_int, crypto) {
                Ok(listener) => listeners.push(listener),
                Err(error) => {
                    for listener in listeners {
                        // SAFETY: Category 8 - FFI boundary. Every listener
                        // in this partial Vec was created and bound above
                        // by this function and observed by no other thread.
                        unsafe {
                            srt_close(listener);
                        }
                    }
                    return Err(error);
                }
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let base = listeners.len() / thread_count;
        let remainder = listeners.len() % thread_count;
        let mut threads = Vec::with_capacity(thread_count);
        let mut cursor = 0usize;
        for worker_idx in 0..thread_count {
            let take = base + usize::from(worker_idx < remainder);
            if take == 0 {
                continue;
            }
            let owned: Vec<SrtSocket> = listeners[cursor..cursor + take].to_vec();
            cursor += take;
            let thread_stop = stop.clone();
            let thread_counters = counters.clone();
            let handle = std::thread::Builder::new()
                .name(format!("harness-srt-sink-pool-{worker_idx}"))
                .spawn(move || discard_loop(owned, &thread_stop, &thread_counters))
                .map_err(|error| format!("spawn harness SRT sink thread: {error}"))?;
            threads.push(handle);
        }

        tracing::info!(
            "[harness-srt-sink] listening on {} port(s) ({thread_count} thread(s), \
             {base}-{} ports/thread, udp_buffer={udp_buffer})",
            ports.len(),
            base + usize::from(remainder > 0),
        );

        Ok(Self {
            listeners,
            stop,
            counters,
            threads,
        })
    }

    /// Stop every worker thread, join them, then close every listener
    /// socket exactly once (no thread touches a listener outside its own
    /// owned chunk while running, and all threads have joined by the time
    /// we close anything here).
    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
        tracing::info!(
            "[harness-srt-sink] stopped {} port(s) (accepted={} discarded={}MB closed={})",
            self.listeners.len(),
            self.counters.accepted.load(Ordering::Relaxed),
            self.counters.discarded_bytes.load(Ordering::Relaxed) / (1024 * 1024),
            self.counters.closed.load(Ordering::Relaxed),
        );
        for listener in self.listeners.drain(..) {
            // SAFETY: Category 8 - FFI boundary. Every worker thread that
            // could have touched this listener has joined above.
            unsafe {
                srt_close(listener);
            }
        }
    }
}

/// One worker's private accept-and-discard loop over its exclusively-owned
/// `listeners`. No other thread ever calls anything on these sockets, so no
/// lock is needed for either the listener round-robin or the accepted
/// `clients` list.
fn discard_loop(listeners: Vec<SrtSocket>, stop: &AtomicBool, counters: &SinkCounters) {
    let mut clients: Vec<SrtSocket> = Vec::with_capacity(1024);
    let mut client_idx = 0usize;
    let mut listener_idx = 0usize;
    let mut buf = [0u8; 1316];
    let mut empty_streak: usize = 0;

    while !stop.load(Ordering::Relaxed) {
        let listener = listeners[listener_idx % listeners.len()];
        listener_idx = listener_idx.wrapping_add(1);

        let mut peer = SockaddrIn {
            sin_family: 0,
            sin_port: 0,
            sin_addr: 0,
            sin_zero: [0; 8],
        };
        let mut peer_len = std::mem::size_of::<SockaddrIn>() as c_int;
        // SAFETY: Category 8 - FFI boundary. Non-blocking accept on a
        // listener this thread exclusively owns; out-parameters are stack
        // locals sized by `peer_len`.
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

        client_idx %= clients.len();
        let sock = clients[client_idx];
        // SAFETY: Category 8 - FFI boundary. `sock` was accepted by this
        // thread and is never touched by any other thread; `buf` is a
        // correctly-sized stack buffer.
        let n = unsafe { srt_recv(sock, buf.as_mut_ptr(), buf.len() as c_int) };
        if n > 0 {
            counters
                .discarded_bytes
                .fetch_add(n as u64, Ordering::Relaxed);
            client_idx += 1;
            empty_streak = 0;
        } else {
            let mut sys_errno: c_int = 0;
            // SAFETY: Category 8 - FFI boundary. `sys_errno` is a valid
            // stack-local out-parameter.
            let err = unsafe { srt_getlasterror(&mut sys_errno) };
            if err == SRT_EASYNCRCV || err == SRT_ETIMEOUT {
                // Nothing buffered yet -- not a close. A fresh accept
                // commonly has no data on its first poll.
                client_idx += 1;
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
                clients.swap_remove(client_idx);
                counters.closed.fetch_add(1, Ordering::Relaxed);
                if client_idx >= clients.len() {
                    client_idx = 0;
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

#[path = "harness_srt_sink/rust_sink.rs"]
mod rust_sink;

pub(crate) enum HarnessSrtSinkPeer {
    Libsrt(HarnessSrtSinkPool),
    Rust(rust_sink::RustHarnessSrtSinkPool),
}

impl HarnessSrtSinkPeer {
    pub(crate) fn start(
        backend: HarnessSrtSinkBackend,
        ports: &[u16],
        udp_buffer: i32,
        thread_count: usize,
        crypto: &HarnessSrtCrypto,
    ) -> Result<Self, String> {
        match backend {
            HarnessSrtSinkBackend::Libsrt => {
                HarnessSrtSinkPool::start(ports, udp_buffer, thread_count, crypto).map(Self::Libsrt)
            }
            HarnessSrtSinkBackend::Rust => {
                let scaling = RustSinkScaling::from_env()?;
                rust_sink::RustHarnessSrtSinkPool::start(
                    ports,
                    udp_buffer,
                    thread_count,
                    scaling,
                    crypto,
                )
                .map(Self::Rust)
            }
        }
    }

    pub(crate) fn stop(self) {
        match self {
            Self::Libsrt(pool) => pool.stop(),
            Self::Rust(pool) => pool.stop(),
        }
    }
}
#[cfg(test)]
#[path = "harness_srt_sink/tests.rs"]
mod tests;
