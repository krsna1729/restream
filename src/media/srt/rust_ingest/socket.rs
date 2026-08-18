use std::io;
use std::net::UdpSocket;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(super) fn bind_reuseport(port: u16, udp_buffer: usize) -> io::Result<UdpSocket> {
    // SAFETY: The arguments are the platform's documented socket constants;
    // the returned descriptor is checked before it is wrapped in OwnedFd.
    let fd = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd was returned by socket and is owned exclusively here.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_socket_option(fd.as_raw_fd(), libc::SO_REUSEPORT, 1)?;
    if udp_buffer > 0 {
        let buffer = udp_buffer.min(i32::MAX as usize) as libc::c_int;
        set_socket_option(fd.as_raw_fd(), libc::SO_RCVBUF, buffer)?;
        set_socket_option(fd.as_raw_fd(), libc::SO_SNDBUF, buffer)?;
    }

    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr { s_addr: 0 },
        sin_zero: [0; 8],
    };
    // SAFETY: address is initialized, has the exact sockaddr_in layout, and
    // remains alive for the duration of bind.
    let result = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_in).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UdpSocket::from(fd))
}

fn set_socket_option(
    fd: std::os::fd::RawFd,
    option: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    // SAFETY: fd is owned by the caller, value is a valid c_int, and the
    // option length matches the pointed-to value.
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&value as *const libc::c_int).cast::<libc::c_void>(),
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    (result == 0)
        .then_some(())
        .ok_or_else(io::Error::last_os_error)
}
