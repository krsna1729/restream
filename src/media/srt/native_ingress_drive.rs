//! Native SRT ingress protocol driving and TX flush routines.
//!
//! Separated from `native_ingress.rs` to keep production file sizes well
//! beneath the repository line-budget thresholds.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;

use restream_dataplane::udp::{UdpSendCompletion, UringUdpDriver};
use restream_dataplane::{TxLease, TxPool};
use srt_transport::{AdmissionOptions, IngressTelemetry, PeerTable};

use crate::media::snapshots::ListenerSocketStats;

use super::{
    LEGACY_TX_SLOTS, MAX_OUTBOUND, NativeSrtIngressStats, PROTO_TX_SLOT_SIZE, SrtIngressEvent,
    timestamp_now,
};

/// Sink that writes SRT protocol replies directly into the worker's
/// preallocated TX pool and queues them for the driver send path. No
/// intermediate `Vec<u8>` on the normal hot path.
pub(super) struct ProtoTxSink<'a> {
    pub(super) outbound: &'a mut VecDeque<ProtoDatagram>,
    pub(super) tx_pool: &'a mut TxPool,
    pub(super) pool_empty: &'a mut u64,
    pub(super) leased: Option<TxLease>,
}

#[derive(Clone, Copy)]
pub(super) struct ProtoDatagram {
    pub(super) peer: SocketAddr,
    pub(super) lease: TxLease,
    pub(super) len: usize,
}

