//! Tests for the WI3.4 packet-rate contract sampler.

use super::packet_contract::*;
use super::packet_contract_peers::peer_state_from_json;

/// `begin`/`finish` drive one process-global sampler, so the tests that use it
/// must not run concurrently with each other. Async-aware because the guard is
/// held across the `record` awaits.
pub(super) static SAMPLER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use crate::resource_sweep::ResourceScenarioMeta;
use crate::resource_sweep::packet_contract_verdict::sample_validity;
use serde_json::{Value, json};

/// A complete `/metrics/system` shard projection: every counter the verdict
/// reads is present, so a fixture that means "nothing is wrong" really is
/// healthy rather than merely under-specified.
pub(super) fn system_with_owners(owners: Value) -> Value {
    json!({
        "egressShards": [{
            "protocol": "srt",
            "state": "healthy",
            "loopIterations": 100,
            "mediaTicks": 7,
            "readyVisits": 40,
            "retryEvents": 0,
            "resyncCount": 0,
            "driverBudgetViolations": 0,
            "queueOverflows": 0,
            "budgetExhaustions": 0,
            "srtOwners": owners,
        }]
    })
}

#[test]
fn rates_are_null_on_the_first_sample_after_a_reset_and_for_missing_readings() {
    assert_eq!(rate_per_sec(Some(1_000), None, 1.0), None);
    assert_eq!(rate_per_sec(Some(1_000), Some(500), 1.0), Some(500.0));
    assert_eq!(rate_per_sec(Some(1_000), Some(2_000), 1.0), None, "reset");
    assert_eq!(
        rate_per_sec(Some(1_000), Some(500), 0.0),
        None,
        "no interval"
    );
    assert_eq!(
        rate_per_sec(None, Some(500), 1.0),
        None,
        "source absent now"
    );
    assert_eq!(
        rate_per_sec(Some(1_000), None, 1.0),
        None,
        "source absent before"
    );
}

#[test]
fn counters_split_data_and_control_for_srt_shards_only() {
    let system = json!({
        "egressShards": [
            {
                "protocol": "srt",
                "loopIterations": 100,
                "mediaTicks": 7,
                "readyVisits": 40,
                "retryEvents": 2,
                "srtOwners": [
                    {"present": true, "txPackets": 1000, "txCompletedOk": 900, "rxPackets": 30,
                     "serviceVisits": 55, "serviceActions": 900, "maintenanceActions": 4,
                     "txClass": {"dataFirst": 850, "dataRetransmit": 100, "ack": 50}},
                    {"present": false}
                ]
            },
            {
                "protocol": "srt",
                "loopIterations": 50,
                "mediaTicks": 3,
                "readyVisits": 20,
                "retryEvents": 0,
                "srtOwners": [
                    {"present": true, "txPackets": 500, "txCompletedOk": 400, "rxPackets": 10,
                     "serviceVisits": 25, "serviceActions": 400, "maintenanceActions": 1,
                     "txClass": {"dataFirst": 500, "dataRetransmit": 0}}
                ]
            },
            {
                // A non-SRT shard's loops must not dilute the contract.
                "protocol": "rtmp",
                "loopIterations": 999,
                "mediaTicks": 999,
                "readyVisits": 999,
                "srtOwners": [{"present": false, "txPackets": 999}]
            }
        ]
    });
    let counters = PacketCounters::read(&system);
    assert_eq!(counters.srt_tx_datagrams, Some(1500));
    assert_eq!(counters.srt_tx_completed, Some(1300));
    assert_eq!(counters.srt_rx_datagrams, Some(40));
    assert_eq!(
        counters.srt_data_first,
        Some(1350),
        "first transmissions only"
    );
    assert_eq!(counters.srt_data_retx, Some(100));
    assert_eq!(counters.srt_control, Some(50), "txPackets - DATA");
    assert_eq!(counters.srt_service_visits, Some(80));
    assert_eq!(counters.srt_service_actions, Some(1300));
    assert_eq!(counters.srt_maintenance_actions, Some(5));
    assert_eq!(counters.shard_loop_iterations, Some(150));
    assert_eq!(counters.shard_media_ticks, Some(10));
    assert_eq!(counters.shard_ready_visits, Some(60));
    assert_eq!(counters.shard_retries, Some(2));
    assert_eq!(srt_shards(&system).len(), 2);
}

