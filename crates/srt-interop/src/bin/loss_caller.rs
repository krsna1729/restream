//! Sustained-throughput SRT caller over the pure-Rust Core, paced to a
//! target bitrate for a configured duration -- the Rust-side counterpart to
//! test/native/srt-loss-caller.c, for docs/srt-pure-rust-plan.md Phase 4's
//! differential loss/latency testing (Rust Core vs libsrt under identical
//! tc netem impairment). Unlike caller.rs (a Phase 3 one-shot wire-interop
//! check), this sends for the full nominal duration and reports the Core's
//! own SenderStats at the end, in a format comparable to (not identical to)
//! the C helper's STATS line -- see field notes below.
//!
//! Usage: srt-interop-loss-caller <host> <port> <duration_secs> <latency_ms> [bitrate_bps]
//!
//! Pacing is delegated to the Core itself (`ConnectionOptions::
//! max_bandwidth_bytes_per_sec` + `can_send_with_pacing`); the driver's job
//! is only to wake up at exactly the right time, via mio -- see
//! mio_driver.rs's doc comment for why a blocking-socket/thread::sleep
//! driver isn't good enough here. Even with mio, `poll()`'s own syscall
//! latency was measured to cap achieved throughput at ~55-60% of the 8 Mbps
//! nominal target (worse at higher bitrates, better at lower -- the
//! signature of a fixed per-lap cost against a shrinking pacing period);
//! spinning the loop itself (no syscall) below SPIN_THRESHOLD instead of
//! calling `poll()` brought that to ~78-90% and RTT down to match libsrt's
//! own baseline (~0.3-2ms vs libsrt's ~0.2-0.7ms, both far below the naive
//! thread::sleep driver's earlier 1.8-3ms). As with the old driver, the
//! orchestration script should still read achieved throughput from each
//! cell's own STATS line rather than assume the requested bitrate was hit.

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, Timestamp};
use srt_interop::mio_driver::{drain_outputs, due_timers};
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

