use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use bytes::Bytes;
use mio::net::UdpSocket;
use rand::RngExt;
use shiguredo_srt::{
    ConnectionOptions, ConnectionOutput, ConnectionState, ErrorKind, GroupExtensionData,
    GroupMemberState, KeyLength, SRTGROUP_MASK, SrtConnection, SrtGroup, TimerId, Timestamp,
};

use crate::media::egress::backend::{CloseReason, Readiness};
use crate::media::srt::{
    NativeSendBacklog, SrtEgressInterest, SrtMessageSender, SrtSendFailure, SrtSendResult,
    SrtSenderStats, receive_buffer_packets_from_bytes,
};

pub(super) struct SrtRustGroupSender {
    socket: Option<UdpSocket>,
    peers: Vec<SocketAddr>,
    group: SrtGroup,
    member_by_socket_id: HashMap<u32, u32>,
    timers: HashMap<u32, HashMap<TimerId, Timestamp>>,
    pending_datagrams: VecDeque<(SocketAddr, Vec<u8>)>,
    recv_buf: Vec<u8>,
    started: Instant,
    closed: bool,
}

impl SrtRustGroupSender {
    pub(super) fn connect(
        config: &crate::media::srt::SrtFabricEgressConnectConfig<'_>,
    ) -> Result<Self, String> {
        let peers = config.peer_addrs().to_vec();
        if peers.len() < 2 {
            return Err("Rust SRT group caller requires at least two peers".to_string());
        }
        let (sndbuf, rcvbuf, latency, maxbw, fc) = config.buffer_parameters();
        let std_socket = StdUdpSocket::bind(("0.0.0.0", 0))
            .map_err(|error| format!("bind Rust SRT group socket: {error}"))?;
        super::rs_sender::configure_udp_buffer(std_socket.as_raw_fd(), libc::SO_SNDBUF, sndbuf)?;
        super::rs_sender::configure_udp_buffer(std_socket.as_raw_fd(), libc::SO_RCVBUF, rcvbuf)?;
        std_socket
            .set_nonblocking(true)
            .map_err(|error| format!("set Rust SRT group socket nonblocking: {error}"))?;
        let socket = UdpSocket::from_std(std_socket);
        let started = Instant::now();
        let group_id = SRTGROUP_MASK | nonzero_random_u32() & 0x3FFF_FFFF;
        let group = SrtGroup::new(group_id, config.bond_mode().group_mode())
            .map_err(|error| format!("create Rust SRT group: {error}"))?;
        let mut sender = Self {
            socket: Some(socket),
            peers,
            group,
            member_by_socket_id: HashMap::new(),
            timers: HashMap::new(),
            pending_datagrams: VecDeque::new(),
            recv_buf: vec![0; 64 * 1024],
            started,
            closed: false,
        };
        for (index, _) in sender.peers.iter().enumerate() {
            let weight = if index == 0 { 1 } else { 0 };
            let mut options = group_options(config, rcvbuf, latency, maxbw, fc, group_id, weight)?;
            options.socket_id = nonzero_random_u32();
            let mut connection = SrtConnection::new_caller(options);
            let socket_id = connection.socket_id();
            connection
                .connect(sender.now())
                .map_err(|error| format!("start Rust SRT group handshake: {error}"))?;
            sender
                .group
                .add_member(index as u32 + 1, weight, connection)
                .map_err(|error| format!("add Rust SRT group member: {error}"))?;
            sender
                .member_by_socket_id
                .insert(socket_id, index as u32 + 1);
            sender.timers.insert(index as u32 + 1, HashMap::new());
        }
        sender.flush_outputs(sender.now())?;
        Ok(sender)
    }

    pub(super) fn raw_fd(&self) -> Option<RawFd> {
        self.socket.as_ref().map(AsRawFd::as_raw_fd)
    }

    fn now(&self) -> Timestamp {
        Timestamp::from_micros(self.started.elapsed().as_micros() as u64)
    }

    fn send_datagram(&self, peer: SocketAddr, packet: &[u8]) -> io::Result<()> {
        self.socket
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Rust SRT group closed"))?
            .send_to(packet, peer)
            .map(|_| ())
    }

