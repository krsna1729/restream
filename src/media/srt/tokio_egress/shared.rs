use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

use restream_dataplane::udp::{UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpPoller};
use shiguredo_srt::Timestamp;
use srt_transport::{DatagramSink, LogicalCallerState, OutputDrainBudget, RecvBatch};

use super::{desired_udp_buf, recv_budget};

const SRT_UDP_SEND_CAPACITY: usize = 16;
type PendingDatagram = (SocketAddr, Vec<u8>);

pub(crate) struct SharedSrtEgress {
    pub(crate) socket: UdpSocket,
    pub(crate) callers: srt_transport::CallerTable,
    pub(crate) outbound: VecDeque<(SocketAddr, Vec<u8>)>,
    free_outbound: Vec<Vec<u8>>,
    recv_batch: RecvBatch,
    poller: UringUdpPoller,
    send_completions: Box<[UdpSendCompletion]>,
    inflight: Box<[Option<PendingDatagram>]>,
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
        let mut poller = UringUdpPoller::new_fixed_with_send_capacity(1, 32, SRT_UDP_SEND_CAPACITY)
            .map_err(|error| error.to_string())?;
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
            send_completions: vec![
                UdpSendCompletion {
                    slot: 0,
                    generation: 0,
                    result: 0,
                };
                SRT_UDP_SEND_CAPACITY
            ]
            .into_boxed_slice(),
            inflight: std::iter::repeat_with(|| None)
                .take(SRT_UDP_SEND_CAPACITY)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
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

    pub(crate) fn send_shared(
        &mut self,
        caller_id: srt_transport::LogicalCallerId,
        message: &shiguredo_srt::Bytes,
        now: Timestamp,
    ) -> Result<usize, shiguredo_srt::Error> {
        let Some(mut caller) = self.callers.logical_caller_mut(&caller_id) else {
            return Err(shiguredo_srt::Error::with_reason(
                shiguredo_srt::ErrorKind::InvalidState,
                "shared SRT caller no longer exists",
            ));
        };
        match caller.state() {
            Some(LogicalCallerState::Disconnected) | None => {
                Err(shiguredo_srt::Error::with_reason(
                    shiguredo_srt::ErrorKind::InvalidState,
                    "shared SRT caller is disconnected",
                ))
            }
            Some(LogicalCallerState::Connecting) => Ok(0),
            Some(LogicalCallerState::Connected) if !caller.can_send() => Ok(0),
            Some(LogicalCallerState::Connected) => {
                let mut sink = SharedTxSink {
                    outbound: &mut self.outbound,
                    free: &mut self.free_outbound,
                    leased: None,
                };
                caller.send_shared_into(message.clone(), now, &mut sink)
            }
        }
    }

    pub(crate) fn flush_outbound(&mut self) -> Result<bool, String> {
        let completed = self
            .poller
            .drain_send_completions(&mut self.send_completions);
        for completion_index in 0..completed {
            let completion = self.send_completions[completion_index];
            let slot = completion.slot as usize;
            let Some((peer, packet)) = self.inflight.get_mut(slot).and_then(Option::take) else {
                return Err(format!(
                    "UDP completion {} has no in-flight datagram",
                    completion.slot
                ));
            };
            if completion.result < 0 {
                let error = std::io::Error::from_raw_os_error(-completion.result);
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    self.outbound.push_front((peer, packet));
                } else {
                    return Err(error.to_string());
                }
            } else if completion.result as usize == packet.len() {
                self.recycle_packet(packet);
            } else {
                return Err(format!(
                    "short UDP datagram send: wrote {} bytes",
                    completion.result
                ));
            }
        }
        while let Some(operation_slot) = self.inflight.iter().position(Option::is_none) {
            let Some((peer, packet)) = self.outbound.pop_front() else {
                break;
            };
            match self.poller.submit_send_on_slot(
                self.socket.as_raw_fd(),
                0,
                operation_slot as u32,
                1,
                peer,
                &packet,
            ) {
                Ok(()) => self.inflight[operation_slot] = Some((peer, packet)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.outbound.push_front((peer, packet));
                    break;
                }
                Err(error) => {
                    self.outbound.push_front((peer, packet));
                    return Err(error.to_string());
                }
            }
        }
        Ok(self.outbound.is_empty() && self.inflight.iter().all(Option::is_none))
    }

    fn recycle_packet(&mut self, packet: Vec<u8>) {
        if self.free_outbound.len() < 256 {
            self.free_outbound.push(packet);
        }
    }
}

struct SharedTxSink<'a> {
    outbound: &'a mut VecDeque<(SocketAddr, Vec<u8>)>,
    free: &'a mut Vec<Vec<u8>>,
    leased: Option<Vec<u8>>,
}

impl DatagramSink for SharedTxSink<'_> {
    fn send_owned(&mut self, peer: SocketAddr, packet: Vec<u8>) -> Result<(), Vec<u8>> {
        if self.leased.is_some() || packet.len() > 64 * 1024 {
            return Err(packet);
        }
        let Some(token) = self.free.pop() else {
            return Err(packet);
        };
        drop(token);
        self.outbound.push_back((peer, packet));
        Ok(())
    }

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

    fn abort(&mut self) {
        if let Some(storage) = self.leased.take() {
            self.free.push(storage);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_datagrams_reuse_the_protocol_buffer_without_copying() {
        let peer = "127.0.0.1:9000".parse().unwrap();
        let mut outbound = VecDeque::new();
        let mut free = vec![Vec::with_capacity(64)];
        let packet = Vec::from([1_u8, 2, 3]);
        let pointer = packet.as_ptr();
        let mut sink = SharedTxSink {
            outbound: &mut outbound,
            free: &mut free,
            leased: None,
        };

        DatagramSink::send_owned(&mut sink, peer, packet).unwrap();

        assert_eq!(free.len(), 0);
        assert_eq!(
            outbound.front().map(|(_, packet)| packet.as_ptr()),
            Some(pointer)
        );
    }
}
