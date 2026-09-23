//! The `egress-duty` verdict: every acceptance fence as a named check, plus the
//! artifact's closing blocks (control partition, per-output counters, receiver
//! summary and the reconciliation).

use std::collections::BTreeMap;
use std::path::Path;

use super::super::*;

use super::*;

const VISIT_MAX_BYTES_MIN: u64 = 188;
const VISIT_MAX_BYTES_MAX: u64 = 16 * 1024 * 1024;

pub(super) fn parse_bitrate_bps(raw: &str) -> Option<f64> {
    let normalized = raw.trim();
    let suffix = normalized.chars().last();
    let (number, multiplier) = match suffix {
        Some(suffix) if suffix.is_ascii_alphabetic() => {
            let number = &normalized[..normalized.len().saturating_sub(suffix.len_utf8())];
            let multiplier = match suffix.to_ascii_uppercase() {
                'K' => 1_000.0,
                'M' => 1_000_000.0,
                'G' => 1_000_000_000.0,
                _ => 1.0,
            };
            (number, multiplier)
        }
        _ => (normalized, 1.0),
    };
    number
        .trim()
        .parse::<f64>()
        .ok()
        .map(|value| value * multiplier)
}

fn effective_visit_max_bytes(raw: Option<&str>) -> Option<u64> {
    raw?.trim()
        .parse::<u64>()
        .ok()
        .map(|value| value.clamp(VISIT_MAX_BYTES_MIN, VISIT_MAX_BYTES_MAX))
}

pub(super) struct VerdictInput<'a> {
    pub(super) cfg: &'a EgressDutyConfig,
    pub(super) artifact: &'a mut Artifact,
    pub(super) receiver_row: Option<ReceiverRow>,
    pub(super) receiver_teardown: Value,
    pub(super) receiver_exit: Value,
    pub(super) receiver_stdout_text: &'a str,
    pub(super) receiver_stderr_text: &'a str,
    pub(super) window_samples: &'a [Value],
    pub(super) window_secs_observed: f64,
    pub(super) outputs_before: &'a [OutputSample],
    pub(super) outputs_after: &'a [OutputSample],
    pub(super) receiver_net_delta: Value,
    pub(super) engine_before: Value,
    pub(super) engine_after: Value,
    pub(super) engine_deltas: Value,
    pub(super) engine_final: Value,
    pub(super) fault_before: bool,
    pub(super) fault_after: bool,
    pub(super) fault_final: bool,
    pub(super) data_first_delta: Option<u64>,
    pub(super) wire_delta: Option<u64>,
    pub(super) shard_threads: &'a [EgressShardThread],
    pub(super) all_egress_threads: &'a [EgressShardThread],
    pub(super) srt_shard_indices: &'a [u64],
    pub(super) affinity_rows: &'a [Value],
    pub(super) per_index_counts: &'a BTreeMap<u32, usize>,
    pub(super) visit_max_bytes: Option<u64>,
    pub(super) visit_max_env_value: Option<String>,
    pub(super) tsv_path: &'a Path,
    pub(super) receiver_stdout: &'a Path,
    pub(super) receiver_stderr: &'a Path,
    pub(super) restream_log: &'a Path,
    pub(super) publisher_log: &'a Path,
}

