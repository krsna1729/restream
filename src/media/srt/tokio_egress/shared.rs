use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

use restream_dataplane::udp::{UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpPoller};
use restream_dataplane::{TxLease, TxPool};
use shiguredo_srt::Timestamp;
use srt_transport::{DatagramSink, LogicalCallerState, OutputDrainBudget, RecvBatch};

use super::{desired_udp_buf, recv_budget};

const SRT_UDP_SEND_CAPACITY: usize = 16;
const MAX_OUTBOUND: usize = 256;
const TX_SLOT_SIZE: usize = 64 * 1024;
const FAMILY_COUNT: usize = 2;

#[derive(Debug, Clone, Copy)]
struct PendingDatagram {
    peer: SocketAddr,
    lease: TxLease,
    len: usize,
}

pub(super) fn family_index(peer: SocketAddr) -> usize {
    usize::from(peer.is_ipv6())
}

fn bind_family(is_ipv6: bool) -> Result<UdpFamily, String> {
    let bind = if is_ipv6 {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
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
    Ok(UdpFamily {
        socket,
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
        ready: UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        },
    })
}

struct UdpFamily {
    socket: UdpSocket,
    poller: UringUdpPoller,
    send_completions: Box<[UdpSendCompletion]>,
    inflight: Box<[Option<PendingDatagram>]>,
    ready: UdpReadyEvent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SrtNativeMetrics {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub sqes: u64,
    pub tx_pool_empty: u64,
    pub cqes: u64,
    pub stale_completions: u64,
    pub cq_overflows: u64,
}

pub(crate) struct SharedSrtEgress {
    families: [Option<UdpFamily>; FAMILY_COUNT],
    pub(crate) callers: srt_transport::CallerTable,
    outbound: [VecDeque<PendingDatagram>; FAMILY_COUNT],
    tx_pool: TxPool,
    recv_batch: RecvBatch,
    native_metrics: SrtNativeMetrics,
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
        self.families
            .iter()
            .flatten()
            .find_map(|family| family.socket.local_addr().ok())
            .map(|address| address.port())
    }

    #[cfg(test)]
    pub(crate) fn drive_calls(&self) -> u64 {
        self.drive_calls
    }

    #[cfg(test)]
    pub(crate) fn bind(peer: SocketAddr) -> Result<Self, String> {
        Self::bind_for_peers(std::slice::from_ref(&peer))
    }

    #[cfg(test)]
    pub(crate) fn enqueue_test_datagram(&mut self, peer: SocketAddr, packet: Vec<u8>) -> bool {
        let mut sink = SharedTxSink {
            outbound: &mut self.outbound,
            tx_pool: &mut self.tx_pool,
            pool_empty: &mut self.native_metrics.tx_pool_empty,
            leased: None,
        };
        sink.send_owned(peer, packet).is_ok()
    }

    #[cfg(test)]
    pub(crate) fn outbound_empty(&self) -> bool {
        self.outbound.iter().all(VecDeque::is_empty)
    }

    pub(crate) fn bind_for_peers(peers: &[SocketAddr]) -> Result<Self, String> {
        let families = std::array::from_fn(|_| None);
        let mut shared = Self {
            families,
            callers: srt_transport::CallerTable::new(),
            outbound: std::array::from_fn(|_| VecDeque::with_capacity(MAX_OUTBOUND)),
            tx_pool: TxPool::new(MAX_OUTBOUND, TX_SLOT_SIZE)
                .expect("SRT TX pool capacity is valid"),
            recv_batch: RecvBatch::new(),
            native_metrics: SrtNativeMetrics::default(),
            #[cfg(test)]
            drive_calls: 0,
        };
        shared.ensure_for_peers(peers)?;
        Ok(shared)
    }

    pub(crate) fn ensure_for_peers(&mut self, peers: &[SocketAddr]) -> Result<(), String> {
        if peers.is_empty() {
            return Err("SRT connect requires a peer address".to_string());
        }
        for family_index in 0..FAMILY_COUNT {
            let is_ipv6 = family_index == 1;
            if peers.iter().any(|peer| peer.is_ipv6() == is_ipv6)
                && self.families[family_index].is_none()
            {
                self.families[family_index] = Some(bind_family(is_ipv6)?);
            }
        }
        Ok(())
    }

