//! WI3.4 packet-rate benchmark contract sampling.
//!
//! One flat record per sample, sourced from the production packet-rate
//! surfaces (`/metrics/system` `egressShards`, `capacity` and `ioUring`) plus
//! host kernel/NIC drop counters. Cumulative counters are differenced between
//! samples, so the artifact carries per-second rates; a first sample, a
//! counter reset, or a source the host/product does not provide reads `null`
//! rather than a fabricated zero.
//!
//! Naming follows the roadmap's fixed targets: `srtTxDatagramsPerSec` is every
//! SRT TX datagram (DATA plus control), `srtDataFirstPps` is first-transmission
//! DATA (the number to compare against the product-workload target),
//! `srtDataRetransmitPps` is retransmitted DATA, and `srtControlPps` is the
//! remainder. RX datagrams and timer/maintenance work keep their own names and
//! are never folded into "packet events".
//!
//! This establishes the measurement contract in
//! `docs/srt-compio-roadmap.md` §10. It measures; it does not optimize.
//! Metrics the product cannot source yet (scheduler wake rate, SQEs per
//! submission, io_uring enters/s, and `cycles/packet` without a PMU) are
//! recorded in the summary's `unavailable` block with the reason, never as a
//! zero. Every artifact carries the run's git tree, workload and peer
//! environment, one summary per `(scenario, output count)` rung, and a
//! validity verdict per sample and per rung.
//!
//! State lives in one process-global sampler: a harness measurement mode runs
//! once per process, and the resource-sweep scenarios share one artifact.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::measurement::round2;
use super::*;

/// Minimum rated counter window for a rung to count as baseline evidence.
/// Configuration alone is not evidence: the window is measured from the prime
/// to the last rated sample.
pub(super) const MIN_RATED_WINDOW_SECS: f64 = 10.0;

/// `/metrics/system` `egressShards[].state` — the shard health enum, which is
/// part of the current schema (`healthy`/`stalled`/`stopped`/`panicked`). It
/// is deliberately read through this constant so it cannot be confused with
/// the removed *output status* `state` field the harness guardrail bans.
const SHARD_HEALTH_FIELD: &str = "state";

/// Cumulative counters that only become rates across two samples. Every field
/// is an `Option`: a source the host or the product does not provide stays
/// `None` and the derived rate reads `null`, never a fabricated zero.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct PacketCounters {
    /// SRT Owner TX submissions: every datagram handed to a socket.
    pub(super) srt_tx_datagrams: Option<u64>,
    /// SRT Owner TX completions observed.
    pub(super) srt_tx_completed: Option<u64>,
    /// SRT Owner RX datagrams (publisher DATA plus control traffic).
    pub(super) srt_rx_datagrams: Option<u64>,
    /// SRT first-transmission DATA datagrams (`txClass.dataFirst`).
    pub(super) srt_data_first: Option<u64>,
    /// SRT retransmitted DATA datagrams (`txClass.dataRetransmit`).
    pub(super) srt_data_retx: Option<u64>,
    /// SRT control datagrams submitted (ACK, ACKACK, NAK, keepalive, ...).
    pub(super) srt_control: Option<u64>,
    pub(super) srt_service_visits: Option<u64>,
    pub(super) srt_service_actions: Option<u64>,
    pub(super) srt_maintenance_actions: Option<u64>,
    pub(super) shard_loop_iterations: Option<u64>,
    pub(super) shard_media_ticks: Option<u64>,
    pub(super) shard_ready_visits: Option<u64>,
    pub(super) shard_retries: Option<u64>,
    pub(super) udp_in_errors: Option<u64>,
    pub(super) udp_rcvbuf_errors: Option<u64>,
    pub(super) udp_sndbuf_errors: Option<u64>,
    pub(super) nic_rx_dropped: Option<u64>,
    pub(super) nic_tx_dropped: Option<u64>,
}

