//! A wildcard UDP socket for harness tests that need another process's
//! socket in a port's reuseport group.

use std::net::UdpSocket;
use std::os::fd::FromRawFd;
use std::time::Duration;

/// A wildcard UDP socket with `SO_REUSEPORT`, a large receive buffer and a
/// read timeout so the drain loop can observe the stop flag.
pub(crate) fn bind_drain_socket(
    port: u16,
    rcvbuf: usize,
    timeout: Duration,
    reuseport: bool,
) -> Result<UdpSocket, String> {
    // SAFETY: socket(2) takes no pointers; the result is checked below.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    // Own the fd from here on so every early return closes it.
    // SAFETY: `fd` is a fresh socket checked above and owned by nothing else.
    let socket = unsafe { UdpSocket::from_raw_fd(fd) };
    let enable: libc::c_int = 1;
    // SO_REUSEPORT only when explicitly asked for: per-socket reuseport spreads
    // by flow hash, and with a handful of destination flows it leaves threads
    // idle while one socket absorbs everything.
    let mut options = vec![(libc::SO_REUSEADDR, enable)];
    if reuseport {
        options.push((libc::SO_REUSEPORT, enable));
    }
    for (name, value) in options {
        // SAFETY: the value pointer is a live c_int local of the length passed; `fd` is an open
        // socket for the call.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                name,
                &value as *const _ as *const libc::c_void,
                std::mem::size_of_val(&value) as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(format!(
                "setsockopt({name}): {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    let rcvbuf = rcvbuf as libc::c_int;
    // SAFETY: the value pointer is a live c_int local of the length passed; `fd` is an open
    // socket for the call.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &rcvbuf as *const _ as *const libc::c_void,
            std::mem::size_of_val(&rcvbuf) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "setsockopt(SO_RCVBUF): {}",
            std::io::Error::last_os_error()
        ));
    }
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr { s_addr: 0 },
        sin_zero: [0; 8],
    };
    // SAFETY: `address` is a live sockaddr_in of the length passed; `fd` is owned by `socket`.
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&addr) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "bind(:{port}): {}",
            std::io::Error::last_os_error()
        ));
    }
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    Ok(socket)
}
