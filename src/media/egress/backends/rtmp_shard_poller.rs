use std::net::SocketAddr;
use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::media::egress::scheduler::LeafKey;

use super::tcp::{TcpConnectAttempt, TcpEgressInterest, TcpEgressPollError, TcpReadyLeaf};

pub(crate) trait RtmpReadinessPoller {
    fn ready_capacity(&self) -> usize;

    fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        key: LeafKey,
        generation: u64,
        timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError>;

    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError>;

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError>;

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError>;
}

#[cfg(test)]
impl<O> RtmpReadinessPoller for super::tcp::TcpEgressPoller<O>
where
    O: super::tcp::TcpPollOps,
{
    fn ready_capacity(&self) -> usize {
        self.ready_capacity()
    }

    fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        _key: LeafKey,
        _generation: u64,
        timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError> {
        super::tcp_connect::connect_fabric_tcp_egress_socket(
            super::tcp_connect::TcpFabricConnectConfig {
                peer_addr,
                connect_timeout: timeout,
            },
        )
        .map(TcpConnectAttempt::Connected)
        .map_err(|error| TcpEgressPollError {
            operation: error.operation,
            code: error.source.raw_os_error().unwrap_or(libc::EIO),
            message: error.source.to_string(),
        })
    }

    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        self.register_leaf(fd, key, generation, interest)
    }

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        self.remove(fd)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}

impl RtmpReadinessPoller for super::tcp::IoUringTcpPoller {
    fn ready_capacity(&self) -> usize {
        self.ready_capacity()
    }

    fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        key: LeafKey,
        generation: u64,
        _timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError> {
        self.start_connect(peer_addr, key, generation)
    }

    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        self.register_leaf(fd, key, generation, interest)
    }

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        self.remove(fd)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}
