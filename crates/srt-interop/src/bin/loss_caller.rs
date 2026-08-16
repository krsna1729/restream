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
//! max_bandwidth_bytes_per_sec` + `can_send_with_pacing`) rather than
//! reimplementing the C helper's app-level nanosleep deadline math -- this
//! is the real Driver's pacing model (Phase 6/7), so exercising it here is
//! more representative than a from-scratch pacer would be.
//!
//! This blocking single-thread driver's own `thread::sleep`-based wait loop
//! measurably undershoots the nominal target bitrate in practice (observed
//! 40-90% of target across runs) -- OS sleep/scheduling granularity, not a
//! Core pacing defect (verified by comparing measured send intervals
//! against the Core's own computed `packet_send_period` in isolation, and
//! by a deterministic in-process test with no real sockets/sleep involved
//! at all). The orchestration script for the differential matrix should
//! read achieved throughput from each cell's own STATS line
//! (`pkt_sent`/`pkt_recv`) rather than assume the requested bitrate was hit,
//! and compare loss/RTT as rates against that achieved baseline -- not as
//! an absolute-throughput benchmark, which this tool is not (see benches/
//! for that).

use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, TimerId, Timestamp};
use srt_interop::driver::drain_outputs;
use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

const PAYLOAD_SIZE: usize = 1316;
const DEFAULT_BITRATE_BPS: u64 = 8_000_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

    let socket = UdpSocket::bind("0.0.0.0:0").expect("bind");
    socket
        .connect(format!("{host}:{port}"))
        .expect("connect (UDP, no handshake yet)");
    // Non-blocking + a bounded thread::sleep for the pacing wait, rather
    // than blocking recv with a fixed SO_RCVTIMEO: setting the socket
    // timeout every loop iteration to track `time_until_send` was itself a
    // syscall on the hot path and, combined with a fixed short timeout,
    // quantized every pacing interval upward -- observed empirically as a
    // ~25-40% throughput shortfall against the requested bitrate before
    // this fix.
    socket.set_nonblocking(true).expect("set_nonblocking");
    const MAX_WAIT: Duration = Duration::from_millis(2);

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

    let mut timers: HashMap<TimerId, Timestamp> = HashMap::new();
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

        // Drain every packet currently available (non-blocking) before
        // deciding how long to wait.
        loop {
            match socket.recv(&mut buf) {
                Ok(n) => {
                    let t = now(start);
                    let _ = conn.feed_recv_buf(&buf[..n], t);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(e) => {
                    eprintln!("[loss-caller] recv error: {e}");
                    break;
                }
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
            // ready, then wait for the next event/timer) rather than
            // sending at most one packet per poll tick.
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
            let wait = Duration::from_micros(conn.time_until_send(now(start))).min(MAX_WAIT);
            if wait > Duration::ZERO {
                std::thread::sleep(wait);
            }
        } else {
            std::thread::sleep(MAX_WAIT);
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
