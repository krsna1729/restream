use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

use restream_dataplane::udp::{
    UdpDriverDatagram, UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpDriver,
};
use restream_dataplane::{TxLease, TxPool};
use shiguredo_srt::Timestamp;
use srt_transport::{DatagramSink, LogicalCallerState, OutputDrainBudget};

use super::{desired_udp_buf, recv_budget};

/// Per-family in-flight sends. The driver ring owns `FAMILY_COUNT *`
/// per-family slots so global slot `family * SRT_UDP_SEND_CAPACITY + local`
/// is always in range.
const SRT_UDP_SEND_CAPACITY: usize = 16;
const MAX_OUTBOUND: usize = 256;
const TX_SLOT_SIZE: usize = 64 * 1024;
const FAMILY_COUNT: usize = 2;
/// One ring for both families: the shard's IPv4/IPv6 sockets are two fixed
/// slots on one [`UringUdpDriver`] instead of one poller per family.
const DRIVER_RING_ENTRIES: u32 = 64;
const DRIVER_BUFFERS_PER_SLOT: u16 = 64;
const DRIVER_BUFFER_SIZE: usize = 2_048;

#[derive(Debug, Clone, Copy)]
struct PendingDatagram {
    peer: SocketAddr,
    lease: TxLease,
    len: usize,
}

pub(super) fn family_index(peer: SocketAddr) -> usize {
    usize::from(peer.is_ipv6())
}

fn bind_socket(is_ipv6: bool) -> Result<UdpSocket, String> {
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
    Ok(socket)
}

struct UdpFamily {
    socket: UdpSocket,
    inflight: Box<[Option<PendingDatagram>]>,
    ready: UdpReadyEvent,
}

