//! A raw SRT listener that accepts a real connection and deliberately never
//! drains it — the receiver shape needed to exercise the fabric's
//! *backpressured-but-connected* egress path
//! (`classify_stall`/`observe_stall`, `src/media/egress/policy.rs` and
//! `src/media/egress/backends/srt.rs`) live.
//!
//! **Why this exists.** The obvious way to build a stalled destination —
//! `SIGSTOP` a real MediaMTX receiver — does not produce backpressure. It
//! freezes *every* thread in the receiver, including libsrt's own ACK and
//! keepalive threads, so the sender declares the connection broken within
//! seconds and falls into connect-retry against an unreachable peer (see
//! `fault_recovery/srt_stall.rs`). Backpressure needs the opposite: a peer
//! whose libsrt keeps the connection fully alive at the protocol layer while
//! the application above it never reads a byte.
//!
//! **How it produces backpressure.** The listener never calls `srt_recv`, so
//! its receive buffer fills and stays full, and two inherited socket options
//! make the sender feel that immediately:
//!
//! - `SRTO_TLPKTDROP = 0`. Sender-side too-late-packet dropping is gated on
//!   the *peer's* handshake flag (`m_bPeerTLPktDrop` in libsrt's
//!   `CUDT::sndDropTooLate`), so a receiver that advertises TLPKTDROP off
//!   stops the sender from discarding its backlog. Without this the sender
//!   drops old packets instead of building the backlog the stall classifier
//!   measures.
//! - `SRTO_FC` / `SRTO_RCVBUF` at libsrt's 32-packet minimum, so the flow
//!   window closes after a few dozen kilobytes rather than after megabytes.
//!
//! Options set on the listening socket are inherited by accepted sockets
//! (libsrt copy-constructs the accepted socket from the listener), so both
//! apply to the connection restream actually sends on.
//!
//! **Why the FFI is hand-written here.** `restream::media::srt` re-exports
//! only the handful of libsrt entry points its own benches need; the
//! observation surface this sink depends on (`srt_getsockstate`, and
//! `SRTO_RCVDATA` through `srt_getsockopt`) is not part of it. Rather than
//! widen a production module's public API for a test tool, the few
//! declarations needed are written out here, checked against
//! `srtcore/srt.h` of the pinned libsrt build. Linking is free: the harness
//! binary already links the same static `libsrt.a` as the library crate.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

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
    fn srt_getsockstate(u: SrtSocket) -> c_int;
    fn srt_setsockopt(
        u: SrtSocket,
        level: c_int,
        optname: c_int,
        optval: *const c_void,
        optlen: c_int,
    ) -> c_int;
    fn srt_getsockopt(
        u: SrtSocket,
        level: c_int,
        optname: c_int,
        optval: *mut c_void,
        optlen: *mut c_int,
    ) -> c_int;
    fn srt_getlasterror_str() -> *const c_char;
}

const SRT_INVALID_SOCK: SrtSocket = -1;
const SRTS_CONNECTED: c_int = 5;

/// `LOG_CRIT`. Non-blocking `srt_accept` reports "no pending connection
/// available at the moment" as an *error* on every idle poll, which would
/// bury a passing run's output; nothing else in this process uses libsrt.
const LIBSRT_LOG_LEVEL: c_int = 2;

const SRTO_RCVSYN: c_int = 2;
const SRTO_FC: c_int = 4;
const SRTO_RCVBUF: c_int = 6;
const SRTO_RCVDATA: c_int = 20;
const SRTO_TLPKTDROP: c_int = 31;

