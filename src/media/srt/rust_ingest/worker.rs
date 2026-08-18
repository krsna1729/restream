use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use shiguredo_srt::Timestamp;
use tokio::sync::mpsc::{Receiver, Sender};

use super::connection::RustConnection;
use super::types::{IngestEvent, WorkerCommand};

#[path = "pump.rs"]
mod pump;

#[derive(Clone)]
pub(super) struct WorkerOptions {
    pub(super) passphrase: Option<String>,
    pub(super) key_length: shiguredo_srt::KeyLength,
    pub(super) tsbpd_delay: u16,
}

pub(super) fn spawn(
    worker_index: usize,
    socket: std::net::UdpSocket,
    stop: Arc<AtomicBool>,
    commands: Receiver<WorkerCommand>,
    events: Sender<IngestEvent>,
    options: WorkerOptions,
    policy_store: Arc<super::super::SrtIngestPolicyStore>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("restream-srt-rust-ingest-{worker_index}"))
        .spawn(move || {
            run(
                worker_index,
                socket,
                stop,
                commands,
                events,
                options,
                policy_store,
            )
        })
}

fn run(
    worker_index: usize,
    std_socket: std::net::UdpSocket,
    stop: Arc<AtomicBool>,
    mut commands: Receiver<WorkerCommand>,
    events: Sender<IngestEvent>,
    options: WorkerOptions,
    policy_store: Arc<super::super::SrtIngestPolicyStore>,
) {
    let mut socket = UdpSocket::from_std(std_socket);
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(error) => {
            tracing::error!(worker = worker_index, %error, "Rust SRT ingest poll creation failed");
            return;
        }
    };
    if let Err(error) = poll
        .registry()
        .register(&mut socket, Token(0), Interest::READABLE)
    {
        tracing::error!(worker = worker_index, %error, "Rust SRT ingest socket registration failed");
        return;
    }

    let start = Instant::now();
    let mut next_socket_id = (std::process::id() as u32)
        .wrapping_add((worker_index as u32).wrapping_mul(0x10001))
        .max(1);
    let mut connections = std::collections::HashMap::<std::net::SocketAddr, RustConnection>::new();
    let mut packet = vec![0u8; 64 * 1024];
    let mut poll_events = Events::with_capacity(1);

    while !stop.load(Ordering::Acquire) {
        if pump::process_commands(
            &mut commands,
            &mut connections,
            &socket,
            &events,
            timestamp(start),
        ) {
            break;
        }
        let wait = pump::poll_wait(&connections, timestamp(start));
        if let Err(error) = poll.poll(&mut poll_events, Some(wait))
            && error.kind() != io::ErrorKind::Interrupted
        {
            tracing::error!(worker = worker_index, %error, "Rust SRT ingest poll failed");
            break;
        }
        for event in &poll_events {
            if event.token() == Token(0)
                && !pump::receive_packets(&mut pump::ReceiveState {
                    socket: &mut socket,
                    connections: &mut connections,
                    events: &events,
                    options: &options,
                    policy_store: &policy_store,
                    worker_index,
                    next_socket_id: &mut next_socket_id,
                    start,
                    packet: &mut packet,
                })
            {
                stop.store(true, Ordering::Release);
                break;
            }
        }
        if !pump::service_timers(&mut connections, &mut socket, &events, timestamp(start)) {
            stop.store(true, Ordering::Release);
        }
    }
}

fn timestamp(start: Instant) -> Timestamp {
    Timestamp::from_micros(start.elapsed().as_micros() as u64)
}