    pub(crate) fn drive(&mut self, now: Timestamp) -> Result<(), String> {
        #[cfg(test)]
        {
            self.drive_calls = self.drive_calls.saturating_add(1);
        }
        let mut feed_error = None;
        for family_index in 0..FAMILY_COUNT {
            let Some(family) = self.families[family_index].as_mut() else {
                continue;
            };
            let ready_count = family
                .poller
                .poll(Duration::ZERO, std::slice::from_mut(&mut family.ready))
                .map_err(|error| error.to_string())?;
            if ready_count != 0 && family.ready.readable {
                let budget = recv_budget();
                for _ in 0..budget.max_rounds {
                    let received = self
                        .recv_batch
                        .recv(family.socket.as_raw_fd())
                        .map_err(|error| error.to_string())?;
                    for (addr, data) in self.recv_batch.iter(received) {
                        let Some(peer) = addr else { continue };
                        self.native_metrics.rx_packets =
                            self.native_metrics.rx_packets.saturating_add(1);
                        self.native_metrics.rx_bytes = self
                            .native_metrics
                            .rx_bytes
                            .saturating_add(data.len() as u64);
                        if let Err(error) = self.callers.feed(peer, data, now) {
                            feed_error.get_or_insert(error);
                        }
                    }
                    if received < self.recv_batch.capacity() {
                        break;
                    }
                }
            }
            if ready_count != 0 {
                family
                    .poller
                    .register(family.socket.as_raw_fd(), 0, 1, UdpInterest::READ_WRITE)
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(error) = feed_error {
            return Err(error.to_string());
        }
        if !self.flush_outbound()? {
            return Ok(());
        }
        let budget = OutputDrainBudget::default();
        let mut sink = SharedTxSink {
            outbound: &mut self.outbound,
            tx_pool: &mut self.tx_pool,
            pool_empty: &mut self.native_metrics.tx_pool_empty,
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
                    tx_pool: &mut self.tx_pool,
                    pool_empty: &mut self.native_metrics.tx_pool_empty,
                    leased: None,
                };
                caller.send_shared_into(message.clone(), now, &mut sink)
            }
        }
    }

    pub(crate) fn flush_outbound(&mut self) -> Result<bool, String> {
        let (families, outbound, tx_pool, native_metrics) = (
            &mut self.families,
            &mut self.outbound,
            &mut self.tx_pool,
            &mut self.native_metrics,
        );
        for family_index in 0..FAMILY_COUNT {
            let Some(family) = families[family_index].as_mut() else {
                continue;
            };
            let completed = family
                .poller
                .drain_send_completions(&mut family.send_completions);
            for completion_index in 0..completed {
                let completion = family.send_completions[completion_index];
                let slot = completion.slot as usize;
                let Some(pending) = family.inflight.get_mut(slot).and_then(Option::take) else {
                    return Err(format!(
                        "UDP completion {} has no in-flight datagram",
                        completion.slot
                    ));
                };
                if completion.result < 0 {
                    let error = std::io::Error::from_raw_os_error(-completion.result);
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        outbound[family_index].push_front(pending);
                    } else {
                        let _ = tx_pool.complete(pending.lease);
                        let _ = tx_pool.release(pending.lease);
                        return Err(error.to_string());
                    }
                } else if completion.result as usize == pending.len {
                    native_metrics.tx_packets = native_metrics.tx_packets.saturating_add(1);
                    native_metrics.tx_bytes =
                        native_metrics.tx_bytes.saturating_add(pending.len as u64);
                    if !tx_pool.complete(pending.lease) || !tx_pool.release(pending.lease) {
                        return Err("SRT TX lease completion failed".to_string());
                    }
                } else {
                    let _ = tx_pool.complete(pending.lease);
                    let _ = tx_pool.release(pending.lease);
                    return Err(format!(
                        "short UDP datagram send: wrote {} bytes",
                        completion.result
                    ));
                }
            }
            while let Some(operation_slot) = family.inflight.iter().position(Option::is_none) {
                let Some(pending) = outbound[family_index].pop_front() else {
                    break;
                };
                let Some(packet) = tx_pool
                    .slot(pending.lease)
                    .and_then(|packet| packet.get(..pending.len))
                else {
                    let _ = tx_pool.complete(pending.lease);
                    let _ = tx_pool.release(pending.lease);
                    return Err("SRT TX lease is invalid".to_string());
                };
                match family.poller.submit_send_on_slot(
                    family.socket.as_raw_fd(),
                    0,
                    operation_slot as u32,
                    1,
                    pending.peer,
                    packet,
                ) {
                    Ok(()) => {
                        native_metrics.sqes = native_metrics.sqes.saturating_add(1);
                        family.inflight[operation_slot] = Some(pending);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        outbound[family_index].push_front(pending);
                        break;
                    }
                    Err(error) => {
                        outbound[family_index].push_front(pending);
                        return Err(error.to_string());
                    }
                }
            }
        }
        Ok(outbound.iter().all(VecDeque::is_empty)
            && families
                .iter()
                .flatten()
                .all(|family| family.inflight.iter().all(Option::is_none)))
    }

    pub(crate) fn native_metrics(&self) -> SrtNativeMetrics {
        let pollers = self
            .families
            .iter()
            .flatten()
            .map(|family| family.poller.metrics());
        SrtNativeMetrics {
            cqes: pollers.clone().map(|metrics| metrics.completions).sum(),
            stale_completions: pollers
                .clone()
                .map(|metrics| metrics.stale_completions)
                .sum(),
            cq_overflows: pollers.map(|metrics| metrics.cq_overflows).sum(),
            ..self.native_metrics
        }
    }
}

struct SharedTxSink<'a> {
    outbound: &'a mut [VecDeque<PendingDatagram>; FAMILY_COUNT],
    tx_pool: &'a mut TxPool,
    pool_empty: &'a mut u64,
    leased: Option<TxLease>,
}

