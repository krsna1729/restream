//! Remote sink-peer telemetry for the packet-rate contract.
//!
//! A rung whose SRT outputs are retargeted at other machines cannot be called
//! healthy from the measuring host alone: the peer hosts' own kernel and NIC
//! drop counters are only visible there. Each peer serves `GET /state` with
//! absolute counters, this module polls every configured peer concurrently
//! once per sample and differences them over each peer's own interval, and the
//! verdict refuses `healthy` without them.
//!
//! Peers are primed together with the local counters, before the rated clock
//! starts, so the first rated sample already covers a full interval of peer
//! evidence rather than starting one interval late.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::measurement::round2;

/// Where the remote sink peers publish their state.
#[derive(Clone, Debug)]
pub(super) struct PeerStateConfig {
    pub(super) hosts: Vec<String>,
    pub(super) port: u16,
    /// How many of the rung's outputs each host is expected to receive, in
    /// `hosts` order. Empty until the runner reports it.
    pub(super) expected_outputs_per_host: Vec<usize>,
}

/// One peer reading, parsed from the sink's `/state` endpoint. Counters are
/// absolute so the measuring host can difference them per sample window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PeerState {
    pub(super) run_id: String,
    pub(super) accepted: u64,
    pub(super) closed: u64,
    pub(super) discarded_bytes: u64,
    pub(super) udp_in_errors: Option<u64>,
    pub(super) udp_rcvbuf_errors: Option<u64>,
    pub(super) udp_sndbuf_errors: Option<u64>,
    /// Non-loopback interface drops, summed on the peer host. A receiving NIC
    /// can drop before UDP sees a packet, so these are part of the peer's
    /// losslessness evidence.
    pub(super) nic_rx_dropped: Option<u64>,
    pub(super) nic_tx_dropped: Option<u64>,
    /// The sink process's own CPU affinity, as it reports it.
    pub(super) cpus_allowed: Option<String>,
}

/// One poll of one peer: the reading (or the failure) and when it was taken.
/// The timestamp makes each peer's rate interval its own, not the measuring
/// host's common sampling interval.
#[derive(Clone, Debug)]
pub(super) struct PeerReading {
    pub(super) host: String,
    pub(super) observed_at: Instant,
    pub(super) state: Result<PeerState, String>,
}

/// HTTP authority for a peer host: IPv6 literals are bracketed so a raw
/// `2001:db8::5` host works exactly like it does for the SRT URLs.
pub(super) fn peer_state_url(host: &str, port: u16) -> String {
    let authority = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    format!("http://{authority}:{port}/state")
}

/// Parse one `/state` response. Every connection/payload counter must be
/// present — a peer that answers without them is not telemetry. The kernel
/// and NIC counters stay `None` when the peer cannot read them, which the
/// verdict treats as a missing sensor rather than as zero drops.
pub(super) fn peer_state_from_json(host: &str, value: &Value) -> Result<PeerState, String> {
    let run_id = value["runId"]
        .as_str()
        .ok_or_else(|| format!("{host}: no runId"))?
        .to_string();
    let required = |key: &str| -> Result<u64, String> {
        value[key]
            .as_u64()
            .ok_or_else(|| format!("{host}: no {key}"))
    };
    Ok(PeerState {
        run_id,
        accepted: required("accepted")?,
        closed: required("closed")?,
        discarded_bytes: required("discardedBytes")?,
        udp_in_errors: value["udpInErrors"].as_u64(),
        udp_rcvbuf_errors: value["udpRcvbufErrors"].as_u64(),
        udp_sndbuf_errors: value["udpSndbufErrors"].as_u64(),
        nic_rx_dropped: value["nicRxDropped"].as_u64(),
        nic_tx_dropped: value["nicTxDropped"].as_u64(),
        cpus_allowed: value["cpusAllowedList"].as_str().map(str::to_string),
    })
}