/// libsrt's minimum flight-flag size; smaller values are rejected outright.
const MIN_FLIGHT_PACKETS: c_int = 32;
/// Receive buffer expressed in bytes; libsrt clamps it to `SRTO_FC` packets.
const RECEIVE_BUFFER_BYTES: c_int = MIN_FLIGHT_PACKETS * 1316;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn srt_error() -> String {
    // SAFETY: Category 8 - FFI boundary. `srt_getlasterror_str` returns a
    // pointer to libsrt's thread-local static message buffer, valid until the
    // next libsrt call on this thread; it is copied out before returning.
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
        // SAFETY: Category 8 - FFI boundary. `srt_startup` takes no arguments
        // and is required exactly once per process before any other libsrt
        // call; `Once` provides that guarantee. `srt_setloglevel` takes a
        // plain syslog-style level.
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

/// Packets sitting unread in the socket's receive buffer, or `None` if the
/// socket is no longer readable (broken or closed).
fn unread_packets(socket: SrtSocket) -> Option<u64> {
    let mut value: c_int = 0;
    let mut len = std::mem::size_of::<c_int>() as c_int;
    // SAFETY: Category 8 - FFI boundary. `socket` is a live libsrt socket
    // owned by this module; `value`/`len` are stack locals matching the
    // `SRTO_RCVDATA` integer contract in `srt.h`.
    let result = unsafe {
        srt_getsockopt(
            socket,
            0,
            SRTO_RCVDATA,
            std::ptr::from_mut(&mut value).cast::<c_void>(),
            &mut len,
        )
    };
    (result == 0).then(|| value.max(0) as u64)
}

fn socket_is_connected(socket: SrtSocket) -> bool {
    // SAFETY: Category 8 - FFI boundary. Pure state query on a libsrt socket
    // owned by this module.
    unsafe { srt_getsockstate(socket) == SRTS_CONNECTED }
}

#[derive(Default)]
struct SinkCounters {
    accepted: AtomicU64,
    connected_now: AtomicU64,
    peak_connected: AtomicU64,
    peak_unread_packets: AtomicU64,
    last_unread_packets: AtomicU64,
}

/// A sample of what the sink has seen, safe to fold into a result artifact.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawSrtSinkObservation {
    /// Connections accepted since the sink started.
    pub(crate) accepted: u64,
    /// Connections currently in libsrt's `SRTS_CONNECTED` state.
    pub(crate) connected_now: u64,
    /// Highest simultaneous connected count observed.
    pub(crate) peak_connected: u64,
    /// Highest number of packets seen queued and unread on any connection.
    pub(crate) peak_unread_packets: u64,
    /// Most recent unread-packet sample.
    pub(crate) last_unread_packets: u64,
}

impl RawSrtSinkObservation {
    pub(crate) fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "accepted": self.accepted,
            "connectedNow": self.connected_now,
            "peakConnected": self.peak_connected,
            "peakUnreadPackets": self.peak_unread_packets,
            "lastUnreadPackets": self.last_unread_packets,
            "bytesRead": 0,
        })
    }
}

/// An SRT listener that accepts connections and never reads from them.
pub(crate) struct RawSrtStallSink {
    port: u16,
    stop: Arc<AtomicBool>,
    counters: Arc<SinkCounters>,
    thread: Option<JoinHandle<()>>,
}

impl RawSrtStallSink {
    /// Bind and listen on `port`, then run the accept-and-hold loop on a
    /// dedicated OS thread (libsrt calls never belong on a Tokio worker).
    pub(crate) fn start(port: u16) -> Result<Self, String> {
        srt_startup_once();

        // SAFETY: Category 8 - FFI boundary. No arguments; returns a socket
        // id or `SRT_INVALID_SOCK`, checked below.
        let listener = unsafe { srt_create_socket() };
        if listener == SRT_INVALID_SOCK {
            return Err(format!("create raw SRT sink socket: {}", srt_error()));
        }

        let configured =
            configure_listener(listener).and_then(|()| bind_and_listen(listener, port));
        if let Err(error) = configured {
            // SAFETY: Category 8 - FFI boundary. Closing the socket created
            // immediately above, which no other thread has observed yet.
            unsafe {
                srt_close(listener);
            }
            return Err(error);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(SinkCounters::default());
        let thread_stop = stop.clone();
        let thread_counters = counters.clone();
        let thread = std::thread::Builder::new()
            .name(format!("raw-srt-stall-sink-{port}"))
            .spawn(move || accept_and_hold(listener, &thread_stop, &thread_counters))
            .map_err(|error| format!("spawn raw SRT sink thread: {error}"))?;

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
            peak_unread_packets: self.counters.peak_unread_packets.load(Ordering::Relaxed),
            last_unread_packets: self.counters.last_unread_packets.load(Ordering::Relaxed),
        }
    }

