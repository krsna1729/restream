use std::net::SocketAddr;
use std::os::raw::c_int;

use super::to_libc_sockaddr;
use crate::media::srt::buffer_sizing::EgressBufferOpts;
use crate::media::srt::socket::{EGRESS_UDP_RCVBUF, srt_set_egress_opts};
use crate::media::srt::srt_crypto::{SrtCryptoConfig, apply_srt_crypto_socket};
use crate::media::srt::sys::{
    SRT_GTYPE_BACKUP, SRTSOCKET, SrtGroupMemberConfig, srt_close, srt_connect_group,
    srt_create_group, srt_getlasterror_str, srt_prepare_endpoint,
};
use crate::media::srt::{
    SrtEgressSendMode, SrtEgressSocketError, apply_srt_egress_stream_id,
    configure_connected_srt_egress_socket, srt_log_effective_opts,
};

pub(in crate::media::srt) struct SrtBondedEgressConnectConfig<'a> {
    pub(in crate::media::srt) peer_addrs: &'a [SocketAddr],
    pub(in crate::media::srt) stream_id: &'a str,
    pub(in crate::media::srt) crypto: Option<&'a SrtCryptoConfig>,
    pub(in crate::media::srt) send_mode: SrtEgressSendMode,
    /// See `SrtSingleEgressConnectConfig::buffer_opts`.
    pub(in crate::media::srt) buffer_opts: EgressBufferOpts,
}

pub(in crate::media::srt) fn connect_bonded_srt_egress_socket(
    config: SrtBondedEgressConnectConfig<'_>,
) -> Result<SRTSOCKET, String> {
    connect_bonded_srt_egress_socket_with(config, LibSrtBondedConnectOps)
}

fn connect_bonded_srt_egress_socket_with<O>(
    config: SrtBondedEgressConnectConfig<'_>,
    mut ops: O,
) -> Result<SRTSOCKET, String>
where
    O: SrtBondedConnectOps,
{
    let socket = ops.create_group()?;

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

    let mut members: Vec<O::Member> = config
        .peer_addrs
        .iter()
        .enumerate()
        .map(|(index, &peer_addr)| ops.prepare_member(peer_addr, index == 0))
        .collect();

    if let Err(error) = ops.connect_group(socket, &mut members) {
        ops.close(socket);
        return Err(error);
    }
    if let Err(error) = ops.configure_connected_socket(socket, config.send_mode) {
        ops.close(socket);
        return Err(error.to_string());
    }

    ops.set_egress_opts(socket, &config.buffer_opts);
    ops.log_effective_opts(socket);
    Ok(socket)
}

trait SrtBondedConnectOps {
    type Member;

    fn create_group(&mut self) -> Result<SRTSOCKET, String>;
    fn close(&mut self, socket: SRTSOCKET);
    fn apply_crypto(&mut self, socket: SRTSOCKET, crypto: &SrtCryptoConfig) -> Result<(), String>;
    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String>;
    fn prepare_member(&mut self, peer_addr: SocketAddr, primary: bool) -> Self::Member;
    fn connect_group(
        &mut self,
        socket: SRTSOCKET,
        members: &mut [Self::Member],
    ) -> Result<(), String>;
    fn configure_connected_socket(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError>;
    fn set_egress_opts(&mut self, socket: SRTSOCKET, opts: &EgressBufferOpts);
    fn log_effective_opts(&mut self, socket: SRTSOCKET);
}

struct LibSrtBondedConnectOps;

impl SrtBondedConnectOps for LibSrtBondedConnectOps {
    type Member = SrtGroupMemberConfig;

    fn create_group(&mut self) -> Result<SRTSOCKET, String> {
        // SAFETY: Category 8 - FFI boundary. libsrt returns either a valid
        // group socket handle or a negative sentinel, and the sentinel is
        // checked before the handle is used.
        let socket = unsafe { srt_create_group(SRT_GTYPE_BACKUP) };
        if socket < 0 {
            tracing::error!("Failed to create bonding group");
            Err("failed to create bonding group".to_string())
        } else {
            Ok(socket)
        }
    }

    fn close(&mut self, socket: SRTSOCKET) {
        // SAFETY: Category 8 - FFI boundary. The helper calls this only for a
        // group socket handle returned by create_group that has not been
        // returned to the caller.
        unsafe {
            srt_close(socket);
        }
    }

    fn apply_crypto(&mut self, socket: SRTSOCKET, crypto: &SrtCryptoConfig) -> Result<(), String> {
        apply_srt_crypto_socket(socket, crypto)
    }

    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String> {
        apply_srt_egress_stream_id(socket, stream_id).map_err(|error| error.to_string())
    }

    fn prepare_member(&mut self, peer_addr: SocketAddr, primary: bool) -> Self::Member {
        let (peer_storage, addrlen) = to_libc_sockaddr(peer_addr);
        // SAFETY: Category 8 - FFI boundary. `peer_storage` is a correctly
        // initialized sockaddr_storage for `peer_addr`; `addrlen` matches the
        // concrete sockaddr variant and both values live for the duration of
        // this call.
        let mut member = unsafe {
            srt_prepare_endpoint(
                std::ptr::null(),
                &peer_storage as *const _ as *const libc::sockaddr,
                addrlen,
            )
        };
        member.weight = if primary { 1 } else { 0 };
        member
    }

    fn connect_group(
        &mut self,
        socket: SRTSOCKET,
        members: &mut [Self::Member],
    ) -> Result<(), String> {
        // SAFETY: Category 8 - FFI boundary. `socket` is a live SRT group
        // socket and `members` is a valid mutable slice of group-member
        // descriptors for the duration of the call.
        let result =
            unsafe { srt_connect_group(socket, members.as_mut_ptr(), members.len() as c_int) };
        if result >= 0 {
            return Ok(());
        }

        // SAFETY: Category 8 - FFI boundary. libsrt returns a thread-local
        // NUL-terminated error string pointer valid until the next SRT call on
        // this thread.
        let error = unsafe { std::ffi::CStr::from_ptr(srt_getlasterror_str()) };
        let message = error.to_string_lossy();
        tracing::error!("[srt-egress] Bonded connection failed: {}", message);
        Err(format!("bonded connection failed: {message}"))
    }

    fn configure_connected_socket(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        configure_connected_srt_egress_socket(socket, mode)
    }

    fn set_egress_opts(&mut self, socket: SRTSOCKET, opts: &EgressBufferOpts) {
        srt_set_egress_opts(socket, opts);
    }

    fn log_effective_opts(&mut self, socket: SRTSOCKET) {
        srt_log_effective_opts(socket, "egress-bonded", EGRESS_UDP_RCVBUF);
    }
}

#[cfg(test)]
#[path = "bonded_tests.rs"]
mod tests;
