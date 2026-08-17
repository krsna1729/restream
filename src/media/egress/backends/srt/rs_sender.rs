use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::time::Instant;

use bytes::Bytes;
use mio::net::UdpSocket;
use rand::RngExt;
use shiguredo_srt::{
    ConnectionOptions, ConnectionOutput, ConnectionState, ErrorKind, KeyLength, SrtConnection,
    TimerId, Timestamp,
};

use crate::media::egress::backend::{CloseReason, Readiness};
use crate::media::srt::{
    NativeSendBacklog, SrtEgressInterest, SrtLeafHandle, SrtMessageSender, SrtSendFailure,
    SrtSendResult, SrtSenderStats,
};

pub(crate) struct SrtRustMessageSender {
    socket: Option<UdpSocket>,
    peer: SocketAddr,
    conn: SrtConnection,
    timers: HashMap<TimerId, Timestamp>,
    pending_datagrams: VecDeque<Vec<u8>>,
    recv_buf: Vec<u8>,
    started: Instant,
    closed: bool,
}

impl SrtRustMessageSender {
    pub(crate) fn connect(
        config: &crate::media::srt::SrtFabricEgressConnectConfig<'_>,
    ) -> Result<Self, String> {
        let peer = match config.peer_addrs() {
            [peer] => *peer,
            [] => return Err("Rust SRT caller requires one peer address".to_string()),
            _ => return Err("Rust SRT caller does not support bonding yet".to_string()),
        };
        let (sndbuf, rcvbuf, latency, maxbw, _fc) = config.buffer_parameters();
        let std_socket = StdUdpSocket::bind(("0.0.0.0", 0))
            .map_err(|error| format!("bind Rust SRT caller socket: {error}"))?;
        configure_udp_buffer(std_socket.as_raw_fd(), libc::SO_SNDBUF, sndbuf)?;
        configure_udp_buffer(std_socket.as_raw_fd(), libc::SO_RCVBUF, rcvbuf)?;
        std_socket
            .set_nonblocking(true)
            .map_err(|error| format!("set Rust SRT caller nonblocking: {error}"))?;
        std_socket
            .connect(peer)
            .map_err(|error| format!("connect Rust SRT caller socket to {peer}: {error}"))?;
        let socket = UdpSocket::from_std(std_socket);
        let started = Instant::now();
        let mut options = ConnectionOptions {
            socket_id: nonzero_random_u32(),
            tsbpd_delay: latency.clamp(0, u16::MAX as i32) as u16,
            stream_id: Some(config.stream_id().to_string()),
            max_bandwidth_bytes_per_sec: (maxbw > 0).then_some((maxbw / 8) as u64),
            ..ConnectionOptions::default()
        };
        if let Some((passphrase, pbkeylen)) = config.crypto_parameters() {
            let key_length = match pbkeylen {
                16 => KeyLength::Aes128,
                32 => KeyLength::Aes256,
                other => {
                    return Err(format!(
                        "Rust SRT caller supports pbkeylen 16 or 32, got {other}"
                    ));
                }
            };
            let mut salt = [0u8; 16];
            let mut sek = vec![0u8; key_length.len()];
            rand::rng().fill(&mut salt);
            rand::rng().fill(sek.as_mut_slice());
            options.passphrase = Some(passphrase.to_string());
            options.key_length = key_length;
            options.crypto_salt = Some(salt);
            options.crypto_sek = Some(sek);
        }
        let mut sender = Self {
            socket: Some(socket),
            peer,
            conn: SrtConnection::new_caller(options),
            timers: HashMap::new(),
            pending_datagrams: VecDeque::new(),
            recv_buf: vec![0; 64 * 1024],
            started,
            closed: false,
        };
        let now = sender.now();
        sender
            .conn
            .connect(now)
            .map_err(|error| format!("start Rust SRT caller handshake: {error}"))?;
        sender.flush_outputs(now)?;
        Ok(sender)
    }

    pub(crate) fn raw_fd(&self) -> Option<RawFd> {
        self.socket.as_ref().map(AsRawFd::as_raw_fd)
    }

    fn now(&self) -> Timestamp {
        Timestamp::from_micros(self.started.elapsed().as_micros() as u64)
    }