/// Poll every configured peer concurrently. A failure is recorded against
/// that host, never dropped: a remote rung cannot be healthy without the peer
/// telemetry it depends on. Concurrency matters because a slow or unreachable
/// peer must not stretch the sampling window it is part of.
pub(super) async fn poll_peers(config: PeerStateConfig) -> Vec<PeerReading> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build();
    let client = match client {
        Ok(client) => client,
        Err(error) => {
            let error = format!("http client unavailable: {error}");
            let observed_at = Instant::now();
            return config
                .hosts
                .iter()
                .map(|host| PeerReading {
                    host: host.clone(),
                    observed_at,
                    state: Err(error.clone()),
                })
                .collect();
        }
    };
    let mut tasks = tokio::task::JoinSet::new();
    for host in &config.hosts {
        let client = client.clone();
        let host = host.clone();
        let port = config.port;
        tasks.spawn(async move {
            let url = peer_state_url(&host, port);
            let state = match client.get(&url).send().await {
                Ok(response) if response.status().is_success() => match response.text().await {
                    Ok(body) => match serde_json::from_str::<Value>(&body) {
                        Ok(value) => peer_state_from_json(&host, &value),
                        Err(error) => Err(format!("{host}: invalid state JSON: {error}")),
                    },
                    Err(error) => Err(format!("{host}: unreadable state body: {error}")),
                },
                Ok(response) => Err(format!("{host}: HTTP {}", response.status())),
                Err(error) => Err(format!("{host}: {error}")),
            };
            PeerReading {
                host,
                observed_at: Instant::now(),
                state,
            }
        });
    }
    let mut readings = Vec::with_capacity(config.hosts.len());
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(reading) => readings.push(reading),
            Err(error) => readings.push(PeerReading {
                host: "unknown".to_string(),
                observed_at: Instant::now(),
                state: Err(format!("peer poll task failed: {error}")),
            }),
        }
    }
    readings.sort_by(|left, right| left.host.cmp(&right.host));
    readings
}

/// Per-rung peer history: the previous reading per host (with its timestamp)
/// and the first `runId` seen per host, so a sink restart mid-rung is visible.
#[derive(Default)]
pub(super) struct PeerFold {
    previous: HashMap<String, PeerReading>,
    first_run: HashMap<String, String>,
    /// Host order and expected outputs per host, from the run configuration.
    hosts: Vec<String>,
    expected_outputs_per_host: Vec<usize>,
}

impl PeerFold {
    /// Remember the configured host order and how many outputs each host is
    /// expected to receive.
    pub(super) fn configure(&mut self, hosts: &[String], expected_outputs_per_host: &[usize]) {
        self.hosts = hosts.to_vec();
        self.expected_outputs_per_host = expected_outputs_per_host.to_vec();
    }

    /// Forget the history: a new `(scenario, outputs)` rung starts a new window.
    pub(super) fn clear(&mut self) {
        self.previous.clear();
        self.first_run.clear();
    }

    /// Take the pre-rated baseline for every peer: no record is emitted, the
    /// readings only become the interval origin for the first rated sample.
    pub(super) fn prime(&mut self, readings: &[PeerReading]) {
        for reading in readings {
            if let Ok(state) = &reading.state {
                self.first_run
                    .entry(reading.host.clone())
                    .or_insert_with(|| state.run_id.clone());
                self.previous.insert(reading.host.clone(), reading.clone());
            }
        }
    }

