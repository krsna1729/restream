use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::AsRawFd;

use shiguredo_srt::Timestamp;
use srt_transport::OutputDrainBudget;
use tokio::io::Interest;
use tokio::net::UdpSocket;

use super::{DESIRED_UDP_BUF, SHARED_IO_BATCH_CAPACITY};

pub(crate) struct SharedSrtEgress {
    pub(crate) socket: UdpSocket,
    pub(crate) callers: srt_transport::CallerTable,
    pub(crate) outbound: Vec<(SocketAddr, Vec<u8>)>,
    pub(crate) outbound_cursor: usize,
}

impl SharedSrtEgress {
    #[cfg(test)]
    pub(crate) fn local_port(&self) -> Option<u16> {
        self.socket.local_addr().ok().map(|address| address.port())
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
        srt_transport::set_sock_bufs(socket.as_raw_fd(), DESIRED_UDP_BUF)
            .map_err(|error| error.to_string())?;
        let socket = UdpSocket::from_std(socket).map_err(|error| error.to_string())?;
        runtime
            .block_on(socket.writable())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            socket,
            callers: srt_transport::CallerTable::new(),
            outbound: Vec::new(),
            outbound_cursor: 0,
        })
    }

    pub(crate) fn drive(&mut self, now: Timestamp) -> Result<(), String> {
        let mut buffer = [0_u8; 2048];
        loop {
            match self.socket.try_recv_from(&mut buffer) {
                Ok((size, peer)) => self
                    .callers
                    .feed(peer, &buffer[..size], now)
                    .map_err(|error| error.to_string())?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.to_string()),
            };
        }
        if !self.flush_outbound()? {
            return Ok(());
        }
        self.callers
            .poll_outbound_bounded(now, OutputDrainBudget::default(), &mut self.outbound);
        self.outbound_cursor = 0;
        let _ = self.flush_outbound()?;
        Ok(())
    }

    pub(crate) fn flush_outbound(&mut self) -> Result<bool, String> {
        while let Some((peer, packet)) = self.outbound.get(self.outbound_cursor) {
            if peer.is_ipv4() {
                let empty = (
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
                    &[][..],
                );
                let mut batch = [empty; SHARED_IO_BATCH_CAPACITY];
                let mut count = 0;
                for (slot, (peer, packet)) in batch.iter_mut().zip(
                    self.outbound[self.outbound_cursor..]
                        .iter()
                        .take(SHARED_IO_BATCH_CAPACITY)
                        .take_while(|(peer, _)| peer.is_ipv4()),
                ) {
                    *slot = (*peer, packet.as_slice());
                    count += 1;
                }
                let sent = match self.socket.try_io(Interest::WRITABLE, || {
                    let sent =
                        srt_transport::sendmsg_batch(self.socket.as_raw_fd(), &batch[..count])?;
                    if sent == 0 {
                        Err(std::io::ErrorKind::WouldBlock.into())
                    } else {
                        Ok(sent)
                    }
                }) {
                    Ok(sent) => sent,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(false);
                    }
                    Err(error) => return Err(error.to_string()),
                };
                self.outbound_cursor += sent;
                if sent < count {
                    return Ok(false);
                }
                continue;
            }
            match self.socket.try_send_to(packet, *peer) {
                Ok(sent) if sent == packet.len() => self.outbound_cursor += 1,
                Ok(sent) => {
                    return Err(format!(
                        "short UDP datagram send: wrote {sent} of {} bytes",
                        packet.len()
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error.to_string()),
            }
        }
        self.outbound.clear();
        self.outbound_cursor = 0;
        Ok(true)
    }
}