    fn flush_outputs(&mut self, now: Timestamp) -> Result<(), String> {
        while let Some(packet) = self.pending_datagrams.pop_front() {
            match self.send_datagram(&packet) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.pending_datagrams.push_front(packet);
                    return Ok(());
                }
                Err(error) => {
                    return Err(format!("send Rust SRT datagram to {}: {error}", self.peer));
                }
            }
        }
        while let Some(output) = self.conn.poll_output() {
            match output {
                ConnectionOutput::SendPacket(packet) => match self.send_datagram(&packet) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.pending_datagrams.push_back(packet);
                        return Ok(());
                    }
                    Err(error) => {
                        return Err(format!("send Rust SRT datagram to {}: {error}", self.peer));
                    }
                },
                ConnectionOutput::SetTimer {
                    id,
                    duration_micros,
                } => {
                    self.timers.insert(id, now.add_micros(duration_micros));
                }
                ConnectionOutput::ClearTimer { id } => {
                    self.timers.remove(&id);
                }
            }
        }
        Ok(())
    }

    fn send_datagram(&self, packet: &[u8]) -> io::Result<()> {
        self.socket
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Rust SRT socket closed"))?
            .send(packet)
            .map(|_| ())
    }

    fn process_timers(&mut self, now: Timestamp) -> Result<(), String> {
        let due: Vec<TimerId> = self
            .timers
            .iter()
            .filter_map(|(id, deadline)| (now.as_micros() >= deadline.as_micros()).then_some(*id))
            .collect();
        for id in due {
            self.timers.remove(&id);
            self.conn
                .handle_timer(id, now)
                .map_err(|error| format!("Rust SRT timer {id:?}: {error}"))?;
        }
        Ok(())
    }

    fn receive_packets(&mut self, now: Timestamp) -> Result<(), String> {
        loop {
            let result = self
                .socket
                .as_ref()
                .ok_or_else(|| "Rust SRT socket closed".to_string())?
                .recv(&mut self.recv_buf);
            let size = match result {
                Ok(size) => size,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("receive Rust SRT datagram: {error}")),
            };
            self.conn
                .feed_recv_buf(&self.recv_buf[..size], now)
                .map_err(|error| format!("process Rust SRT datagram: {error}"))?;
        }
        Ok(())
    }
}

impl SrtMessageSender for SrtRustMessageSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        if self.closed || self.conn.state() != ConnectionState::Connected {
            return SrtSendResult::WouldBlock;
        }
        let now = self.now();
        if let Err(error) = self.conn.send(message, now) {
            return if error.kind == ErrorKind::InvalidState {
                SrtSendResult::WouldBlock
            } else {
                SrtSendResult::Failed(SrtSendFailure {
                    reason: "rust_srt_send",
                    detail: error.to_string(),
                    retryable: true,
                })
            };
        }
        match self.flush_outputs(now) {
            Ok(()) => SrtSendResult::Accepted {
                bytes: message.len(),
            },
            Err(detail) => SrtSendResult::Failed(SrtSendFailure {
                reason: "rust_srt_datagram",
                detail,
                retryable: true,
            }),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        self.closed = true;
        let now = self.now();
        self.conn.disconnect(now);
        let _ = self.flush_outputs(now);
        self.socket.take();
        self.pending_datagrams.clear();
    }

    fn on_readiness(&mut self, readiness: Readiness) {
        if self.closed {
            return;
        }
        let now = self.now();
        let result = self
            .process_timers(now)
            .and_then(|()| {
                readiness
                    .readable
                    .then(|| self.receive_packets(now))
                    .transpose()
                    .map(|_| ())
            })
            .and_then(|()| self.flush_outputs(now));
        if result.is_err() {
            self.closed = true;
        }
        while self.conn.poll_event().is_some() {}
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
                .map(|packet| packet.len() as u64)
                .sum(),
            packets: self.pending_datagrams.len().min(u32::MAX as usize) as u32,
            ms: 0,
        })
    }

    fn sender_quality_stats(&self) -> Option<SrtSenderStats> {
        let stats = self.conn.sender_stats()?;
        Some(SrtSenderStats {
            packets_retransmit_total: stats.total_retransmits as u64,
            send_buf_bytes: stats.packets_in_buffer as i32,
            send_buf_available_bytes: 0,
            flight_size_packets: stats.packets_in_buffer as i32,
            ..SrtSenderStats::default()
        })
    }
}

fn configure_udp_buffer(fd: RawFd, option: libc::c_int, value: i32) -> Result<(), String> {
    if value <= 0 {
        return Ok(());
    }
    let result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            &value as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "set Rust SRT UDP buffer {option}={value}: {}",
            io::Error::last_os_error()
        ))
    }
}

fn nonzero_random_u32() -> u32 {
    let value = rand::random::<u32>();
    if value == 0 { 1 } else { value }
}

pub(crate) fn connected_transport(
    config: crate::media::srt::SrtFabricEgressConnectConfig<'_>,
) -> Result<super::SrtConnectedTransport, String> {
    let sender = SrtRustMessageSender::connect(&config)?;
    let handle = sender
        .raw_fd()
        .map(SrtLeafHandle::Rust)
        .ok_or_else(|| "Rust SRT caller socket was closed during connect".to_string())?;
    let (sndbuf, _, _, _, _) = config.buffer_parameters();
    Ok(super::SrtConnectedTransport {
        handle,
        sender: Box::new(sender),
        configured_sndbuf: Some(sndbuf),
    })
}
