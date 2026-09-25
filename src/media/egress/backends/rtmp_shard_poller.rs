use std::net::SocketAddr;
use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::media::egress::scheduler::LeafKey;

use super::tcp::{TcpConnectAttempt, TcpEgressPollError, TcpReadyLeaf};

pub(crate) trait RtmpReadinessPoller {
    fn ready_capacity(&self) -> usize;

    fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        key: LeafKey,
        generation: u64,
        timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError>;

    /// Hand an established connection to completion-driven I/O.
    fn register_connection(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        stream: &super::compio_tcp::CompioTcpStream,
    ) -> Result<(), TcpEgressPollError>;

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError>;

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError>;

    /// Park the shard inside the poller's runtime until a command,
    /// completion or timeout; the poller owns the only wait that drives I/O.
    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<crate::media::egress::command::EgressCommand>,
        max_wait: Duration,
    ) -> Option<crate::media::egress::shard::EgressShardIdleWake>;
}

impl RtmpReadinessPoller for super::compio_tcp::CompioTcpPoller {
    fn ready_capacity(&self) -> usize {
        self.ready_capacity()
    }

    fn start_connect(
        &mut self,
        peer_addr: SocketAddr,
        key: LeafKey,
        generation: u64,
        timeout: Duration,
    ) -> Result<TcpConnectAttempt, TcpEgressPollError> {
        self.start_connect(peer_addr, key, generation, timeout)
    }

    fn register_connection(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        stream: &super::compio_tcp::CompioTcpStream,
    ) -> Result<(), TcpEgressPollError> {
        super::compio_tcp::CompioTcpPoller::register_connection(self, fd, key, generation, stream)
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

    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<crate::media::egress::command::EgressCommand>,
        max_wait: Duration,
    ) -> Option<crate::media::egress::shard::EgressShardIdleWake> {
        Some(self.wait_idle(commands, max_wait))
    }
}
