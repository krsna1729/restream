//! `srt.backup-failover`: an SRT Backup bond loses its active path
//! mid-stream, and the receiver's stream stays continuous.
//!
//! One output bonds two legs to one harness sink port (one receiving
//! group), each through its own UDP relay
//! (`srt://relay-a?…&bond=relay-b&type=backup`). Once media flows, the
//! relay carrying it blackholes both directions: to the sender the active
//! path just goes silent. Which leg is active is srt-rs's choice, so the
//! mode measures it rather than assuming the first. The standby must take
//! over and replay the flight the dead path never delivered (srt-rs
//! `SrtGroup` Backup failover), so the sink's MPEG-TS continuity check
//! (`ts_continuity.rs`) sees no counter jump, delivery resumes, and the
//! output never redials.

use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use super::*;

const MODE: &str = "srt.backup-failover";
/// Steady delivery through the relay before the cut.
const STEADY: Duration = Duration::from_secs(3);
/// Watch after the cut: past srt-rs's peer-idle break, so a sender that
/// only fails over once the dead leg breaks is seen too.
const WATCH: Duration = Duration::from_secs(12);
/// The last part of the watch, where delivery must run at steady pace.
const RESUMED: Duration = Duration::from_secs(5);
/// Media, not keepalives, crossed the relay before the cut.
const MIN_RELAYED_BYTES: u64 = 100_000;

/// A UDP relay from `listen` to `target` that can blackhole both
/// directions.
struct UdpRelay {
    cut: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    relayed_bytes: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl UdpRelay {
    fn start(listen: u16, target: SocketAddr) -> Result<Self, String> {
        let front =
            UdpSocket::bind(("127.0.0.1", listen)).map_err(|e| format!("relay bind: {e}"))?;
        let back = UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("relay bind: {e}"))?;
        back.connect(target)
            .map_err(|e| format!("relay connect: {e}"))?;
        for socket in [&front, &back] {
            socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        }
        let mut relay = Self {
            cut: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            relayed_bytes: Arc::new(AtomicU64::new(0)),
            thread: None,
        };
        let (cut, stop, relayed) = (
            relay.cut.clone(),
            relay.stop.clone(),
            relay.relayed_bytes.clone(),
        );
        relay.thread = Some(
            std::thread::Builder::new()
                .name("srt-relay".into())
                .spawn(move || relay_loop(&front, &back, &cut, &stop, &relayed))
                .map_err(|e| format!("spawn relay: {e}"))?,
        );
        Ok(relay)
    }

    fn cut(&self) {
        self.cut.store(true, Ordering::Release);
    }

    fn relayed_bytes(&self) -> u64 {
        self.relayed_bytes.load(Ordering::Acquire)
    }
}

impl Drop for UdpRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn relay_loop(
    front: &UdpSocket,
    back: &UdpSocket,
    cut: &AtomicBool,
    stop: &AtomicBool,
    relayed: &AtomicU64,
) {
    let mut buf = vec![0u8; 65_536];
    let mut client = None;
    let mut fds = [front.as_raw_fd(), back.as_raw_fd()].map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    while !stop.load(Ordering::Acquire) {
        // SAFETY: `fds` is a live array of initialized pollfd for its length.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 50) } <= 0 {
            continue;
        }
        let open = !cut.load(Ordering::Acquire);
        while let Ok((len, from)) = front.recv_from(&mut buf) {
            client = Some(from);
            if open && back.send(&buf[..len]).is_ok() {
                relayed.fetch_add(len as u64, Ordering::AcqRel);
            }
        }
        while let Ok(len) = back.recv(&mut buf) {
            if let Some(client) = client.filter(|_| open) {
                let _ = front.send_to(&buf[..len], client);
            }
        }
    }
}