    /// Build one sample's `peers` array from this poll, updating the history.
    /// Each peer's rates use its own observation interval.
    pub(super) fn record(&mut self, readings: &[PeerReading]) -> Value {
        let mut peers = Vec::with_capacity(readings.len());
        for reading in readings {
            let expected_outputs = self
                .hosts
                .iter()
                .position(|host| host == &reading.host)
                .and_then(|index| self.expected_outputs_per_host.get(index).copied());
            match &reading.state {
                Ok(state) => {
                    let previous = self.previous.get(&reading.host);
                    let interval_secs = previous.map(|previous| {
                        reading
                            .observed_at
                            .saturating_duration_since(previous.observed_at)
                            .as_secs_f64()
                    });
                    let run_id_changed = self
                        .first_run
                        .get(&reading.host)
                        .is_some_and(|first| first != &state.run_id);
                    self.first_run
                        .entry(reading.host.clone())
                        .or_insert_with(|| state.run_id.clone());
                    let previous_state = previous.and_then(|previous| previous.state.as_ref().ok());
                    // The raw delta is the authoritative quantity: a window
                    // integration must use this peer's own interval, not the
                    // measuring host's sampling interval.
                    let payload_bytes_delta = previous_state.and_then(|previous| {
                        state.discarded_bytes.checked_sub(previous.discarded_bytes)
                    });
                    let peer_rate = |current: Option<u64>, previous: Option<u64>| {
                        interval_secs.and_then(|interval| rate_over(current, previous, interval))
                    };
                    peers.push(json!({
                        "host": reading.host,
                        "expectedOutputs": expected_outputs,
                        "cpusAllowedList": state.cpus_allowed,
                        "runId": state.run_id,
                        "runIdChanged": run_id_changed,
                        "intervalSecs": interval_secs.map(round2),
                        "acceptedPerSec": peer_rate(
                            Some(state.accepted),
                            previous_state.map(|previous| previous.accepted),
                        ),
                        "closedPerSec": peer_rate(
                            Some(state.closed),
                            previous_state.map(|previous| previous.closed),
                        ),
                        "payloadBytesPerSec": peer_rate(
                            Some(state.discarded_bytes),
                            previous_state.map(|previous| previous.discarded_bytes),
                        ),
                        "payloadBytesDelta": payload_bytes_delta,
                        "udpInErrorsPerSec": peer_rate(
                            state.udp_in_errors,
                            previous_state.and_then(|previous| previous.udp_in_errors),
                        ),
                        "udpRcvbufErrorsPerSec": peer_rate(
                            state.udp_rcvbuf_errors,
                            previous_state.and_then(|previous| previous.udp_rcvbuf_errors),
                        ),
                        "udpSndbufErrorsPerSec": peer_rate(
                            state.udp_sndbuf_errors,
                            previous_state.and_then(|previous| previous.udp_sndbuf_errors),
                        ),
                        "nicRxDroppedPerSec": peer_rate(
                            state.nic_rx_dropped,
                            previous_state.and_then(|previous| previous.nic_rx_dropped),
                        ),
                        "nicTxDroppedPerSec": peer_rate(
                            state.nic_tx_dropped,
                            previous_state.and_then(|previous| previous.nic_tx_dropped),
                        ),
                        "error": Value::Null,
                    }));
                    self.previous.insert(reading.host.clone(), reading.clone());
                }
                Err(error) => peers.push(json!({ "host": reading.host, "error": error })),
            }
        }
        Value::Array(peers)
    }
}