impl PacketCounters {
    /// Fold one `/metrics/system` snapshot plus the host drop counters.
    /// Only SRT shards count: the contract describes the SRT datapath, and a
    /// mixed run must not dilute it with RTMP/sink shard loops. Any counter
    /// whose source is absent reads `None`.
    pub(super) fn read(system: &Value) -> Self {
        let shards = srt_shards(system);
        let owners: Vec<&Value> = shards
            .iter()
            .flat_map(|shard| shard["srtOwners"].as_array().into_iter().flatten())
            .filter(|owner| owner["present"] == true)
            .collect();
        let owner_field = |key: &str| {
            sum_all(
                owners
                    .iter()
                    .map(|owner| owner.get(key).and_then(Value::as_u64)),
            )
        };
        let class_field = |key: &str| {
            sum_all(owners.iter().map(|owner| {
                owner
                    .get("txClass")
                    .and_then(|class| class.get(key))
                    .and_then(Value::as_u64)
            }))
        };
        let shard_field = |key: &str| {
            sum_all(
                shards
                    .iter()
                    .map(|shard| shard.get(key).and_then(Value::as_u64)),
            )
        };
        let data_first = class_field("dataFirst");
        let data_retx = class_field("dataRetransmit");
        let tx_datagrams = owner_field("txPackets");
        let srt_control = match (tx_datagrams, data_first, data_retx) {
            (Some(tx), Some(first), Some(retx)) => Some(tx.saturating_sub(first + retx)),
            _ => None,
        };
        let host = HostDropCounters::read();
        Self {
            srt_tx_datagrams: tx_datagrams,
            srt_tx_completed: owner_field("txCompletedOk"),
            srt_rx_datagrams: owner_field("rxPackets"),
            srt_data_first: data_first,
            srt_data_retx: data_retx,
            srt_control,
            srt_service_visits: owner_field("serviceVisits"),
            srt_service_actions: owner_field("serviceActions"),
            srt_maintenance_actions: owner_field("maintenanceActions"),
            shard_loop_iterations: shard_field("loopIterations"),
            shard_media_ticks: shard_field("mediaTicks"),
            shard_ready_visits: shard_field("readyVisits"),
            shard_retries: shard_field("retryEvents"),
            udp_in_errors: host.udp_in_errors,
            udp_rcvbuf_errors: host.udp_rcvbuf_errors,
            udp_sndbuf_errors: host.udp_sndbuf_errors,
            nic_rx_dropped: host.nic_rx_dropped,
            nic_tx_dropped: host.nic_tx_dropped,
        }
    }
}

/// Parse `/proc/net/snmp`'s `Udp:` header/value rows. Each column is `None`
/// when the running kernel does not publish it — a missing column is not a
/// zero-drop verdict. `None` for the whole result when there is no `Udp:` row.
pub(super) fn parse_udp_snmp(text: &str) -> Option<(Option<u64>, Option<u64>, Option<u64>)> {
    let mut lines = text
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
        column("InErrors"),
        column("RcvbufErrors"),
        column("SndbufErrors"),
    ))
}

/// Sum a set of optional readings. `None` when the set is empty or any member
/// is missing, so a partially observable total is never reported as a number.
fn sum_all(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    let mut total = 0_u64;
    let mut seen = false;
    for value in values {
        seen = true;
        total = total.saturating_add(value?);
    }
    seen.then_some(total)
}

/// Every SRT-protocol shard in one `/metrics/system` snapshot.
pub(super) fn srt_shards(system: &Value) -> Vec<&Value> {
    system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|shard| shard["protocol"] == "srt")
        .collect()
}

/// Kernel UDP error counters and NIC drop counters for the host. `None` for
/// any counter the host does not expose.
#[derive(Default, Clone, Copy)]
struct HostDropCounters {
    pub(super) udp_in_errors: Option<u64>,
    pub(super) udp_rcvbuf_errors: Option<u64>,
    pub(super) udp_sndbuf_errors: Option<u64>,
    pub(super) nic_rx_dropped: Option<u64>,
    pub(super) nic_tx_dropped: Option<u64>,
}

