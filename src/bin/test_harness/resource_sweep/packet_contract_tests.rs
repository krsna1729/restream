//! Tests for the WI3.4 packet-rate contract sampler.

use super::packet_contract::*;
use crate::resource_sweep::ResourceScenarioMeta;
use crate::resource_sweep::packet_contract_verdict::sample_validity;
use serde_json::{Value, json};

fn system_with_owners(owners: Value) -> Value {
    json!({
        "egressShards": [{
            "protocol": "srt",
            "state": "healthy",
            "loopIterations": 100,
            "mediaTicks": 7,
            "readyVisits": 40,
            "retryEvents": 0,
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
fn clean_sample() -> Value {
    json!({
        "srtTxDatagramsPerSec": 100_000.0,
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
        "shardFeedResyncs": 0,
        "shardDriverBudgetViolations": 0,
        "shardQueueOverflows": 0,
        "ownerServiceBudgetExhausted": 0,
    })
}

fn sample_with(patch: Value) -> Value {
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
        (json!({"ownerTxFailedSends": 2}), "ownerTxFailedSends=2"),
        (json!({"shardFeedResyncs": 1}), "feed resync"),
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
#[test]
fn summary_groups_samples_by_rung() {
    let work_dir =
        std::env::temp_dir().join(format!("packet-contract-rungs-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).unwrap();
    begin(
        &work_dir,
        RunMetadata {
            git_sha: Some("deadbeef".to_string()),
            git_dirty: Some(false),
            restream_binary: "target/bench/restream".to_string(),
            bitrate_label: "8M".to_string(),
            peer_mode: "sink".to_string(),
            peer_targets: vec!["127.0.0.1".to_string()],
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
    // First sample is the baseline; the second is rated.
    record(&system, 1.0, 100.0, &meta(100)).unwrap();
    system["egressShards"][0]["srtOwners"][0]["txPackets"] = json!(3_000);
    system["egressShards"][0]["srtOwners"][0]["txClass"]["dataFirst"] = json!(2_700);
    record(&system, 1.0, 200.0, &meta(100)).unwrap();

    let summary_path = finish(&work_dir).unwrap().expect("summary written");
    let summary: Value = serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();

    assert_eq!(summary["run"]["gitSha"], "deadbeef");
    assert_eq!(summary["run"]["peerMode"], "sink");
    assert_eq!(summary["rungs"].as_array().unwrap().len(), 1);
    let rung = &summary["rungs"][0];
    assert_eq!(rung["outputs"], 100);
    assert_eq!(rung["samples"], 2);
    assert_eq!(rung["ratedSamples"], 1, "the first sample carries no rates");
    assert_eq!(
        rung["validity"]["status"], "invalid",
        "6 owners expected, 1 present"
    );
    assert_eq!(
        rung["ratesPerSec"]["srtDataFirstPps"]["mean"], 1800.0,
        "only the rated sample contributes"
    );
    let _ = std::fs::remove_dir_all(&work_dir);
}