/// Score every fence and write the artifact's closing blocks. A `verdict` of
/// `healthy` means every `required` check passed; a failed one is named in
/// `verdictReasons` with its raw numbers, never loosened.
pub(super) fn emit(input: VerdictInput<'_>) -> Result<(), String> {
    let VerdictInput {
        cfg,
        artifact,
        receiver_row,
        receiver_teardown,
        receiver_exit,
        receiver_stdout_text,
        receiver_stderr_text,
        window_samples,
        window_secs_observed,
        outputs_before,
        outputs_after,
        receiver_net_delta,
        engine_before,
        engine_after,
        engine_deltas,
        engine_final,
        fault_before,
        fault_after,
        fault_final,
        data_first_delta,
        wire_delta,
        shard_threads,
        all_egress_threads,
        affinity_rows,
        srt_shard_indices,
        per_index_counts,
        visit_max_bytes,
        visit_max_env_value,
        tsv_path,
        receiver_stdout,
        receiver_stderr,
        restream_log,
        publisher_log,
    } = input;

    // ── Verdict ─────────────────────────────────────────────────────────
    let mut checks: Vec<Value> = Vec::new();
    let mut failed_required: Vec<String> = Vec::new();
    let mut check = |name: &str, required: bool, ok: bool, detail: Value| {
        if required && !ok {
            failed_required.push(name.to_string());
        }
        checks.push(json!({"name": name, "required": required, "ok": ok, "detail": detail}));
    };

    let receiver_summary = receiver_row.as_ref().map(ReceiverRow::json);
    let (sec_a, sec_b) = match receiver_row.as_ref() {
        Some(row) => (row.number("sec_a"), row.number("sec_b")),
        None => (None, None),
    };
    let protocol_loss_ok = matches!((sec_a, sec_b), (Some(0), Some(0)));
    check(
        "receiver_protocol_loss",
        !cfg.capacity_mode,
        protocol_loss_ok,
        json!({
            "secA": sec_a,
            "secB": sec_b,
            "required": !cfg.capacity_mode,
            "rule": "sec_a (wire DATA lost) and sec_b (duplicates) are measured quality outputs in WI3.7 capacity mode; the frozen arm requires both 0",
            "receiverRowPresent": receiver_row.is_some(),
        }),
    );

    let (udp_rcvbuf, udp_in, udp_no_ports) = match receiver_row.as_ref() {
        Some(row) => (
            row.number("udp_rcvbuf_err"),
            row.number("udp_in_err"),
            row.number("udp_no_ports"),
        ),
        None => (None, None, None),
    };
    check(
        "receiver_kernel_drops",
        true,
        matches!(
            (udp_rcvbuf, udp_in, udp_no_ports),
            (Some(0), Some(0), Some(0))
        ),
        json!({
            "udpRcvbufErr": udp_rcvbuf,
            "udpInErr": udp_in,
            "udpNoPorts": udp_no_ports,
            "rule": "the receiver's own kernel-side UDP counters must all be 0 over the whole run",
        }),
    );

    let (datapath_dropped, local_dropped, retry_overflow) = match receiver_row.as_ref() {
        Some(row) => (
            row.number("datapath_q_dropped"),
            row.number("local_dropped"),
            row.number("retry_overflow"),
        ),
        None => (None, None, None),
    };
    check(
        "receiver_datapath_loss",
        true,
        matches!(
            (datapath_dropped, local_dropped, retry_overflow),
            (Some(0), Some(0), Some(0))
        ),
        json!({
            "datapathQDropped": datapath_dropped,
            "localDropped": local_dropped,
            "retryOverflow": retry_overflow,
            "rule": "the receiver harness's own datapath/retry queues must report no rejection",
        }),
    );

    let receiver_network_drop_paths = [
        ("udpInErrors", &["udp", "InErrors"][..]),
        ("udpRcvbufErrors", &["udp", "RcvbufErrors"][..]),
        ("udpNoPorts", &["udp", "NoPorts"][..]),
        (
            "interfaceRxDropped",
            &["interfaces", "interfaceRxDropped"][..],
        ),
        (
            "interfaceTxDropped",
            &["interfaces", "interfaceTxDropped"][..],
        ),
        ("vethRxDropped", &["interfaces", "vethRxDropped"][..]),
        ("vethTxDropped", &["interfaces", "vethTxDropped"][..]),
        ("softnetDropped", &["softnet", "dropped"][..]),
    ];
    let receiver_network_drops: serde_json::Map<String, Value> = receiver_network_drop_paths
        .iter()
        .map(|(name, path)| {
            (
                (*name).to_string(),
                counter_at(&receiver_net_delta, path).map_or(Value::Null, |value| json!(value)),
            )
        })
        .collect();
    let receiver_network_ok = !receiver_network_drops.is_empty()
        && receiver_network_drops
            .values()
            .all(|value| value.as_u64() == Some(0));
    check(
        "receiver_network_drops",
        cfg.capacity_mode,
        receiver_network_ok,
        json!({
            "delta": receiver_net_delta,
            "requiredFields": receiver_network_drop_paths.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            "selected": receiver_network_drops,
            "rule": "receiver UDP, veth, and softnet drop counters must be readable and remain zero; softnet time-squeeze is retained as a pressure output, while retransmit/duplicate counters are quality outputs",
        }),
    );

    let data_retx_delta = counter_at(&engine_deltas, &["txClass", "dataRetransmit"]);
    let data_retx_lifecycle = counter_at(&engine_final, &["txClass", "dataRetransmit"]);
    check(
        "zero_data_retransmission",
        !cfg.capacity_mode,
        data_retx_delta == Some(0),
        json!({
            "windowDataRetransmit": data_retx_delta,
            "lifecycleDataRetransmit": data_retx_lifecycle,
            "required": !cfg.capacity_mode,
            "rule": "DATA retransmits are a measured quality output in WI3.7 capacity mode; the frozen arm requires txClass.dataRetransmit delta 0",
        }),
    );
    let requested_visit_max_bytes = visit_max_env_value
        .as_deref()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let expected_effective_visit_max_bytes = match visit_max_env_value.as_deref() {
        Some(raw) => effective_visit_max_bytes(Some(raw)),
        None => visit_max_bytes,
    };
    let visit_max_readback_ok = expected_effective_visit_max_bytes == visit_max_bytes;
    check(
        "visit_burst_bound_readback",
        true,
        visit_max_readback_ok,
        json!({
            "envValue": visit_max_env_value,
            "requestedVisitMaxBytes": requested_visit_max_bytes,
            "expectedEffectiveVisitMaxBytes": expected_effective_visit_max_bytes,
            "observedVisitMaxBytes": visit_max_bytes,
            "clamp": {"min": VISIT_MAX_BYTES_MIN, "max": VISIT_MAX_BYTES_MAX},
            "rule": "the product effective-config event must be read back; an explicit RESTREAM_EGRESS_VISIT_MAX_BYTES value is compared after the product's [188,16MiB] clamp",
        }),
    );

    let established = receiver_row
        .as_ref()
        .and_then(|row| row.number("established"));
    check(
        "connections_established",
        true,
        established == Some(cfg.outputs as u64),
        json!({
            "receiverEstablished": established,
            "requestedConnections": cfg.outputs,
            "rule": "every requested output must have a receiver-visible SRT connection",
            "receiverConfiguredConnections": receiver_row.as_ref().and_then(|row| row.get("conns")),
        }),
    );

    // Stability: every output running at every window sample with a strictly
    // increasing delivered-packet count. Keyed by outputId — the telemetry
    // surface's `egresses` array is a map iteration, so its order is not a
    // stable identity across samples.
    let mut stability_failures = Vec::new();
    let mut stability_rows = Vec::new();
    for baseline in outputs_before {
        let mut statuses_ok = true;
        let mut packets: Vec<Option<u64>> = Vec::new();
        for sample in window_samples {
            let row = sample["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["outputId"] == baseline.output_id.as_str());
            match row {
                Some(row) => {
                    if row["status"] != "running" {
                        statuses_ok = false;
                    }
                    packets.push(row["packetsOut"].as_u64());
                }
                None => statuses_ok = false,
            }
        }
        let strictly_increasing = packets
            .windows(2)
            .all(|pair| matches!(pair, [Some(before), Some(after)] if after > before));
        let ok = statuses_ok && strictly_increasing;
        stability_rows.push(json!({
            "outputId": baseline.output_id,
            "outputName": baseline.name,
            "statusesRunning": statuses_ok,
            "packetsOutSeries": packets,
            "ok": ok,
        }));
        if !ok {
            stability_failures.push(json!({
                "outputId": baseline.output_id,
                "outputName": baseline.name,
                "statusesRunning": statuses_ok,
                "packetsOutSeries": packets,
            }));
        }
    }
    check(
        "outputs_stable_all_window",
        true,
        stability_failures.is_empty(),
        json!({
            "failures": stability_failures,
            "outputs": stability_rows,
            "samples": window_samples.iter().map(|sample| sample["atSecs"].clone()).collect::<Vec<_>>(),
            "rule": "every output reports status=running at every window sample with a strictly increasing packetsOut",
        }),
    );

    check(
        "no_owner_fault",
        true,
        !(fault_before || fault_after || fault_final),
        json!({
            "baseline": fault_before,
            "closing": fault_after,
            "afterDrain": fault_final,
            "rule": "no SRT owner may report faulted in any snapshot",
        }),
    );

    let tx_failed_sends = counter_at(&engine_deltas, &["txFailedSends"]);
    let tx_short_sends = counter_at(&engine_deltas, &["txShortSends"]);
    let protocol_output_failures = counter_at(&engine_deltas, &["protocolOutputFailures"]);
    let queue_overflows = counter_at(&engine_deltas, &["queueOverflows"]);
    let product_output_fault_free = matches!(
        (
            tx_failed_sends,
            tx_short_sends,
            protocol_output_failures,
            queue_overflows,
        ),
        (Some(0), Some(0), Some(0), Some(0))
    );
    check(
        "product_output_faults",
        cfg.capacity_mode,
        product_output_fault_free,
        json!({
            "txFailedSends": tx_failed_sends,
            "txShortSends": tx_short_sends,
            "protocolOutputFailures": protocol_output_failures,
            "queueOverflows": queue_overflows,
            "rule": "failed/short sends, protocol output failures, and bounded product queue overflows invalidate an apparatus row; TX pool exhaustion remains a capacity signal",
        }),
    );

    let engine_first_data = counter_at(&engine_final, &["txClass", "dataFirst"]);
    let receiver_data = receiver_row.as_ref().and_then(|row| row.number("pkt_sent"));
    let residual = match (engine_first_data, receiver_data) {
        (Some(engine), Some(receiver)) => Some(engine as i64 - receiver as i64),
        _ => None,
    };
    check(
        "receiver_reconciliation",
        true,
        residual == Some(0),
        json!({
            "rule": "receiver pkt_sent (logical DATA payloads received, whole receiver lifetime) must equal the engine's txClass.dataFirst total over the whole Restream lifetime; residual = engine - receiver",
            "engineTxClassDataFirst": engine_first_data,
            "receiverPktSent": receiver_data,
            "residual": residual,
            "engineFirstDataWireDatagrams": counter_at(&engine_final, &["txPackets"]),
            "receiverCoreTotal": receiver_row.as_ref().and_then(|row| row.number("core_total")),
            "scopeNote": "whole-run, not window-only: the receiver has no per-interval output and cannot delimit a window",
        }),
    );

    let mut matched_indices: Vec<u32> = shard_threads.iter().map(|thread| thread.index).collect();
    matched_indices.sort_unstable();
    matched_indices.dedup();
    let expected_indices: Vec<u64> = cfg
        .requested_shards
        .map(|count| (0..u64::from(count)).collect())
        .unwrap_or_default();
    let topology_ok = cfg.requested_shards.is_none()
        || (srt_shard_indices == expected_indices
            && matched_indices
                .iter()
                .map(|index| u64::from(*index))
                .eq(expected_indices.iter().copied()));
    let affinity_ok = cfg.requested_shards.is_none()
        || affinity_rows.iter().all(|row| {
            row["error"].is_null()
                && row["requestedCpu"].as_u64().is_some()
                && row["observedAffinity"]
                    .as_array()
                    .is_some_and(|cpus| cpus == &[row["requestedCpu"].clone()])
        });
    check(
        "requested_shard_topology",
        cfg.requested_shards.is_some(),
        topology_ok,
        json!({
            "requested": cfg.requested_shards,
            "expectedIndices": expected_indices,
            "metricsIndices": srt_shard_indices,
            "threadIndices": matched_indices,
            "rule": "WI3.7 requested shard count must equal the observed SRT metrics and thread index set",
        }),
    );
    check(
        "requested_shard_affinity",
        cfg.requested_shards.is_some(),
        affinity_ok,
        json!({
            "configuredCpus": cfg.shard_cpus.iter().collect::<Vec<_>>(),
            "affinity": affinity_rows,
            "rule": "WI3.7 shard index N must be pinned to the Nth configured CPU and read back from sched_getaffinity",
        }),
    );
    check(
        "expected_single_srt_shard_thread",
        false,
        shard_threads.len() == 1,
        json!({
            "expected": 1,
            "observed": shard_threads.len(),
            "observedShardIndices": matched_indices,
            "threadsPerIndex": per_index_counts,
            "allEgressShardThreads": all_egress_threads.iter().map(EgressShardThread::json).collect::<Vec<_>>(),
            "note": "legacy WI3.6 diagnostic only; WI3.7 uses requested_shard_topology",
        }),
    );
    const PERSISTENT_BUDGET_EVENTS: u64 = 2;
    let tx_exhaustions = counter_at(&engine_deltas, &["txExhaustions"]);
    let service_budget_exhausted = counter_at(&engine_deltas, &["serviceBudgetExhausted"]);
    let driver_budget_violations = counter_at(&engine_deltas, &["driverBudgetViolations"]);
    let ready_overflows = counter_at(&engine_deltas, &["readyOverflows"]);
    let tx_in_flight = counter_at(&engine_after, &["txInFlight"]);
    let caller_queued_before = counter_at(&engine_before, &["callerQueued"]);
    let caller_queued = counter_at(&engine_after, &["callerQueued"]);
    let caller_queued_hwm = counter_at(&engine_deltas, &["callerQueuedHwm"]);
    let caller_queue_longest_us = counter_at(&engine_deltas, &["callerQueueLongestUs"]);
    let caller_queue_growth = match (caller_queued_before, caller_queued) {
        (Some(before), Some(after)) => after > before,
        _ => false,
    };
    let sustained_queue_us = (window_secs_observed * 1_000_000.0 / 10.0)
        .max(1_000.0)
        .round() as u64;
    let sustained_caller_queue = caller_queued.unwrap_or(0) > 0
        && caller_queue_longest_us.unwrap_or(0) >= sustained_queue_us;
    let sender_backlog = caller_queue_growth || sustained_caller_queue;
    let sender_saturated = sender_backlog
        || tx_exhaustions.unwrap_or(0) > 0
        || service_budget_exhausted.unwrap_or(0) >= PERSISTENT_BUDGET_EVENTS
        || driver_budget_violations.unwrap_or(0) >= PERSISTENT_BUDGET_EVENTS
        || ready_overflows.unwrap_or(0) > 0;
    let amplification = match (wire_delta, data_first_delta) {
        (Some(wire), Some(first)) if first > 0 => {
            json!({
                "wireDatagramsPerDataFirst": round6(wire as f64 / first as f64),
                "controlPerDataFirst": round6((wire as f64 - first as f64) / first as f64),
            })
        }
        _ => Value::Null,
    };
    let receiver_apparatus_limited = receiver_row.is_none()
        || !matches!(
            (udp_rcvbuf, udp_in, udp_no_ports),
            (Some(0), Some(0), Some(0))
        )
        || !matches!(
            (datapath_dropped, local_dropped, retry_overflow),
            (Some(0), Some(0), Some(0))
        )
        || !receiver_network_ok
        || established != Some(cfg.outputs as u64)
        || residual != Some(0);
    let capacity_classification = if receiver_apparatus_limited {
        "receiver-apparatus-limited"
    } else if sender_saturated {
        "sender-saturated"
    } else {
        "stable-unclassified"
    };
    let apparatus_valid = failed_required.is_empty();
    let offered_bitrate_bps = parse_bitrate_bps(&cfg.bitrate);
    let offered_payload_gbps =
        offered_bitrate_bps.map(|bps| bps * cfg.outputs as f64 / 1_000_000_000.0);
    let data_first_rate = data_first_delta
        .filter(|_| window_secs_observed > 0.0)
        .map(|packets| packets as f64 / window_secs_observed);
    let first_data_payload_gbps = data_first_rate.map(|packets_per_sec| {
        packets_per_sec * f64::from(HARNESS_SRT_PACKET_SIZE) * 8.0 / 1_000_000_000.0
    });
    let wire_rate = wire_delta
        .filter(|_| window_secs_observed > 0.0)
        .map(|packets| packets as f64 / window_secs_observed);
    let wire_payload_gbps = wire_rate.map(|packets_per_sec| {
        packets_per_sec * f64::from(HARNESS_SRT_PACKET_SIZE) * 8.0 / 1_000_000_000.0
    });
    artifact.set(
        "capacity",
        json!({
            "mode": cfg.capacity_mode,
            "apparatusValid": apparatus_valid,
            "classification": capacity_classification,
            "offered": {
                "bitrate": cfg.bitrate,
                "outputs": cfg.outputs,
                "payloadGbps": offered_payload_gbps.map(round6),
            },
            "firstData": {
                "packets": data_first_delta,
                "packetsPerSec": data_first_rate.map(round6),
                "payloadGbps": first_data_payload_gbps.map(round6),
            },
            "wire": {
                "datagrams": wire_delta,
                "datagramsPerSec": wire_rate.map(round6),
                "payloadGbps": wire_payload_gbps.map(round6),
            },
            "cpu": artifact.value.get("cpu").cloned().unwrap_or(Value::Null),
            "senderSignals": {
                "saturated": sender_saturated,
                "txExhaustions": tx_exhaustions,
                "serviceBudgetExhausted": service_budget_exhausted,
                "serviceBudgetSaturationThreshold": PERSISTENT_BUDGET_EVENTS,
                "driverBudgetViolations": driver_budget_violations,
                "driverBudgetSaturationThreshold": PERSISTENT_BUDGET_EVENTS,
                "readyOverflows": ready_overflows,
                "txInFlight": tx_in_flight,
                "txHighWaterDelta": counter_at(&engine_deltas, &["txHighWater"]),
                "txHighWaterClosing": counter_at(&engine_after, &["txHighWater"]),
                "callerQueued": caller_queued,
                "callerQueuedHwm": caller_queued_hwm,
                "callerQueueLongestUs": caller_queue_longest_us,
                "callerQueueGrowth": caller_queue_growth,
                "sustainedCallerQueue": sustained_caller_queue,
                "sustainedQueueThresholdUs": sustained_queue_us,
                "backlog": sender_backlog,
            },
            "serviceSignals": {
                "visits": counter_at(&engine_deltas, &["serviceVisits"]),
                "actions": counter_at(&engine_deltas, &["serviceActions"]),
                "durationSumUs": counter_at(&engine_deltas, &["serviceDurationSumUs"]),
                "durationMaxUs": counter_at(&engine_deltas, &["serviceDurationMaxUs"]),
                "driverBudgetViolations": counter_at(&engine_deltas, &["driverBudgetViolations"]),
                "readyDepthHwm": counter_at(&engine_deltas, &["readyDepthHwm"]),
                "readyOverflows": counter_at(&engine_deltas, &["readyOverflows"]),
                "loopDurationSumUs": counter_at(&engine_deltas, &["loopDurationSumUs"]),
                "backlog": sender_backlog,
                "txInFlight": tx_in_flight,
                "callerQueued": caller_queued,
                "missedTicks": Value::Null,
                "missedTicksSource": "not exposed by /metrics/system",
                "latenessUs": {"p50": Value::Null, "p99": Value::Null, "max": Value::Null, "source": "not exposed by /metrics/system"},
            },
            "quality": {
                "dataRetransmit": data_retx_delta,
                "receiverWireLossSecA": sec_a,
                "receiverDuplicatesSecB": sec_b,
                "controlAmplification": amplification,
            },
            "receiverSignals": {
                "apparatusLimited": receiver_apparatus_limited,
                "protocolLossSecA": sec_a,
                "duplicatesSecB": sec_b,
                "softnetTimeSqueeze": counter_at(&receiver_net_delta, &["softnet", "timeSqueeze"]),
            },
            "ladderStop": receiver_apparatus_limited || sender_saturated,
            "rule": "WI3.7 retransmits/duplicates are measured outputs; receiver/kernel/datapath faults reject an apparatus row, while sender exhaustion, lateness, and backlog classify demand saturation",
        }),
    );
    let mut control_partition = serde_json::Map::new();
    for key in TX_CLASS_FIELDS {
        control_partition.insert((*key).to_string(), engine_deltas["txClass"][key].clone());
    }
    artifact.set(
        "control",
        json!({
            "partition": Value::Object(control_partition),
            "protocolAmplification": amplification,
            "totals": {
                "dataFirst": data_first_delta,
                "dataRetransmit": data_retx_delta,
                "wireDatagramsSubmitted": wire_delta,
                "wireDatagramsCompleted": counter_at(&engine_deltas, &["txCompletedOk"]),
                "txHighWater": counter_at(&engine_deltas, &["txHighWater"]),
                "txExhaustions": tx_exhaustions,
                "txFailedSends": tx_failed_sends,
                "txShortSends": tx_short_sends,
                "protocolOutputFailures": protocol_output_failures,
                "queueOverflows": queue_overflows,
                "serviceBudgetExhausted": service_budget_exhausted,
                "txInFlight": tx_in_flight,
                "callerQueued": caller_queued,
                "callerQueuedHwm": caller_queued_hwm,
                "callerQueueLongestUs": caller_queue_longest_us,
                "serviceVisits": counter_at(&engine_deltas, &["serviceVisits"]),
                "serviceActions": counter_at(&engine_deltas, &["serviceActions"]),
                "serviceDurationSumUs": counter_at(&engine_deltas, &["serviceDurationSumUs"]),
                "serviceDurationMaxUs": counter_at(&engine_deltas, &["serviceDurationMaxUs"]),
                "maintenanceActions": counter_at(&engine_deltas, &["maintenanceActions"]),
                "sourceTicks": counter_at(&engine_deltas, &["mediaTicks"]),
            },
        }),
    );
    artifact.set(
        "outputs",
        json!({
            "count": cfg.outputs,
            "baseline": outputs_before.iter().map(OutputSample::json).collect::<Vec<_>>(),
            "closing": outputs_after.iter().map(OutputSample::json).collect::<Vec<_>>(),
        }),
    );
    artifact.set(
        "receiver",
        json!({
            "summary": receiver_summary,
            "statsLine": receiver_stats_line(receiver_stdout_text),
            "stdoutTail": receiver_stdout_text.lines().rev().take(8).collect::<Vec<_>>(),
            "stderrTail": receiver_stderr_text.lines().rev().take(8).collect::<Vec<_>>(),
            "exit": receiver_exit,
            "teardown": receiver_teardown,
            "elapsedSecs": receiver_row.as_ref().and_then(|row| row.float("elapsed_s")),
            "cpuUserMs": receiver_row.as_ref().and_then(|row| row.float("cpu_user_ms")),
            "cpuSysMs": receiver_row.as_ref().and_then(|row| row.float("cpu_sys_ms")),
            // Peak depth against capacity, plus the product-side burst bound the
            // capacity has to cover: this is the ledger entry that shows whether
            // the receiver was the limiter.
            "datapathQueue": {
                "horizonMs": receiver_row.as_ref().and_then(|row| row.number("datapath_q_horizon_ms")),
                "capacityPerConnection": receiver_row.as_ref().and_then(|row| row.number("datapath_q_cap_per_queue")),
                "connections": receiver_row.as_ref().and_then(|row| row.number("datapath_q_count")),
                "totalCapacity": receiver_row.as_ref().and_then(|row| row.number("datapath_q_total_cap")),
                "peakDepthMax": receiver_row.as_ref().and_then(|row| row.number("datapath_q_peak_depth_max")),
                "full": receiver_row.as_ref().and_then(|row| row.number("datapath_q_full")),
                "dropped": receiver_row.as_ref().and_then(|row| row.number("datapath_q_dropped")),
                "retryHorizonMs": receiver_row.as_ref().and_then(|row| row.number("retry_horizon_ms")),
                "capacityCoversVisitBurst": match (receiver_row.as_ref().and_then(|row| row.number("datapath_q_cap_per_queue")), visit_max_bytes.map(|bytes| bytes / u64::from(HARNESS_SRT_PACKET_SIZE))) {
                    (Some(capacity), Some(burst)) => Some(capacity >= burst),
                    _ => None,
                },
            },
        }),
    );
    artifact.set("checks", Value::Array(checks));
    artifact.set(
        "verdict",
        json!(if failed_required.is_empty() {
            "healthy"
        } else {
            "rejected"
        }),
    );
    artifact.set("verdictReasons", json!(failed_required));
    artifact.set(
        "artifacts",
        json!({
            "receiverTsv": tsv_path.display().to_string(),
            "receiverStdout": receiver_stdout.display().to_string(),
            "receiverStderr": receiver_stderr.display().to_string(),
            "restreamLog": restream_log.display().to_string(),
            "publisherLog": publisher_log.display().to_string(),
            "artifactPath": artifact.path.display().to_string(),
        }),
    );
    if !failed_required.is_empty() {
        println!(
            "[egress-duty] {}",
            json!({"phase": "verdict", "verdict": "rejected", "reasons": failed_required})
        );
    }
    Ok(())
}
