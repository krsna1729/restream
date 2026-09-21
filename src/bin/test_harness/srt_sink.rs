//! Standalone SRT sink peer for multi-host packet-rate rungs.
//!
//! Runs the harness's in-process `srt-rs` accept-and-discard listener pool on
//! this machine so a rung measured elsewhere can fan out to a real remote peer
//! (the measuring host sets `RESOURCE_SWEEP_SRT_PEER_HOSTS` to this machine).
//! It serves a `GET /state` endpoint with absolute counters — the measuring
//! host differences them per sample window and gates the rung's validity on
//! them — prints the same numbers periodically, and reports them again in its
//! final artifact.
//!
//! The peer's kernel UDP error counters are only visible here, never in the
//! measuring host's artifact, which is exactly why the state endpoint exists:
//! a remote rung may not be called healthy while its sink host is dropping.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// `InErrors`, `RcvbufErrors` and `SndbufErrors` from `/proc/net/snmp`'s
/// `Udp:` row: the kernel counters that show a sink host dropping datagrams it
/// could not buffer. `None` when the file or any column is unavailable.
fn host_udp_drop_counters() -> Option<(u64, u64, u64)> {
    let snmp = std::fs::read_to_string("/proc/net/snmp").ok()?;
    let mut lines = snmp
        .lines()
        .filter_map(|line| line.strip_prefix("Udp:"))
        .map(str::split_whitespace);
    let (header, values) = (lines.next()?, lines.next()?);
    let column = |name: &str| -> Option<u64> {
        header
            .clone()
            .position(|field| field == name)
            .and_then(|index| values.clone().nth(index))
            .and_then(|value| value.parse::<u64>().ok())
    };
    Some((
        column("InErrors")?,
        column("RcvbufErrors")?,
        column("SndbufErrors")?,
    ))
}

/// Identity of one sink process: a restarted sink must be visible as a new
/// run, because its cumulative counters restart too.
fn run_id() -> String {
    let started_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    format!("{:x}-{started_ms:x}", std::process::id())
}

/// Non-loopback interface drop counters, summed: a receiving NIC can drop
/// before UDP ever sees the packet, so a peer's losslessness evidence needs
/// them alongside the kernel UDP counters. `None` when no interface could be
/// read, so the measuring host sees a missing sensor rather than zero drops.
fn host_nic_drop_counters() -> (Option<u64>, Option<u64>) {
    let mut rx = None;
    let mut tx = None;
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            if entry.file_name() == "lo" {
                continue;
            }
            let statistics = entry.path().join("statistics");
            if let Some(value) = read_counter_file(&statistics.join("rx_dropped")) {
                rx = Some(rx.unwrap_or(0) + value);
            }
            if let Some(value) = read_counter_file(&statistics.join("tx_dropped")) {
                tx = Some(tx.unwrap_or(0) + value);
            }
        }
    }
    (rx, tx)
}

/// Kernel UDP error counters since this sink started, for the human-readable
/// log line; the state endpoint serves the absolute values.
fn udp_drops_since_start(start: Option<(u64, u64, u64)>) -> Option<(u64, u64, u64)> {
    let (start_in, start_rcv, start_snd) = start?;
    let (in_errors, rcvbuf, sndbuf) = host_udp_drop_counters()?;
    Some((
        in_errors.saturating_sub(start_in),
        rcvbuf.saturating_sub(start_rcv),
        sndbuf.saturating_sub(start_snd),
    ))
}

fn read_counter_file(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// Absolute counters served to the measuring host, plus the run identity.
#[allow(clippy::too_many_arguments)]
fn state_json(
    run: &str,
    started_ms: u128,
    ports: &[u16],
    counters: SrtSinkCounters,
    udp_drops: Option<(u64, u64, u64)>,
    nic_rx_dropped: Option<u64>,
    nic_tx_dropped: Option<u64>,
) -> Value {
    json!({
        "runId": run,
        "startedAtMs": started_ms,
        "ports": ports,
        "accepted": counters.accepted,
        "closed": counters.closed,
        "discardedBytes": counters.discarded_bytes,
        "udpInErrors": udp_drops.map(|drops| drops.0),
        "udpRcvbufErrors": udp_drops.map(|drops| drops.1),
        "udpSndbufErrors": udp_drops.map(|drops| drops.2),
        "nicRxDropped": nic_rx_dropped,
        "nicTxDropped": nic_tx_dropped,
        "uptimeSecs": (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(started_ms)
            .saturating_sub(started_ms)) / 1000,
    })
}

/// One `GET /state` request per connection: no keep-alive, no routing.
async fn serve_state(
    listener: TcpListener,
    run: String,
    started_ms: u128,
    ports: Vec<u16>,
    counters: SrtSinkCountersHandle,
) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            continue;
        };
        let (nic_rx_dropped, nic_tx_dropped) = host_nic_drop_counters();
        let body = state_json(
            &run,
            started_ms,
            &ports,
            counters.snapshot(),
            host_udp_drop_counters(),
            nic_rx_dropped,
            nic_tx_dropped,
        )
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let mut request = [0_u8; 512];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
    }
}

