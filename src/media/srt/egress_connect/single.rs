use std::net::SocketAddr;
use std::os::raw::{c_int, c_void};

use super::{
    SrtEgressMuxerPortClaim, bind_srt_egress_muxer_port, connected_srt_local_port,
    set_srt_reuseaddr,
};
use crate::media::srt::buffer_sizing::EgressBufferOpts;
use crate::media::srt::socket::{
    EGRESS_UDP_RCVBUF, last_srt_error, srt_set_connect_timeout, srt_set_egress_opts, to_sockaddr_in,
};
use crate::media::srt::srt_crypto::{SrtCryptoConfig, apply_srt_crypto_socket};
use crate::media::srt::sys::{SRTSOCKET, sockaddr_in, srt_close, srt_connect, srt_setsockopt};
use crate::media::srt::{
    SrtEgressSendMode, SrtEgressSocketError, apply_srt_egress_stream_id,
    configure_connected_srt_egress_socket, srt_log_effective_opts,
};
use tracing::{info, warn};

pub(in crate::media::srt) struct SrtSingleEgressConnectConfig<'a> {
    pub(in crate::media::srt) peer_addr: SocketAddr,
    pub(in crate::media::srt) stream_id: &'a str,
    pub(in crate::media::srt) crypto: Option<&'a SrtCryptoConfig>,
    pub(in crate::media::srt) connect_timeout_ms: u64,
    pub(in crate::media::srt) send_mode: SrtEgressSendMode,
    pub(in crate::media::srt) muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
    /// Resolved SRT socket options for this destination — see
    /// `EgressBufferOpts` in `buffer_sizing.rs` for how callers derive this
    /// (formula/constant defaults, with any explicit URL overrides applied).
    pub(in crate::media::srt) buffer_opts: EgressBufferOpts,
}

pub(in crate::media::srt) fn connect_single_srt_egress_socket(
    config: SrtSingleEgressConnectConfig<'_>,
) -> Result<SRTSOCKET, String> {
    connect_single_srt_egress_socket_with(config, LibSrtSingleConnectOps)
}

fn connect_single_srt_egress_socket_with<O>(
    config: SrtSingleEgressConnectConfig<'_>,
    mut ops: O,
) -> Result<SRTSOCKET, String>
where
    O: SrtSingleConnectOps,
{
    let socket = ops.create_socket()?;
    ops.set_connect_timeout(socket, config.connect_timeout_ms);
    ops.set_egress_opts(socket, &config.buffer_opts);

    if let Err(error) = ops.set_reuseaddr(socket) {
        ops.close(socket);
        return Err(error);
    }
    if let Some(crypto) = config.crypto
        && let Err(error) = ops.apply_crypto(socket, crypto)
    {
        ops.close(socket);
        return Err(error);
    }
    if let Err(error) = ops.apply_stream_id(socket, config.stream_id) {
        ops.close(socket);
        return Err(error);
    }

    let muxer_port_claim = config.muxer_port_claim;
    if let Some(claim) = muxer_port_claim.as_ref()
        && let Some(port) = claim.bind_port()
        && let Err(error) = ops.bind_muxer_port(socket, port)
    {
        // The shard's learned muxer port is no longer bindable — libsrt
        // released it when the last socket on that multiplexer closed and
        // something else has since taken it. Forget the recording so the
        // retry autoselects a fresh port instead of this shard wedging on a
        // port it no longer owns. This attempt still fails: a failed
        // `srt_bind` leaves the socket half-opened, so it is closed and the
        // caller's normal reconnect path starts over on a clean one.
        warn!(
            port,
            err = %error,
            "[srt-egress] reusable local UDP muxer port no longer bindable; retry will autoselect"
        );
        claim.forget_stale_port();
        ops.close(socket);
        return Err(error);
    }

    if matches!(config.send_mode, SrtEgressSendMode::FabricNonblocking)
        && let Err(error) = ops.set_nonblocking_connect(socket)
    {
        ops.close(socket);
        return Err(error);
    }

    if let Err(error) = ops.connect(socket, config.peer_addr) {
        ops.close(socket);
        return Err(error);
    }
    if let Err(error) = ops.configure_connected_socket(socket, config.send_mode) {
        ops.close(socket);
        return Err(error.to_string());
    }

    if let Some(claim) = muxer_port_claim {
        match ops.connected_local_port(socket) {
            Ok(port) => {
                if claim.record_first_connected_port(port) {
                    info!(
                        port,
                        "[srt-egress] Reusing local UDP muxer port for compatible egress sockets"
                    );
                }
            }
            Err(error) => {
                warn!(err = %error, "[srt-egress] connected without recording reusable muxer port")
            }
        }
    }

    ops.log_effective_opts(socket);
    Ok(socket)
}

