//! Shared smol-based low-level helpers for the sustained-throughput
//! loss-caller/loss-listener binaries -- the smol entry in the
//! docs/srt-pure-rust-plan.md Phase 4 driver-framework bake-off. See
//! mio_driver.rs's doc comment for why a blocking-socket/thread::sleep
//! driver isn't good enough, and Cargo.toml's doc comment for why each
//! backend gets its own small driver module rather than one shared trait.

use shiguredo_srt::{ConnectionOutput, SrtConnection, TimerId, Timestamp};
use std::collections::HashMap;

pub type UdpSocket = smol::Async<std::net::UdpSocket>;

pub async fn drain_outputs(
    conn: &mut SrtConnection,
    socket: &UdpSocket,
    timers: &mut HashMap<TimerId, Timestamp>,
    now: Timestamp,
) {
    while let Some(out) = conn.poll_output() {
        match out {
            ConnectionOutput::SendPacket(bytes) => {
                let _ = socket.write_with(|inner| inner.send(&bytes)).await;
            }
            ConnectionOutput::SetTimer {
                id,
                duration_micros,
            } => {
                timers.insert(id, now.add_micros(duration_micros));
            }
            ConnectionOutput::ClearTimer { id } => {
                timers.remove(&id);
            }
        }
    }
}

pub fn due_timers(timers: &mut HashMap<TimerId, Timestamp>, now: Timestamp) -> Vec<TimerId> {
    let due: Vec<TimerId> = timers
        .iter()
        .filter(|(_, deadline)| now.as_micros() >= deadline.as_micros())
        .map(|(id, _)| *id)
        .collect();
    for id in &due {
        timers.remove(id);
    }
    due
}

pub fn time_until_earliest_timer(
    timers: &HashMap<TimerId, Timestamp>,
    now: Timestamp,
    default_us: u64,
) -> u64 {
    timers
        .values()
        .map(|deadline| deadline.as_micros().saturating_sub(now.as_micros()))
        .min()
        .unwrap_or(default_us)
}

/// Attempt one non-blocking recv without actually awaiting readiness --
pub fn try_recv(socket: &UdpSocket, buf: &mut [u8]) -> Option<std::io::Result<usize>> {
    match socket.get_ref().recv(buf) {
        Ok(n) => Some(Ok(n)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(error) => Some(Err(error)),
    }
}

pub fn try_recv_from(
    socket: &UdpSocket,
    buf: &mut [u8],
) -> Option<std::io::Result<(usize, std::net::SocketAddr)>> {
    match socket.get_ref().recv_from(buf) {
        Ok((n, addr)) => Some(Ok((n, addr))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(error) => Some(Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smol::Async;
    use std::net::UdpSocket as StdUdpSocket;

    #[test]
    fn try_recv_reads_a_ready_datagram_without_awaiting() {
        let receiver = Async::<StdUdpSocket>::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("bind receiver");
        let sender = StdUdpSocket::bind("127.0.0.1:0").expect("bind sender");
        sender
            .send_to(
                b"ping",
                receiver.get_ref().local_addr().expect("receiver address"),
            )
            .expect("send datagram");

        let mut buf = [0u8; 4];
        let result = try_recv(&receiver, &mut buf)
            .expect("datagram should be ready")
            .expect("recv datagram");

        assert_eq!(result, 4);
        assert_eq!(&buf, b"ping");
    }
}
