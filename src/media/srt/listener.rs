use std::net::SocketAddr;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{error, info, warn};

use super::buffer_sizing::srt_set_ingest_latency_opts;
use super::socket::{
    DESIRED_UDP_BUF, SrtListenerCloser, enable_srt_group_connect, from_sockaddr_in,
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

impl SrtServer {
    pub async fn run(self: Arc<Self>, port: u16) {
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
        srt_set_highbitrate_opts(server_sock);
        let listener_udp_recv_capacity =
            srt_log_effective_opts(server_sock, "listener", DESIRED_UDP_BUF);

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