/// An absent source must read `None`, never zero: the whole point of the
/// contract's `unavailable` handling is that "no sensor" and "sensor says
/// zero" are different facts.
#[test]
fn absent_sources_read_none_instead_of_zero() {
    let counters = PacketCounters::read(&json!({"egressShards": []}));
    assert_eq!(counters.srt_tx_datagrams, None, "no SRT shard at all");
    assert_eq!(counters.shard_loop_iterations, None);
    assert_eq!(counters.shard_retries, None);

    // A shard without the Owner class breakdown cannot produce the split.
    let counters = PacketCounters::read(&system_with_owners(json!([
        {"present": true, "txPackets": 10}
    ])));
    assert_eq!(counters.srt_tx_datagrams, Some(10));
    assert_eq!(counters.srt_data_first, None);
    assert_eq!(counters.srt_control, None);
    assert_eq!(counters.shard_loop_iterations, Some(100));

    // Class present on one owner but not the other: the total stays
    // unobservable rather than silently under-reporting.
    let counters = PacketCounters::read(&system_with_owners(json!([
        {"present": true, "txPackets": 10, "txClass": {"dataFirst": 8}},
        {"present": true, "txPackets": 5}
    ])));
    assert_eq!(counters.srt_tx_datagrams, Some(15));
    assert_eq!(counters.srt_data_first, None);
}

#[test]
fn udp_snmp_columns_are_none_when_the_kernel_does_not_publish_them() {
    let text = "Udp: InDatagrams NoPorts InErrors RcvbufErrors\nUdp: 10 0 3 7\n";
    assert_eq!(
        parse_udp_snmp(text),
        Some((Some(3), Some(7), None)),
        "SndbufErrors is absent from this row"
    );
    assert_eq!(parse_udp_snmp("Tcp: InSegs\nTcp: 1\n"), None, "no Udp row");
    assert_eq!(
        parse_udp_snmp("Udp: InErrors\nUdp: 1\n"),
        Some((Some(1), None, None))
    );
}

/// A rated sample with nothing wrong: healthy, no reasons.
pub(super) fn clean_sample() -> Value {
    json!({
        "srtTxDatagramsPerSec": 100_000.0,
        "srtDataRetransmitPps": 0.0,
        "ratedSecs": 12.0,
        "egressShardCount": 6,
        "srtOwnerCount": 6,
        "shardUnhealthyCount": 0,
        "capacityActiveLeaves": 100,
        "shardRetriesPerSec": 0.0,
        "ownerFaulted": false,
        "ownerTxFailedSends": 0,
        "ownerTxExhaustions": 0,
        "udpInErrorsPerSec": 0.0,
        "udpRcvbufErrorsPerSec": 0.0,
        "udpSndbufErrorsPerSec": 0.0,
        "nicRxDroppedPerSec": 0.0,
        "nicTxDroppedPerSec": 0.0,
        "ownerTxFailedSendsDelta": 0,
        "ownerTxExhaustionsDelta": 0,
        "ownerServiceBudgetExhaustedDelta": 0,
        "ownerRxRingDroppedDelta": 0,
        "ownerRxTruncatedDelta": 0,
        "shardFeedResyncsDelta": 0,
        "shardDriverBudgetViolationsDelta": 0,
        "shardQueueOverflowsDelta": 0,
        "srtDataFirstPps": 76_000.0,
    })
}

pub(super) fn sample_with(patch: Value) -> Value {
    let mut sample = clean_sample();
    if let (Value::Object(base), Value::Object(patch)) = (&mut sample, patch) {
        for (key, value) in patch {
            base.insert(key, value);
        }
    }
    sample
}