    /// Stop accepting, close every held connection, and join the thread.
    pub(crate) fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RawSrtStallSink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn configure_listener(listener: SrtSocket) -> Result<(), String> {
    // Non-blocking accept, so the loop below can honour the stop flag without
    // needing a second socket to interrupt itself.
    set_int_option(listener, SRTO_RCVSYN, 0, "SRTO_RCVSYN")?;
    // The two options that turn "nobody is reading" into real sender-visible
    // backpressure — see the module docs.
    set_int_option(listener, SRTO_TLPKTDROP, 0, "SRTO_TLPKTDROP")?;
    set_int_option(listener, SRTO_FC, MIN_FLIGHT_PACKETS, "SRTO_FC")?;
    set_int_option(listener, SRTO_RCVBUF, RECEIVE_BUFFER_BYTES, "SRTO_RCVBUF")
}

fn bind_and_listen(listener: SrtSocket, port: u16) -> Result<(), String> {
    let addr = SockaddrIn {
        sin_family: libc::AF_INET as u16,
        sin_port: port.to_be(),
        sin_addr: u32::to_be(0x7f00_0001),
        sin_zero: [0; 8],
    };
    // SAFETY: Category 8 - FFI boundary. `addr` is a fully initialised
    // `sockaddr_in` living on this stack frame for the duration of the call,
    // and its declared length matches the struct.
    let bound = unsafe { srt_bind(listener, &addr, std::mem::size_of::<SockaddrIn>() as c_int) };
    if bound != 0 {
        return Err(format!("bind raw SRT sink on {port}: {}", srt_error()));
    }
    // SAFETY: Category 8 - FFI boundary. `listener` is bound above.
    if unsafe { srt_listen(listener, 8) } != 0 {
        return Err(format!("listen raw SRT sink on {port}: {}", srt_error()));
    }
    Ok(())
}

/// Accept connections and hold them open without ever reading, sampling
/// liveness and receive-buffer depth for the test's evidence trail.
fn accept_and_hold(listener: SrtSocket, stop: &AtomicBool, counters: &SinkCounters) {
    let mut held: Vec<SrtSocket> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let mut peer = SockaddrIn {
            sin_family: 0,
            sin_port: 0,
            sin_addr: 0,
            sin_zero: [0; 8],
        };
        let mut peer_len = std::mem::size_of::<SockaddrIn>() as c_int;
        // SAFETY: Category 8 - FFI boundary. Non-blocking accept on a
        // listening socket owned by this thread; the out-parameters are
        // stack locals sized by `peer_len`.
        let accepted = unsafe { srt_accept(listener, &mut peer, &mut peer_len) };
        if accepted != SRT_INVALID_SOCK {
            held.push(accepted);
            counters.accepted.fetch_add(1, Ordering::Relaxed);
        }

        sample_held_connections(&held, counters);
        std::thread::sleep(ACCEPT_POLL_INTERVAL);
    }

    for socket in held {
        // SAFETY: Category 8 - FFI boundary. Sockets accepted by this thread
        // and never shared outside it.
        unsafe {
            srt_close(socket);
        }
    }
    // SAFETY: Category 8 - FFI boundary. The listening socket owned by this
    // thread, closed exactly once as the loop exits.
    unsafe {
        srt_close(listener);
    }
}

fn sample_held_connections(held: &[SrtSocket], counters: &SinkCounters) {
    let mut connected = 0u64;
    let mut peak_unread = 0u64;
    let mut last_unread = 0u64;
    for &socket in held {
        if socket_is_connected(socket) {
            connected += 1;
        }
        if let Some(packets) = unread_packets(socket) {
            peak_unread = peak_unread.max(packets);
            last_unread = packets;
        }
    }
    counters.connected_now.store(connected, Ordering::Relaxed);
    counters
        .peak_connected
        .fetch_max(connected, Ordering::Relaxed);
    counters
        .peak_unread_packets
        .fetch_max(peak_unread, Ordering::Relaxed);
    counters
        .last_unread_packets
        .store(last_unread, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Borrow a free UDP port from the kernel. The sink binds UDP itself, so
    /// this adds no capability the test did not already need.
    fn free_udp_port() -> u16 {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind probe socket");
        socket.local_addr().expect("probe socket addr").port()
    }

    /// The sink must bind, report its port, and shut down cleanly with no
    /// connection ever offered — the "nothing happened" path that every
    /// fault case depends on for teardown.
    #[test]
    fn raw_srt_stall_sink_starts_and_stops_without_connections() {
        let port = free_udp_port();
        let sink = RawSrtStallSink::start(port).expect("start raw SRT stall sink");
        assert_eq!(sink.port(), port);

        let observation = sink.observe();
        assert_eq!(observation.accepted, 0);
        assert_eq!(observation.connected_now, 0);

        sink.stop();
    }

    /// A second sink on the same port must fail rather than silently share
    /// it — otherwise a port collision would look like a stalled destination
    /// instead of a harness setup error.
    #[test]
    fn raw_srt_stall_sink_rejects_a_port_already_bound() {
        let port = free_udp_port();
        let sink = RawSrtStallSink::start(port).expect("start raw SRT stall sink");
        let conflict = RawSrtStallSink::start(port);
        assert!(
            conflict.is_err(),
            "second sink on port {port} unexpectedly bound"
        );
        sink.stop();
    }
}
