use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::AsRawFd;

use shiguredo_srt::Timestamp;
use srt_transport::{OutputDrainBudget, RecvBatch, apply_send_result};
use tokio::io::Interest;
use tokio::net::UdpSocket;

use super::{desired_udp_buf, recv_budget, shared_io_batch_capacity};

pub(crate) struct SharedSrtEgress {
    pub(crate) socket: UdpSocket,
    pub(crate) callers: srt_transport::CallerTable,
    pub(crate) outbound: Vec<(SocketAddr, Vec<u8>)>,
    recv_batch: RecvBatch,
    /// Times `drive` has run, so the readiness-path invariant in
    /// `drive_shared_srt_egress` (driving does not scale with the number of
    /// leaves sharing this state) is directly assertable instead of
    /// inferred. Test-only: production carries no counter.
    #[cfg(test)]
    drive_calls: u64,
}

impl SharedSrtEgress {
    #[cfg(test)]
    pub(crate) fn local_port(&self) -> Option<u16> {
        self.socket.local_addr().ok().map(|address| address.port())
    }

    #[cfg(test)]
    pub(crate) fn drive_calls(&self) -> u64 {
        self.drive_calls
    }

    pub(crate) fn bind(
        peer: SocketAddr,
        runtime: &tokio::runtime::Runtime,
    ) -> Result<Self, String> {
        let bind = match peer.ip() {
            IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };
        let socket = std::net::UdpSocket::bind(bind).map_err(|error| error.to_string())?;
        socket
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        srt_transport::set_sock_bufs(socket.as_raw_fd(), desired_udp_buf())
            .map_err(|error| error.to_string())?;
        let socket = UdpSocket::from_std(socket).map_err(|error| error.to_string())?;
        runtime
            .block_on(socket.writable())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            socket,
            callers: srt_transport::CallerTable::new(),
            outbound: Vec::new(),
            recv_batch: RecvBatch::new(),
            #[cfg(test)]
            drive_calls: 0,
        })
    }

    pub(crate) fn drive(&mut self, now: Timestamp) -> Result<(), String> {
        #[cfg(test)]
        {
            self.drive_calls = self.drive_calls.saturating_add(1);
        }
        let mut feed_error = None;
        {
            let socket = &self.socket;
            let recv_batch = &mut self.recv_batch;
            let callers = &mut self.callers;
            srt_transport::tokio_transport::drain_readable(
                socket,
                recv_batch,
                recv_budget(),
                |addr, data| {
                    let Some(peer) = addr else {
                        return;
                    };
                    if let Err(error) = callers.feed(peer, data, now) {
                        feed_error.get_or_insert(error);
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        }
        if let Some(error) = feed_error {
            return Err(error.to_string());
        }
        if !self.flush_outbound()? {
            return Ok(());
        }
        self.callers
            .poll_outbound_bounded(now, OutputDrainBudget::default(), &mut self.outbound);
        let _ = self.flush_outbound()?;
        Ok(())
    }

    pub(crate) fn flush_outbound(&mut self) -> Result<bool, String> {
        while let Some((peer, _)) = self.outbound.first() {
            if peer.is_ipv4() {
                let count = self
                    .outbound
                    .iter()
                    .take(shared_io_batch_capacity())
                    .take_while(|(peer, _)| peer.is_ipv4())
                    .count();
                let result =
                    self.socket
                        .try_io(Interest::WRITABLE, || {
                            match srt_transport::sendmsg_batch(
                                self.socket.as_raw_fd(),
                                &self.outbound[..count],
                            )? {
                                0 => Err(std::io::ErrorKind::WouldBlock.into()),
                                sent => Ok(sent),
                            }
                        });
                let report = apply_send_result(&mut self.outbound, result)
                    .map_err(|error| error.to_string())?;
                // `apply_send_result` compares against the whole queue. A full
                // prefix send of `count` with more packets still queued is not
                // kernel backpressure — keep offering the next IPv4 run.
                if report.sent < count {
                    return Ok(false);
                }
                continue;
            }
            let (peer, packet_len) = {
                let (peer, packet) = &self.outbound[0];
                (*peer, packet.len())
            };
            match self.socket.try_send_to(&self.outbound[0].1, peer) {
                Ok(sent) if sent == packet_len => {
                    self.outbound.remove(0);
                }
                Ok(sent) => {
                    return Err(format!(
                        "short UDP datagram send: wrote {sent} of {packet_len} bytes"
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(true)
    }
}
