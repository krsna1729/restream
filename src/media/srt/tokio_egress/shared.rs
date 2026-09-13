use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

use restream_dataplane::udp::{UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpPoller};
use shiguredo_srt::Timestamp;
use srt_transport::{DatagramSink, OutputDrainBudget, RecvBatch};

use super::{desired_udp_buf, recv_budget};

pub(crate) struct SharedSrtEgress {
    pub(crate) socket: UdpSocket,
    pub(crate) callers: srt_transport::CallerTable,
    pub(crate) outbound: VecDeque<(SocketAddr, Vec<u8>)>,
    free_outbound: Vec<Vec<u8>>,
    recv_batch: RecvBatch,
    poller: UringUdpPoller,
    send_completions: [UdpSendCompletion; 1],
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

    pub(crate) fn bind(peer: SocketAddr) -> Result<Self, String> {
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
        let mut poller = UringUdpPoller::new_fixed(1, 32).map_err(|error| error.to_string())?;
        poller
            .register_fixed(socket.as_raw_fd(), 0, 1, UdpInterest::READ_WRITE)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            socket,
            callers: srt_transport::CallerTable::new(),
            outbound: VecDeque::with_capacity(256),
            free_outbound: (0..256).map(|_| Vec::with_capacity(64 * 1024)).collect(),
            recv_batch: RecvBatch::new(),
            poller,
            send_completions: [UdpSendCompletion {
                slot: 0,
                generation: 0,
                result: 0,
            }],
            #[cfg(test)]
            drive_calls: 0,
        })
    }

    pub(crate) fn drive(&mut self, now: Timestamp) -> Result<(), String> {
        #[cfg(test)]
        {
            self.drive_calls = self.drive_calls.saturating_add(1);
        }
        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }];
        let ready_count = self
            .poller
            .poll(Duration::ZERO, &mut ready)
            .map_err(|error| error.to_string())?;
        let readable = ready[..ready_count]
            .iter()
            .any(|event| event.readable && event.slot == 0 && event.generation == 1);
        let mut feed_error = None;
        if readable {
            let budget = recv_budget();
            for _ in 0..budget.max_rounds {
                let received = self
                    .recv_batch
                    .recv(self.socket.as_raw_fd())
                    .map_err(|error| error.to_string())?;
                for (addr, data) in self.recv_batch.iter(received) {
                    let Some(peer) = addr else { continue };
                    if let Err(error) = self.callers.feed(peer, data, now) {
                        feed_error.get_or_insert(error);
                    }
                }
                if received < self.recv_batch.capacity() {
                    break;
                }
            }
        }
        if let Some(error) = feed_error {
            return Err(error.to_string());
        }
        if ready_count != 0 {
            self.poller
                .register(self.socket.as_raw_fd(), 0, 1, UdpInterest::READ_WRITE)
                .map_err(|error| error.to_string())?;
        }
        if !self.flush_outbound()? {
            return Ok(());
        }
        let budget = OutputDrainBudget::default();
        let mut sink = SharedTxSink {
            outbound: &mut self.outbound,
            free: &mut self.free_outbound,
            leased: None,
        };
        self.callers.poll_outbound_into(now, budget, &mut sink);
        let _ = self.flush_outbound()?;
        Ok(())
    }

    pub(crate) fn flush_outbound(&mut self) -> Result<bool, String> {
        let completed = self
            .poller
            .drain_send_completions(&mut self.send_completions);
        if completed != 0 {
            let completion = self.send_completions[0];
            if completion.result < 0 {
                let error = std::io::Error::from_raw_os_error(-completion.result);
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(error.to_string());
                }
            } else if self
                .outbound
                .front()
                .is_some_and(|(_, packet)| completion.result as usize == packet.len())
            {
                self.recycle_front(1);
            } else {
                return Err(format!(
                    "short UDP datagram send: wrote {} bytes",
                    completion.result
                ));
            }
        }
        let Some((peer, packet)) = self.outbound.front() else {
            return Ok(true);
        };
        match self
            .poller
            .submit_send(self.socket.as_raw_fd(), 0, 1, *peer, packet)
        {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }

    fn recycle_front(&mut self, count: usize) {
        for _ in 0..count {
            let Some((_, packet)) = self.outbound.pop_front() else {
                break;
            };
            if self.free_outbound.len() < 256 {
                self.free_outbound.push(packet);
            }
        }
    }
}

struct SharedTxSink<'a> {
    outbound: &'a mut VecDeque<(SocketAddr, Vec<u8>)>,
    free: &'a mut Vec<Vec<u8>>,
    leased: Option<Vec<u8>>,
}

impl DatagramSink for SharedTxSink<'_> {
    fn acquire(&mut self, max_len: usize) -> Option<&mut [std::mem::MaybeUninit<u8>]> {
        if self.leased.is_some() {
            return None;
        }
        let mut storage = self.free.pop()?;
        if max_len > storage.capacity() {
            self.free.push(storage);
            return None;
        }
        storage.clear();
        self.leased = Some(storage);
        Some(
            self.leased
                .as_mut()
                .map(Vec::spare_capacity_mut)
                .expect("leased TX storage is present"),
        )
    }

    fn commit(&mut self, peer: SocketAddr, len: usize) -> bool {
        let Some(mut storage) = self.leased.take() else {
            return false;
        };
        if len > storage.capacity() {
            self.free.push(storage);
            return false;
        }
        // `acquire` exposes exactly this vector's spare capacity and the
        // default DatagramSink::send initializes every committed byte.
        // SAFETY: every byte in `0..len` was initialized by that default
        // implementation before it called commit.
        unsafe { storage.set_len(len) };
        self.outbound.push_back((peer, storage));
        true
    }
}