impl HostDropCounters {
    /// `/proc/net/snmp`'s `Udp:` row plus `/sys/class/net/*/statistics`
    /// drops. Loopback is excluded from the NIC sum: its drop counters are
    /// not a NIC signal, and a loopback-only run would otherwise report them
    /// as if they were.
    fn read() -> Self {
        let mut counters = Self::default();
        if let Ok(snmp) = std::fs::read_to_string("/proc/net/snmp")
            && let Some((in_errors, rcvbuf, sndbuf)) = parse_udp_snmp(&snmp)
        {
            counters.udp_in_errors = in_errors;
            counters.udp_rcvbuf_errors = rcvbuf;
            counters.udp_sndbuf_errors = sndbuf;
        }
        let mut nic_rx = None;
        let mut nic_tx = None;
        if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name == "lo" {
                    continue;
                }
                let statistics = entry.path().join("statistics");
                if let Some(value) = read_counter_file(&statistics.join("rx_dropped")) {
                    nic_rx = Some(nic_rx.unwrap_or(0) + value);
                }
                if let Some(value) = read_counter_file(&statistics.join("tx_dropped")) {
                    nic_tx = Some(nic_tx.unwrap_or(0) + value);
                }
            }
        }
        counters.nic_rx_dropped = nic_rx;
        counters.nic_tx_dropped = nic_tx;
        counters
    }
}

fn read_counter_file(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn f64_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

/// `current - previous` as a per-second rate. `None` on the first sample, a
/// counter reset, a non-positive interval, or a missing reading on either
/// side — an unobservable source never becomes a rate.
pub(super) fn rate_per_sec(
    current: Option<u64>,
    previous: Option<u64>,
    elapsed_secs: f64,
) -> Option<f64> {
    let (current, previous) = (current?, previous?);
    (elapsed_secs > 0.0 && current >= previous).then(|| (current - previous) as f64 / elapsed_secs)
}

/// Run-level metadata every artifact carries, so a result can never be read
/// without its tree, workload and environment.
pub(super) struct RunMetadata {
    pub(super) git_sha: Option<String>,
    pub(super) git_dirty: Option<bool>,
    pub(super) restream_binary: String,
    pub(super) bitrate_label: String,
    pub(super) peer_mode: String,
    pub(super) peer_targets: Vec<String>,
    /// Present only when the rung points at remote sink peers, in which case
    /// their state endpoints gate the rung's validity.
    pub(super) peer_state: Option<PeerStateConfig>,
    pub(super) sample_secs: u64,
    pub(super) settle_secs: u64,
    pub(super) sample_interval_ms: u64,
    pub(super) egress_counts: Vec<usize>,
    pub(super) scenario_filter: Vec<String>,
}

impl RunMetadata {
    pub(super) fn from_env(env: &ResourceSweepEnv) -> Self {
        Self {
            git_sha: git_output(&["rev-parse", "HEAD"]),
            // Untracked-but-not-ignored files count as dirty: a "clean tree"
            // claim must mean the whole work tree, not just tracked files.
            git_dirty: git_output(&["status", "--porcelain"])
                .map(|status| !status.trim().is_empty()),
            restream_binary: env.restream_bin.display().to_string(),
            bitrate_label: env.bitrate.clone(),
            peer_mode: env.peer_mode.as_str().to_string(),
            peer_targets: env.srt_peer_targets(),
            peer_state: env.peer_state_config(),
            sample_secs: env.sample_secs,
            settle_secs: env.settle_secs,
            sample_interval_ms: env.sample_interval_ms,
            egress_counts: env.egress_counts.clone(),
            scenario_filter: env
                .scenario_filter
                .as_ref()
                .map(|set| {
                    let mut filter: Vec<String> = set.iter().cloned().collect();
                    filter.sort();
                    filter
                })
                .unwrap_or_default(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "generatedAt": chrono::Utc::now().to_rfc3339(),
            "gitSha": self.git_sha,
            "gitDirty": self.git_dirty,
            "restreamBinary": self.restream_binary,
            "bitrateLabel": self.bitrate_label,
            "peerMode": self.peer_mode,
            "peerTargets": self.peer_targets,
            "peerStateEndpoint": self.peer_state.as_ref().map(|state| json!({
                "hosts": state.hosts,
                "port": state.port,
            })),
            "sampleSecs": self.sample_secs,
            "settleSecs": self.settle_secs,
            "sampleIntervalMs": self.sample_interval_ms,
            "egressCounts": self.egress_counts,
            "scenarioFilter": self.scenario_filter,
        })
    }
}

/// One `git` probe; `None` when git is unavailable or the call fails.
fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Contract artifact state: previous counters plus every emitted record.
struct PacketContractSampler {
    previous: Option<PacketCounters>,
    /// When `previous` was read. Packet rates are measured over this interval,
    /// which starts at the prime — not at the first rated sample.
    previous_at: Option<Instant>,
    /// Start of the rated window, set by `prime`.
    rated_started: Option<Instant>,
    /// `(scenario, outputs)` of the previous sample. Counters are only
    /// comparable inside one rung, so a change resets the history instead of
    /// differencing a new rung against the previous rung's last sample.
    previous_key: Option<(String, u64)>,
    peer_fold: PeerFold,
    samples: Vec<Value>,
    jsonl: PathBuf,
    run: RunMetadata,
}

use super::packet_contract_peers::{PeerFold, PeerStateConfig, poll_peers};
use super::packet_contract_verdict::sample_validity;

static SAMPLER: Mutex<Option<PacketContractSampler>> = Mutex::new(None);

fn with_sampler<T>(f: impl FnOnce(&mut PacketContractSampler) -> T) -> Option<T> {
    let mut guard = SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.as_mut().map(f)
}

/// Start recording into `work_dir` (`packet-contract-samples.jsonl` +
/// `packet-contract.json`). Idempotent per run.
pub(super) fn begin(work_dir: &Path, run: RunMetadata) {
    let jsonl = samples_jsonl(work_dir);
    let _ = std::fs::remove_file(&jsonl);
    let _ = std::fs::remove_file(summary_json(work_dir));
    *SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PacketContractSampler {
        previous: None,
        previous_at: None,
        rated_started: None,
        previous_key: None,
        peer_fold: PeerFold::default(),
        samples: Vec::new(),
        jsonl,
        run,
    });
}

