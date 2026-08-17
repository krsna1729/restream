//! Sustained-throughput SRT caller over the pure-Rust Core, on tokio -- the
//! tokio entry in the docs/srt-pure-rust-plan.md Phase 4 driver-framework
//! bake-off. See mio_driver.rs's doc comment for the shared background,
//! and Cargo.toml's doc comment for why each backend is a separate binary
//! rather than one shared trait.
//!
//! Usage: srt-interop-loss-caller-tokio <host> <port> <duration_secs> <latency_ms> [bitrate_bps]
//!
//! Runs on a `current_thread` tokio runtime (a single OS thread, no
//! work-stealing) -- the fairest comparison against mio's single-threaded
//! reactor loop; a multi-threaded runtime would spend cycles on
//! cross-thread waker/task migration that this one-connection benchmark
//! wouldn't exercise fairly.

use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, TimerId, Timestamp};
use srt_interop::tokio_driver::{drain_outputs, due_timers};
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

const PAYLOAD_SIZE: usize = 1316;
const DEFAULT_BITRATE_BPS: u64 = 8_000_000;
const CONNECT_TIMEOUT: Duration = srt_interop::INTEROP_CONNECT_TIMEOUT;
const MAX_WAIT: Duration = Duration::from_millis(20);
// sleep for wait-minus-this margin via tokio::time::sleep, then let the
// loop's own next iteration(s) spin off the last TAIL_SPIN syscall-free --
// see loss_caller_mio.rs's doc comment for the measured tradeoff (an
// earlier version here spun the *entire* wait below a fixed threshold and
// measured ~93% CPU use even at moderate bitrates).
const TAIL_SPIN: Duration = Duration::from_micros(300);

#[tokio::main(flavor = "current_thread")]
async fn main() {
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

    let socket = UdpSocket::bind("0.0.0.0:0").await.expect("bind");
    socket.connect(peer_addr).await.expect("connect");

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
    drain_outputs(&mut conn, &socket, &mut timers, now(start)).await;

    let payload = vec![0x42u8; PAYLOAD_SIZE];
    let mut total_sent: u64 = 0;
    let mut connected = false;
    let mut stream_deadline: Option<Instant> = None;
    let connect_deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut buf = [0u8; 2048];

    loop {
        if !connected && Instant::now() >= connect_deadline {
            eprintln!(
                "[loss-caller-tokio] connect timed out, state={:?}",
                conn.state()
            );
            break;
        }
        if let Some(d) = stream_deadline
            && Instant::now() >= d
        {
            break;
        }

        let wait = if connected {
            Duration::from_micros(conn.time_until_send(now(start))).min(MAX_WAIT)
        } else {
            MAX_WAIT
        };

        let block_for = wait.saturating_sub(TAIL_SPIN);
        if block_for > Duration::ZERO {
            tokio::select! {
                res = socket.recv(&mut buf) => {
                    if let Ok(n) = res {
                        let t = now(start);
                        let _ = conn.feed_recv_buf(&buf[..n], t);
                    }
                }
                _ = tokio::time::sleep(block_for) => {}
            }
        }

        // Drain any further packets already queued, non-blocking.
        loop {
            match socket.try_recv(&mut buf) {
                Ok(n) => {
                    let t = now(start);
                    let _ = conn.feed_recv_buf(&buf[..n], t);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("[loss-caller-tokio] recv error: {e}");
                    break;
                }
            }
        }

        let t = now(start);
        for id in due_timers(&mut timers, t) {
            let _ = conn.handle_timer(id, t);
        }
        drain_outputs(&mut conn, &socket, &mut timers, t).await;

        while let Some(ev) = conn.poll_event() {
            match ev {
                ConnectionEvent::Connected => {
                    connected = true;
                    println!("CONNECTED");
                    stream_deadline = Some(Instant::now() + Duration::from_secs_f64(duration_secs));
                }
                ConnectionEvent::Disconnected { reason } => {
                    eprintln!("[loss-caller-tokio] disconnected: {reason}");
                    stream_deadline = Some(Instant::now());
                }
                ConnectionEvent::Error(msg) => {
                    eprintln!("[loss-caller-tokio] error: {msg}");
                }
                _ => {}
            }
        }

        if connected {
            loop {
                let t = now(start);
                if !conn.can_send_with_pacing(t) {
                    break;
                }
                if conn.send(&payload, t).is_err() {
                    break;
                }
                total_sent += 1;
                drain_outputs(&mut conn, &socket, &mut timers, t).await;
            }
        }
    }

    let stats = conn.sender_stats();
    let elapsed_s = start.elapsed().as_secs_f64();
    let p = srt_interop::cpu_stats::process_stats();
    match stats {
        Some(s) => println!(
            "STATS role=caller backend=tokio pkt_sent={total_sent} pkt_sent_total={} \
             pkt_retrans_total={} pkt_loss_list_len={} elapsed_s={elapsed_s:.3} \
             cpu_user_ms={:.1} cpu_sys_ms={:.1} peak_rss_kb={}",
            s.total_sent,
            s.total_retransmits,
            s.packets_in_loss_list,
            p.cpu_user_ms,
            p.cpu_sys_ms,
            p.peak_rss_kb
        ),
        None => println!(
            "STATS role=caller backend=tokio pkt_sent={total_sent} pkt_sent_total=0 \
             pkt_retrans_total=0 pkt_loss_list_len=0 elapsed_s={elapsed_s:.3} \
             cpu_user_ms={:.1} cpu_sys_ms={:.1} peak_rss_kb={}",
            p.cpu_user_ms, p.cpu_sys_ms, p.peak_rss_kb
        ),
    }

    if !connected {
        std::process::exit(1);
    }
}