pub(crate) async fn srt_backup_failover() -> Result<Value, String> {
    let work_dir = artifact_path(MODE);
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ports = TestPorts::from_env();
    let base = harness_port_defaults().ffmpeg_srt_sink_base;
    let sink_port = base.checked_add(3000).ok_or("sink port overflow")?;
    let relay_ports = [
        base.checked_add(3001).ok_or("relay port overflow")?,
        base.checked_add(3002).ok_or("relay port overflow")?,
    ];

    let sink = HarnessSrtSinkPool::start_checking_ts(&[sink_port], 8 * 1024 * 1024)?;
    let sink_addr = SocketAddr::from(([127, 0, 0, 1], sink_port));
    let relays = [
        UdpRelay::start(relay_ports[0], sink_addr)?,
        UdpRelay::start(relay_ports[1], sink_addr)?,
    ];
    let (mut child, api) = start_restream_api(
        &default_restream_bin(),
        &ports,
        &work_dir.join("backup-failover.sqlite"),
        &work_dir.join("restream.log"),
    )
    .await?;
    let outcome = run(&api, &ports, &sink, &relays, relay_ports).await;
    stop_child(&mut child).await;
    drop(relays);
    sink.stop();
    let result = outcome?;
    std::fs::write(
        work_dir.join(format!("{MODE}.json")),
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    if result["passed"] == true {
        Ok(result)
    } else {
        Err(format!("{MODE}: continuity gate failed: {result}"))
    }
}

/// The live part; its publisher is `kill_on_drop`, so an early error does
/// not leak it.
async fn run(
    api: &RampApi,
    ports: &TestPorts,
    sink: &HarnessSrtSinkPool,
    relays: &[UdpRelay; 2],
    relay_ports: [u16; 2],
) -> Result<Value, String> {
    let stream_key = "backup-failover-input";
    let pid = create_pipeline_with_stream_key(api, "backup-failover", stream_key).await?;
    let fixture = restream_engine::test_fixtures::bench_transport_fixture("h264", "1.5M", false)?;
    let mut publisher = spawn_publisher(
        &fixture,
        &harness_srt_ffmpeg_url(ports.srt, stream_key, HarnessSrtMode::Publish, None),
        "mpegts",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, Duration::from_secs(45)).await?;
    let url = format!(
        "{}&bond=127.0.0.1:{}&type=backup",
        harness_srt_output_url(relay_ports[0], "backup-failover", HarnessSrtMode::Publish),
        relay_ports[1],
    );
    let oid = create_output(api, &pid, "backup-failover", &url, "source").await?;
    start_output(api, &pid, &oid).await?;
    wait_for_outputs_progress(
        api,
        &pid,
        std::slice::from_ref(&oid),
        Duration::from_secs(30),
    )
    .await?;

    let steady_from = (
        Instant::now(),
        sink.snapshot(),
        relays.each_ref().map(UdpRelay::relayed_bytes),
    );
    tokio::time::sleep(STEADY).await;
    let at_cut = sink.snapshot();
    let relayed = [0, 1].map(|leg| relays[leg].relayed_bytes() - steady_from.2[leg]);
    let active = usize::from(relayed[1] > relayed[0]);
    let steady_rate = (at_cut.ts_packets - steady_from.1.ts_packets) as f64
        / steady_from.0.elapsed().as_secs_f64();
    relays[active].cut();
    let cut_at = Instant::now();

    let mut not_running = Vec::new();
    let mut resumed_from = None;
    while cut_at.elapsed() < WATCH {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if resumed_from.is_none() && cut_at.elapsed() >= WATCH - RESUMED {
            resumed_from = Some((Instant::now(), sink.snapshot().ts_packets));
        }
        let (status, _) = api.get_output_status(&pid, &oid).await?;
        if status.retrying || status.status != "running" {
            not_running.push(format!(
                "{}@{:.1}s",
                status.status,
                cut_at.elapsed().as_secs_f64()
            ));
        }
    }
    let end = sink.snapshot();
    let (resumed_at, resumed_packets) = resumed_from.ok_or("watch ended before its last part")?;
    let resumed_rate =
        (end.ts_packets - resumed_packets) as f64 / resumed_at.elapsed().as_secs_f64();
    let drops = sink.counters().drops();
    let checks = [
        (
            "mediaCrossedTheActiveRelay",
            relayed[active] >= MIN_RELAYED_BYTES,
        ),
        (
            "continuous",
            end.ts_continuity_errors == 0 && end.ts_misaligned == 0,
        ),
        ("deliveryResumed", resumed_rate >= steady_rate / 2.0),
        (
            "neverRedialed",
            end.accepted == at_cut.accepted && not_running.is_empty(),
        ),
    ];
    let passed = steady_rate > 0.0 && checks.iter().all(|(_, ok)| *ok);
    let result = json!({
        "mode": MODE,
        "passed": passed,
        "checks": checks.iter().map(|(name, ok)| (name.to_string(), json!(ok))).collect::<serde_json::Map<_, _>>(),
        "relayedBytesSteady": relayed,
        "cutLeg": active,
        "tsPacketsPerSecSteady": steady_rate,
        "tsPacketsPerSecResumed": resumed_rate,
        "tsContinuityErrors": end.ts_continuity_errors,
        "tsMisaligned": end.ts_misaligned,
        "sinkAcceptedAtCut": at_cut.accepted,
        "sinkAcceptedAtEnd": end.accepted,
        "outputNotRunning": not_running,
        "sinkDrops": drops.json(),
    });
    stop_child(&mut publisher).await;
    Ok(result)
}
