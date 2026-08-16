//! Sustained-throughput SRT listener over the pure-Rust Core, for
//! docs/srt-pure-rust-plan.md Phase 4's differential loss/latency testing --
//! the Rust-side counterpart to test/native/srt-loss-listener.c. Unlike
//! listener.rs (a Phase 3 one-shot wire-interop check), this receives for a
//! configured duration under injected network impairment (tc netem, applied
//! by the orchestration script) and reports the Core's own ReceiverStats at
//! the end, in a format comparable to (not identical to) the C helper's
//! STATS line -- see field notes below.
//!
//! Usage: srt-interop-loss-listener <port> <duration_secs> <latency_ms>
//!
//! Single-peer only, same as listener.rs: waits for one datagram,
//! `connect()`s the socket to that sender, then drives the handshake and
//! the sustained receive loop.
//!
//! Unlike the C helper, no rolling-snapshot workaround is needed here:
//! `ReceiverStats` is a plain in-memory getter on our own Core state, not a
//! socket-state query (`srt_bistats`) that can race the peer's close and
//! fail -- a single read after the loop exits is always safe.

use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, TimerId, Timestamp};
use srt_interop::driver::drain_outputs;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: {} <port> <duration_secs> <latency_ms>", args[0]);
        std::process::exit(2);
    }
    let port = &args[1];
    let duration_secs: f64 = args[2].parse().expect("duration_secs");
    let latency_ms: u16 = args[3].parse().expect("latency_ms");

    let socket = UdpSocket::bind(format!("0.0.0.0:{port}")).expect("bind");
    println!("LISTENING");
    // Short poll timeout throughout, including while waiting for the
    // caller's first packet -- a single fixed 2ms timeout keeps the
    // bootstrap and steady-state phases the same loop (see below).
    socket
        .set_read_timeout(Some(Duration::from_millis(2)))
        .expect("set_read_timeout");

    let start = Instant::now();
    let now = |start: Instant| Timestamp::from_micros(start.elapsed().as_micros() as u64);

    let options = ConnectionOptions {
        socket_id: std::process::id(),
        tsbpd_delay: latency_ms,
        ..Default::default()
    };
    let mut conn = SrtConnection::new_listener(options);
    let mut timers: HashMap<TimerId, Timestamp> = HashMap::new();

    let mut total_received: u64 = 0;
    let mut connected = false;
    let mut stream_deadline: Option<Instant> = None;
    let connect_deadline = Instant::now() + Duration::from_secs(5);
    let mut peer: Option<SocketAddr> = None;
    let mut buf = [0u8; 2048];

    loop {
        if !connected && Instant::now() >= connect_deadline {
            eprintln!(
                "[loss-listener] connect timed out, state={:?}",
                conn.state()
            );
            break;
        }
        if let Some(d) = stream_deadline
            && Instant::now() >= d
        {
            break;
        }

        // recv_from, not recv: until the first packet arrives there is no
        // connected peer to restrict to, and a stray/malformed first
        // datagram (this is a real UDP socket exposed to netem-impaired
        // traffic, not a controlled test fixture) must not crash the
        // process -- feed_recv_buf errors are ignored here exactly like
        // every later packet, not unwrapped. A prior version used a
        // separate `.expect()`-based bootstrap for just the first packet
        // and could panic (default Rust panic exit code 101) if that
        // packet failed to parse as a valid handshake.
        match socket.recv_from(&mut buf) {
            Ok((n, addr)) => {
                if peer.is_none() {
                    if let Err(e) = socket.connect(addr) {
                        eprintln!("[loss-listener] connect to peer failed: {e}");
                        continue;
                    }
                    peer = Some(addr);
                }
                let t = now(start);
                let _ = conn.feed_recv_buf(&buf[..n], t);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("[loss-listener] recv error: {e}");
                break;
            }
        }

        let t = now(start);
        let due: Vec<TimerId> = timers
            .iter()
            .filter(|(_, deadline)| t.as_micros() >= deadline.as_micros())
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            timers.remove(&id);
            let _ = conn.handle_timer(id, t);
        }
        drain_outputs(&mut conn, &socket, &mut timers, t);

        while let Some(ev) = conn.poll_event() {
            match ev {
                ConnectionEvent::Connected => {
                    connected = true;
                    println!("CONNECTED");
                    stream_deadline = Some(Instant::now() + Duration::from_secs_f64(duration_secs));
                }
                ConnectionEvent::DataReceived { .. } => {
                    total_received += 1;
                }
                ConnectionEvent::Disconnected { reason } => {
                    eprintln!("[loss-listener] disconnected: {reason}");
                    stream_deadline = Some(Instant::now());
                }
                ConnectionEvent::Error(msg) => {
                    eprintln!("[loss-listener] error: {msg}");
                }
                _ => {}
            }
        }
    }

    let stats = conn.receiver_stats();
    let elapsed_s = start.elapsed().as_secs_f64();
    match stats {
        // total_lost is directly comparable to libsrt's pktRcvLossTotal.
        // total_duplicates has no direct libsrt-side counterpart printed by
        // the C helper (which reports pktRcvDropTotal, TLPKTDROP-driven --
        // a different concept); reported here honestly as duplicates, not
        // relabeled as drops.
        Some(s) => println!(
            "STATS role=listener pkt_recv={total_received} pkt_recv_total={} \
             pkt_rcv_loss_total={} pkt_rcv_dup_total={} rtt_ms={:.3} elapsed_s={elapsed_s:.3}",
            s.total_received,
            s.total_lost,
            s.total_duplicates,
            s.rtt as f64 / 1000.0
        ),
        None => println!(
            "STATS role=listener pkt_recv={total_received} pkt_recv_total=0 \
             pkt_rcv_loss_total=0 pkt_rcv_dup_total=0 rtt_ms=0.000 elapsed_s={elapsed_s:.3}"
        ),
    }

    if !connected {
        std::process::exit(1);
    }
}
