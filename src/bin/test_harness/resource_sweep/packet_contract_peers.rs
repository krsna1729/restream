//! Remote sink-peer telemetry for the packet-rate contract.
//!
//! A rung whose SRT outputs are retargeted at other machines cannot be called
//! healthy from the measuring host alone: the peer hosts' own kernel drop
//! counters are only visible there. Each peer serves `GET /state` with
//! absolute counters, this module polls them once per sample and differences
//! them over the same window, and the verdict refuses `healthy` without them.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};

use super::measurement::round2;
use super::packet_contract::rate_per_sec;

/// Where the remote sink peers publish their state, and how many are expected.
#[derive(Clone, Debug)]
pub(super) struct PeerStateConfig {
    pub(super) hosts: Vec<String>,
    pub(super) port: u16,
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
}

/// Parse one `/state` response. Every connection/payload counter must be
/// present — a peer that answers without them is not telemetry. The kernel
/// UDP counters stay `None` when the peer cannot read its own `/proc`.
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
    })
}

/// Poll every configured peer's state endpoint. A failure is recorded against
/// that host, never dropped: a remote rung cannot be healthy without the peer
/// telemetry it depends on.
pub(super) async fn poll_peers(
    config: &PeerStateConfig,
) -> Vec<(String, Result<PeerState, String>)> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return config
                .hosts
                .iter()
                .map(|host| {
                    (
                        host.clone(),
                        Err(format!("http client unavailable: {error}")),
                    )
                })
                .collect();
        }
    };
    let mut readings = Vec::with_capacity(config.hosts.len());
    for host in &config.hosts {
        let url = format!("http://{host}:{}/state", config.port);
        let reading = match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => match response.text().await {
                Ok(body) => match serde_json::from_str::<Value>(&body) {
                    Ok(value) => peer_state_from_json(host, &value),
                    Err(error) => Err(format!("{host}: invalid state JSON: {error}")),
                },
                Err(error) => Err(format!("{host}: unreadable state body: {error}")),
            },
            Ok(response) => Err(format!("{host}: HTTP {}", response.status())),
            Err(error) => Err(format!("{host}: {error}")),
        };
        readings.push((host.clone(), reading));
    }
    readings
}

/// Per-rung peer history: previous counters per host, and the first `runId`
/// seen per host so a sink restart mid-rung is visible.
#[derive(Default)]
pub(super) struct PeerFold {
    previous: HashMap<String, PeerState>,
    first_run: HashMap<String, String>,
}

impl PeerFold {
    /// Forget the history: a new `(scenario, outputs)` rung starts a new window.
    pub(super) fn clear(&mut self) {
        self.previous.clear();
        self.first_run.clear();
    }

    /// Build one sample's `peers` array from this poll, updating the history.
    pub(super) fn record(
        &mut self,
        readings: &[(String, Result<PeerState, String>)],
        elapsed_secs: f64,
    ) -> Value {
        let mut peers = Vec::with_capacity(readings.len());
        for (host, reading) in readings {
            match reading {
                Ok(state) => {
                    let previous = self.previous.get(host).cloned();
                    let run_id_changed = self
                        .first_run
                        .get(host)
                        .is_some_and(|first| first != &state.run_id);
                    self.first_run
                        .entry(host.clone())
                        .or_insert_with(|| state.run_id.clone());
                    let peer_rate = |current: Option<u64>, previous: Option<u64>| {
                        rate_per_sec(current, previous, elapsed_secs).map(round2)
                    };
                    peers.push(json!({
                        "host": host,
                        "runId": state.run_id,
                        "runIdChanged": run_id_changed,
                        "acceptedPerSec": peer_rate(
                            Some(state.accepted),
                            previous.as_ref().map(|previous| previous.accepted),
                        ),
                        "closedPerSec": peer_rate(
                            Some(state.closed),
                            previous.as_ref().map(|previous| previous.closed),
                        ),
                        "payloadBytesPerSec": peer_rate(
                            Some(state.discarded_bytes),
                            previous.as_ref().map(|previous| previous.discarded_bytes),
                        ),
                        "udpInErrorsPerSec": peer_rate(
                            state.udp_in_errors,
                            previous.as_ref().and_then(|previous| previous.udp_in_errors),
                        ),
                        "udpRcvbufErrorsPerSec": peer_rate(
                            state.udp_rcvbuf_errors,
                            previous
                                .as_ref()
                                .and_then(|previous| previous.udp_rcvbuf_errors),
                        ),
                        "error": Value::Null,
                    }));
                    self.previous.insert(host.clone(), state.clone());
                }
                Err(error) => peers.push(json!({ "host": host, "error": error })),
            }
        }
        Value::Array(peers)
    }
}