/// The peer-side half of a multi-host rung: bind the SRT sink listeners, serve
/// the state endpoint the measuring host polls, print per-interval counters,
/// and stop on SIGINT/SIGTERM with a final record.
pub(crate) async fn srt_sink_mode() -> Result<Value, String> {
    let ports: Vec<u16> = match std::env::var("SRT_SINK_PORTS") {
        Ok(raw) => raw
            .split(',')
            .filter_map(|part| part.trim().parse::<u16>().ok())
            .collect(),
        Err(_) => vec![harness_port_defaults().mtx_srt],
    };
    if ports.is_empty() {
        return Err("SRT_SINK_PORTS must list at least one port".to_string());
    }
    let state_port = env_usize(
        "SRT_SINK_STATE_PORT",
        harness_port_defaults().mtx_api as usize,
    );
    let state_port = u16::try_from(state_port).map_err(|_| "SRT_SINK_STATE_PORT out of range")?;
    let default_threads =
        std::thread::available_parallelism().map_or(1, |count| count.get().min(4));
    let threads = env_usize("HARNESS_SRT_SINK_THREADS", default_threads);
    let udp_buffer = env_usize("HARNESS_SRT_SINK_UDP_BUFFER", 8 * 1024 * 1024);
    let interval_secs = env_secs("SRT_SINK_REPORT_SECS", 5).max(1);
    let pool = HarnessSrtSinkPool::start(&ports, udp_buffer, threads)?;

    let started = Instant::now();
    let started_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let run = run_id();
    let start_drops = host_udp_drop_counters();

    // The state endpoint is what makes a remote rung's validity checkable: the
    // measuring host polls it every sample and refuses `healthy` without it.
    // Dual-stack first (Linux maps IPv4 onto [::] by default), IPv4-only as
    // the fallback, so a peer host addressed by raw IPv6 can still be polled.
    let listener = match TcpListener::bind(format!("[::]:{state_port}")).await {
        Ok(listener) => listener,
        Err(dual_stack_error) => TcpListener::bind(format!("0.0.0.0:{state_port}"))
            .await
            .map_err(|ipv4_error| {
                format!(
                    "srt-sink state endpoint on {state_port}: {dual_stack_error} / {ipv4_error}"
                )
            })?,
    };
    let state_task = tokio::spawn(serve_state(
        listener,
        run.clone(),
        started_ms,
        ports.clone(),
        pool.counters(),
    ));
    println!(
        "[srt-sink] run {run} listening on {ports:?} with {threads} thread(s), {udp_buffer} B udp buffer; \
         state endpoint on {state_port}/state (dual-stack when available); send SIGINT/SIGTERM to stop"
    );

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("install SIGTERM handler: {e}"))?;
    let stopped_by = loop {
        tokio::select! {
            _ = ticker.tick() => {
                let counters = pool.snapshot();
                let drops = udp_drops_since_start(start_drops);
                println!(
                    "[srt-sink] {}",
                    json!({
                        "runId": run,
                        "uptimeSecs": started.elapsed().as_secs(),
                        "accepted": counters.accepted,
                        "closed": counters.closed,
                        "discardedBytes": counters.discarded_bytes,
                        "udpInErrorsSinceStart": drops.map(|drops| drops.0),
                        "udpRcvbufErrorsSinceStart": drops.map(|drops| drops.1),
                        "udpSndbufErrorsSinceStart": drops.map(|drops| drops.2),
                    })
                );
            }
            _ = tokio::signal::ctrl_c() => break "ctrl_c",
            _ = sigterm.recv() => break "sigterm",
        }
    };

    let counters = pool.snapshot();
    let drops = udp_drops_since_start(start_drops);
    state_task.abort();
    pool.stop();
    Ok(json!({
        "mode": "srt-sink",
        "runId": run,
        "ports": ports,
        "statePort": state_port,
        "threads": threads,
        "udpBufferBytes": udp_buffer,
        "uptimeSecs": started.elapsed().as_secs(),
        "stoppedBy": stopped_by,
        "accepted": counters.accepted,
        "closed": counters.closed,
        "discardedBytes": counters.discarded_bytes,
        "udpInErrorsSinceStart": drops.map(|drops| drops.0),
        "udpRcvbufErrorsSinceStart": drops.map(|drops| drops.1),
        "udpSndbufErrorsSinceStart": drops.map(|drops| drops.2),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_json_carries_absolute_counters_and_the_run_identity() {
        let counters = SrtSinkCounters {
            accepted: 100,
            discarded_bytes: 5_000,
            closed: 2,
        };
        let state = state_json(
            "run-1",
            1_000,
            &[8891],
            counters,
            Some((3, 7, 1)),
            Some(2),
            Some(0),
        );
        assert_eq!(state["runId"], "run-1");
        assert_eq!(state["ports"][0], 8891);
        assert_eq!(state["accepted"], 100);
        assert_eq!(state["discardedBytes"], 5_000);
        assert_eq!(state["udpInErrors"], 3);
        assert_eq!(state["udpRcvbufErrors"], 7);
        assert_eq!(state["udpSndbufErrors"], 1);
        assert_eq!(state["nicRxDropped"], 2);
        assert_eq!(state["nicTxDropped"], 0);

        // A host that cannot read /proc/net/snmp or its NIC counters reports
        // null, not zero: the measuring side must see a missing sensor.
        let state = state_json("run-1", 1_000, &[8891], counters, None, None, None);
        assert!(state["udpInErrors"].is_null());
        assert!(state["udpRcvbufErrors"].is_null());
        assert!(state["udpSndbufErrors"].is_null());
        assert!(state["nicRxDropped"].is_null());
        assert!(state["nicTxDropped"].is_null());
    }

    #[test]
    fn run_ids_differ_between_sink_processes() {
        let first = run_id();
        assert!(first.contains('-'), "{first}");
    }
}
