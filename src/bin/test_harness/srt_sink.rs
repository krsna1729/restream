//! Standalone SRT sink peer for multi-host packet-rate rungs.
//!
//! Runs the harness's in-process `srt-rs` accept-and-discard listener pool on
//! this machine so a rung measured elsewhere can fan out to a real remote peer
//! (the measuring host sets `RESOURCE_SWEEP_SRT_PEER_HOSTS` to this machine).
//! It reports its own counters periodically and as a final artifact, and it
//! also reports this host's kernel UDP error/receive-buffer counters — a
//! remote peer's drops are only visible here, never in the measuring host's
//! artifact.

use std::time::{Duration, Instant};

use super::*;

/// `InErrors` and `RcvbufErrors` from `/proc/net/snmp`'s `Udp:` row: the
/// kernel counters that show a sink host dropping datagrams it could not
/// buffer. `None` when the file or either column is unavailable.
fn host_udp_drop_counters() -> Option<(u64, u64)> {
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
    Some((column("InErrors")?, column("RcvbufErrors")?))
}

/// The peer-side half of a multi-host rung: bind the SRT sink listeners, print
/// per-interval counters, and stop on SIGINT/SIGTERM with a final record.
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
    let default_threads =
        std::thread::available_parallelism().map_or(1, |count| count.get().min(4));
    let threads = env_usize("HARNESS_SRT_SINK_THREADS", default_threads);
    let udp_buffer = env_usize("HARNESS_SRT_SINK_UDP_BUFFER", 8 * 1024 * 1024);
    let interval_secs = env_secs("SRT_SINK_REPORT_SECS", 5).max(1);
    let pool = HarnessSrtSinkPool::start(&ports, udp_buffer, threads)?;

    let started = Instant::now();
    let start_drops = host_udp_drop_counters();
    println!(
        "[srt-sink] listening on {ports:?} with {threads} thread(s), {} B udp buffer; \
         send SIGINT/SIGTERM to stop",
        udp_buffer
    );

    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("install SIGTERM handler: {e}"))?;
    let stopped_by = loop {
        tokio::select! {
            _ = ticker.tick() => {
                let counters = pool.snapshot();
                let drops = host_udp_drop_counters().zip(start_drops).map(
                    |((errors, rcvbuf), (start_errors, start_rcvbuf))| {
                        (
                            errors.saturating_sub(start_errors),
                            rcvbuf.saturating_sub(start_rcvbuf),
                        )
                    },
                );
                println!(
                    "[srt-sink] {}",
                    json!({
                        "uptimeSecs": started.elapsed().as_secs(),
                        "accepted": counters.accepted,
                        "closed": counters.closed,
                        "discardedBytes": counters.discarded_bytes,
                        "udpInErrors": drops.map(|drops| drops.0),
                        "udpRcvbufErrors": drops.map(|drops| drops.1),
                    })
                );
            }
            _ = tokio::signal::ctrl_c() => break "ctrl_c",
            _ = sigterm.recv() => break "sigterm",
        }
    };

    let counters = pool.snapshot();
    let drops = host_udp_drop_counters().zip(start_drops).map(
        |((errors, rcvbuf), (start_errors, start_rcvbuf))| {
            (
                errors.saturating_sub(start_errors),
                rcvbuf.saturating_sub(start_rcvbuf),
            )
        },
    );
    pool.stop();
    Ok(json!({
        "mode": "srt-sink",
        "ports": ports,
        "threads": threads,
        "udpBufferBytes": udp_buffer,
        "uptimeSecs": started.elapsed().as_secs(),
        "stoppedBy": stopped_by,
        "accepted": counters.accepted,
        "closed": counters.closed,
        "discardedBytes": counters.discarded_bytes,
        "udpInErrors": drops.map(|drops| drops.0),
        "udpRcvbufErrors": drops.map(|drops| drops.1),
    }))
}