#[test]
fn validity_separates_healthy_contaminated_and_invalid_rungs() {
    let (status, reasons) = sample_validity(&clean_sample(), 100).unwrap();
    assert_eq!(status, "healthy");
    assert!(reasons.is_empty(), "{reasons:?}");

    // Host/peer drops: measured, but not a lossless path.
    let (status, reasons) =
        sample_validity(&sample_with(json!({"udpRcvbufErrorsPerSec": 1_988.0})), 100).unwrap();
    assert_eq!(status, "contaminated");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("udpRcvbufErrorsPerSec"))
    );

    // A missing sensor is a reason too, not a silent pass.
    let (status, reasons) = sample_validity(
        &sample_with(json!({"udpRcvbufErrorsPerSec": Value::Null})),
        100,
    )
    .unwrap();
    assert_eq!(status, "contaminated");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("not observable"))
    );

    // Missing participants, retries and faults invalidate the rung.
    for (patch, needle) in [
        (
            json!({"capacityActiveLeaves": 99}),
            "99 of 100 expected outputs",
        ),
        (
            json!({"egressShardCount": 0, "srtOwnerCount": 0}),
            "no live SRT shard",
        ),
        (json!({"shardUnhealthyCount": 1}), "non-healthy state"),
        (json!({"shardRetriesPerSec": 3.0}), "output retries"),
        (json!({"ownerFaulted": true}), "owner faulted"),
        (
            json!({"ownerTxFailedSendsDelta": 2}),
            "ownerTxFailedSendsDelta=2 in the rated window",
        ),
        (
            json!({"shardFeedResyncsDelta": 1}),
            "shardFeedResyncsDelta=1 in the rated window",
        ),
    ] {
        let (status, reasons) = sample_validity(&sample_with(patch), 100).unwrap();
        assert_eq!(status, "invalid", "{reasons:?}");
        assert!(
            reasons.iter().any(|reason| reason.contains(needle)),
            "expected {needle} in {reasons:?}"
        );
    }

    // The first sample has no interval and therefore no verdict.
    assert!(
        sample_validity(
            &sample_with(json!({"srtTxDatagramsPerSec": Value::Null})),
            100
        )
        .is_none()
    );
}

