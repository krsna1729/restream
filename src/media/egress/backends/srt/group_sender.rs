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
    GroupMemberState, GroupMode, KeyLength, SRTGROUP_MASK, SrtConnection, SrtGroup, TimerId,
    Timestamp,
};

use crate::media::egress::backend::{CloseReason, Readiness};
use crate::media::srt::{
    NativeSendBacklog, SrtEgressInterest, SrtMessageSender, SrtSendFailure, SrtSendResult,
    SrtSenderStats,
};

pub(super) struct SrtRustGroupSender {
    socket: Option<UdpSocket>,
    peers: Vec<SocketAddr>,
    group: SrtGroup,
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
        let group = SrtGroup::new(group_id, GroupMode::Backup)
            .map_err(|error| format!("create Rust SRT group: {error}"))?;
        let mut sender = Self {
            socket: Some(socket),
            peers,
            group,
            timers: HashMap::new(),
            pending_datagrams: VecDeque::new(),
            recv_buf: vec![0; 64 * 1024],
            started,
            closed: false,
        };
        for (index, _) in sender.peers.iter().enumerate() {
            let weight = if index == 0 { 1 } else { 0 };
            let mut options = group_options(config, latency, maxbw, fc, group_id, weight)?;
            options.socket_id = nonzero_random_u32();
            let mut connection = SrtConnection::new_caller(options);
            connection
                .connect(sender.now())
                .map_err(|error| format!("start Rust SRT group handshake: {error}"))?;
            sender
                .group
                .add_member(index as u32 + 1, weight, connection)
                .map_err(|error| format!("add Rust SRT group member: {error}"))?;
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
            let Some(member_id) = self.peers.iter().position(|candidate| *candidate == peer) else {
                continue;
            };
            let Some(member) = self.group.member_mut(member_id as u32 + 1) else {
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
    latency: i32,
    maxbw: i64,
    fc: i32,
    group_id: u32,
    weight: u16,
) -> Result<ConnectionOptions, String> {
    let mut options = ConnectionOptions {
        tsbpd_delay: latency.clamp(0, u16::MAX as i32) as u16,
        stream_id: Some(config.stream_id().to_string()),
        max_bandwidth_bytes_per_sec: (maxbw > 0).then_some((maxbw / 8) as u64),
        flow_window_packets: fc.max(32) as u32,
        group_extension: Some(GroupExtensionData {
            group_id,
            group_type: shiguredo_srt::GroupType::Backup,
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
