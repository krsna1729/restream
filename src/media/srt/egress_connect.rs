use std::net::SocketAddr;
use std::os::raw::{c_int, c_void};
use std::sync::{Mutex, MutexGuard};

use super::socket::{check_srt_option_result, last_srt_error, to_sockaddr_in};
use super::sys::{
    SRTO_REUSEADDR, SRTSOCKET, sockaddr_in, srt_bind, srt_getsockname, srt_setsockopt,
};

#[path = "egress_connect/bonded.rs"]
mod bonded;
#[path = "egress_connect/fabric.rs"]
mod fabric;
#[path = "egress_connect/prepare.rs"]
mod prepare;
#[path = "egress_connect/single.rs"]
mod single;

pub(in crate::media::srt) use bonded::{
    SrtBondedEgressConnectConfig, connect_bonded_srt_egress_socket,
};
pub(crate) use fabric::{SrtFabricEgressConnectConfig, connect_fabric_srt_egress_socket};
pub(crate) use prepare::SrtFabricEgressConnectSpec;
pub(in crate::media::srt) use single::{
    SrtSingleEgressConnectConfig, connect_single_srt_egress_socket,
};

#[cfg(test)]
pub(crate) async fn resolve_host(host_port: &str) -> Option<SocketAddr> {
    match host_port.parse::<SocketAddr>() {
        Ok(a) => Some(a),
        Err(_) => tokio::net::lookup_host(host_port)
            .await
            .ok()
            .and_then(|mut addrs| addrs.next()),
    }
}

pub(crate) fn to_libc_sockaddr(addr: SocketAddr) -> (libc::sockaddr_storage, c_int) {
    // SAFETY: Category 4 - uninitialized memory. `sockaddr_storage` permits an
    // all-zero bit pattern, and each address branch writes the concrete
    // sockaddr variant fields before the storage is passed to libsrt.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match addr {
        SocketAddr::V4(v4) => {
            let sin = &mut storage as *mut _ as *mut libc::sockaddr_in;
            // SAFETY: Category 8 - FFI layout boundary. `sin` points into the
            // zeroed `sockaddr_storage` with enough space for `sockaddr_in`;
            // AF_INET is written before the remaining IPv4 fields.
            unsafe {
                (*sin).sin_family = libc::AF_INET as libc::sa_family_t;
                (*sin).sin_port = v4.port().to_be();
                (*sin).sin_addr.s_addr = u32::from_ne_bytes(v4.ip().octets());
            }
            (storage, std::mem::size_of::<libc::sockaddr_in>() as c_int)
        }
        SocketAddr::V6(v6) => {
            let sin6 = &mut storage as *mut _ as *mut libc::sockaddr_in6;
            // SAFETY: Category 8 - FFI layout boundary. `sin6` points into the
            // zeroed `sockaddr_storage` with enough space for `sockaddr_in6`;
            // AF_INET6 is written before the remaining IPv6 fields.
            unsafe {
                (*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
                (*sin6).sin6_port = v6.port().to_be();
                (*sin6).sin6_addr.s6_addr = v6.ip().octets();
            }
            (storage, std::mem::size_of::<libc::sockaddr_in6>() as c_int)
        }
    }
}

pub(crate) fn set_srt_reuseaddr(sock: SRTSOCKET) -> Result<(), String> {
    let reuse: c_int = 1;
    // SAFETY: Category 8 - FFI boundary. `sock` is a live SRT socket and
    // `reuse` is a correctly-sized c_int option value whose pointer is valid
    // for the duration of the call.
    unsafe {
        check_srt_option_result(
            "SRTO_REUSEADDR",
            srt_setsockopt(
                sock,
                0,
                SRTO_REUSEADDR,
                &reuse as *const _ as *const c_void,
                std::mem::size_of::<c_int>() as c_int,
            ),
        )
    }
}

pub(crate) fn bind_srt_egress_muxer_port(sock: SRTSOCKET, port: u16) -> Result<(), String> {
    let sin = to_sockaddr_in(SocketAddr::from(([0, 0, 0, 0], port)));
    // SAFETY: Category 8 - FFI boundary. `sock` is a live SRT socket and `sin`
    // is a stack-allocated IPv4 sockaddr with the matching size argument.
    let result = unsafe { srt_bind(sock, &sin, std::mem::size_of::<sockaddr_in>() as c_int) };
    if result >= 0 {
        Ok(())
    } else {
        let (code, message) = last_srt_error();
        Err(format!(
            "failed to bind reusable SRT egress muxer port {port}: {message} ({code})"
        ))
    }
}

pub(crate) fn connected_srt_local_port(sock: SRTSOCKET) -> Result<u16, String> {
    // SAFETY: Category 4 - uninitialized memory. `sockaddr_in` permits an
    // all-zero bit pattern and libsrt writes the address before we read it.
    let mut sin: sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<sockaddr_in>() as c_int;
    // SAFETY: Category 8 - FFI boundary. `sock` is a live SRT socket; `sin`
    // and `len` are valid mutable pointers for the duration of the call.
    let result = unsafe { srt_getsockname(sock, &mut sin, &mut len) };
    if result >= 0 {
        Ok(u16::from_be(sin.sin_port))
    } else {
        let (code, message) = last_srt_error();
        Err(format!(
            "failed to read reusable SRT egress muxer port: {message} ({code})"
        ))
    }
}

pub(crate) enum SrtEgressMuxerPortClaim<'a> {
    First(MutexGuard<'a, Option<u16>>),
    /// A port learned by an earlier connect on this shard. The state is kept
    /// by reference (not as a held guard) so concurrent reusers never
    /// serialize on it, while `forget_stale_port` can still clear a
    /// recording that is no longer bindable.
    Reuse {
        state: &'a Mutex<Option<u16>>,
        port: u16,
    },
}

impl SrtEgressMuxerPortClaim<'_> {
    pub(crate) fn bind_port(&self) -> Option<u16> {
        match self {
            SrtEgressMuxerPortClaim::First(_) => None,
            SrtEgressMuxerPortClaim::Reuse { port, .. } => Some(*port),
        }
    }

    /// Drops a recorded port that could not be re-bound, so the next connect
    /// on this shard autoselects a fresh one.
    ///
    /// libsrt destroys a multiplexer once its last socket closes and releases
    /// the underlying UDP port with it; if the port is then taken by
    /// something else, every later connect that binds it fails. Without this,
    /// a shard whose outputs were all removed and re-added could wedge
    /// permanently on a port it no longer owns.
    pub(crate) fn forget_stale_port(&self) {
        let SrtEgressMuxerPortClaim::Reuse { state, port } = self else {
            return;
        };
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if *guard == Some(*port) {
            *guard = None;
        }
    }

    pub(crate) fn record_first_connected_port(self, port: u16) -> bool {
        match self {
            SrtEgressMuxerPortClaim::First(mut guard) => {
                if guard.is_none() {
                    *guard = Some(port);
                    true
                } else {
                    false
                }
            }
            SrtEgressMuxerPortClaim::Reuse { .. } => false,
        }
    }
}

pub(crate) fn claim_srt_egress_muxer_port(
    state: &Mutex<Option<u16>>,
) -> SrtEgressMuxerPortClaim<'_> {
    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(port) = *guard {
        drop(guard);
        SrtEgressMuxerPortClaim::Reuse { state, port }
    } else {
        SrtEgressMuxerPortClaim::First(guard)
    }
}
