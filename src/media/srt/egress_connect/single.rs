use std::net::SocketAddr;
use std::os::raw::c_int;

use super::{
    SrtEgressMuxerPortClaim, bind_srt_egress_muxer_port, connected_srt_local_port,
    set_srt_reuseaddr,
};
use crate::media::srt::socket::{
    srt_set_connect_timeout, srt_set_highbitrate_opts, to_sockaddr_in,
};
use crate::media::srt::srt_crypto::{SrtCryptoConfig, apply_srt_crypto_socket};
use crate::media::srt::sys::{SRTSOCKET, sockaddr_in, srt_close, srt_connect};
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
    ops.set_highbitrate_opts(socket);

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
    if let Some(port) = muxer_port_claim
        .as_ref()
        .and_then(SrtEgressMuxerPortClaim::bind_port)
        && let Err(error) = ops.bind_muxer_port(socket, port)
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
    fn set_highbitrate_opts(&mut self, socket: SRTSOCKET);
    fn set_reuseaddr(&mut self, socket: SRTSOCKET) -> Result<(), String>;
    fn apply_crypto(&mut self, socket: SRTSOCKET, crypto: &SrtCryptoConfig) -> Result<(), String>;
    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String>;
    fn bind_muxer_port(&mut self, socket: SRTSOCKET, port: u16) -> Result<(), String>;
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

    fn set_highbitrate_opts(&mut self, socket: SRTSOCKET) {
        srt_set_highbitrate_opts(socket);
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

    fn connect(&mut self, socket: SRTSOCKET, peer_addr: SocketAddr) -> Result<(), String> {
        let sin = to_sockaddr_in(peer_addr);
        // SAFETY: Category 8 - FFI boundary. `socket` is a live SRT socket,
        // and `sin` is a correctly-sized sockaddr_in for libsrt connect.
        let result =
            unsafe { srt_connect(socket, &sin, std::mem::size_of::<sockaddr_in>() as c_int) };
        if result < 0 {
            Err("connection failed".to_string())
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
        srt_log_effective_opts(socket, "egress");
    }
}

#[cfg(test)]
#[path = "single_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "single_tests.rs"]
mod tests;
