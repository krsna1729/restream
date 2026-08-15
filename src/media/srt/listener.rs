use std::net::SocketAddr;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{error, info, warn};

use super::buffer_sizing::srt_set_ingest_latency_opts;
use super::socket::{
    SrtListenerCloser, enable_srt_group_connect, from_sockaddr_in, last_srt_error,
    srt_log_effective_opts, srt_set_highbitrate_opts, to_sockaddr_in,
};
use super::srt_crypto::{apply_srt_crypto_socket, srt_crypto_from_resolved};
use super::srt_monitor::monitor_listener_socket;
use super::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};
use super::sys::*;
use super::{SrtIngestPolicyStore, SrtServer};
use crate::domain::srt_ingest::{
    DEFAULT_SRT_INGEST_LATENCY_MS, ResolvedSrtCrypto, ResolvedSrtIngestConfig,
};
use crate::media::srt::sys::{srt_bind, srt_close, srt_create_socket, srt_listen, srt_recv};

const SRT_REJX_UNAUTHORIZED: c_int = 1401;
const SRT_REJX_BAD_MODE: c_int = 1405;
const SRT_REJX_ISE: c_int = 1500;

unsafe extern "C" fn srt_listener_policy_callback(
    opaq: *mut c_void,
    ns: SRTSOCKET,
    hsversion: c_int,
    peeraddr: *const libc::sockaddr,
    streamid: *const c_char,
) -> c_int {
    // SAFETY: libsrt supplies callback arguments valid for this callback
    // invocation; catch_unwind prevents a Rust panic crossing the C ABI.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        srt_listener_policy_callback_inner(opaq, ns, hsversion, peeraddr, streamid)
    }));

    match result {
        Ok(code) => code,
        Err(_) => {
            error!("[srt] listener policy callback panicked; rejecting connection");
            // SAFETY: ns is the live socket supplied by libsrt for this
            // callback invocation.
            unsafe {
                srt_setrejectreason(ns, SRT_REJX_ISE);
            }
            -1
        }
    }
}

unsafe fn srt_listener_policy_callback_inner(
    opaq: *mut c_void,
    ns: SRTSOCKET,
    _hsversion: c_int,
    _peeraddr: *const libc::sockaddr,
    streamid: *const c_char,
) -> c_int {
    if opaq.is_null() {
        // SAFETY: ns is the live socket supplied by libsrt for this callback.
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_ISE);
        }
        return -1;
    }

    // SAFETY: opaq was created from the Arc-owned policy store in run(); that
    // Arc is held by SrtServer for at least the listener callback lifetime.
    let store = unsafe { &*(opaq as *const SrtIngestPolicyStore) };
    let streamid = if streamid.is_null() {
        String::new()
    } else {
        // SAFETY: non-null streamid is a NUL-terminated string owned by libsrt
        // for the duration of this callback invocation.
        unsafe { std::ffi::CStr::from_ptr(streamid) }
            .to_string_lossy()
            .to_string()
    };
    let parsed = parse_srt_stream_id(&streamid);
    if !matches!(
        parsed.mode,
        SrtConnectionMode::Publish | SrtConnectionMode::Read
    ) || parsed.stream_key.is_empty()
    {
        // SAFETY: ns is the live socket supplied by libsrt for this callback.
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_BAD_MODE);
        }
        return -1;
    }

    let Some(policy) = store.resolved_policy(&parsed.stream_key) else {
        // SAFETY: ns is the live socket supplied by libsrt for this callback.
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_UNAUTHORIZED);
        }
        return -1;
    };

    if let Some(crypto) = srt_crypto_from_resolved(policy.crypto)
        && apply_srt_crypto_socket(ns, &crypto).is_err()
    {
        // SAFETY: ns is the live socket supplied by libsrt for this callback.
        unsafe {
            srt_setrejectreason(ns, SRT_REJX_ISE);
        }
        return -1;
    }

    // This accept-hook callback is the only point ingest can still apply a
    // PREBIND option: `ns` is a real, distinct socket already (unlike the
    // shared listener), but libsrt has not yet called `open()` on it — see
    // `srt_set_ingest_latency_opts`'s doc comment for the confirmed
    // ordering and why this can only be sized from our own resolved
    // latency, never the value actually negotiated with the caller.
    srt_set_ingest_latency_opts(ns, policy.latency_ms);

    0
}