pub(super) fn samples_jsonl(work_dir: &Path) -> PathBuf {
    work_dir.join("packet-contract-samples.jsonl")
}

pub(super) fn summary_json(work_dir: &Path) -> PathBuf {
    work_dir.join("packet-contract.json")
}

/// Take the pre-rated baseline: the cumulative counters and every configured
/// peer's state, immediately after the settle period and before the rated
/// clock starts. Without this the first rated sample would be spent creating a
/// baseline, so the rated window would begin one sampling interval late.
pub(super) async fn prime(system: &Value, meta: &ResourceScenarioMeta<'_>) -> Result<(), String> {
    prime_at(system, meta, Instant::now()).await
}

/// [`prime`] with an explicit observation time (tests drive the clock).
pub(super) async fn prime_at(
    system: &Value,
    meta: &ResourceScenarioMeta<'_>,
    observed_at: Instant,
) -> Result<(), String> {
    let peer_config = with_sampler(|sampler| sampler.run.peer_state.clone()).flatten();
    let peer_readings = match &peer_config {
        Some(config) => poll_peers(config).await,
        None => Vec::new(),
    };
    with_sampler(|sampler| {
        sampler.previous = Some(PacketCounters::read(system));
        sampler.previous_at = Some(observed_at);
        sampler.rated_started = Some(observed_at);
        // The prime belongs to this rung, so the first rated sample must not
        // look like a rung change and reset what the prime just established.
        sampler.previous_key = Some((meta.scenario.to_string(), meta.outputs as u64));
        sampler.peer_fold.clear();
        sampler.peer_fold.prime(&peer_readings);
    });
    Ok(())
}

/// Record one sample. A no-op when [`begin`] was never called.
pub(super) async fn record(
    system: &Value,
    elapsed_secs: f64,
    cpu_pct: f64,
    meta: &ResourceScenarioMeta<'_>,
) -> Result<(), String> {
    record_at(system, elapsed_secs, cpu_pct, meta, Instant::now()).await
}