const PAYLOAD_SIZE: usize = 1316;
const DEFAULT_BITRATE_BPS: u64 = 8_000_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET: Token = Token(0);
// Upper bound on the mio poll timeout so the loop still notices
// connect_deadline/stream_deadline promptly even when there's nothing else
// pending (e.g. still waiting on the handshake).
const MAX_POLL_WAIT: Duration = Duration::from_millis(20);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!(
            "usage: {} <host> <port> <duration_secs> <latency_ms> [bitrate_bps]",
            args[0]
        );
        std::process::exit(2);
    }
    let host = &args[1];
    let port = &args[2];
    let duration_secs: f64 = args[3].parse().expect("duration_secs");
    let latency_ms: u16 = args[4].parse().expect("latency_ms");
    let bitrate_bps: u64 = args
        .get(5)
        .map(|s| s.parse().expect("bitrate_bps"))
        .unwrap_or(DEFAULT_BITRATE_BPS);

    let peer_addr = format!("{host}:{port}")
        .to_socket_addrs()
        .expect("resolve host:port")
        .next()
        .expect("no address resolved");

    let mut socket = UdpSocket::bind("0.0.0.0:0".parse().unwrap()).expect("bind");
    socket.connect(peer_addr).expect("connect");

    let mut poll = Poll::new().expect("mio Poll::new");
    poll.registry()
        .register(&mut socket, SOCKET, Interest::READABLE)
        .expect("register socket");
    let mut events = Events::with_capacity(128);

    let options = ConnectionOptions {
        socket_id: std::process::id(),
        tsbpd_delay: latency_ms,
        max_bandwidth_bytes_per_sec: Some(bitrate_bps / 8),
        ..Default::default()
    };
    let mut conn = SrtConnection::new_caller(options);

    let start = Instant::now();
    let now = |start: Instant| Timestamp::from_micros(start.elapsed().as_micros() as u64);
    conn.connect(now(start))
        .expect("connect() should queue INDUCTION");

    let mut timers: HashMap<shiguredo_srt::TimerId, Timestamp> = HashMap::new();
    drain_outputs(&mut conn, &socket, &mut timers, now(start));

    let payload = vec![0x42u8; PAYLOAD_SIZE];
    let mut total_sent: u64 = 0;
    let mut connected = false;
    let mut stream_deadline: Option<Instant> = None;
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut buf = [0u8; 2048];

    loop {
        if !connected && Instant::now() >= connect_deadline {
            eprintln!("[loss-caller] connect timed out, state={:?}", conn.state());
            break;
        }
        if let Some(d) = stream_deadline
            && Instant::now() >= d
        {
            break;
        }

        let wait = if connected {
            Duration::from_micros(conn.time_until_send(now(start))).min(MAX_POLL_WAIT)
        } else {
            MAX_POLL_WAIT
        };
        // Below SPIN_THRESHOLD, skip the epoll_wait syscall and let the
        // loop itself spin (non-blocking recv + pacing recheck each lap):
        // at high pacing rates (e.g. ~1.36ms/packet at 8 Mbps) the syscall
        // cost of poll() is a large enough fraction of the period that
        // calling it unconditionally on every lap was measured to cap
        // achieved throughput well below target (56% of nominal at 8 Mbps,
        // rising smoothly to 83% at 1 Mbps -- the signature of a fixed
        // per-lap cost against a shrinking period, not an OS timer-
        // granularity wall). Above the threshold there's genuine idle time
        // worth actually blocking for, so still use poll() there.
        const SPIN_THRESHOLD: Duration = Duration::from_millis(3);
        if wait > SPIN_THRESHOLD
            && let Err(e) = poll.poll(&mut events, Some(wait))
        {
            // EINTR and similar are routine under a signal-heavy test host;
            // just loop back and recompute the wait.
            if e.kind() != std::io::ErrorKind::Interrupted {
                eprintln!("[loss-caller] poll error: {e}");
                break;
            }
        }

        loop {
            match socket.recv(&mut buf) {
                Ok(n) => {
                    let t = now(start);
                    let _ = conn.feed_recv_buf(&buf[..n], t);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("[loss-caller] recv error: {e}");
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
                ConnectionEvent::Disconnected { reason } => {
                    eprintln!("[loss-caller] disconnected: {reason}");
                    stream_deadline = Some(Instant::now());
                }
                ConnectionEvent::Error(msg) => {
                    eprintln!("[loss-caller] error: {msg}");
                }
                _ => {}
            }
        }

        if connected {
            // Drain every packet the Core's own pacer currently allows,
            // matching how a real Driver would behave (send until not
            // ready, then wait for the next event/timer).
            loop {
                let t = now(start);
                if !conn.can_send_with_pacing(t) {
                    break;
                }
                if conn.send(&payload, t).is_err() {
                    break;
                }
                total_sent += 1;
                drain_outputs(&mut conn, &socket, &mut timers, t);
            }
        }
    }

    let stats = conn.sender_stats();
    let elapsed_s = start.elapsed().as_secs_f64();
    match stats {
        // total_retransmits/packets_in_loss_list are directly comparable to
        // libsrt's pktRetransTotal/current loss-list size. Sender-side RTT
        // is not currently exposed by SenderStats (only ReceiverStats
        // tracks it) -- the loss-listener's STATS line is the RTT source of
        // record for this differential comparison.
        Some(s) => println!(
            "STATS role=caller pkt_sent={total_sent} pkt_sent_total={} \
             pkt_retrans_total={} pkt_loss_list_len={} elapsed_s={elapsed_s:.3}",
            s.total_sent, s.total_retransmits, s.packets_in_loss_list
        ),
        None => println!(
            "STATS role=caller pkt_sent={total_sent} pkt_sent_total=0 \
             pkt_retrans_total=0 pkt_loss_list_len=0 elapsed_s={elapsed_s:.3}"
        ),
    }

    if !connected {
        std::process::exit(1);
    }
}