impl srt_transport::DatagramSink for ProtoTxSink<'_> {
    fn send_owned(&mut self, peer: SocketAddr, packet: Vec<u8>) -> Result<(), Vec<u8>> {
        if self.leased.is_some()
            || packet.len() > PROTO_TX_SLOT_SIZE
            || self.outbound.len() >= MAX_OUTBOUND
        {
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
        self.outbound.push_back(ProtoDatagram {
            peer,
            lease,
            len: packet.len(),
        });
        Ok(())
    }

    fn acquire(&mut self, max_len: usize) -> Option<&mut [std::mem::MaybeUninit<u8>]> {
        if self.leased.is_some() || max_len > PROTO_TX_SLOT_SIZE {
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
        if len > PROTO_TX_SLOT_SIZE || self.outbound.len() >= MAX_OUTBOUND {
            let _ = self.tx_pool.abort(lease);
            return false;
        }
        if !self.tx_pool.submit(lease) {
            let _ = self.tx_pool.abort(lease);
            return false;
        }
        self.outbound.push_back(ProtoDatagram { peer, lease, len });
        true
    }

    fn abort(&mut self) {
        if let Some(lease) = self.leased.take() {
            let _ = self.tx_pool.abort(lease);
        }
    }
}

/// Fire due timers, collect replies directly into the TX pool, submit
/// them on the shared ring, and forward async-requiring control events.
/// All protocol progression stays on the socket owner; Tokio receives only
/// Connected/Media/Disconnected.
#[allow(clippy::too_many_arguments)]
pub(super) fn drive_protocol(
    driver: &mut UringUdpDriver,
    fd: RawFd,
    peers: &mut PeerTable,
    _admission: &AdmissionOptions,
    _telemetry: &IngressTelemetry,
    tx_pool: &mut TxPool,
    proto_outbound: &mut VecDeque<ProtoDatagram>,
    proto_inflight: &mut [Option<ProtoDatagram>],
    proto_completions: &mut [UdpSendCompletion],
    stashed: &mut Vec<UdpSendCompletion>,
    proto_events: &mut Vec<srt_transport::AdmissionEvent>,
    tx_pool_empty: &mut u64,
    events_tx: &mpsc::Sender<SrtIngressEvent>,
    event_drops: &mut u64,
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    flush_proto_outbound(
        driver,
        fd,
        proto_outbound,
        proto_inflight,
        proto_completions,
        stashed,
        tx_pool,
        stats,
        listener_stats,
    )?;
    {
        let mut sink = ProtoTxSink {
            outbound: proto_outbound,
            tx_pool,
            pool_empty: tx_pool_empty,
            leased: None,
        };
        let now = timestamp_now();
        let mut out: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
        peers.poll_outbound(now, &mut out);
        for (peer, packet) in out.drain(..) {
            use srt_transport::DatagramSink;
            let _ = sink.send_owned(peer, packet);
        }
    }
    flush_proto_outbound(
        driver,
        fd,
        proto_outbound,
        proto_inflight,
        proto_completions,
        stashed,
        tx_pool,
        stats,
        listener_stats,
    )?;
    proto_events.clear();
    peers.poll_events(proto_events);
    for event in proto_events.drain(..) {
        use shiguredo_srt::ConnectionEvent;
        let srt_transport::AdmissionEvent {
            representative_peer: peer,
            logical_peer,
            event,
        } = event;
        match event {
            ConnectionEvent::Connected => {
                let stream_id = peers
                    .logical_peer(&logical_peer)
                    .and_then(|entry| entry.stream_id().map(str::to_owned))
                    .unwrap_or_default();
                if events_tx
                    .try_send(SrtIngressEvent::Connected {
                        peer,
                        logical_peer,
                        stream_id,
                    })
                    .is_err()
                {
                    *event_drops = event_drops.saturating_add(1);
                }
            }
            ConnectionEvent::DataReceived { payload, .. } => {
                if events_tx
                    .try_send(SrtIngressEvent::Media {
                        peer,
                        logical_peer,
                        payload,
                    })
                    .is_err()
                {
                    *event_drops = event_drops.saturating_add(1);
                }
            }
            ConnectionEvent::Disconnected { reason } => {
                if events_tx
                    .try_send(SrtIngressEvent::Disconnected {
                        peer,
                        logical_peer,
                        reason,
                    })
                    .is_err()
                {
                    *event_drops = event_drops.saturating_add(1);
                }
                let _ = peers.remove(logical_peer);
            }
            ConnectionEvent::StateChanged(_)
            | ConnectionEvent::Error(_)
            | ConnectionEvent::KeyRefreshNeeded { .. } => {}
        }
    }
    Ok(())
}

/// Compat (no-ring) protocol drive: blocking sends, same event forwarding.
#[allow(clippy::too_many_arguments)]
pub(super) fn drive_protocol_compat(
    socket: &std::net::UdpSocket,
    peers: &mut PeerTable,
    _admission: &AdmissionOptions,
    _telemetry: &IngressTelemetry,
    tx_pool: &mut TxPool,
    proto_outbound: &mut VecDeque<ProtoDatagram>,
    proto_events: &mut Vec<srt_transport::AdmissionEvent>,
    tx_pool_empty: &mut u64,
    events_tx: &mpsc::Sender<SrtIngressEvent>,
    event_drops: &mut u64,
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    use srt_transport::DatagramSink;
    let now = timestamp_now();
    let mut out: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
    peers.poll_outbound(now, &mut out);
    let mut sink = ProtoTxSink {
        outbound: proto_outbound,
        tx_pool,
        pool_empty: tx_pool_empty,
        leased: None,
    };
    for (peer, packet) in out.drain(..) {
        let _ = sink.send_owned(peer, packet);
    }
    while let Some(datagram) = proto_outbound.pop_front() {
        let Some(bytes) = tx_pool
            .slot(datagram.lease)
            .and_then(|slot| slot.get(..datagram.len))
        else {
            let _ = tx_pool.complete(datagram.lease);
            let _ = tx_pool.release(datagram.lease);
            continue;
        };
        let owned = bytes.to_vec();
        if let Ok(n) = socket.send_to(&owned, datagram.peer)
            && n == owned.len()
        {
            stats.sent_datagrams.fetch_add(1, Ordering::Relaxed);
            listener_stats
                .native_tx_datagrams
                .fetch_add(1, Ordering::Relaxed);
        }
        let _ = tx_pool.complete(datagram.lease);
        let _ = tx_pool.release(datagram.lease);
    }
    proto_events.clear();
    peers.poll_events(proto_events);
    for event in proto_events.drain(..) {
        use shiguredo_srt::ConnectionEvent;
        let srt_transport::AdmissionEvent {
            representative_peer: peer,
            logical_peer,
            event,
        } = event;
        match event {
            ConnectionEvent::Connected => {
                let stream_id = peers
                    .logical_peer(&logical_peer)
                    .and_then(|entry| entry.stream_id().map(str::to_owned))
                    .unwrap_or_default();
                if events_tx
                    .try_send(SrtIngressEvent::Connected {
                        peer,
                        logical_peer,
                        stream_id,
                    })
                    .is_err()
                {
                    *event_drops = event_drops.saturating_add(1);
                }
            }
            ConnectionEvent::DataReceived { payload, .. } => {
                if events_tx
                    .try_send(SrtIngressEvent::Media {
                        peer,
                        logical_peer,
                        payload,
                    })
                    .is_err()
                {
                    *event_drops = event_drops.saturating_add(1);
                }
            }
            ConnectionEvent::Disconnected { reason } => {
                if events_tx
                    .try_send(SrtIngressEvent::Disconnected {
                        peer,
                        logical_peer,
                        reason,
                    })
                    .is_err()
                {
                    *event_drops = event_drops.saturating_add(1);
                }
                let _ = peers.remove(logical_peer);
            }
            ConnectionEvent::StateChanged(_)
            | ConnectionEvent::Error(_)
            | ConnectionEvent::KeyRefreshNeeded { .. } => {}
        }
    }
    Ok(())
}

fn retire_proto_completion(
    local: usize,
    completion: UdpSendCompletion,
    outbound: &mut VecDeque<ProtoDatagram>,
    inflight: &mut [Option<ProtoDatagram>],
    tx_pool: &mut TxPool,
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    let Some(datagram) = inflight.get_mut(local).and_then(Option::take) else {
        return Err(io::Error::other("native SRT TX completion has no owner"));
    };
    if completion.result < 0 {
        let error = io::Error::from_raw_os_error(-completion.result);
        if error.kind() == io::ErrorKind::WouldBlock {
            outbound.push_front(datagram);
            return Ok(());
        }
        let _ = tx_pool.complete(datagram.lease);
        let _ = tx_pool.release(datagram.lease);
        return Err(error);
    }
    if completion.result as usize != datagram.len {
        let _ = tx_pool.complete(datagram.lease);
        let _ = tx_pool.release(datagram.lease);
        return Err(io::Error::other("native SRT TX completed a short datagram"));
    }
    stats.sent_datagrams.fetch_add(1, Ordering::Relaxed);
    listener_stats
        .native_tx_datagrams
        .fetch_add(1, Ordering::Relaxed);
    let _ = tx_pool.complete(datagram.lease);
    let _ = tx_pool.release(datagram.lease);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flush_proto_outbound(
    driver: &mut UringUdpDriver,
    fd: RawFd,
    outbound: &mut VecDeque<ProtoDatagram>,
    inflight: &mut [Option<ProtoDatagram>],
    completions: &mut [UdpSendCompletion],
    stashed: &mut Vec<UdpSendCompletion>,
    tx_pool: &mut TxPool,
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    let mut index = 0;
    while index < stashed.len() {
        let completion = stashed[index];
        let Some(local) = (completion.slot as usize).checked_sub(LEGACY_TX_SLOTS) else {
            index += 1;
            continue;
        };
        stashed.swap_remove(index);
        retire_proto_completion(
            local,
            completion,
            outbound,
            inflight,
            tx_pool,
            stats,
            listener_stats,
        )?;
    }
    let completed = driver.drain_send_completions(completions);
    for completion in completions[..completed].iter().copied() {
        let Some(local) = (completion.slot as usize).checked_sub(LEGACY_TX_SLOTS) else {
            stashed.push(completion);
            continue;
        };
        retire_proto_completion(
            local,
            completion,
            outbound,
            inflight,
            tx_pool,
            stats,
            listener_stats,
        )?;
    }
    while let Some(operation_slot) = inflight.iter().position(Option::is_none) {
        let Some(datagram) = outbound.pop_front() else {
            break;
        };
        let Some(packet) = tx_pool
            .slot(datagram.lease)
            .and_then(|slot| slot.get(..datagram.len))
        else {
            let _ = tx_pool.complete(datagram.lease);
            let _ = tx_pool.release(datagram.lease);
            continue;
        };
        let global_slot = (LEGACY_TX_SLOTS + operation_slot) as u32;
        match driver.submit_send(fd, 0, global_slot, 1, datagram.peer, packet) {
            Ok(()) => inflight[operation_slot] = Some(datagram),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                outbound.push_front(datagram);
                break;
            }
            Err(error) => {
                outbound.push_front(datagram);
                return Err(error);
            }
        }
    }
    Ok(())
}

fn retire_legacy_completion(
    slot: usize,
    completion: UdpSendCompletion,
    outbound: &mut VecDeque<(SocketAddr, Vec<u8>)>,
    inflight: &mut [Option<(SocketAddr, Vec<u8>)>],
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    let Some((peer, packet)) = inflight.get_mut(slot).and_then(Option::take) else {
        return Err(io::Error::other("native SRT TX completion has no owner"));
    };
    if completion.result < 0 {
        let error = io::Error::from_raw_os_error(-completion.result);
        if error.kind() == io::ErrorKind::WouldBlock {
            outbound.push_front((peer, packet));
            return Ok(());
        }
        return Err(error);
    }
    if completion.result as usize != packet.len() {
        return Err(io::Error::other("native SRT TX completed a short datagram"));
    }
    stats.sent_datagrams.fetch_add(1, Ordering::Relaxed);
    listener_stats
        .native_tx_datagrams
        .fetch_add(1, Ordering::Relaxed);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flush_outbound(
    poller: &mut UringUdpDriver,
    fd: i32,
    outbound: &mut VecDeque<(SocketAddr, Vec<u8>)>,
    inflight: &mut [Option<(SocketAddr, Vec<u8>)>],
    completions: &mut [UdpSendCompletion],
    stashed: &mut Vec<UdpSendCompletion>,
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    let mut index = 0;
    while index < stashed.len() {
        let completion = stashed[index];
        let slot = completion.slot as usize;
        if slot >= LEGACY_TX_SLOTS {
            index += 1;
            continue;
        }
        stashed.swap_remove(index);
        retire_legacy_completion(slot, completion, outbound, inflight, stats, listener_stats)?;
    }
    let completed = poller.drain_send_completions(completions);
    for completion in completions[..completed].iter().copied() {
        let slot = completion.slot as usize;
        if slot >= LEGACY_TX_SLOTS {
            stashed.push(completion);
            continue;
        }
        retire_legacy_completion(slot, completion, outbound, inflight, stats, listener_stats)?;
    }

    while let Some(operation_slot) = inflight.iter().position(Option::is_none) {
        let Some((peer, packet)) = outbound.pop_front() else {
            break;
        };
        match poller.submit_send(fd, 0, operation_slot as u32, 1, peer, &packet) {
            Ok(()) => inflight[operation_slot] = Some((peer, packet)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                outbound.push_front((peer, packet));
                break;
            }
            Err(error) => {
                outbound.push_front((peer, packet));
                return Err(error);
            }
        }
    }
    Ok(())
}