    fn flush_outputs(&mut self, now: Timestamp) -> Result<(), String> {
        while let Some((peer, packet)) = self.pending_datagrams.pop_front() {
            match self.send_datagram(peer, &packet) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.pending_datagrams.push_front((peer, packet));
                    return Ok(());
                }
                Err(error) => return Err(format!("send Rust SRT group datagram: {error}")),
            }
        }
        let member_ids: Vec<u32> = self
            .group
            .members()
            .iter()
            .map(|member| member.id())
            .collect();
        for member_id in member_ids {
            let peer = self.peers[member_id as usize - 1];
            while let Some(output) = self
                .group
                .member_mut(member_id)
                .and_then(|member| member.connection_mut().poll_output())
            {
                match output {
                    ConnectionOutput::SendPacket(packet) => {
                        match self.send_datagram(peer, &packet) {
                            Ok(()) => {}
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                self.pending_datagrams.push_back((peer, packet));
                                return Ok(());
                            }
                            Err(error) => {
                                return Err(format!("send Rust SRT group datagram: {error}"));
                            }
                        }
                    }
                    ConnectionOutput::SetTimer {
                        id,
                        duration_micros,
                    } => {
                        self.timers
                            .entry(member_id)
                            .or_default()
                            .insert(id, now.add_micros(duration_micros));
                    }
                    ConnectionOutput::ClearTimer { id } => {
                        if let Some(timers) = self.timers.get_mut(&member_id) {
                            timers.remove(&id);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn process_timers(&mut self, now: Timestamp) -> Result<(), String> {
        let due: Vec<(u32, TimerId)> = self
            .timers
            .iter()
            .flat_map(|(member_id, timers)| {
                timers.iter().filter_map(|(id, deadline)| {
                    (now.as_micros() >= deadline.as_micros()).then_some((*member_id, *id))
                })
            })
            .collect();
        for (member_id, id) in due {
            if let Some(timers) = self.timers.get_mut(&member_id) {
                timers.remove(&id);
            }
            let Some(member) = self.group.member_mut(member_id) else {
                continue;
            };
            member
                .connection_mut()
                .handle_timer(id, now)
                .map_err(|error| format!("Rust SRT group timer {id:?}: {error}"))?;
        }
        Ok(())
    }

    fn receive_packets(&mut self, now: Timestamp) -> Result<(), String> {
        loop {
            let (size, peer) = match self
                .socket
                .as_ref()
                .ok_or_else(|| "Rust SRT group socket closed".to_string())?
                .recv_from(&mut self.recv_buf)
            {
                Ok(received) => received,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("receive Rust SRT group datagram: {error}")),
            };
            let Some(member_id) = member_id_for_datagram(
                &self.peers,
                &self.member_by_socket_id,
                peer,
                &self.recv_buf[..size],
            ) else {
                continue;
            };
            let Some(member) = self.group.member_mut(member_id) else {
                continue;
            };
            member
                .connection_mut()
                .feed_recv_buf(&self.recv_buf[..size], now)
                .map_err(|error| format!("process Rust SRT group datagram: {error}"))?;
        }
        Ok(())
    }

    fn service(&mut self, now: Timestamp, readable: bool) -> Result<(), String> {
        self.process_timers(now)?;
        if readable {
            self.receive_packets(now)?;
        }
        while self.group.poll_data(now).is_some() {}
        self.flush_outputs(now)?;
        if self
            .group
            .members()
            .iter()
            .all(|member| member.state() == GroupMemberState::Broken)
        {
            self.closed = true;
        }
        Ok(())
    }

    fn active_can_send(&self) -> bool {
        self.group.members().iter().any(|member| {
            member.state() == GroupMemberState::Active
                && member.connection().state() == ConnectionState::Connected
                && member.connection().can_send_with_pacing(self.now())
        })
    }
}

fn group_options(
    config: &crate::media::srt::SrtFabricEgressConnectConfig<'_>,
    rcvbuf: i32,
    latency: i32,
    maxbw: i64,
    fc: i32,
    group_id: u32,
    weight: u16,
) -> Result<ConnectionOptions, String> {
    let flow_window_packets = fc.max(32) as u32;
    let mut options = ConnectionOptions {
        tsbpd_delay: latency.clamp(0, u16::MAX as i32) as u16,
        stream_id: Some(config.stream_id().to_string()),
        max_bandwidth_bytes_per_sec: (maxbw > 0).then_some((maxbw / 8) as u64),
        flow_window_packets,
        receive_buffer_packets: receive_buffer_packets_from_bytes(rcvbuf, flow_window_packets),
        group_extension: Some(GroupExtensionData {
            group_id,
            group_type: config.bond_mode().group_type(),
            flags: 0,
            weight,
        }),
        ..ConnectionOptions::default()
    };
    if let Some((passphrase, pbkeylen)) = config.crypto_parameters() {
        options.key_length = match pbkeylen {
            16 => KeyLength::Aes128,
            24 => KeyLength::Aes192,
            32 => KeyLength::Aes256,
            other => return Err(format!("Rust SRT group caller got pbkeylen {other}")),
        };
        let mut salt = [0u8; 16];
        let mut sek = vec![0u8; options.key_length.len()];
        rand::rng().fill(&mut salt);
        rand::rng().fill(sek.as_mut_slice());
        options.passphrase = Some(passphrase.to_string());
        options.crypto_salt = Some(salt);
        options.crypto_sek = Some(sek);
    }
    Ok(options)
}

fn nonzero_random_u32() -> u32 {
    let value = rand::random::<u32>();
    if value == 0 { 1 } else { value }
}

impl SrtMessageSender for SrtRustGroupSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        if self.closed || !self.active_can_send() {
            return SrtSendResult::WouldBlock;
        }
        let now = self.now();
        match self.group.send(message, now) {
            Ok(_) => match self.flush_outputs(now) {
                Ok(()) => SrtSendResult::Accepted {
                    bytes: message.len(),
                },
                Err(detail) => SrtSendResult::Failed(SrtSendFailure {
                    reason: "rust_srt_group_datagram",
                    detail,
                    retryable: true,
                }),
            },
            Err(error) if error.kind == ErrorKind::InvalidState => SrtSendResult::WouldBlock,
            Err(error) => SrtSendResult::Failed(SrtSendFailure {
                reason: "rust_srt_group_send",
                detail: error.to_string(),
                retryable: true,
            }),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        self.closed = true;
        let now = self.now();
        let member_ids: Vec<u32> = self
            .group
            .members()
            .iter()
            .map(|member| member.id())
            .collect();
        for member_id in member_ids {
            if let Some(member) = self.group.member_mut(member_id) {
                member.connection_mut().disconnect(now);
            }
        }
        let _ = self.flush_outputs(now);
        self.socket.take();
        self.pending_datagrams.clear();
    }

    fn on_readiness(&mut self, readiness: Readiness) {
        if !self.closed && self.service(self.now(), readiness.readable).is_err() {
            self.closed = true;
        }
    }

    fn next_timer_deadline(&self) -> Option<Instant> {
        self.timers
            .values()
            .flat_map(|timers| timers.values())
            .filter_map(|deadline| {
                self.started
                    .checked_add(Duration::from_micros(deadline.as_micros()))
            })
            .min()
    }

    fn next_send_deadline(&self) -> Option<Instant> {
        if self.closed {
            return None;
        }
        self.group
            .members()
            .iter()
            .filter(|member| {
                member.state() == GroupMemberState::Active
                    && member.connection().state() == ConnectionState::Connected
                    && member.connection().can_send()
            })
            .filter_map(|member| {
                let wait = member.connection().time_until_send(self.now());
                (wait > 0).then(|| Instant::now() + Duration::from_micros(wait))
            })
            .min()
    }

    fn on_wakeup(&mut self) {
        if !self.closed && self.service(self.now(), false).is_err() {
            self.closed = true;
        }
    }

    fn write_ready(&self) -> bool {
        self.active_can_send()
    }

    fn is_closed(&self) -> bool {
        self.closed
    }

    fn readiness_interest(&self) -> SrtEgressInterest {
        SrtEgressInterest {
            readable: true,
            writable: !self.pending_datagrams.is_empty(),
        }
    }

    fn dynamic_readiness(&self) -> bool {
        true
    }

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        (!self.pending_datagrams.is_empty()).then(|| NativeSendBacklog {
            bytes: self
                .pending_datagrams
                .iter()
                .map(|(_, packet)| packet.len() as u64)
                .sum(),
            packets: self.pending_datagrams.len().min(u32::MAX as usize) as u32,
            ms: 0,
        })
    }

    fn sender_quality_stats(&self) -> Option<SrtSenderStats> {
        let mut stats = SrtSenderStats::default();
        let mut any = false;
        for member in self.group.members() {
            if let Some(sender) = member.connection().sender_stats() {
                any = true;
                stats.packets_retransmit_total += sender.total_retransmits as u64;
                stats.send_buf_bytes += sender.packets_in_buffer as i32;
                stats.flight_size_packets += sender.packets_in_buffer as i32;
            }
        }
        any.then_some(stats)
    }
}

