//! Sustained-throughput SRT caller over the pure-Rust Core, on smol -- the
//! smol entry in the docs/srt-pure-rust-plan.md Phase 4 driver-framework
//! bake-off. smol is built from micro-crates (async-io/polling, itself a
//! readiness-based reactor much like mio) rather than tokio's integrated
//! runtime -- a useful data point for whether the extra reactor layer
//! between smol and the raw epoll fd costs anything measurable versus
//! mio's direct usage. See mio_driver.rs's doc comment for shared
//! background and Cargo.toml's doc comment for why each backend is a
//! separate binary rather than one shared trait.
//!
//! Usage: srt-interop-loss-caller-smol <host> <port> <duration_secs> <latency_ms> [bitrate_bps]

use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, TimerId, Timestamp};
use smol::Timer;
use smol::net::UdpSocket;
use srt_interop::smol_driver::{drain_outputs, due_timers, try_recv};
use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

const PAYLOAD_SIZE: usize = 1316;
const DEFAULT_BITRATE_BPS: u64 = 8_000_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_WAIT: Duration = Duration::from_millis(20);
// See loss_caller_mio.rs's doc comment for the measured tradeoff behind
// this "sleep the bulk, spin the tail" split.
const TAIL_SPIN: Duration = Duration::from_micros(300);

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
    let host = args[1].clone();
    let port = args[2].clone();
    let duration_secs: f64 = args[3].parse().expect("duration_secs");
    let latency_ms: u16 = args[4].parse().expect("latency_ms");
    let bitrate_bps: u64 = args
        .get(5)
        .map(|s| s.parse().expect("bitrate_bps"))
        .unwrap_or(DEFAULT_BITRATE_BPS);

    smol::block_on(async {
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
                    "[loss-caller-smol] connect timed out, state={:?}",
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
                let recv_fut = async { socket.recv(&mut buf).await.ok() };
                let timer_fut = async {
                    Timer::after(block_for).await;
                    None
                };
                if let Some(n) = futures_lite::future::or(recv_fut, timer_fut).await {
                    let t = now(start);
                    let _ = conn.feed_recv_buf(&buf[..n], t);
                }
            }

            // Drain any further packets already queued, non-blocking.
            while let Some(res) = try_recv(&socket, &mut buf) {
                match res {
                    Ok(n) => {
                        let t = now(start);
                        let _ = conn.feed_recv_buf(&buf[..n], t);
                    }
                    Err(_) => break,
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
                        stream_deadline =
                            Some(Instant::now() + Duration::from_secs_f64(duration_secs));
                    }
                    ConnectionEvent::Disconnected { reason } => {
                        eprintln!("[loss-caller-smol] disconnected: {reason}");
                        stream_deadline = Some(Instant::now());
                    }
                    ConnectionEvent::Error(msg) => {
                        eprintln!("[loss-caller-smol] error: {msg}");
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
                "STATS role=caller backend=smol pkt_sent={total_sent} pkt_sent_total={} \
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
                "STATS role=caller backend=smol pkt_sent={total_sent} pkt_sent_total=0 \
                 pkt_retrans_total=0 pkt_loss_list_len=0 elapsed_s={elapsed_s:.3} \
                 cpu_user_ms={:.1} cpu_sys_ms={:.1} peak_rss_kb={}",
                p.cpu_user_ms, p.cpu_sys_ms, p.peak_rss_kb
            ),
        }

        if !connected {
            std::process::exit(1);
        }
    });
}
