//! Native UDP ownership for the SRT admission loop.
//!
//! The protocol table remains on its async owner. This worker owns the UDP
//! descriptor, the readiness ring, and a fixed receive-buffer reserve; only
//! bounded packet ownership crosses the runtime boundary.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use restream_dataplane::udp::{UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpPoller};
use srt_transport::{RecvBatch, RecvBudget, drain_recv_fd};
use tokio::sync::mpsc;
use tracing::error;

use crate::media::snapshots::ListenerSocketStats;

const CHANNEL_CAPACITY: usize = 256;
const BUFFER_SIZE: usize = RecvBatch::DEFAULT_BUF_LEN;
const MAX_OUTBOUND: usize = 1024;
const SEND_CAPACITY: usize = 64;

pub(crate) struct NativeSrtDatagram {
    pub(crate) peer: SocketAddr,
    pub(crate) buffer: Box<[u8]>,
    pub(crate) len: usize,
}

impl NativeSrtDatagram {
    #[cfg(test)]
    pub(crate) fn payload(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}

#[derive(Debug, Default)]
pub(crate) struct NativeSrtIngressStats {
    pub(crate) dropped_pool: AtomicU64,
    pub(crate) dropped_channel: AtomicU64,
    pub(crate) recv_datagrams: AtomicU64,
    pub(crate) sent_datagrams: AtomicU64,
}

pub(crate) struct NativeSrtIngress {
    pub(crate) inbound: mpsc::Receiver<NativeSrtDatagram>,
    pub(crate) recycled: mpsc::Sender<Box<[u8]>>,
    pub(crate) outbound: mpsc::Sender<(SocketAddr, Vec<u8>)>,
    pub(crate) stats: Arc<NativeSrtIngressStats>,
}

impl NativeSrtIngress {
    pub(crate) fn start(
        socket: UdpSocket,
        listener_stats: Arc<ListenerSocketStats>,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let (inbound_tx, inbound) = mpsc::channel(CHANNEL_CAPACITY);
        let (recycled, recycled_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (outbound, outbound_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let stats = Arc::new(NativeSrtIngressStats::default());
        let worker_stats = stats.clone();
        thread::Builder::new()
            .name("restream-srt-ingress".to_string())
            .spawn(move || {
                if let Err(error) = run_worker(
                    socket,
                    inbound_tx,
                    recycled_rx,
                    outbound_rx,
                    worker_stats,
                    listener_stats,
                ) {
                    error!(%error, "native SRT ingress stopped");
                }
            })
            .map_err(|error| io::Error::other(format!("spawn SRT ingress: {error}")))?;
        Ok(Self {
            inbound,
            recycled,
            outbound,
            stats,
        })
    }
}

fn deliver_datagram(
    inbound_tx: &mpsc::Sender<NativeSrtDatagram>,
    packet: NativeSrtDatagram,
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> bool {
    match inbound_tx.try_send(packet) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            stats.dropped_channel.fetch_add(1, Ordering::Relaxed);
            listener_stats
                .native_rx_channel_drops
                .fetch_add(1, Ordering::Relaxed);
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn run_worker(
    socket: UdpSocket,
    inbound_tx: mpsc::Sender<NativeSrtDatagram>,
    mut recycled_rx: mpsc::Receiver<Box<[u8]>>,
    mut outbound_rx: mpsc::Receiver<(SocketAddr, Vec<u8>)>,
    stats: Arc<NativeSrtIngressStats>,
    listener_stats: Arc<ListenerSocketStats>,
) -> io::Result<()> {
    let fd = socket.as_raw_fd();
    let mut poller = UringUdpPoller::new_fixed_with_send_capacity(1, 256, SEND_CAPACITY)?;
    poller.register_fixed(fd, 0, 1, UdpInterest::READ)?;
    let mut recv_batch = RecvBatch::new();
    let mut free = (0..CHANNEL_CAPACITY)
        .map(|_| vec![0_u8; BUFFER_SIZE].into_boxed_slice())
        .collect::<Vec<_>>();
    let mut outbound = VecDeque::with_capacity(MAX_OUTBOUND);
    let mut inflight = std::iter::repeat_with(|| None)
        .take(SEND_CAPACITY)
        .collect::<Vec<Option<(SocketAddr, Vec<u8>)>>>();
    let mut completions = vec![
        UdpSendCompletion {
            slot: 0,
            generation: 0,
            result: 0,
        };
        SEND_CAPACITY
    ];
    let mut ready = [UdpReadyEvent {
        fd: -1,
        slot: 0,
        generation: 0,
        readable: false,
        writable: false,
    }];
    let budget = RecvBudget::default();

    loop {
        while let Ok(buffer) = recycled_rx.try_recv() {
            if free.len() < CHANNEL_CAPACITY {
                free.push(buffer);
            }
        }
        while outbound.len() < MAX_OUTBOUND {
            match outbound_rx.try_recv() {
                Ok(packet) => outbound.push_back(packet),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        flush_outbound(
            &mut poller,
            fd,
            &mut outbound,
            &mut inflight,
            &mut completions,
            &stats,
            &listener_stats,
        )?;

        if inbound_tx.is_closed() && outbound.is_empty() && inflight.iter().all(Option::is_none) {
            return Ok(());
        }
        let ready_count = poller.poll(Duration::from_millis(5), &mut ready)?;
        let readable = ready[..ready_count]
            .iter()
            .any(|event| event.readable && event.slot == 0 && event.generation == 1);
        if readable {
            let report = drain_recv_fd(fd, &mut recv_batch, budget, |peer, data| {
                let Some(peer) = peer else { return };
                stats.recv_datagrams.fetch_add(1, Ordering::Relaxed);
                listener_stats
                    .native_rx_datagrams
                    .fetch_add(1, Ordering::Relaxed);
                let Some(mut buffer) = free.pop() else {
                    stats.dropped_pool.fetch_add(1, Ordering::Relaxed);
                    listener_stats
                        .native_rx_pool_drops
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                };
                buffer[..data.len()].copy_from_slice(data);
                let packet = NativeSrtDatagram {
                    peer,
                    buffer,
                    len: data.len(),
                };
                let _ = deliver_datagram(&inbound_tx, packet, &stats, &listener_stats);
            })?;
            let _ = report;
            poller.register(fd, 0, 1, UdpInterest::READ)?;
        }
        flush_outbound(
            &mut poller,
            fd,
            &mut outbound,
            &mut inflight,
            &mut completions,
            &stats,
            &listener_stats,
        )?;
    }
}

fn flush_outbound(
    poller: &mut UringUdpPoller,
    fd: i32,
    outbound: &mut VecDeque<(SocketAddr, Vec<u8>)>,
    inflight: &mut [Option<(SocketAddr, Vec<u8>)>],
    completions: &mut [UdpSendCompletion],
    stats: &NativeSrtIngressStats,
    listener_stats: &ListenerSocketStats,
) -> io::Result<()> {
    let completed = poller.drain_send_completions(completions);
    for completion in completions[..completed].iter().copied() {
        let slot = completion.slot as usize;
        let Some((peer, packet)) = inflight.get_mut(slot).and_then(Option::take) else {
            return Err(io::Error::other("native SRT TX completion has no owner"));
        };
        if completion.result < 0 {
            let error = io::Error::from_raw_os_error(-completion.result);
            if error.kind() == io::ErrorKind::WouldBlock {
                outbound.push_front((peer, packet));
                continue;
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
    }

    while let Some(operation_slot) = inflight.iter().position(Option::is_none) {
        let Some((peer, packet)) = outbound.pop_front() else {
            break;
        };
        match poller.submit_send_on_slot(fd, 0, operation_slot as u32, 1, peer, &packet) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;

    #[tokio::test]
    async fn full_application_channel_drops_without_blocking_native_owner() {
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        let stats = NativeSrtIngressStats::default();
        let listener_stats = ListenerSocketStats::default();
        let packet = || NativeSrtDatagram {
            peer: "127.0.0.1:9000".parse().unwrap(),
            buffer: vec![1_u8].into_boxed_slice(),
            len: 1,
        };

        assert!(deliver_datagram(
            &inbound_tx,
            packet(),
            &stats,
            &listener_stats
        ));
        assert!(!deliver_datagram(
            &inbound_tx,
            packet(),
            &stats,
            &listener_stats
        ));
        assert_eq!(stats.dropped_channel.load(Ordering::Relaxed), 1);
        assert_eq!(
            listener_stats
                .native_rx_channel_drops
                .load(Ordering::Relaxed),
            1
        );
        let _ = inbound_rx.recv().await;
    }

    #[tokio::test]
    async fn native_ingress_delivers_and_recycles_bounded_datagram() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer = receiver.local_addr().unwrap();
        let mut ingress =
            match NativeSrtIngress::start(receiver, Arc::new(ListenerSocketStats::default())) {
                Ok(ingress) => ingress,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                    ) =>
                {
                    return;
                }
                Err(error) => panic!("native UDP unavailable: {error}"),
            };
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.send_to(b"srt", peer).unwrap();
        let mut packet = ingress.inbound.recv().await.unwrap();
        assert_eq!(packet.payload(), b"srt");
        assert_eq!(packet.peer, sender.local_addr().unwrap());
        let buffer = std::mem::replace(
            &mut packet.buffer,
            vec![0_u8; BUFFER_SIZE].into_boxed_slice(),
        );
        ingress.recycled.send(buffer).await.unwrap();
        ingress
            .outbound
            .send((sender.local_addr().unwrap(), b"reply".to_vec()))
            .await
            .unwrap();
        let mut reply = [0_u8; 5];
        sender.recv(&mut reply).unwrap();
        assert_eq!(&reply, b"reply");
        assert_eq!(ingress.stats.recv_datagrams.load(Ordering::Relaxed), 1);
        assert_eq!(ingress.stats.sent_datagrams.load(Ordering::Relaxed), 1);
    }
}
