//! Sustained-throughput SRT listener over the pure-Rust Core, on monoio --
//! the monoio entry in the docs/srt-pure-rust-plan.md Phase 4
//! driver-framework bake-off. See loss_caller_monoio.rs and
//! mio_driver.rs's doc comments for shared background (completion-based
//! I/O, owned buffers handed to/from the kernel via io_uring).
//!
//! Usage: srt-interop-loss-listener-monoio <port> <duration_secs> <latency_ms>

use monoio::net::udp::UdpSocket;
use shiguredo_srt::{ConnectionEvent, ConnectionOptions, SrtConnection, TimerId, Timestamp};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const MAX_WAIT: Duration = Duration::from_millis(20);

fn due_timers(timers: &mut HashMap<TimerId, Timestamp>, now: Timestamp) -> Vec<TimerId> {
    let due: Vec<TimerId> = timers
        .iter()
        .filter(|(_, deadline)| now.as_micros() >= deadline.as_micros())
        .map(|(id, _)| *id)
        .collect();
    for id in &due {
        timers.remove(id);
    }
    due
}

fn time_until_earliest_timer(
    timers: &HashMap<TimerId, Timestamp>,
    now: Timestamp,
    default_us: u64,
) -> u64 {
    timers
        .values()
        .map(|deadline| deadline.as_micros().saturating_sub(now.as_micros()))
        .min()
        .unwrap_or(default_us)
}

async fn drain_outputs(
    conn: &mut SrtConnection,
    socket: &UdpSocket,
    timers: &mut HashMap<TimerId, Timestamp>,
    now: Timestamp,
) {
    while let Some(out) = conn.poll_output() {
        match out {
            shiguredo_srt::ConnectionOutput::SendPacket(bytes) => {
                let (_res, _buf) = socket.send(bytes).await;
            }
            shiguredo_srt::ConnectionOutput::SetTimer {
                id,
                duration_micros,
            } => {
                timers.insert(id, now.add_micros(duration_micros));
            }
            shiguredo_srt::ConnectionOutput::ClearTimer { id } => {
                timers.remove(&id);
            }
        }
    }
}

#[monoio::main(timer_enabled = true)]
async fn main() {
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

    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port))).expect("bind");
    println!("LISTENING");

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

    loop {
        if !connected && Instant::now() >= connect_deadline {
            eprintln!(
                "[loss-listener-monoio] connect timed out, state={:?}",
                conn.state()
            );
            break;
        }
        if let Some(d) = stream_deadline
            && Instant::now() >= d
        {
            break;
        }

        let wait = Duration::from_micros(time_until_earliest_timer(
            &timers,
            now(start),
            MAX_WAIT.as_micros() as u64,
        ))
        .min(MAX_WAIT);

        // A fresh buffer per attempt -- see loss_caller_monoio.rs's doc
        // comment on the same pattern: io_uring ops can't be safely
        // cancelled mid-flight, so a timed-out recv's buffer is simply
        // abandoned rather than reused.
        if let Ok((res, buf)) = monoio::time::timeout(wait, socket.recv_from(vec![0u8; 2048])).await
            && let Ok((n, addr)) = res
        {
            if peer.is_none() {
                if let Err(e) = socket.connect(addr).await {
                    eprintln!("[loss-listener-monoio] connect to peer failed: {e}");
                } else {
                    peer = Some(addr);
                }
            }
            let t = now(start);
            let _ = conn.feed_recv_buf(&buf[..n], t);
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
                ConnectionEvent::DataReceived { .. } => {
                    total_received += 1;
                }
                ConnectionEvent::Disconnected { reason } => {
                    eprintln!("[loss-listener-monoio] disconnected: {reason}");
                    stream_deadline = Some(Instant::now());
                }
                ConnectionEvent::Error(msg) => {
                    eprintln!("[loss-listener-monoio] error: {msg}");
                }
                _ => {}
            }
        }
    }

    let stats = conn.receiver_stats();
    let elapsed_s = start.elapsed().as_secs_f64();
    let p = srt_interop::cpu_stats::process_stats();
    match stats {
        Some(s) => println!(
            "STATS role=listener backend=monoio pkt_recv={total_received} pkt_recv_total={} \
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
            "STATS role=listener backend=monoio pkt_recv={total_received} pkt_recv_total=0 \
             pkt_rcv_loss_total=0 pkt_rcv_dup_total=0 rtt_ms=0.000 elapsed_s={elapsed_s:.3} \
             cpu_user_ms={:.1} cpu_sys_ms={:.1} peak_rss_kb={}",
            p.cpu_user_ms, p.cpu_sys_ms, p.peak_rss_kb
        ),
    }

    if !connected {
        std::process::exit(1);
    }
}