impl DatagramSink for SharedTxSink<'_> {
    fn send_owned(&mut self, peer: SocketAddr, packet: Vec<u8>) -> Result<(), Vec<u8>> {
        let queue = &mut self.outbound[family_index(peer)];
        if self.leased.is_some() || packet.len() > TX_SLOT_SIZE || queue.len() >= MAX_OUTBOUND {
            return Err(packet);
        }
        let Some(lease) = self.tx_pool.acquire() else {
            *self.pool_empty = self.pool_empty.saturating_add(1);
            return Err(packet);
        };
        let Some(storage) = self.tx_pool.slot_mut(lease) else {
            let _ = self.tx_pool.abort(lease);
            return Err(packet);
        };
        storage[..packet.len()].copy_from_slice(&packet);
        if !self.tx_pool.submit(lease) {
            let _ = self.tx_pool.abort(lease);
            return Err(packet);
        }
        queue.push_back(PendingDatagram {
            peer,
            lease,
            len: packet.len(),
        });
        Ok(())
    }

    fn acquire(&mut self, max_len: usize) -> Option<&mut [std::mem::MaybeUninit<u8>]> {
        if self.leased.is_some() || max_len > TX_SLOT_SIZE {
            return None;
        }
        let Some(lease) = self.tx_pool.acquire() else {
            *self.pool_empty = self.pool_empty.saturating_add(1);
            return None;
        };
        self.leased = Some(lease);
        let storage = self
            .tx_pool
            .slot_mut(lease)
            .expect("new TX lease is writable");
        // SAFETY: `MaybeUninit<u8>` has the same layout as `u8`; the
        // DatagramSink contract initializes exactly the committed prefix.
        Some(unsafe {
            std::slice::from_raw_parts_mut(
                storage.as_mut_ptr().cast::<std::mem::MaybeUninit<u8>>(),
                storage.len(),
            )
        })
    }

    fn commit(&mut self, peer: SocketAddr, len: usize) -> bool {
        let Some(lease) = self.leased.take() else {
            return false;
        };
        let queue = &mut self.outbound[family_index(peer)];
        if len > TX_SLOT_SIZE || queue.len() >= MAX_OUTBOUND {
            let _ = self.tx_pool.abort(lease);
            return false;
        }
        if !self.tx_pool.submit(lease) {
            let _ = self.tx_pool.abort(lease);
            return false;
        }
        queue.push_back(PendingDatagram { peer, lease, len });
        true
    }

    fn abort(&mut self) {
        if let Some(lease) = self.leased.take() {
            let _ = self.tx_pool.abort(lease);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_datagrams_use_a_bounded_tx_lease() {
        let peer = "127.0.0.1:9000".parse().unwrap();
        let mut outbound = std::array::from_fn(|_| VecDeque::new());
        let mut tx_pool = TxPool::new(1, TX_SLOT_SIZE).unwrap();
        let mut pool_empty = 0;
        let mut sink = SharedTxSink {
            outbound: &mut outbound,
            tx_pool: &mut tx_pool,
            pool_empty: &mut pool_empty,
            leased: None,
        };

        DatagramSink::send_owned(&mut sink, peer, Vec::from([1_u8, 2, 3])).unwrap();

        assert_eq!(tx_pool.available(), 0);
        let lease = outbound[family_index(peer)].front().unwrap().lease;
        assert_eq!(tx_pool.slot(lease).unwrap()[..3], [1_u8, 2, 3]);
    }

    #[test]
    fn outbound_admission_is_hard_bounded_per_address_family() {
        let ipv4 = "127.0.0.1:9000".parse().unwrap();
        let ipv6 = "[::1]:9000".parse().unwrap();
        let mut outbound = std::array::from_fn(|_| VecDeque::new());
        let mut tx_pool = TxPool::new(MAX_OUTBOUND, TX_SLOT_SIZE).unwrap();
        let mut pool_empty = 0;
        let mut sink = SharedTxSink {
            outbound: &mut outbound,
            tx_pool: &mut tx_pool,
            pool_empty: &mut pool_empty,
            leased: None,
        };

        for _ in 0..MAX_OUTBOUND {
            DatagramSink::send_owned(&mut sink, ipv4, vec![1]).unwrap();
        }
        let rejected = DatagramSink::send_owned(&mut sink, ipv4, vec![2, 3]);
        assert_eq!(rejected, Err(vec![2, 3]));

        assert_eq!(outbound[family_index(ipv4)].len(), MAX_OUTBOUND);
        assert_eq!(pool_empty, 0);

        let mut ipv6_outbound = std::array::from_fn(|_| VecDeque::new());
        let mut ipv6_tx_pool = TxPool::new(1, TX_SLOT_SIZE).unwrap();
        let mut ipv6_pool_empty = 0;
        let mut ipv6_sink = SharedTxSink {
            outbound: &mut ipv6_outbound,
            tx_pool: &mut ipv6_tx_pool,
            pool_empty: &mut ipv6_pool_empty,
            leased: None,
        };
        DatagramSink::send_owned(&mut ipv6_sink, ipv6, vec![4]).unwrap();
        assert_eq!(ipv6_outbound[family_index(ipv6)].len(), 1);
    }
}
