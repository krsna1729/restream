use std::net::SocketAddr;

/// The IP family of one shared caller socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressFamily {
    V4,
    V6,
}

impl AddressFamily {
    pub const COUNT: usize = 2;

    pub fn of(addr: SocketAddr) -> Self {
        if addr.is_ipv6() { Self::V6 } else { Self::V4 }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::V4 => 0,
            Self::V6 => 1,
        }
    }
}