/// One artifact must describe one rung per `(scenario, output count)`:
/// walking several rungs in one run may not average them together.
#[tokio::test]
async fn summary_groups_samples_by_rung() {
    let _guard = SAMPLER_TEST_LOCK.lock().await;
    let work_dir =
        std::env::temp_dir().join(format!("packet-contract-rungs-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    begin(
        &work_dir,
        RunMetadata {
            git_sha: Some("deadbeef".to_string()),
            git_dirty: Some(false),
            restream_binary: "target/bench/restream".to_string(),
            restream_bin_explicit: false,
            bitrate_label: "8M".to_string(),
            peer_mode: "sink".to_string(),
            peer_targets: vec!["127.0.0.1".to_string()],
            peer_state: None,
            build: None,
            lifecycle: "isolated".to_string(),
            sample_secs: 10,
            settle_secs: 10,
            sample_interval_ms: 1000,
            egress_counts: vec![100, 300],
            scenario_filter: vec!["egress-growth-source-srt".to_string()],
        },
    );
    let meta = |outputs: usize| ResourceScenarioMeta {
        scenario: "egress-growth-source-srt",
        label: format!("{outputs}-per-group"),
        pipelines: 1,
        outputs,
        ingest_types: "h264-srt".to_string(),
        egress_mix: "srt-source".to_string(),
        transcode: "no",
    };
    let mut system = system_with_owners(json!([{
        "present": true, "txPackets": 1_000, "txCompletedOk": 1_000, "rxPackets": 10,
        "serviceVisits": 10, "serviceActions": 100, "maintenanceActions": 0,
        "txClass": {"dataFirst": 900, "dataRetransmit": 100},
    }]));
    system["capacity"] = json!({"activeLeaves": 100});
    // The runner primes, then rates every sample against the previous reading.
    let t0 = std::time::Instant::now();
    prime_at(&system, &meta(100), t0).await.unwrap();
    record_at(
        &system,
        1.0,
        100.0,
        &meta(100),
        t0 + std::time::Duration::from_secs(1),
    )
    .await
    .unwrap();
    system["egressShards"][0]["srtOwners"][0]["txPackets"] = json!(3_000);
    system["egressShards"][0]["srtOwners"][0]["txClass"]["dataFirst"] = json!(2_700);
    record_at(
        &system,
        1.0,
        200.0,
        &meta(100),
        t0 + std::time::Duration::from_secs(2),
    )
    .await
    .unwrap();

    let summary_path = finish(&work_dir).unwrap().expect("summary written");
    let summary: Value = serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();

    assert_eq!(summary["run"]["gitSha"], "deadbeef");
    assert_eq!(summary["run"]["peerMode"], "sink");
    assert_eq!(summary["rungs"].as_array().unwrap().len(), 1);
    let rung = &summary["rungs"][0];
    assert_eq!(rung["outputs"], 100);
    assert_eq!(rung["samples"], 2);
    assert_eq!(
        rung["workload"]["ingestTypes"], "h264-srt",
        "workload dimensions come from the contract records, not from thin air"
    );
    assert_eq!(rung["workload"]["egressMix"], "srt-source");
    assert_eq!(rung["workload"]["transcode"], "no");
    assert_eq!(
        rung["ratedSamples"], 2,
        "both samples are rated: the prime is the interval origin"
    );
    assert_eq!(
        rung["validity"]["status"], "invalid",
        "6 owners expected, 1 present"
    );
    assert_eq!(
        rung["ratesPerSec"]["srtDataFirstPps"]["mean"], 900.0,
        "each sample is rated over its own one-second interval"
    );
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// The first sample of every rung must be unrated: counters are only
/// comparable inside one rung, so a `(scenario, outputs)` change clears the
/// history instead of differencing the new rung against the old one.
#[tokio::test]
async fn a_rung_change_resets_counter_history() {
    let _guard = SAMPLER_TEST_LOCK.lock().await;
    let work_dir =
        std::env::temp_dir().join(format!("packet-contract-rung-reset-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    begin(
        &work_dir,
        RunMetadata {
            git_sha: None,
            git_dirty: None,
            restream_binary: "restream".to_string(),
            restream_bin_explicit: false,
            bitrate_label: "8M".to_string(),
            peer_mode: "sink".to_string(),
            peer_targets: vec!["127.0.0.1".to_string()],
            peer_state: None,
            build: None,
            lifecycle: "isolated".to_string(),
            sample_secs: 10,
            settle_secs: 10,
            sample_interval_ms: 1000,
            egress_counts: vec![100, 300],
            scenario_filter: vec!["egress-growth-source-srt".to_string()],
        },
    );
    let meta = |outputs: usize| ResourceScenarioMeta {
        scenario: "egress-growth-source-srt",
        label: format!("{outputs}-per-group"),
        pipelines: 1,
        outputs,
        ingest_types: "h264-srt".to_string(),
        egress_mix: "srt-source".to_string(),
        transcode: "no",
    };
    let sample_system = |packets: u64, leaves: u64| {
        let mut system = system_with_owners(json!([{
            "present": true, "txPackets": packets, "txCompletedOk": packets, "rxPackets": 10,
            "serviceVisits": 10, "serviceActions": 100, "maintenanceActions": 0,
            "txClass": {"dataFirst": packets, "dataRetransmit": 0},
        }]));
        system["capacity"] = json!({"activeLeaves": leaves});
        system
    };

    let t0 = std::time::Instant::now();
    let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    // Rung 1: primed, then two rated samples of 2000 DATA/s each.
    prime_at(&sample_system(1_000, 100), &meta(100), at(0))
        .await
        .unwrap();
    record_at(&sample_system(3_000, 100), 1.0, 100.0, &meta(100), at(1))
        .await
        .unwrap();
    record_at(&sample_system(5_000, 100), 1.0, 100.0, &meta(100), at(2))
        .await
        .unwrap();
    // Rung 2 (a different output count) is not primed here, so the boundary
    // itself must clear the history: its first sample is a baseline and the
    // second rates at 4000/s from rung 2's own counters. A cross-rung delta
    // would read 6000/s, so the value distinguishes the two behaviours.
    record_at(&sample_system(7_000, 300), 1.0, 300.0, &meta(300), at(3))
        .await
        .unwrap();
    record_at(&sample_system(11_000, 300), 1.0, 300.0, &meta(300), at(4))
        .await
        .unwrap();

    let summary_path = finish(&work_dir).unwrap().expect("summary written");
    let summary: Value = serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    let rungs = summary["rungs"].as_array().unwrap();
    assert_eq!(rungs.len(), 2);
    for rung in rungs {
        assert_eq!(rung["samples"], 2);
    }
    let rung_100 = rungs.iter().find(|rung| rung["outputs"] == 100).unwrap();
    assert_eq!(
        rung_100["ratedSamples"], 2,
        "a primed rung rates every sample, including the first"
    );
    assert_eq!(rung_100["ratesPerSec"]["srtDataFirstPps"]["mean"], 2000.0);

    // Rung 2 was not primed here, so the rung boundary itself must clear the
    // history: its first sample is a baseline, and the second is rated from
    // rung 2's own counters rather than from rung 1's last sample.
    let rung_300 = rungs.iter().find(|rung| rung["outputs"] == 300).unwrap();
    assert_eq!(
        rung_300["ratedSamples"], 1,
        "an unprimed rung boundary leaves the first sample unrated"
    );
    assert_eq!(
        rung_300["ratesPerSec"]["srtDataFirstPps"]["mean"], 4000.0,
        "measured from rung 2's own baseline, not from rung 1's last sample"
    );
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// An isolated rung must run exactly the outputs it declares: extra live
/// leaves are a workload mismatch too.
#[test]
fn validity_requires_the_exact_output_count() {
    let (status, reasons) =
        sample_validity(&sample_with(json!({"capacityActiveLeaves": 101})), 100).unwrap();
    assert_eq!(status, "invalid", "{reasons:?}");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("101 of 100 expected outputs")),
        "{reasons:?}"
    );
    assert_eq!(
        sample_validity(&clean_sample(), 100).unwrap().0,
        "healthy",
        "an exact match still passes"
    );
}

/// Remote rungs depend on the peer hosts' own drop counters: `healthy`
/// requires same-window telemetry from every configured peer, and a sink
/// that restarted mid-rung invalidates the window.
#[test]
fn remote_peers_gate_the_verdict() {
    let peer = |run_id: &str, run_changed: bool, drops: f64, errors: Value| {
        json!({
            "host": "peer-a",
            "expectedOutputs": 100,
            // 100 outputs at the 8 Mbps workload = 100 MB/s of delivered payload.
            "payloadBytesPerSec": 100_000_000.0,
            "acceptedPerSec": 0.0,
            "closedPerSec": 0.0,
            "runId": run_id,
            "runIdChanged": run_changed,
            "udpRcvbufErrorsPerSec": drops,
            "udpSndbufErrorsPerSec": 0.0,
            "udpInErrorsPerSec": 0.0,
            "nicRxDroppedPerSec": 0.0,
            "nicTxDroppedPerSec": 0.0,
            "error": errors,
        })
    };
    let remote = |peers: Value, expected: u64| {
        sample_with(json!({"expectedPeers": expected, "peers": peers}))
    };

    let (status, reasons) = sample_validity(
        &remote(json!([peer("run-1", false, 0.0, Value::Null)]), 1),
        100,
    )
    .unwrap();
    assert_eq!(status, "healthy", "{reasons:?}");

    // A peer that dropped datagrams in this window.
    let (status, reasons) = sample_validity(
        &remote(json!([peer("run-1", false, 12.5, Value::Null)]), 1),
        100,
    )
    .unwrap();
    assert_eq!(status, "contaminated");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("peer peer-a UDP receive-buffer drops")),
        "{reasons:?}"
    );

    // Missing telemetry from a configured peer is never healthy.
    let (status, reasons) = sample_validity(
        &remote(json!([{"host": "peer-a", "error": "connect: refused"}]), 1),
        100,
    )
    .unwrap();
    assert_ne!(status, "healthy");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("telemetry unavailable")),
        "{reasons:?}"
    );

    // Fewer readings than configured peers is also a hole.
    let (status, reasons) = sample_validity(
        &remote(json!([peer("run-1", false, 0.0, Value::Null)]), 2),
        100,
    )
    .unwrap();
    assert_ne!(status, "healthy");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("expected 2 peer state reading")),
        "{reasons:?}"
    );

    // A sink restart breaks the window even with zero drops.
    let (status, reasons) = sample_validity(
        &remote(json!([peer("run-2", true, 0.0, Value::Null)]), 1),
        100,
    )
    .unwrap();
    assert_eq!(status, "invalid");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("restarted mid-rung")),
        "{reasons:?}"
    );

    // A local run has no peer expectations and is unaffected.
    assert_eq!(sample_validity(&clean_sample(), 100).unwrap().0, "healthy");
}