/// Single-threaded sink discard loop: accepts SRT connections, adds them to
/// a client list, and round-robins non-blocking reads. No per-connection
/// threads, no epoll. Runs inside `spawn_blocking` in sink mode.
fn sink_discard_loop(server_sock: SRTSOCKET) {
    let mut clients: Vec<SRTSOCKET> = Vec::with_capacity(1024);
    let mut idx = 0usize;
    let mut buf = [0u8; 1316];
    let mut accepted: u64 = 0;
    let mut discarded: u64 = 0;
    let mut closed: u64 = 0;
    let mut last_log = std::time::Instant::now();
    // Counts consecutive empty reads. Once it reaches a full lap of the
    // client list with no client having had data, every client was polled
    // and found empty — back off briefly instead of re-polling the same
    // idle set at CPU-bound rate. Reset on any accept or successful read,
    // so a busy set of clients is never throttled.
    let mut empty_streak: usize = 0;
    loop {
        // Non-blocking accept
        let mut client_sin = sockaddr_in {
            sin_family: 0,
            sin_port: 0,
            sin_addr: 0,
            sin_zero: [0; 8],
        };
        let mut len = std::mem::size_of::<sockaddr_in>() as c_int;
        let client_sock = unsafe { srt_accept(server_sock, &mut client_sin, &mut len) };
        if client_sock >= 0 {
            clients.push(client_sock);
            accepted += 1;
            empty_streak = 0;
            continue;
        }
        // Round-robin read from one client per iteration
        if !clients.is_empty() {
            idx %= clients.len();
            let sock = clients[idx];
            let n = unsafe { srt_recv(sock, buf.as_mut_ptr(), 1316) };
            if n > 0 {
                discarded += n as u64;
                idx += 1;
                empty_streak = 0;
            } else if n < 0 && last_srt_error().0 == SRT_EASYNCRCV {
                // Nonblocking socket with nothing to read yet — not a
                // close. A fresh accept commonly has no data buffered on
                // its very first poll; treating this as a close condition
                // tore down every client on its first empty read.
                idx += 1;
                empty_streak += 1;
                if empty_streak >= clients.len() {
                    // A full lap found no data on any client: back off
                    // instead of re-polling an idle set at CPU-bound rate
                    // (measured: one sink instance pegs a full core
                    // spinning empty accept+recv calls with no backoff).
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    empty_streak = 0;
                }
            } else {
                unsafe {
                    srt_close(sock);
                }
                clients.swap_remove(idx);
                closed += 1;
                if idx >= clients.len() && !clients.is_empty() {
                    idx = 0;
                }
                empty_streak = 0;
            }
        } else {
            empty_streak = 0;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Periodic metrics
        if last_log.elapsed().as_secs() >= 10 {
            info!(
                "[srt] SINK_MODE: clients={} accepted={} discarded={}MB closed={}",
                clients.len(),
                accepted,
                discarded / (1024 * 1024),
                closed,
            );
            last_log = std::time::Instant::now();
        }
    }
}

impl SrtServer {
    pub async fn run(self: Arc<Self>, port: u16) {
        // SINK_MODE: minimal discard listener — no pipeline, no auth, no ring.
        // Accepts every connection, reads and discards data at application level.
        if self.engine.config.sink_mode {
            info!(
                "[srt] SINK_MODE: starting discard listener on port {}",
                port
            );
            let server_sock = unsafe { srt_create_socket() };
            if server_sock < 0 {
                return;
            }
            let addr_str = format!("0.0.0.0:{}", port);
            let addr: SocketAddr = match addr_str.parse() {
                Ok(a) => a,
                Err(_) => {
                    unsafe {
                        srt_close(server_sock);
                    }
                    return;
                }
            };
            let sin = to_sockaddr_in(addr);
            unsafe {
                let live: c_int = SRTT_LIVE;
                srt_setsockopt(
                    server_sock,
                    0,
                    SRTO_TRANSTYPE,
                    &live as *const _ as *const c_void,
                    std::mem::size_of::<c_int>() as c_int,
                );
                // Sink mode is a receive-dominant socket at scale (up to
                // 1,200 simultaneous accepted connections in the msr
                // harness), the same shape as the production ingest
                // listener just below in this file — apply the same
                // high-bitrate UDP/SRT buffer preset it already uses
                // instead of leaving libsrt's small defaults in place.
                // Confirmed via isolated libsrt benchmarking that undersized
                // receive buffers are what turns connection fan-in above a
                // few hundred concurrent flows into steady-state send
                // errors on the peer.
                srt_set_highbitrate_opts(server_sock, self.engine.config.srt_udp_buffer as i32);
                let lat: c_int = 250;
                srt_setsockopt(
                    server_sock,
                    0,
                    SRTO_LATENCY,
                    &lat as *const _ as *const c_void,
                    std::mem::size_of::<c_int>() as c_int,
                );
                let reuse: c_int = 1;
                srt_setsockopt(
                    server_sock,
                    0,
                    SRTO_REUSEADDR,
                    &reuse as *const _ as *const c_void,
                    std::mem::size_of::<c_int>() as c_int,
                );
                // Non-blocking accept so the discard loop can service
                // existing clients and print metrics between accepts.
                let sync: c_int = 0;
                srt_setsockopt(
                    server_sock,
                    0,
                    SRTO_RCVSYN,
                    &sync as *const _ as *const c_void,
                    std::mem::size_of::<c_int>() as c_int,
                );
                srt_bind(
                    server_sock,
                    &sin,
                    std::mem::size_of::<sockaddr_in>() as c_int,
                );
                srt_listen(server_sock, 1024);
            }
            info!("[srt] SINK_MODE: listening on {}", addr_str);
            // Spawn the blocking sink loop on tokio's blocking thread pool
            // so we don't occupy a tokio worker. Inside, one tight loop
            // accepts and round-robins non-blocking reads — no per-connection
            // threads, no epoll, no spawn overhead.
            tokio::task::spawn_blocking(move || {
                if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sink_discard_loop(server_sock);
                })) {
                    let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    error!("[srt] SINK_MODE: discard loop panicked: {}", msg);
                }
            });
            // Keep the async function alive until the server shuts down.
            // Without this, the outer `run` returns immediately and the
            // SRT listener task exits — which is treated as a critical
            // failure and triggers server shutdown.
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        }

        // SAFETY: srt_create_socket returns a valid SRT socket handle or -1
        // on error. The socket is closed via SrtListenerCloser on drop or
        // explicitly on bind/listen failure below.
        let server_sock = unsafe { srt_create_socket() };
        if server_sock < 0 {
            error!("Failed to create socket");
            return;
        }

        // SAFETY: Sets SRTT_LIVE transmission type on a valid listener
        // socket. The option value is a stack-allocated c_int.
        unsafe {
            let live_mode: c_int = SRTT_LIVE;
            srt_setsockopt(
                server_sock,
                0,
                SRTO_TRANSTYPE,
                &live_mode as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            );
        }
        let listener_store_ptr = Arc::as_ptr(&self.ingest_policy_store) as *mut c_void;
        // SAFETY: server_sock is live and listener_store_ptr remains valid
        // because self owns the Arc for the entire run future.
        let callback_res = unsafe {
            srt_listen_callback(
                server_sock,
                Some(srt_listener_policy_callback),
                listener_store_ptr,
            )
        };
        if callback_res < 0 {
            error!("[srt] failed to install listener policy callback");
            // SAFETY: no closer exists yet, so this branch exclusively owns
            // the live server_sock handle.
            unsafe {
                srt_close(server_sock);
            }
            return;
        }
        let default_resolved_policy = self
            .ingest_policy_store
            .global_config()
            .resolve()
            .unwrap_or(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Plaintext,
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            });
        if let Some(crypto) = srt_crypto_from_resolved(default_resolved_policy.crypto) {
            info!(
                "[srt] default listener ingest encryption enabled (pbkeylen={})",
                crypto.pbkeylen
            );
        }
        info!(
            "[srt] default listener ingest latency: {}ms (per-pipeline override via SrtPipelineIngestConfig::latency_ms)",
            default_resolved_policy.latency_ms
        );
        match enable_srt_group_connect(server_sock) {
            Ok(()) => {
                self.engine
                    .runtime
                    .listener_stats
                    .bonding_available
                    .store(true, Ordering::Relaxed);
                info!("Bonded ingest enabled on the shared listener (SRTO_GROUPCONNECT)",)
            }
            Err(error) => {
                self.engine
                    .runtime
                    .listener_stats
                    .bonding_available
                    .store(false, Ordering::Relaxed);
                error!(
                    "[srt] WARNING: bonded ingest is unavailable: linked libsrt rejected \
                 SRTO_GROUPCONNECT ({error}). Install/build libsrt with ENABLE_BONDING=ON. \
                 Single-link SRT ingest remains available."
                )
            }
        }
        srt_set_highbitrate_opts(server_sock, self.engine.config.srt_udp_buffer as i32);
        let listener_udp_recv_capacity = srt_log_effective_opts(
            server_sock,
            "listener",
            self.engine.config.srt_udp_buffer as i32,
        );

        let addr_str = format!("0.0.0.0:{}", port);
        let addr = match addr_str.parse::<SocketAddr>() {
            Ok(addr) => addr,
            Err(error) => {
                error!("Invalid address: {:?}", error);
                return;
            }
        };

        let sin = to_sockaddr_in(addr);
        // SAFETY: srt_bind receives a live listener and a correctly sized
        // stack sockaddr. Failure leaves ownership with this function.
        let bind_res = unsafe {
            srt_bind(
                server_sock,
                &sin,
                std::mem::size_of::<sockaddr_in>() as c_int,
            )
        };
        if bind_res < 0 {
            error!("Bind failed");
            // SAFETY: no closer exists yet, so this branch exclusively owns
            // the live server_sock handle.
            unsafe {
                srt_close(server_sock);
            }
            return;
        }

        // SAFETY: server_sock is successfully bound and remains owned here.
        let listen_res = unsafe { srt_listen(server_sock, 1024) };
        if listen_res < 0 {
            error!("Listen failed");
            unsafe {
                srt_close(server_sock);
            }
            return;
        }

        info!("Server listening on srt://{}", addr_str);

        let listener_stats = self.engine.listener_stats_handle();
        tokio::spawn(async move {
            monitor_listener_socket(port, listener_stats, listener_udp_recv_capacity).await;
        });

        let (tx, mut rx) = tokio::sync::mpsc::channel::<(SRTSOCKET, sockaddr_in)>(1024);

        let listener_closer = Arc::new(SrtListenerCloser::new(server_sock));
        let accept_shutdown = Arc::new(AtomicBool::new(false));
        let accept_shutdown_hook = accept_shutdown.clone();
        self.engine.register_listener_shutdown(move || {
            accept_shutdown_hook.store(true, Ordering::Release)
        });
        let listener_shutdown = listener_closer.clone();
        self.engine
            .register_listener_shutdown(move || listener_shutdown.close_once());

        let accept_handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    if accept_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let mut client_sin = sockaddr_in {
                        sin_family: 0,
                        sin_port: 0,
                        sin_addr: 0,
                        sin_zero: [0; 8],
                    };
                    let mut len = std::mem::size_of::<sockaddr_in>() as c_int;
                    // SAFETY: srt_accept runs on a dedicated OS thread.
                    // server_sock remains live until the registered closer
                    // sets accept_shutdown and closes the listener.
                    let client_sock = unsafe { srt_accept(server_sock, &mut client_sin, &mut len) };
                    if client_sock < 0 {
                        if accept_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        // SAFETY: libsrt returns a thread-local NUL-terminated
                        // error string valid until this thread's next SRT call.
                        let err = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) };
                        warn!("accept error: {}", err.to_string_lossy());
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    if tx.blocking_send((client_sock, client_sin)).is_err() {
                        // SAFETY: channel closure transfers no ownership of
                        // client_sock, so the accepting thread closes it.
                        unsafe {
                            srt_close(client_sock);
                        }
                        break;
                    }
                }
            }));
            if result.is_err() {
                error!("Accept thread panicked — ingest listener is down");
            }
        });
        self.engine.register_os_thread(accept_handle);

        while let Some((client_sock, client_addr)) = rx.recv().await {
            let self_clone = self.clone();
            tokio::spawn(async move {
                self_clone
                    .handle_client(client_sock, from_sockaddr_in(client_addr))
                    .await;
            });
        }
    }
}