/// [`record`] with an explicit observation time (tests drive the clock).
pub(super) async fn record_at(
    system: &Value,
    elapsed_secs: f64,
    cpu_pct: f64,
    meta: &ResourceScenarioMeta<'_>,
    observed_at: Instant,
) -> Result<(), String> {
    let peer_config = with_sampler(|sampler| sampler.run.peer_state.clone()).flatten();
    let peer_readings = match &peer_config {
        Some(config) => poll_peers(config).await,
        None => Vec::new(),
    };
    let Some((record, jsonl)) = with_sampler(|sampler| {
        // Counters are comparable only inside one rung: a `(scenario, outputs)`
        // change clears the history so the new rung's first sample is unrated
        // instead of differencing against the previous rung.
        let key = (meta.scenario.to_string(), meta.outputs as u64);
        if sampler.previous_key.as_ref() != Some(&key) {
            sampler.previous = None;
            sampler.previous_at = None;
            sampler.rated_started = Some(observed_at);
            sampler.peer_fold.clear();
            sampler.previous_key = Some(key);
        }
        // Packet rates cover the interval since the last counter reading —
        // the prime for the first rated sample — falling back to the caller's
        // sampling interval when no prime happened.
        let elapsed_secs = sampler
            .previous_at
            .map(|previous_at| {
                observed_at
                    .saturating_duration_since(previous_at)
                    .as_secs_f64()
            })
            .filter(|elapsed| *elapsed > 0.0)
            .unwrap_or(elapsed_secs);
        let counters = PacketCounters::read(system);
        let previous = sampler.previous;
        let previous_ref = previous.as_ref();
        let previous_of = |get: fn(&PacketCounters) -> Option<u64>| previous_ref.and_then(get);
        let rate = |current: Option<u64>, get: fn(&PacketCounters) -> Option<u64>| {
            rate_per_sec(current, previous_of(get), elapsed_secs)
        };
        let shards = srt_shards(system);
        let owners: Vec<&Value> = shards
            .iter()
            .flat_map(|shard| shard["srtOwners"].as_array().into_iter().flatten())
            .collect();
        let max_field = |array: &[&Value], key: &str| -> Option<u64> {
            array.iter().filter_map(|entry| u64_field(entry, key)).max()
        };
        let sum_field = |array: &[&Value], key: &str| -> Option<u64> {
            sum_all(array.iter().map(|entry| u64_field(entry, key)))
        };
        let unhealthy_shards = |shards: &[&Value]| -> Option<u64> {
            (!shards.is_empty()).then(|| {
                shards
                    .iter()
                    .filter(|shard| shard[SHARD_HEALTH_FIELD] != "healthy")
                    .count() as u64
            })
        };
        let capacity = &system["capacity"];
        let flow = &capacity["flow"];
        let tx_datagrams_rate = rate(counters.srt_tx_datagrams, |c| c.srt_tx_datagrams);
        let data_first_rate = rate(counters.srt_data_first, |c| c.srt_data_first);
        let data_retx_rate = rate(counters.srt_data_retx, |c| c.srt_data_retx);
        let cpu_seconds = cpu_pct / 100.0 * elapsed_secs;
        let mut record = serde_json::Map::new();
        // SRT TX datagram taxonomy: first-transmission DATA, retransmitted
        // DATA, control, and their sum. Retransmissions are reported apart
        // from DATA because §9.3's DATA-pps target counts first transmissions;
        // RX and timer/maintenance work stay separately named below.
        // Derived cost signals (the portable stand-in for cycles/packet:
        // no PMU is available on the reference hosts).
        // Scheduler and owner gauges at sample time.
        // Capacity projection and flow view.
        record.insert("scenario".to_string(), json!(meta.scenario));
        record.insert("label".to_string(), json!(meta.label));
        record.insert("outputs".to_string(), json!(meta.outputs));
        record.insert("intervalSecs".to_string(), json!(round2(elapsed_secs)));
        record.insert(
            "ratedSecs".to_string(),
            json!(sampler.rated_started.map(|started| {
                round2(observed_at.saturating_duration_since(started).as_secs_f64())
            })),
        );
        record.insert("cpuPct".to_string(), json!(round2(cpu_pct)));
        record.insert(
            "srtTxDatagramsPerSec".to_string(),
            json!(tx_datagrams_rate.map(round2)),
        );
        record.insert(
            "srtTxCompletedPerSec".to_string(),
            json!(rate(counters.srt_tx_completed, |c| c.srt_tx_completed).map(round2)),
        );
        record.insert(
            "srtRxDatagramsPerSec".to_string(),
            json!(rate(counters.srt_rx_datagrams, |c| c.srt_rx_datagrams).map(round2)),
        );
        record.insert(
            "srtDataFirstPps".to_string(),
            json!(data_first_rate.map(round2)),
        );
        record.insert(
            "srtDataRetransmitPps".to_string(),
            json!(data_retx_rate.map(round2)),
        );
        record.insert(
            "srtControlPps".to_string(),
            json!(rate(counters.srt_control, |c| c.srt_control).map(round2)),
        );
        // Informational: retransmissions as a share of first-transmission
        // DATA. A no-loss rung is ~0; a material share means the path (or the
        // peer) is dropping, which the validity block also reports.
        record.insert(
            "srtRetransmitShare".to_string(),
            json!(match (data_retx_rate, data_first_rate) {
                (Some(retx), Some(first)) if first > 0.0 => {
                    Some(round2(retx / first))
                }
                _ => None,
            }),
        );
        record.insert(
            "srtServiceVisitsPerSec".to_string(),
            json!(rate(counters.srt_service_visits, |c| c.srt_service_visits).map(round2)),
        );
        record.insert(
            "srtServiceActionsPerSec".to_string(),
            json!(rate(counters.srt_service_actions, |c| c.srt_service_actions).map(round2)),
        );
        record.insert(
            "srtMaintenanceActionsPerSec".to_string(),
            json!(
                rate(counters.srt_maintenance_actions, |c| c
                    .srt_maintenance_actions)
                .map(round2)
            ),
        );
        record.insert(
            "shardLoopIterationsPerSec".to_string(),
            json!(rate(counters.shard_loop_iterations, |c| c.shard_loop_iterations).map(round2)),
        );
        record.insert(
            "shardMediaTicksPerSec".to_string(),
            json!(rate(counters.shard_media_ticks, |c| c.shard_media_ticks).map(round2)),
        );
        record.insert(
            "shardReadyVisitsPerSec".to_string(),
            json!(rate(counters.shard_ready_visits, |c| c.shard_ready_visits).map(round2)),
        );
        record.insert(
            "shardRetriesPerSec".to_string(),
            json!(rate(counters.shard_retries, |c| c.shard_retries).map(round2)),
        );
        record.insert(
            "shardUnhealthyCount".to_string(),
            json!(unhealthy_shards(&shards)),
        );
        record.insert(
            "udpInErrorsPerSec".to_string(),
            json!(rate(counters.udp_in_errors, |c| c.udp_in_errors).map(round2)),
        );
        record.insert(
            "udpRcvbufErrorsPerSec".to_string(),
            json!(rate(counters.udp_rcvbuf_errors, |c| c.udp_rcvbuf_errors).map(round2)),
        );
        record.insert(
            "udpSndbufErrorsPerSec".to_string(),
            json!(rate(counters.udp_sndbuf_errors, |c| c.udp_sndbuf_errors).map(round2)),
        );
        record.insert(
            "nicRxDroppedPerSec".to_string(),
            json!(rate(counters.nic_rx_dropped, |c| c.nic_rx_dropped).map(round2)),
        );
        record.insert(
            "nicTxDroppedPerSec".to_string(),
            json!(rate(counters.nic_tx_dropped, |c| c.nic_tx_dropped).map(round2)),
        );
        record.insert(
            "cpuMicrosPerSrtPacket".to_string(),
            json!(
                tx_datagrams_rate
                    .filter(|rate| *rate > 0.0)
                    .map(|rate| round2(cpu_seconds / (rate * elapsed_secs) * 1e6))
            ),
        );
        record.insert(
            "srtTxDatagramsPerOutputPerSec".to_string(),
            json!(
                tx_datagrams_rate
                    .filter(|_| meta.outputs > 0)
                    .map(|rate| round2(rate / meta.outputs as f64))
            ),
        );
        // The number to compare against the roadmap's ~760 DATA packets/s per
        // destination: first transmissions only, retransmissions excluded.
        record.insert(
            "srtDataFirstPpsPerOutput".to_string(),
            json!(
                data_first_rate
                    .filter(|_| meta.outputs > 0)
                    .map(|rate| round2(rate / meta.outputs as f64))
            ),
        );
        record.insert("egressShardCount".to_string(), json!(shards.len()));
        record.insert(
            "srtOwnerCount".to_string(),
            json!(
                owners
                    .iter()
                    .filter(|owner| owner["present"] == true)
                    .count()
            ),
        );
        record.insert(
            "shardReadyDepthMax".to_string(),
            json!(max_field(&shards, "readyDepth")),
        );
        record.insert(
            "shardReadyDepthHwmMax".to_string(),
            json!(max_field(&shards, "readyDepthHwm")),
        );
        record.insert(
            "shardBudgetExhaustions".to_string(),
            json!(sum_field(&shards, "budgetExhaustions")),
        );
        record.insert(
            "shardQueueOverflows".to_string(),
            json!(sum_field(&shards, "queueOverflows")),
        );
        record.insert(
            "shardDriverBudgetViolations".to_string(),
            json!(sum_field(&shards, "driverBudgetViolations")),
        );
        record.insert(
            "shardFeedResyncs".to_string(),
            json!(sum_field(&shards, "resyncCount")),
        );
        record.insert(
            "ownerTxInFlightMax".to_string(),
            json!(max_field(&owners, "txInFlight")),
        );
        record.insert(
            "ownerTxCapacityMax".to_string(),
            json!(max_field(&owners, "txCapacity")),
        );
        record.insert(
            "ownerTxExhaustions".to_string(),
            json!(sum_field(&owners, "txExhaustions")),
        );
        record.insert(
            "ownerTxFailedSends".to_string(),
            json!(sum_field(&owners, "txFailedSends")),
        );
        record.insert(
            "ownerServiceBudgetExhausted".to_string(),
            json!(sum_field(&owners, "serviceBudgetExhausted")),
        );
        record.insert(
            "ownerCallerInFlightHwm".to_string(),
            json!(max_field(&owners, "callerInFlightHwm")),
        );
        record.insert(
            "ownerCallerQueuedHwm".to_string(),
            json!(max_field(&owners, "callerQueuedHwm")),
        );
        record.insert(
            "ownerRxRingDropped".to_string(),
            json!(sum_field(&owners, "rxRingDropped")),
        );
        record.insert(
            "ownerRxTruncated".to_string(),
            json!(sum_field(&owners, "rxTruncated")),
        );
        record.insert(
            "ownerFaulted".to_string(),
            json!(owners.iter().any(|owner| owner["faulted"] == true)),
        );
        record.insert(
            "srtRuntimeIoUring".to_string(),
            json!(
                system["egressShards"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|shard| shard["srtRuntimeIoUring"] == true)
            ),
        );
        record.insert(
            "srtManagedRxAvailable".to_string(),
            json!(
                system["egressShards"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|shard| shard["srtManagedRxAvailable"] == true)
            ),
        );
        record.insert(
            "capacityIngressPps".to_string(),
            json!(f64_field(capacity, "ingressPps").map(round2)),
        );
        record.insert(
            "capacityEgressPps".to_string(),
            json!(f64_field(capacity, "egressPps").map(round2)),
        );
        record.insert(
            "capacityMediaBps".to_string(),
            json!(f64_field(capacity, "mediaBps").map(round2)),
        );
        record.insert(
            "capacityActiveLeaves".to_string(),
            json!(u64_field(capacity, "activeLeaves")),
        );
        record.insert(
            "hottestShardUtil".to_string(),
            json!(f64_field(capacity, "hottestShardUtil").map(round2)),
        );
        record.insert(
            "hottestCenter".to_string(),
            json!(capacity["hottestCenter"].clone()),
        );
        record.insert(
            "flowQueue".to_string(),
            json!(f64_field(flow, "queue").map(round2)),
        );
        record.insert(
            "flowBacklogSlope".to_string(),
            json!(f64_field(flow, "backlogSlope").map(round2)),
        );
        record.insert(
            "flowDeadlineSlackMs".to_string(),
            json!(f64_field(flow, "deadlineSlackMs").map(round2)),
        );
        record.insert(
            "flowDelayMs".to_string(),
            json!(f64_field(flow, "delayMs").map(round2)),
        );
        record.insert(
            "flowErrors".to_string(),
            json!(f64_field(flow, "errors").map(round2)),
        );
        record.insert(
            "flowAmplification".to_string(),
            json!(f64_field(flow, "amplification").map(round2)),
        );
        record.insert("flowStatus".to_string(), json!(flow["status"].clone()));
        if let Some(config) = &peer_config {
            record.insert("expectedPeers".to_string(), json!(config.hosts.len()));
            record.insert(
                "peers".to_string(),
                sampler.peer_fold.record(&peer_readings),
            );
        }
        let mut record = Value::Object(record);
        if let Some((status, reasons)) = sample_validity(&record, meta.outputs as u64)
            && let Value::Object(map) = &mut record
        {
            map.insert(
                "validity".to_string(),
                json!({ "status": status, "reasons": reasons }),
            );
        }
        sampler.previous = Some(counters);
        sampler.previous_at = Some(observed_at);
        sampler.samples.push(record.clone());
        (record, sampler.jsonl.clone())
    }) else {
        return Ok(());
    };
    append_line(
        &jsonl,
        &format!("{}\n", serde_json::to_string(&record).unwrap()),
    )
}