impl UdpFamily {
    fn new(socket: UdpSocket) -> Self {
        Self {
            socket,
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
        }
    }
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
    driver: Option<UringUdpDriver>,
    pub(crate) callers: srt_transport::CallerTable,
    outbound: [VecDeque<PendingDatagram>; FAMILY_COUNT],
    tx_pool: TxPool,
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
            driver: None,
            callers: srt_transport::CallerTable::new(),
            outbound: std::array::from_fn(|_| VecDeque::with_capacity(MAX_OUTBOUND)),
            tx_pool: TxPool::new(MAX_OUTBOUND, TX_SLOT_SIZE)
                .expect("SRT TX pool capacity is valid"),
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
        // Late family bind (IPv6 appearing after IPv4 was bound) rebuilds
        // the one driver ring so both descriptors stay on one ring. In
        // flight sends are drained first; queued outbound datagrams stay
        // queued in `outbound` and are resubmitted after the rebuild.
        let mut want = [false; FAMILY_COUNT];
        for (family_index, entry) in want.iter_mut().enumerate() {
            let is_ipv6 = family_index == 1;
            *entry = peers.iter().any(|peer| peer.is_ipv6() == is_ipv6);
            if *entry && self.families[family_index].is_none() {
                self.families[family_index] = Some(UdpFamily::new(bind_socket(is_ipv6)?));
            }
        }
        let live: Vec<usize> = (0..FAMILY_COUNT)
            .filter(|index| self.families[*index].is_some())
            .collect();
        let driver_live: Vec<usize> = (0..FAMILY_COUNT)
            .filter(|index| {
                self.driver
                    .as_ref()
                    .is_some_and(|_| self.families[*index].is_some())
            })
            .collect();
        if live != driver_live {
            self.rebuild_driver()?;
        }
        Ok(())
    }

    fn rebuild_driver(&mut self) -> Result<(), String> {
        // Queued (not yet submitted) outbound datagrams survive; in-flight
        // sends do not — the old ring is dropped. Refuse the rebuild while
        // any send is still in flight so no TX lease is lost.
        for family in self.families.iter().flatten() {
            if family.inflight.iter().any(Option::is_some) {
                return Err("SRT UDP driver rebuild with sends in flight".to_string());
            }
        }
        let mut driver = UringUdpDriver::new_fixed(
            FAMILY_COUNT,
            DRIVER_RING_ENTRIES,
            FAMILY_COUNT * SRT_UDP_SEND_CAPACITY,
            DRIVER_BUFFERS_PER_SLOT,
            DRIVER_BUFFER_SIZE,
        )
        .map_err(|error| error.to_string())?;
        for family_index in 0..FAMILY_COUNT {
            let Some(family) = self.families[family_index].as_ref() else {
                continue;
            };
            driver
                .register_fixed(
                    family.socket.as_raw_fd(),
                    family_index as u32,
                    1,
                    UdpInterest::READ_WRITE,
                )
                .map_err(|error| error.to_string())?;
        }
        self.driver = Some(driver);
        Ok(())
    }

    pub(crate) fn drive(&mut self, now: Timestamp) -> Result<(), String> {
        #[cfg(test)]
        {
            self.drive_calls = self.drive_calls.saturating_add(1);
        }
        let Some(driver) = self.driver.as_mut() else {
            return Err("SRT UDP driver is not bound".to_string());
        };
        // One ring, one wait: readiness + multishot receives + TX
        // completions + timeout share this single `poll`.
        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; FAMILY_COUNT];
        let budget = recv_budget();
        let datagram_capacity = budget.max_datagrams.max(1);
        let mut datagrams = vec![
            UdpDriverDatagram {
                slot: 0,
                buffer_id: 0,
                offset: 0,
                len: 0,
                peer: "0.0.0.0:0".parse().expect("valid zero socket address"),
            };
            datagram_capacity
        ];
        let mut feed_error = None;
        for _ in 0..budget.max_rounds {
            let (ready_count, received) = driver
                .poll(Duration::ZERO, &mut ready, &mut datagrams)
                .map_err(|error| error.to_string())?;
            for datagram in datagrams.iter().take(received) {
                let family_index = datagram.slot as usize;
                let Some(buffers) = driver.buffers(datagram.slot) else {
                    continue;
                };
                let Some(payload) =
                    buffers.payload(datagram.buffer_id, datagram.offset, datagram.len)
                else {
                    let _ = driver.recycle(datagram.slot, datagram.buffer_id);
                    continue;
                };
                self.native_metrics.rx_packets = self.native_metrics.rx_packets.saturating_add(1);
                self.native_metrics.rx_bytes = self
                    .native_metrics
                    .rx_bytes
                    .saturating_add(payload.len() as u64);
                if let Err(error) = self.callers.feed(datagram.peer, payload, now) {
                    feed_error.get_or_insert(error.to_string());
                }
                let _ = driver.recycle(datagram.slot, datagram.buffer_id);
                let _ = family_index;
            }
            for event in ready.iter().take(ready_count) {
                let Some(family) = self
                    .families
                    .get_mut(event.slot as usize)
                    .and_then(Option::as_mut)
                else {
                    continue;
                };
                family.ready = *event;
                driver
                    .rearm(family.socket.as_raw_fd(), event.slot, event.generation)
                    .map_err(|error| error.to_string())?;
            }
            if received < datagram_capacity {
                break;
            }
        }
        if let Some(error) = feed_error {
            return Err(error);
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
        let (families, driver, outbound, tx_pool, native_metrics) = (
            &mut self.families,
            &mut self.driver,
            &mut self.outbound,
            &mut self.tx_pool,
            &mut self.native_metrics,
        );
        let Some(driver) = driver.as_mut() else {
            return Err("SRT UDP driver is not bound".to_string());
        };
        // Operation slots are driver-global (one ring), while `inflight`
        // stays per family. Split the global slot space evenly: family `f`
        // owns `f * SRT_UDP_SEND_CAPACITY .. (f+1) * SRT_UDP_SEND_CAPACITY`.
        // `UringUdpDriver` retires each slot independently, so families never
        // observe each other's completions.
        for family_index in 0..FAMILY_COUNT {
            let Some(family) = families[family_index].as_mut() else {
                continue;
            };
            let slot_base = family_index * SRT_UDP_SEND_CAPACITY;
            let mut completions = vec![
                UdpSendCompletion {
                    slot: 0,
                    generation: 0,
                    result: 0,
                };
                FAMILY_COUNT * SRT_UDP_SEND_CAPACITY
            ];
            let completed = driver.drain_send_completions(&mut completions);
            // Completions arrive with driver-global slot numbers; map back
            // to this family's local `inflight` index.
            let mut ready = Vec::with_capacity(completed);
            for completion in completions.iter().take(completed) {
                let Some(local) = completion
                    .slot
                    .checked_sub(slot_base as u32)
                    .map(usize::try_from)
                    .and_then(Result::ok)
                else {
                    continue;
                };
                if local >= family.inflight.len() {
                    continue;
                }
                ready.push((*completion, local));
            }
            for (completion, slot) in ready {
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
                let global_slot = (slot_base + operation_slot) as u32;
                match driver.submit_send(
                    family.socket.as_raw_fd(),
                    family_index as u32,
                    global_slot,
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
        let metrics = self.driver.as_ref().map(|driver| driver.metrics());
        SrtNativeMetrics {
            cqes: metrics.map(|metrics| metrics.completions).unwrap_or(0),
            stale_completions: metrics
                .map(|metrics| metrics.stale_completions)
                .unwrap_or(0),
            cq_overflows: metrics.map(|metrics| metrics.cq_overflows).unwrap_or(0),
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

    #[test]
    fn tx_lease_hot_path_does_not_allocate() {
        let peer = "127.0.0.1:9000".parse().unwrap();
        let mut outbound = std::array::from_fn(|_| VecDeque::with_capacity(32));
        let mut tx_pool = TxPool::new(32, TX_SLOT_SIZE).unwrap();
        let mut pool_empty = 0;
        let payload = [7_u8; 32];

        crate::test_alloc::begin();
        for _ in 0..32 {
            let mut sink = SharedTxSink {
                outbound: &mut outbound,
                tx_pool: &mut tx_pool,
                pool_empty: &mut pool_empty,
                leased: None,
            };
            assert!(DatagramSink::send(&mut sink, peer, &payload));
        }
        assert_eq!(crate::test_alloc::end(), 0);
    }
}