trait SrtSingleConnectOps {
    fn create_socket(&mut self) -> Result<SRTSOCKET, String>;
    fn close(&mut self, socket: SRTSOCKET);
    fn set_connect_timeout(&mut self, socket: SRTSOCKET, timeout_ms: u64);
    fn set_egress_opts(&mut self, socket: SRTSOCKET, opts: &EgressBufferOpts);
    fn set_reuseaddr(&mut self, socket: SRTSOCKET) -> Result<(), String>;
    fn apply_crypto(&mut self, socket: SRTSOCKET, crypto: &SrtCryptoConfig) -> Result<(), String>;
    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String>;
    fn bind_muxer_port(&mut self, socket: SRTSOCKET, port: u16) -> Result<(), String>;
    fn set_nonblocking_connect(&mut self, socket: SRTSOCKET) -> Result<(), String>;
    fn connect(&mut self, socket: SRTSOCKET, peer_addr: SocketAddr) -> Result<(), String>;
    fn configure_connected_socket(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError>;
    fn connected_local_port(&mut self, socket: SRTSOCKET) -> Result<u16, String>;
    fn log_effective_opts(&mut self, socket: SRTSOCKET);
}

struct LibSrtSingleConnectOps;

impl SrtSingleConnectOps for LibSrtSingleConnectOps {
    fn create_socket(&mut self) -> Result<SRTSOCKET, String> {
        // SAFETY: Category 8 - FFI boundary. libsrt returns either a valid
        // socket handle or a negative sentinel; the sentinel is checked before
        // the handle is used.
        let socket = unsafe { crate::media::srt::sys::srt_create_socket() };
        if socket < 0 {
            Err("failed to create socket".to_string())
        } else {
            Ok(socket)
        }
    }

    fn close(&mut self, socket: SRTSOCKET) {
        // SAFETY: Category 8 - FFI boundary. The helper calls this only for a
        // socket handle returned by create_socket that has not been returned to
        // the caller.
        unsafe {
            srt_close(socket);
        }
    }

    fn set_connect_timeout(&mut self, socket: SRTSOCKET, timeout_ms: u64) {
        srt_set_connect_timeout(socket, timeout_ms);
    }

    fn set_egress_opts(&mut self, socket: SRTSOCKET, opts: &EgressBufferOpts) {
        srt_set_egress_opts(socket, opts);
    }

    fn set_reuseaddr(&mut self, socket: SRTSOCKET) -> Result<(), String> {
        set_srt_reuseaddr(socket)
    }

    fn apply_crypto(&mut self, socket: SRTSOCKET, crypto: &SrtCryptoConfig) -> Result<(), String> {
        apply_srt_crypto_socket(socket, crypto)
    }

    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String> {
        apply_srt_egress_stream_id(socket, stream_id).map_err(|error| error.to_string())
    }

    fn bind_muxer_port(&mut self, socket: SRTSOCKET, port: u16) -> Result<(), String> {
        bind_srt_egress_muxer_port(socket, port)
    }

    fn set_nonblocking_connect(&mut self, socket: SRTSOCKET) -> Result<(), String> {
        // Fabric egress must never block the shard thread in `srt_connect`:
        // the default SRTO_RCVSYN=1 makes the connect synchronous for up to
        // the connect timeout (10s), freezing every leaf visit and the stall
        // sweep on the shard while one pending connect is in flight (see the
        // 1001/1002 connect failures and 0-sent/3M-dropped leaves at scale).
        // With RCVSYN=0 the connect returns immediately and the handshake
        // completes asynchronously under the same connect-timeout bound.
        // SAFETY: Category 8 - FFI boundary. `socket` is a live SRT socket;
        // the zero value is a correctly-sized c_int for SRTO_RCVSYN.
        let zero: c_int = 0;
        let result = unsafe {
            srt_setsockopt(
                socket,
                0,
                crate::media::srt::sys::SRTO_RCVSYN,
                &zero as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            )
        };
        if result < 0 {
            let (code, message) = last_srt_error();
            Err(format!(
                "failed to set non-blocking connect: {message} ({code})"
            ))
        } else {
            Ok(())
        }
    }

    fn connect(&mut self, socket: SRTSOCKET, peer_addr: SocketAddr) -> Result<(), String> {
        let sin = to_sockaddr_in(peer_addr);
        // SAFETY: Category 8 - FFI boundary. `socket` is a live SRT socket,
        // and `sin` is a correctly-sized sockaddr_in for libsrt connect.
        let result =
            unsafe { srt_connect(socket, &sin, std::mem::size_of::<sockaddr_in>() as c_int) };
        if result < 0 {
            let (code, message) = last_srt_error();
            // SAFETY: Category 8 - FFI boundary. `socket` is still a valid
            // SRT socket handle after a failed `srt_connect`; the reject
            // reason is a read-only integer diagnostic.
            let reject = unsafe { crate::media::srt::sys::srt_getrejectreason(socket) };
            if reject > 0 {
                Err(format!(
                    "connection failed: {message} ({code}) reject_reason={reject}"
                ))
            } else {
                Err(format!("connection failed: {message} ({code})"))
            }
        } else {
            Ok(())
        }
    }

    fn configure_connected_socket(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        configure_connected_srt_egress_socket(socket, mode)
    }

    fn connected_local_port(&mut self, socket: SRTSOCKET) -> Result<u16, String> {
        connected_srt_local_port(socket)
    }

    fn log_effective_opts(&mut self, socket: SRTSOCKET) {
        srt_log_effective_opts(socket, "egress", EGRESS_UDP_RCVBUF);
    }
}

#[cfg(test)]
#[path = "single_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "single_tests.rs"]
mod tests;
