//! Real UDP-backed SRT caller for interop testing against libsrt.
//! See docs/srt-pure-rust-plan.md Phase 3.
//!
//! Usage: srt-interop-caller <host> <port> [stream_id]

use shiguredo_srt::{ConnectionOptions, SrtConnection, Timestamp};
use srt_interop::driver;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <host> <port> [stream_id]", args[0]);
        std::process::exit(2);
    }
    let host = &args[1];
    let port = &args[2];
    let stream_id = args.get(3).cloned();

    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind");
    socket
        .connect(format!("{host}:{port}"))
        .expect("connect (UDP, no handshake yet)");

    let options = ConnectionOptions {
        socket_id: std::process::id(),
        stream_id,
        ..Default::default()
    };
    let mut conn = SrtConnection::new_caller(options);

    let start = Instant::now();
    let now = Timestamp::from_micros(start.elapsed().as_micros() as u64);
    conn.connect(now).expect("connect() should queue INDUCTION");

    let result = driver::run(
        &mut conn,
        &socket,
        start,
        Duration::from_secs(5),
        |_, _, _| {},
    );

    for e in &result.events {
        eprintln!("[caller] {e}");
    }
    if result.connected {
        println!("CONNECTED peer_stream_id={:?}", conn.peer_stream_id());
        std::process::exit(0);
    } else {
        println!("FAILED state={:?}", conn.state());
        std::process::exit(1);
    }
}
