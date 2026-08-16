//! Real UDP-backed SRT listener for interop testing against libsrt.
//! See docs/srt-pure-rust-plan.md Phase 3.
//!
//! Usage: srt-interop-listener <port> [passphrase]
//!
//! Single-peer only: waits for one datagram, `connect()`s the socket to
//! that sender, then drives the handshake. Not the eventual Phase 7
//! production listener (which owns N sockets across a thread pool) -- this
//! exists only to prove wire-level interop.
//!
//! With a passphrase, checks any received payload against the known test
//! payload `srt-interop-caller` sends -- used to live-verify the pure-Rust
//! AES-CTR/AES-KW/PBKDF2 crypto stack decrypts correctly against real
//! libsrt, not just that the handshake completes.

use shiguredo_srt::{ConnectionOptions, SrtConnection, Timestamp};
use srt_interop::driver;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

const EXPECTED_PAYLOAD: &[u8] = b"the quick brown fox jumps over the lazy dog 0123456789";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <port> [passphrase]", args[0]);
        std::process::exit(2);
    }
    let port = &args[1];
    let passphrase = args.get(2).filter(|s| !s.is_empty()).cloned();

    let socket = UdpSocket::bind(format!("0.0.0.0:{port}")).expect("bind");
    eprintln!("[listener] listening on port {port}");

    let start = Instant::now();
    let mut buf = [0u8; 2048];
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout");
    let (n, peer) = socket
        .recv_from(&mut buf)
        .expect("recv_from (first packet)");
    socket.connect(peer).expect("connect to peer");
    eprintln!("[listener] first packet from {peer}, {n} bytes");

    let options = ConnectionOptions {
        socket_id: std::process::id(),
        passphrase,
        ..Default::default()
    };
    let mut conn = SrtConnection::new_listener(options);

    let now = Timestamp::from_micros(start.elapsed().as_micros() as u64);
    conn.feed_recv_buf(&buf[..n], now)
        .expect("feed first packet");

    let result = driver::run(
        &mut conn,
        &socket,
        start,
        Duration::from_secs(5),
        Duration::from_secs(2),
        |_, _, _| {},
    );

    for e in &result.events {
        eprintln!("[listener] {e}");
    }
    if result.connected {
        println!("CONNECTED peer_stream_id={:?}", conn.peer_stream_id());
        for payload in &result.received_payloads {
            let matches = payload.as_slice() == EXPECTED_PAYLOAD;
            println!(
                "RECEIVED {} bytes match_expected={matches} content={:?}",
                payload.len(),
                String::from_utf8_lossy(payload)
            );
        }
        std::process::exit(0);
    } else {
        println!("FAILED state={:?}", conn.state());
        std::process::exit(1);
    }
}
