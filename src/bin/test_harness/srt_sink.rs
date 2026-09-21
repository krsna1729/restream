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

use super::peer_state::{
    bind_state_listener, cpus_allowed_list, host_nic_drop_counters, host_udp_drop_counters,
    pin_to_cpuset, run_id, serve_state_json, udp_drops_since_start,
};
use super::*;

/// Absolute counters served to the measuring host, plus the run identity.
#[allow(clippy::too_many_arguments)]
fn state_json(
    run: &str,
    started_ms: u128,
    cpus_allowed: Option<&str>,
    ports: &[u16],
    counters: SrtSinkCounters,
    udp_drops: Option<(u64, u64, u64)>,
    nic_rx_dropped: Option<u64>,
    nic_tx_dropped: Option<u64>,
) -> Value {
    json!({
        "runId": run,
        "startedAtMs": started_ms,
        "cpusAllowedList": cpus_allowed,
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

/// The sink's own state body: absolute counters, run identity and the observed
/// CPU mask.
fn sink_state(run: &str, started_ms: u128, ports: &[u16], counters: SrtSinkCounters) -> Value {
    let (nic_rx_dropped, nic_tx_dropped) = host_nic_drop_counters();
    state_json(
        run,
        started_ms,
        cpus_allowed_list().as_deref(),
        ports,
        counters,
        host_udp_drop_counters(),
        nic_rx_dropped,
        nic_tx_dropped,
    )
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
    if let Ok(mask) = std::env::var("SRT_SINK_CPUSET")
        && !mask.trim().is_empty()
    {
        // Before the pool spawns its threads: they inherit this mask.
        pin_to_cpuset(mask.trim())?;
        println!("[srt-sink] pinned to cpus {mask}");
    }
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
    let listener = bind_state_listener(state_port).await?;
    let state_task = tokio::spawn(serve_state_json(listener, {
        let run = run.clone();
        let ports = ports.clone();
        let counters = pool.counters();
        std::sync::Arc::new(move || sink_state(&run, started_ms, &ports, counters.snapshot()))
    }));
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
        "cpusAllowedList": cpus_allowed_list(),
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
            Some("2-5"),
            &[8891],
            counters,
            Some((3, 7, 1)),
            Some(2),
            Some(0),
        );
        assert_eq!(state["runId"], "run-1");
        assert_eq!(state["cpusAllowedList"], "2-5");
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
        let state = state_json("run-1", 1_000, None, &[8891], counters, None, None, None);
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
