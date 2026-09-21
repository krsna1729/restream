//! WI3.4 packet-rate benchmark contract sampling.
//!
//! One flat record per sample, sourced from the production packet-rate
//! surfaces (`/metrics/system` `egressShards`, `capacity` and `ioUring`) plus
//! host kernel/NIC drop counters. Cumulative counters are differenced between
//! samples, so the artifact carries per-second rates; a first sample or a
//! counter reset reports `null` rather than a fabricated zero.
//!
//! This establishes the measurement contract in
//! `docs/srt-compio-roadmap.md` §10. It measures; it does not optimize.
//! Metrics the product cannot source yet (scheduler wake rate, SQEs per
//! submission, io_uring enters/s, and `cycles/packet` without a PMU) are
//! recorded in the summary's `unavailable` block with the reason, never as a
//! zero. DATA versus control versus retransmission packet rates come from the
//! Owner's per-class TX counters (`srtOwners[].txClass`).
//!
//! State lives in one process-global sampler: a harness measurement mode runs
//! once per process, and the resource-sweep scenarios share one artifact.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::measurement::round2;
use super::*;

/// Cumulative counters that only become rates across two samples.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct PacketCounters {
    /// SRT Owner TX submissions: every datagram handed to a socket.
    srt_tx_packets: u64,
    /// SRT Owner TX completions observed.
    srt_tx_completed: u64,
    /// SRT Owner RX datagrams (publisher DATA plus control traffic).
    srt_rx_packets: u64,
    /// SRT DATA datagrams submitted (first transmissions plus retransmits).
    srt_tx_data: u64,
    /// SRT DATA retransmissions submitted.
    srt_tx_data_retx: u64,
    /// SRT control datagrams submitted (ACK/ACKACK/NAK/keepalive/...).
    srt_tx_control: u64,
    srt_service_visits: u64,
    srt_service_actions: u64,
    srt_maintenance_actions: u64,
    shard_loop_iterations: u64,
    shard_media_ticks: u64,
    shard_ready_visits: u64,
    udp_in_errors: u64,
    udp_rcvbuf_errors: u64,
    udp_sndbuf_errors: u64,
    nic_rx_dropped: u64,
    nic_tx_dropped: u64,
}

impl PacketCounters {
    /// Fold one `/metrics/system` snapshot plus the host drop counters.
    /// Only SRT shards count: the contract describes the SRT datapath, and a
    /// mixed run must not dilute it with RTMP/sink shard loops.
    fn read(system: &Value) -> Self {
        let mut counters = Self::default();
        for shard in srt_shards(system) {
            counters.shard_loop_iterations += u64_field(shard, "loopIterations");
            counters.shard_media_ticks += u64_field(shard, "mediaTicks");
            counters.shard_ready_visits += u64_field(shard, "readyVisits");
            for owner in shard["srtOwners"].as_array().into_iter().flatten() {
                counters.srt_tx_packets += u64_field(owner, "txPackets");
                counters.srt_tx_completed += u64_field(owner, "txCompletedOk");
                counters.srt_rx_packets += u64_field(owner, "rxPackets");
                let class = &owner["txClass"];
                let data = u64_field(class, "dataFirst") + u64_field(class, "dataRetransmit");
                counters.srt_tx_data += data;
                counters.srt_tx_data_retx += u64_field(class, "dataRetransmit");
                counters.srt_tx_control += u64_field(owner, "txPackets").saturating_sub(data);
                counters.srt_service_visits += u64_field(owner, "serviceVisits");
                counters.srt_service_actions += u64_field(owner, "serviceActions");
                counters.srt_maintenance_actions += u64_field(owner, "maintenanceActions");
            }
        }
        let host = HostDropCounters::read();
        counters.udp_in_errors = host.udp_in_errors;
        counters.udp_rcvbuf_errors = host.udp_rcvbuf_errors;
        counters.udp_sndbuf_errors = host.udp_sndbuf_errors;
        counters.nic_rx_dropped = host.nic_rx_dropped;
        counters.nic_tx_dropped = host.nic_tx_dropped;
        counters
    }
}

