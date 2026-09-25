//! Transport-neutral TCP egress types shared by the RTMP/RTMPS fabric shard and
//! its Compio poller ([`super::compio_tcp::CompioTcpPoller`]): readiness
//! interest, ready-leaf events, poll errors, connect attempts and the
//! nonblocking-connect result check.

use crate::media::egress::scheduler::LeafKey;
use std::io;
use std::os::fd::RawFd;
use std::os::raw::c_int;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TcpEgressInterest {
    pub readable: bool,
    pub writable: bool,
}

impl TcpEgressInterest {
    /// Connect completion: a nonblocking connect finishes when writable.
    pub const WRITE: Self = Self {
        readable: false,
        writable: true,
    };
    #[cfg(test)]
    pub const READ_WRITE: Self = Self {
        readable: true,
        writable: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TcpReadyLeaf {
    pub fd: RawFd,
    pub key: LeafKey,
    pub generation: u64,
    pub readable: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TcpEgressPollError {
    pub operation: &'static str,
    pub code: c_int,
    pub message: String,
}

impl TcpEgressPollError {
    pub(crate) fn new(operation: &'static str, code: c_int, message: String) -> Self {
        Self {
            operation,
            code,
            message,
        }
    }
}

#[derive(Debug)]
pub(crate) enum TcpConnectAttempt {
    Connected(super::compio_tcp::CompioTcpStream),
    InProgress(super::compio_tcp::CompioTcpStream),
}

pub(crate) fn connect_error(fd: RawFd) -> io::Result<()> {
    let mut error = 0;
    let mut length = std::mem::size_of::<c_int>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&mut error as *mut c_int).cast(),
            &mut length,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}
