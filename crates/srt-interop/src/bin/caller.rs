//! Real UDP-backed SRT caller for interop testing against libsrt.
//! See docs/srt-pure-rust-plan.md Phase 3.
//!
//! Usage: srt-interop-caller <host> <port> [stream_id] [passphrase]
//!
//! With a passphrase, sends a known payload after connecting -- used to
//! live-verify the pure-Rust AES-CTR/AES-KW/PBKDF2 crypto stack decrypts
//! correctly against real libsrt, not just that the handshake completes.

use shiguredo_srt::{ConnectionOptions, SrtConnection, Timestamp};
use srt_interop::driver;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

const TEST_PAYLOAD: &[u8] = b"the quick brown fox jumps over the lazy dog 0123456789";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} <host> <port> [stream_id] [passphrase]", args[0]);
        std::process::exit(2);
    }
    let host = &args[1];
    let port = &args[2];
    let stream_id = args.get(3).filter(|s| !s.is_empty()).cloned();
    let passphrase = args.get(4).filter(|s| !s.is_empty()).cloned();

    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind");
    socket
        .connect(format!("{host}:{port}"))
        .expect("connect (UDP, no handshake yet)");

    let crypto_salt = passphrase.as_ref().map(|_| {
        let mut salt = [0u8; 16];
        // Fixed, not cryptographically random -- fine for an interop test
        // binary; a real caller (Phase 6/7 Driver) must use real randomness.
        for (i, b) in salt.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        salt
    });
    let crypto_sek = passphrase.as_ref().map(|_| vec![0x5Au8; 16]);

    let options = ConnectionOptions {
        socket_id: std::process::id(),
        stream_id,
        passphrase,
        crypto_salt,
        crypto_sek,
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
        Duration::from_millis(300),
        |conn, _socket, now| {
            if let Err(e) = conn.send(TEST_PAYLOAD, now) {
                eprintln!("[caller] send failed: {e}");
            }
        },
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