/// `current - previous` over one peer's own interval. `None` on a missing
/// reading, a counter reset, or a non-positive interval.
fn rate_over(current: Option<u64>, previous: Option<u64>, interval_secs: f64) -> Option<f64> {
    let (current, previous) = (current?, previous?);
    (interval_secs > 0.0 && current >= previous)
        .then(|| (current - previous) as f64 / interval_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(run: &str, udp_rcvbuf: Option<u64>) -> PeerState {
        PeerState {
            run_id: run.to_string(),
            accepted: 100,
            closed: 0,
            discarded_bytes: 1_000,
            udp_in_errors: Some(0),
            udp_rcvbuf_errors: udp_rcvbuf,
            udp_sndbuf_errors: Some(0),
            nic_rx_dropped: Some(0),
            nic_tx_dropped: Some(0),
            cpus_allowed: Some("2-5".to_string()),
        }
    }

    fn reading(host: &str, at: Instant, state: Result<PeerState, String>) -> PeerReading {
        PeerReading {
            host: host.to_string(),
            observed_at: at,
            state,
        }
    }

    #[test]
    fn peer_state_urls_bracket_ipv6_hosts() {
        assert_eq!(peer_state_url("peer-a", 9997), "http://peer-a:9997/state");
        assert_eq!(
            peer_state_url("192.0.2.10", 9997),
            "http://192.0.2.10:9997/state"
        );
        assert_eq!(
            peer_state_url("2001:db8::5", 9997),
            "http://[2001:db8::5]:9997/state"
        );
        assert_eq!(
            peer_state_url("[2001:db8::5]", 9997),
            "http://[2001:db8::5]:9997/state"
        );
    }

    /// The prime is the interval origin: the first rated sample already has
    /// peer rates instead of starting one interval late.
    #[test]
    fn priming_makes_the_first_rated_sample_cover_a_peer_interval() {
        let t0 = Instant::now();
        let mut fold = PeerFold::default();
        fold.prime(&[reading("peer-a", t0, Ok(state("run-1", Some(0))))]);
        // Nothing is emitted by the prime itself.
        let primed = fold.record(&[reading(
            "peer-a",
            t0 + Duration::from_secs(1),
            Ok(state("run-1", Some(4))),
        )]);
        let peer = &primed[0];
        assert_eq!(peer["intervalSecs"], 1.0);
        assert_eq!(
            peer["udpRcvbufErrorsPerSec"], 4.0,
            "peer drops from the very first interval are rated evidence"
        );
    }

    /// Each peer's rate uses its own observation interval, not a shared one.
    #[test]
    fn peer_rates_use_the_peers_own_interval() {
        let t0 = Instant::now();
        let mut fold = PeerFold::default();
        fold.prime(&[reading("peer-a", t0, Ok(state("run-1", Some(0))))]);
        // This peer's reading arrives two seconds after its previous one.
        let recorded = fold.record(&[reading(
            "peer-a",
            t0 + Duration::from_secs(2),
            Ok(state("run-1", Some(6))),
        )]);
        assert_eq!(recorded[0]["intervalSecs"], 2.0);
        assert_eq!(recorded[0]["udpRcvbufErrorsPerSec"], 3.0);
    }

    #[test]
    fn a_missing_peer_reading_has_no_rate_and_a_restart_is_flagged() {
        let t0 = Instant::now();
        let mut fold = PeerFold::default();
        fold.prime(&[reading("peer-a", t0, Ok(state("run-1", Some(0))))]);
        let failed = fold.record(&[reading(
            "peer-a",
            t0 + Duration::from_secs(1),
            Err("connect: refused".to_string()),
        )]);
        assert_eq!(failed[0]["error"], "connect: refused");

        let restarted = fold.record(&[reading(
            "peer-a",
            t0 + Duration::from_secs(2),
            Ok(state("run-2", Some(0))),
        )]);
        assert_eq!(restarted[0]["runIdChanged"], true);
    }

    #[test]
    fn peer_state_parsing_rejects_incomplete_telemetry() {
        let parsed = peer_state_from_json(
            "peer-a",
            &json!({
                "runId": "run-1", "accepted": 100, "closed": 1, "discardedBytes": 5_000,
                "udpInErrors": 3, "udpRcvbufErrors": 7, "udpSndbufErrors": 1,
                "nicRxDropped": 2, "nicTxDropped": 0,
            }),
        )
        .unwrap();
        assert_eq!(parsed.udp_sndbuf_errors, Some(1));
        assert_eq!(parsed.nic_rx_dropped, Some(2));

        // A peer that cannot read its own /proc reports null, which the
        // verdict treats as a missing sensor rather than zero drops.
        let parsed = peer_state_from_json(
            "peer-a",
            &json!({
                "runId": "run-1", "accepted": 0, "closed": 0, "discardedBytes": 0,
                "udpInErrors": Value::Null, "udpRcvbufErrors": Value::Null,
            }),
        )
        .unwrap();
        assert_eq!(parsed.udp_in_errors, None);
        assert_eq!(parsed.nic_rx_dropped, None);

        for incomplete in [
            json!({"accepted": 1, "closed": 0, "discardedBytes": 0}),
            json!({"runId": "run-1", "closed": 0, "discardedBytes": 0}),
        ] {
            assert!(
                peer_state_from_json("peer-a", &incomplete).is_err(),
                "{incomplete}"
            );
        }
    }
}
