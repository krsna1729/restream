//! Real UDP-backed SRT listener for interop testing against libsrt.
//! See docs/srt-pure-rust-plan.md Phase 3.
//!
//! Usage: srt-interop-listener <port>
//!
//! Single-peer only: waits for one datagram, `connect()`s the socket to
//! that sender, then drives the handshake. Not the eventual Phase 7
//! production listener (which owns N sockets across a thread pool) -- this
//! exists only to prove wire-level interop.

use shiguredo_srt::{ConnectionOptions, SrtConnection, Timestamp};
use srt_interop::driver;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <port>", args[0]);
        std::process::exit(2);
    }
    let port = &args[1];

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
        |_, _, _| {},
    );

    for e in &result.events {
        eprintln!("[listener] {e}");
    }
    if result.connected {
        println!("CONNECTED peer_stream_id={:?}", conn.peer_stream_id());
        std::process::exit(0);
    } else {
        println!("FAILED state={:?}", conn.state());
        std::process::exit(1);
    }
}
