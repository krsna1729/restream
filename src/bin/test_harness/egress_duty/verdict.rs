//! The `egress-duty` verdict: every acceptance fence as a named check, plus the
//! artifact's closing blocks (control partition, per-output counters, receiver
//! summary and the reconciliation).

use std::collections::BTreeMap;
use std::path::Path;

use super::super::*;

use super::*;

/// Everything the verdict reads, gathered by `run::run_duty_inner` so the
/// checks stay a function of the run's own samples rather than of the order
/// they were taken in.
pub(super) struct VerdictInput<'a> {
    pub(super) cfg: &'a EgressDutyConfig,
    pub(super) artifact: &'a mut Artifact,
    pub(super) receiver_row: Option<ReceiverRow>,
    pub(super) receiver_teardown: Value,
    pub(super) receiver_exit: Value,
    pub(super) receiver_stdout_text: &'a str,
    pub(super) receiver_stderr_text: &'a str,
    pub(super) window_samples: &'a [Value],
    pub(super) outputs_before: &'a [OutputSample],
    pub(super) outputs_after: &'a [OutputSample],
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
        outputs_before,
        outputs_after,
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
        true,
        protocol_loss_ok,
        json!({
            "secA": sec_a,
            "secB": sec_b,
            "rule": "sec_a (wire DATA lost) and sec_b (duplicates) must both be 0 over the whole run",
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

    let data_retx_delta = counter_at(&engine_deltas, &["txClass", "dataRetransmit"]);
    let data_retx_lifecycle = counter_at(&engine_final, &["txClass", "dataRetransmit"]);
    check(
        "zero_data_retransmission",
        true,
        data_retx_delta == Some(0),
        json!({
            "windowDataRetransmit": data_retx_delta,
            "lifecycleDataRetransmit": data_retx_lifecycle,
            "rule": "txClass.dataRetransmit delta over the rated window must be 0",
        }),
    );
    let requested_visit_max_bytes = visit_max_env_value
        .as_deref()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let visit_max_readback_ok = match visit_max_env_value.as_deref() {
        Some(_) => requested_visit_max_bytes == visit_max_bytes,
        None => visit_max_bytes.is_some(),
    };
    check(
        "visit_burst_bound_readback",
        true,
        visit_max_readback_ok,
        json!({
            "envValue": visit_max_env_value,
            "requestedVisitMaxBytes": requested_visit_max_bytes,
            "observedVisitMaxBytes": visit_max_bytes,
            "rule": "the product effective-config event must be read back; when RESTREAM_EGRESS_VISIT_MAX_BYTES is set, the observed value must equal it",
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

    // Informational: the Stage D plan expects one SRT shard thread. Production
    // sizes SRT shards by CPU (`SrtCpuParallel` → clamp(effective_cpus,2,8)),
    // independent of RESTREAM_EGRESS_SHARDS, so a multi-shard process is
    // reported rather than rejected.
    let mut matched_indices: Vec<u32> = shard_threads.iter().map(|thread| thread.index).collect();
    matched_indices.sort_unstable();
    matched_indices.dedup();
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
            "note": "reported, not fenced: SRT shard count is CPU-derived (clamp(effective_cpus,2,8)), RESTREAM_EGRESS_SHARDS only sets the initial pool, and each shard contributes a second same-named thread (see shardThreads.note)",
        }),
    );

    // ── Artifact ────────────────────────────────────────────────────────
    let mut control_partition = serde_json::Map::new();
    for key in TX_CLASS_FIELDS {
        control_partition.insert((*key).to_string(), engine_deltas["txClass"][key].clone());
    }
    let amplification = match (wire_delta, data_first_delta) {
        (Some(wire), Some(first)) if first > 0 => {
            json!({
                "wireDatagramsPerDataFirst": round6(wire as f64 / first as f64),
                "controlPerDataFirst": round6((wire as f64 - first as f64) / first as f64),
            })
        }
        _ => Value::Null,
    };
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
                "txInFlight": counter_at(&engine_after, &["txInFlight"]),
                "serviceVisits": counter_at(&engine_deltas, &["serviceVisits"]),
                "serviceActions": counter_at(&engine_deltas, &["serviceActions"]),
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