fn member_id_for_datagram(
    peers: &[SocketAddr],
    member_by_socket_id: &HashMap<u32, u32>,
    peer: SocketAddr,
    packet: &[u8],
) -> Option<u32> {
    packet_destination_socket_id(packet)
        .and_then(|socket_id| (socket_id != 0).then(|| member_by_socket_id.get(&socket_id)))
        .flatten()
        .copied()
        .or_else(|| {
            peers
                .iter()
                .position(|candidate| *candidate == peer)
                .map(|index| index as u32 + 1)
        })
}

fn packet_destination_socket_id(packet: &[u8]) -> Option<u32> {
    let bytes = packet.get(12..16)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiguredo_srt::{ConnectionEvent, ControlPacket, ControlType, GroupMode, SrtPacket};

    fn transfer(caller: &mut SrtConnection, listener: &mut SrtConnection, now: Timestamp) {
        while let Some(output) = caller.poll_output() {
            if let ConnectionOutput::SendPacket(packet) = output {
                listener
                    .feed_recv_buf(&packet, now)
                    .expect("packet should decode");
            }
        }
    }

    fn establish_pair(socket_id: u32) -> (SrtConnection, SrtConnection) {
        let mut caller = SrtConnection::new_caller(ConnectionOptions {
            socket_id,
            tsbpd_delay: 0,
            ..Default::default()
        });
        let mut listener = SrtConnection::new_listener(ConnectionOptions {
            tsbpd_delay: 0,
            ..Default::default()
        });
        caller
            .connect(Timestamp::from_micros(0))
            .expect("caller should connect");
        for round in 0..10 {
            let now = Timestamp::from_micros(round * 10_000);
            transfer(&mut caller, &mut listener, now);
            while let Some(output) = listener.poll_output() {
                if let ConnectionOutput::SendPacket(packet) = output {
                    caller
                        .feed_recv_buf(&packet, now)
                        .expect("response should decode");
                }
            }
            if caller.state() == ConnectionState::Connected
                && listener.state() == ConnectionState::Connected
            {
                while caller.poll_event().is_some() {}
                while listener.poll_event().is_some() {}
                return (caller, listener);
            }
        }
        panic!("pair did not connect");
    }

    fn data_packets(connection: &mut SrtConnection) -> Vec<Vec<u8>> {
        connection
            .poll_output()
            .into_iter()
            .filter_map(|output| match output {
                ConnectionOutput::SendPacket(packet)
                    if matches!(SrtPacket::decode(&packet), Ok(SrtPacket::Data(_))) =>
                {
                    Some(packet)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn same_peer_feedback_uses_destination_socket_id() {
        let peer = "127.0.0.1:9000".parse().expect("peer address");
        let mut members = HashMap::new();
        members.insert(101, 1);
        members.insert(202, 2);
        let mut packet = Vec::new();
        ControlPacket::new(ControlType::Ack, 0, 202).encode(&mut packet);

        assert_eq!(
            member_id_for_datagram(&[peer, peer], &members, peer, &packet),
            Some(2)
        );
    }

    #[test]
    fn unknown_destination_socket_id_falls_back_to_peer_tuple() {
        let peer = "127.0.0.1:9000".parse().expect("peer address");
        let mut members = HashMap::new();
        members.insert(101, 1);
        members.insert(202, 2);
        let mut packet = Vec::new();
        ControlPacket::new(ControlType::Ack, 0, 303).encode(&mut packet);

        assert_eq!(
            member_id_for_datagram(&[peer, peer], &members, peer, &packet),
            Some(1)
        );
    }

    #[test]
    fn same_peer_feedback_reaches_the_socket_id_member() {
        let (caller_a, _listener_a) = establish_pair(101);
        let (caller_b, mut listener_b) = establish_pair(202);
        let receiver = StdUdpSocket::bind(("127.0.0.1", 0)).expect("receiver should bind");
        receiver
            .set_nonblocking(true)
            .expect("receiver should be nonblocking");
        let receiver_addr = receiver.local_addr().expect("receiver address");
        let source = StdUdpSocket::bind(("127.0.0.1", 0)).expect("source should bind");
        let peer = source.local_addr().expect("source address");
        let caller_b_socket_id = caller_b.socket_id();

        listener_b
            .send(b"feedback", Timestamp::from_micros(100_000))
            .expect("listener should send feedback");
        let packet = data_packets(&mut listener_b)
            .into_iter()
            .next()
            .expect("feedback should produce a data packet");
        source
            .send_to(&packet, receiver_addr)
            .expect("feedback should reach the shared socket");

        let mut group = SrtGroup::new(0x4000_0005, GroupMode::Broadcast).expect("group");
        group.add_member(1, 100, caller_a).expect("member one");
        group.add_member(2, 100, caller_b).expect("member two");
        let mut sender = SrtRustGroupSender {
            socket: Some(UdpSocket::from_std(receiver)),
            peers: vec![peer, peer],
            group,
            member_by_socket_id: HashMap::from([(caller_b_socket_id, 2)]),
            timers: HashMap::new(),
            pending_datagrams: VecDeque::new(),
            recv_buf: vec![0; 64 * 1024],
            started: Instant::now(),
            closed: false,
        };

        sender
            .receive_packets(Timestamp::from_micros(100_000))
            .expect("feedback should be accepted");
        assert!(matches!(
            sender
                .group
                .member_mut(2)
                .expect("socket-ID member should exist")
                .connection_mut()
                .poll_event(),
            Some(ConnectionEvent::DataReceived { payload, .. }) if payload == b"feedback"
        ));
    }
}