/// Every SRT-protocol shard in one `/metrics/system` snapshot.
fn srt_shards(system: &Value) -> Vec<&Value> {
    system["egressShards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|shard| shard["protocol"] == "srt")
        .collect()
}

/// Kernel UDP error counters and NIC drop counters for the host.
#[derive(Default, Clone, Copy)]
struct HostDropCounters {
    udp_in_errors: u64,
    udp_rcvbuf_errors: u64,
    udp_sndbuf_errors: u64,
    nic_rx_dropped: u64,
    nic_tx_dropped: u64,
}

impl HostDropCounters {
    /// `/proc/net/snmp`'s `Udp:` row plus `/sys/class/net/*/statistics`
    /// drops. Loopback is excluded from the NIC sum: its drop counters are
    /// not a NIC signal, and a loopback-only run would otherwise report
    /// them as if they were.
    fn read() -> Self {
        let mut counters = Self::default();
        if let Ok(snmp) = std::fs::read_to_string("/proc/net/snmp") {
            let mut lines = snmp
                .lines()
                .filter_map(|line| line.strip_prefix("Udp:"))
                .map(str::split_whitespace);
            if let (Some(header), Some(values)) = (lines.next(), lines.next()) {
                let column = |name: &str| -> u64 {
                    header
                        .clone()
                        .position(|field| field == name)
                        .and_then(|index| values.clone().nth(index))
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0)
                };
                counters.udp_in_errors = column("InErrors");
                counters.udp_rcvbuf_errors = column("RcvbufErrors");
                counters.udp_sndbuf_errors = column("SndbufErrors");
            }
        }
        if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name == "lo" {
                    continue;
                }
                let statistics = entry.path().join("statistics");
                counters.nic_rx_dropped += read_counter_file(&statistics.join("rx_dropped"));
                counters.nic_tx_dropped += read_counter_file(&statistics.join("tx_dropped"));
            }
        }
        counters
    }
}

fn read_counter_file(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn f64_field(value: &Value, key: &str) -> Option<f64> {
    value[key].as_f64()
}

/// `current - previous` as a per-second rate. `None` on the first sample, a
/// counter reset, or a non-positive interval.
fn rate_per_sec(current: u64, previous: Option<u64>, elapsed_secs: f64) -> Option<f64> {
    let previous = previous?;
    (elapsed_secs > 0.0 && current >= previous).then(|| (current - previous) as f64 / elapsed_secs)
}

/// Contract artifact state: previous counters plus every emitted record.
#[derive(Default)]
struct PacketContractSampler {
    previous: Option<PacketCounters>,
    samples: Vec<Value>,
    jsonl: PathBuf,
}

static SAMPLER: Mutex<Option<PacketContractSampler>> = Mutex::new(None);

fn with_sampler<T>(f: impl FnOnce(&mut PacketContractSampler) -> T) -> Option<T> {
    let mut guard = SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.as_mut().map(f)
}

/// Start recording into `work_dir` (`packet-contract-samples.jsonl` +
/// `packet-contract.json`). Idempotent per run.
pub(super) fn begin(work_dir: &Path) {
    let jsonl = samples_jsonl(work_dir);
    let _ = std::fs::remove_file(&jsonl);
    let _ = std::fs::remove_file(summary_json(work_dir));
    *SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PacketContractSampler {
        jsonl,
        ..PacketContractSampler::default()
    });
}

pub(super) fn samples_jsonl(work_dir: &Path) -> PathBuf {
    work_dir.join("packet-contract-samples.jsonl")
}

pub(super) fn summary_json(work_dir: &Path) -> PathBuf {
    work_dir.join("packet-contract.json")
}

