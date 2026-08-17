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
//! the sustained receive loop -- via mio, not blocking sockets; see
//! mio_driver.rs's doc comment for why.
//!
//! Unlike the C helper, no rolling-snapshot workaround is needed here:
//! `ReceiverStats` is a plain in-memory getter on our own Core state, not a
//! socket-state query (`srt_bistats`) that can race the peer's close and
//! fail -- a single read after the loop exits is always safe.

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, TimerId, Timestamp};
use srt_interop::mio_driver::{drain_outputs, due_timers};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const SOCKET: Token = Token(0);
// Upper bound on the mio poll timeout so the loop still notices
// connect_deadline/stream_deadline promptly even when idle (e.g. no ACK
// timer armed yet because the caller hasn't connected).
const MAX_POLL_WAIT: Duration = Duration::from_millis(20);

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
    let port: u16 = args[1].parse().expect("port");
    let duration_secs: f64 = args[2].parse().expect("duration_secs");
    let latency_ms: u16 = args[3].parse().expect("latency_ms");

    let mut socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))).expect("bind");
    println!("LISTENING");

    let mut poll = Poll::new().expect("mio Poll::new");
    poll.registry()
        .register(&mut socket, SOCKET, Interest::READABLE)
        .expect("register socket");
    let mut events = Events::with_capacity(128);

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
    let connect_deadline = Instant::now() + srt_interop::INTEROP_CONNECT_TIMEOUT;
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

        // Track the earliest-due armed timer (e.g. the 10ms ACK timer),
        // capped at MAX_POLL_WAIT -- a fixed poll interval here would add
        // up to its own duration of jitter to when ACKs actually go out,
        // polluting the RTT measurement this differential matrix exists
        // to make.
        let wait = Duration::from_micros(srt_interop::mio_driver::time_until_earliest_timer(
            &timers,
            now(start),
            MAX_POLL_WAIT.as_micros() as u64,
        ))
        .min(MAX_POLL_WAIT);
        poll.poll(&mut events, Some(wait)).ok();

        // recv_from, not recv: until the first packet arrives there is no
        // connected peer to restrict to, and a stray/malformed first
        // datagram (this is a real UDP socket exposed to netem-impaired
        // traffic, not a controlled test fixture) must not crash the
        // process -- feed_recv_buf errors are ignored here exactly like
        // every later packet, not unwrapped.
        loop {
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
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("[loss-listener] recv error: {e}");
                    break;
                }
            }
        }

        let t = now(start);
        for id in due_timers(&mut timers, t) {
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
    let p = srt_interop::cpu_stats::process_stats();
    match stats {
        // total_lost is directly comparable to libsrt's pktRcvLossTotal.
        // total_duplicates has no direct libsrt-side counterpart printed by
        // the C helper (which reports pktRcvDropTotal, TLPKTDROP-driven --
        // a different concept); reported here honestly as duplicates, not
        // relabeled as drops.
        Some(s) => println!(
            "STATS role=listener backend=mio pkt_recv={total_received} pkt_recv_total={} \
             pkt_rcv_loss_total={} pkt_rcv_dup_total={} rtt_ms={:.3} elapsed_s={elapsed_s:.3} \
             cpu_user_ms={:.1} cpu_sys_ms={:.1} peak_rss_kb={}",
            s.total_received,
            s.total_lost,
            s.total_duplicates,
            s.rtt as f64 / 1000.0,
            p.cpu_user_ms,
            p.cpu_sys_ms,
            p.peak_rss_kb
        ),
        None => println!(
            "STATS role=listener backend=mio pkt_recv={total_received} pkt_recv_total=0 \
             pkt_rcv_loss_total=0 pkt_rcv_dup_total=0 rtt_ms=0.000 elapsed_s={elapsed_s:.3} \
             cpu_user_ms={:.1} cpu_sys_ms={:.1} peak_rss_kb={}",
            p.cpu_user_ms, p.cpu_sys_ms, p.peak_rss_kb
        ),
    }

    if !connected {
        std::process::exit(1);
    }
}