#[test]
fn peer_state_parsing_rejects_incomplete_telemetry() {
    let state = peer_state_from_json(
        "peer-a",
        &json!({
            "runId": "run-1", "accepted": 100, "closed": 1, "discardedBytes": 5_000,
            "udpInErrors": 3, "udpRcvbufErrors": 7,
        }),
    )
    .unwrap();
    assert_eq!(state.run_id, "run-1");
    assert_eq!(state.accepted, 100);
    assert_eq!(state.discarded_bytes, 5_000);
    assert_eq!(state.udp_rcvbuf_errors, Some(7));

    // A peer that cannot read its own /proc reports null, which the
    // verdict treats as a missing sensor rather than zero drops.
    let state = peer_state_from_json(
        "peer-a",
        &json!({
            "runId": "run-1", "accepted": 0, "closed": 0, "discardedBytes": 0,
            "udpInErrors": Value::Null, "udpRcvbufErrors": Value::Null,
        }),
    )
    .unwrap();
    assert_eq!(state.udp_in_errors, None);

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

/// The roadmap target is a healthy NO-LOSS path, so any retransmission in
/// the rated window contaminates the rung and an unobservable retransmit
/// counter is a missing sensor. No percentage threshold is invented.
#[test]
fn retransmissions_contaminate_a_no_loss_rung() {
    assert_eq!(
        sample_validity(&sample_with(json!({"srtDataRetransmitPps": 0.0})), 100)
            .unwrap()
            .0,
        "healthy"
    );
    let (status, reasons) =
        sample_validity(&sample_with(json!({"srtDataRetransmitPps": 12.0})), 100).unwrap();
    assert_eq!(status, "contaminated");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("SRT retransmissions in the rated window")),
        "{reasons:?}"
    );
    let (status, reasons) = sample_validity(
        &sample_with(json!({"srtDataRetransmitPps": Value::Null})),
        100,
    )
    .unwrap();
    assert_ne!(status, "healthy");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("srtDataRetransmitPps is not observable")),
        "{reasons:?}"
    );
}