/// Record one sample. A no-op when [`begin`] was never called.
pub(super) fn record(
    system: &Value,
    elapsed_secs: f64,
    cpu_pct: f64,
    meta: &ResourceScenarioMeta<'_>,
) -> Result<(), String> {
    let Some((record, jsonl)) = with_sampler(|sampler| {
        let counters = PacketCounters::read(system);
        let previous = sampler.previous;
        let previous_ref = previous.as_ref();
        let previous_of = |get: fn(&PacketCounters) -> u64| previous_ref.map(get);
        let rate = |current: u64, get: fn(&PacketCounters) -> u64| {
            rate_per_sec(current, previous_of(get), elapsed_secs)
        };
        let shards = srt_shards(system);
        let owners: Vec<&Value> = shards
            .iter()
            .flat_map(|shard| shard["srtOwners"].as_array().into_iter().flatten())
            .collect();
        let max_field = |array: &[&Value], key: &str| -> u64 {
            array
                .iter()
                .map(|entry| u64_field(entry, key))
                .max()
                .unwrap_or(0)
        };
        let sum_field = |array: &[&Value], key: &str| -> u64 {
            array.iter().map(|entry| u64_field(entry, key)).sum()
        };
        let capacity = &system["capacity"];
        let flow = &capacity["flow"];
        let tx_packets_rate = rate(counters.srt_tx_packets, |c| c.srt_tx_packets);
        let cpu_seconds = cpu_pct / 100.0 * elapsed_secs;
        let mut record = serde_json::Map::new();
        // Packet events per second on the SRT owner path.
        // Derived cost signals (the portable stand-in for cycles/packet:
        // no PMU is available on the reference hosts).
        // Scheduler and owner gauges at sample time.
        // Capacity projection and flow view.
        record.insert("scenario".to_string(), json!(meta.scenario));
        record.insert("label".to_string(), json!(meta.label));
        record.insert("outputs".to_string(), json!(meta.outputs));
        record.insert("intervalSecs".to_string(), json!(round2(elapsed_secs)));
        record.insert("cpuPct".to_string(), json!(round2(cpu_pct)));
        record.insert(
            "srtTxPacketsPerSec".to_string(),
            json!(tx_packets_rate.map(round2)),
        );
        record.insert(
            "srtTxCompletedPerSec".to_string(),
            json!(rate(counters.srt_tx_completed, |c| c.srt_tx_completed).map(round2)),
        );
        record.insert(
            "srtRxPacketsPerSec".to_string(),
            json!(rate(counters.srt_rx_packets, |c| c.srt_rx_packets).map(round2)),
        );
        record.insert(
            "srtDataPps".to_string(),
            json!(rate(counters.srt_tx_data, |c| c.srt_tx_data).map(round2)),
        );
        record.insert(
            "srtRetransmitsPerSec".to_string(),
            json!(rate(counters.srt_tx_data_retx, |c| c.srt_tx_data_retx).map(round2)),
        );
        record.insert(
            "srtControlPps".to_string(),
            json!(rate(counters.srt_tx_control, |c| c.srt_tx_control).map(round2)),
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
            "shardReadyVisitsPerSec".to_string(),
            json!(rate(counters.shard_ready_visits, |c| c.shard_ready_visits).map(round2)),
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
                tx_packets_rate
                    .filter(|rate| *rate > 0.0)
                    .map(|rate| round2(cpu_seconds / (rate * elapsed_secs) * 1e6))
            ),
        );
        record.insert(
            "srtPacketsPerOutputPerSec".to_string(),
            json!(
                tx_packets_rate
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
        let record = Value::Object(record);
        sampler.previous = Some(counters);
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

/// Write the summary (means/peaks over the run plus the explicit
/// `unavailable` list) and return its path. A no-op when [`begin`] was never
/// called.
pub(super) fn finish(work_dir: &Path) -> Result<Option<PathBuf>, String> {
    let Some(samples) = with_sampler(|sampler| std::mem::take(&mut sampler.samples)) else {
        return Ok(None);
    };
    if samples.is_empty() {
        return Ok(None);
    }
    let rate_keys = [
        "srtTxPacketsPerSec",
        "srtTxCompletedPerSec",
        "srtRxPacketsPerSec",
        "srtDataPps",
        "srtRetransmitsPerSec",
        "srtControlPps",
        "srtServiceVisitsPerSec",
        "srtServiceActionsPerSec",
        "srtMaintenanceActionsPerSec",
        "shardLoopIterationsPerSec",
        "shardMediaTicksPerSec",
        "shardReadyVisitsPerSec",
        "udpInErrorsPerSec",
        "udpRcvbufErrorsPerSec",
        "udpSndbufErrorsPerSec",
        "nicRxDroppedPerSec",
        "nicTxDroppedPerSec",
    ];
    let mut rates = serde_json::Map::new();
    for key in rate_keys {
        let values: Vec<f64> = samples
            .iter()
            .filter_map(|sample| sample[key].as_f64())
            .collect();
        if values.is_empty() {
            continue;
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let peak = values.iter().copied().fold(f64::MIN, f64::max);
        rates.insert(
            key.to_string(),
            json!({
                "mean": round2(mean),
                "peak": round2(peak),
                "samples": values.len(),
            }),
        );
    }
    let gauge_keys = [
        "shardReadyDepthMax",
        "shardReadyDepthHwmMax",
        "shardBudgetExhaustions",
        "shardQueueOverflows",
        "shardDriverBudgetViolations",
        "shardFeedResyncs",
        "ownerTxInFlightMax",
        "ownerTxCapacityMax",
        "ownerTxExhaustions",
        "ownerTxFailedSends",
        "ownerServiceBudgetExhausted",
        "ownerCallerInFlightHwm",
        "ownerCallerQueuedHwm",
        "ownerRxRingDropped",
        "ownerRxTruncated",
    ];
    let mut gauges = serde_json::Map::new();
    for key in gauge_keys {
        gauges.insert(
            key.to_string(),
            json!(
                samples
                    .iter()
                    .map(|sample| sample[key].as_u64().unwrap_or(0))
                    .max()
                    .unwrap_or(0)
            ),
        );
    }
    let mut cost = serde_json::Map::new();
    for key in ["cpuMicrosPerSrtPacket", "srtPacketsPerOutputPerSec"] {
        let values: Vec<f64> = samples
            .iter()
            .filter_map(|sample| sample[key].as_f64())
            .collect();
        if values.is_empty() {
            continue;
        }
        cost.insert(
            key.to_string(),
            json!({
                "mean": round2(values.iter().sum::<f64>() / values.len() as f64),
                "peak": round2(values.iter().copied().fold(f64::MIN, f64::max)),
                "samples": values.len(),
            }),
        );
    }
    let summary = json!({
        "contract": "wi3.4-packet-rate",
        "samplesJsonl": samples_jsonl(work_dir),
        "sampleCount": samples.len(),
        "outputs": samples
            .iter()
            .map(|sample| sample["outputs"].as_u64().unwrap_or(0))
            .max()
            .unwrap_or(0),
        "ratesPerSec": rates,
        "gaugesPeak": gauges,
        "cost": cost,
        "samples": samples,
        "unavailable": {
            "schedulerWakeRate": "ShardMetrics::record_useful_wake/record_empty_wake have no production caller in the current tree, so feedWakesUseful/feedWakesEmpty read 0 for every backend. loopIterationsPerSec, mediaTicksPerSec and readyVisitsPerSec are the scheduler-activity signals that are actually produced.",
            "sqesPerSubmission": "Compio's runtime ring counters are not exposed; the shard sqes/cqes fields are written only by the native RTMP dataplane, so the SRT Owner path has no producer.",
            "ioUringEntersPerSec": "Same gap: no runtime enter counter is published.",
            "cyclesPerPacket": "No PMU on the reference hosts; cpuMicrosPerSrtPacket is the portable stand-in."
        }
    });
    let path = summary_json(work_dir);
    std::fs::write(&path, serde_json::to_vec_pretty(&summary).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_are_null_on_the_first_sample_and_after_a_reset() {
        assert_eq!(rate_per_sec(1_000, None, 1.0), None);
        assert_eq!(rate_per_sec(1_000, Some(500), 1.0), Some(500.0));
        assert_eq!(rate_per_sec(1_000, Some(2_000), 1.0), None, "reset");
        assert_eq!(rate_per_sec(1_000, Some(500), 0.0), None, "no interval");
    }

    #[test]
    fn counters_sum_shard_wakes_and_owner_packets_for_srt_shards_only() {
        let system = json!({
            "egressShards": [
                {
                    "protocol": "srt",
                    "feedWakesUseful": 10,
                    "feedWakesEmpty": 2,
                    "loopIterations": 100,
                    "mediaTicks": 7,
                    "readyVisits": 40,
                    "srtOwners": [
                        {"present": true, "txPackets": 1000, "txCompletedOk": 900, "rxPackets": 30,
                         "serviceVisits": 55, "serviceActions": 900, "maintenanceActions": 4,
                         "txClass": {"dataFirst": 850, "dataRetransmit": 100, "ack": 50}},
                        {"present": false}
                    ]
                },
                {
                    "protocol": "srt",
                    "feedWakesUseful": 5,
                    "feedWakesEmpty": 0,
                    "loopIterations": 50,
                    "mediaTicks": 3,
                    "readyVisits": 20,
                    "srtOwners": [
                        {"present": true, "txPackets": 500, "txCompletedOk": 400, "rxPackets": 10,
                         "serviceVisits": 25, "serviceActions": 400, "maintenanceActions": 1,
                         "txClass": {"dataFirst": 500}}
                    ]
                },
                {
                    // A non-SRT shard's loops must not dilute the contract.
                    "protocol": "rtmp",
                    "feedWakesUseful": 999,
                    "loopIterations": 999,
                    "mediaTicks": 999,
                    "readyVisits": 999,
                    "srtOwners": [{"present": false, "txPackets": 999}]
                }
            ]
        });
        let counters = PacketCounters::read(&system);
        assert_eq!(counters.srt_tx_packets, 1500);
        assert_eq!(counters.srt_tx_completed, 1300);
        assert_eq!(counters.srt_rx_packets, 40);
        assert_eq!(counters.srt_tx_data, 1450, "DATA first + retransmit");
        assert_eq!(counters.srt_tx_data_retx, 100);
        assert_eq!(counters.srt_tx_control, 50, "txPackets - DATA");
        assert_eq!(
            counters.srt_tx_data + counters.srt_tx_control,
            counters.srt_tx_packets
        );
        assert_eq!(counters.srt_service_visits, 80);
        assert_eq!(counters.srt_service_actions, 1300);
        assert_eq!(counters.srt_maintenance_actions, 5);
        assert_eq!(counters.shard_loop_iterations, 150);
        assert_eq!(counters.shard_media_ticks, 10);
        assert_eq!(counters.shard_ready_visits, 60);
        assert_eq!(srt_shards(&system).len(), 2);
    }
}