/// Write the per-rung summary (means/peaks plus the validity verdict) and
/// return its path. A no-op when [`begin`] was never called.
///
/// Per-rung aggregation lives in `packet_contract_summary.rs`; this function
/// owns the artifact plumbing (grouping, path, `unavailable`).
pub(super) fn finish(work_dir: &Path) -> Result<Option<PathBuf>, String> {
    let Some((samples, run)) =
        with_sampler(|sampler| (std::mem::take(&mut sampler.samples), sampler.run.to_json()))
    else {
        return Ok(None);
    };
    if samples.is_empty() {
        return Ok(None);
    }
    // One rung per `(scenario, output count)`: a run that walks several rungs
    // must not average them into one number, and its validity verdicts differ
    // per rung.
    let mut rung_keys: Vec<(String, u64)> = Vec::new();
    for sample in &samples {
        let key = (
            sample["scenario"].as_str().unwrap_or_default().to_string(),
            sample["outputs"].as_u64().unwrap_or(0),
        );
        if !rung_keys.contains(&key) {
            rung_keys.push(key);
        }
    }
    let mut rungs = Vec::new();
    for (scenario, outputs) in &rung_keys {
        let rung_samples: Vec<&Value> = samples
            .iter()
            .filter(|sample| {
                sample["scenario"].as_str() == Some(scenario.as_str())
                    && sample["outputs"].as_u64() == Some(*outputs)
            })
            .collect();
        rungs.push(super::packet_contract_summary::rung_summary(
            scenario,
            *outputs,
            samples.len(),
            &rung_samples,
        ));
    }
    let summary = json!({
        "contract": "wi3.4-packet-rate",
        "run": run,
        "samplesJsonl": samples_jsonl(work_dir),
        "sampleCount": samples.len(),
        "rungs": rungs,
        "samples": samples,
        "unavailable": {
            "schedulerWakeRate": "ShardMetrics::record_useful_wake/record_empty_wake have no production caller in the current tree, so feedWakesUseful/feedWakesEmpty read 0 for every backend. loopIterationsPerSec, mediaTicksPerSec and readyVisitsPerSec are the scheduler-activity signals that are actually produced.",
            "sqesPerSubmission": "Compio's runtime ring counters are not exposed; the shard sqes/cqes fields are written only by the native RTMP dataplane, so the SRT Owner path has no producer.",
            "ioUringEntersPerSec": "Same gap: no runtime enter counter is published.",
            "cyclesPerPacket": "No PMU on the reference hosts; cpuMicrosPerSrtPacket is the portable stand-in.",
        }
    });
    let path = summary_json(work_dir);
    std::fs::write(&path, serde_json::to_vec_pretty(&summary).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(Some(path))
}