/// The rated window is measured evidence, not configuration: a rung whose
/// counter window is shorter than the contract minimum is not a baseline.
#[tokio::test]
async fn a_short_rated_window_is_not_a_baseline() {
    let _guard = SAMPLER_TEST_LOCK.lock().await;
    let work_dir = std::env::temp_dir().join(format!(
        "packet-contract-short-window-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work_dir).unwrap();
    begin(
        &work_dir,
        RunMetadata {
            git_sha: Some("deadbeef".to_string()),
            git_dirty: Some(false),
            restream_binary: "restream".to_string(),
            restream_bin_explicit: false,
            bitrate_label: "8M".to_string(),
            peer_mode: "sink".to_string(),
            peer_targets: vec!["127.0.0.1".to_string()],
            peer_state: None,
            build: None,
            lifecycle: "isolated".to_string(),
            sample_secs: 10,
            settle_secs: 10,
            sample_interval_ms: 1000,
            egress_counts: vec![100],
            scenario_filter: vec!["egress-growth-source-srt".to_string()],
        },
    );
    let meta = ResourceScenarioMeta {
        scenario: "egress-growth-source-srt",
        label: "100-per-group".to_string(),
        pipelines: 1,
        outputs: 100,
        ingest_types: "h264-srt".to_string(),
        egress_mix: "srt-source".to_string(),
        transcode: "no",
    };
    let system = {
        let mut system = system_with_owners(json!([{
            "present": true, "txPackets": 1_000, "txCompletedOk": 1_000, "rxPackets": 10,
            "serviceVisits": 10, "serviceActions": 100, "maintenanceActions": 0,
            "txFailedSends": 0, "txExhaustions": 0, "serviceBudgetExhausted": 0,
            "txClass": {"dataFirst": 1_000, "dataRetransmit": 0},
        }]));
        system["capacity"] = json!({"activeLeaves": 100});
        system
    };
    // A 3 second rated window: primed, then rated three times.
    let t0 = std::time::Instant::now();
    prime_at(&system, &meta, t0).await.unwrap();
    for secs in 1..=3 {
        record_at(
            &system,
            1.0,
            100.0,
            &meta,
            t0 + std::time::Duration::from_secs(secs),
        )
        .await
        .unwrap();
    }
    let summary_path = finish(&work_dir).unwrap().expect("summary written");
    let summary: Value = serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    let rung = &summary["rungs"][0];
    assert_eq!(rung["commonRatedWindowSecs"], 3.0);
    assert_eq!(
        rung["validity"]["status"], "contaminated",
        "validity: {}",
        rung["validity"]
    );
    assert!(
        rung["validity"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("shorter than the 10s")),
        "{}",
        rung["validity"]
    );
    let _ = std::fs::remove_dir_all(&work_dir);
}
