use std::collections::HashMap;

use serde_json::{Value, json};

use super::{
    DEFAULT_TOP_N, MAX_TOP_N, ResourceMapOptions, ResourceMapView, append_hotspots, egress_node,
    execution_for_stage, group_key, group_label, merge_thread_counts, node_score, number_field,
    queue_hotspots, source_ring_node, stage_backend_pid, stage_node, top_nodes,
};
use crate::system_sampling::ChildProcessResourceSnapshot;

#[test]
fn stage_node_reports_child_process_resources_when_pid_is_known() {
    let stage = json!({
        "stageKey": "pipe-1:transcoder:720p",
        "kind": "video:720p:codec:hevc",
        "pipelineId": "pipe-1",
        "lifecycle": {
            "backend": "externalFfmpeg",
            "backendPid": 4242
        },
        "payloadStats": {
            "payloadBytes": 1024
        },
        "metrics": {
            "processingUs": 0
        }
    });
    let child_resources = HashMap::from([(
        4242,
        ChildProcessResourceSnapshot {
            cpu_percent: Some(12.5),
            memory_bytes: Some(64 * 1024 * 1024),
        },
    )]);

    let node = stage_node(&stage, &child_resources);

    assert_eq!(node.get("backendPid").and_then(Value::as_u64), Some(4242));
    assert_eq!(node.get("cpuPercent").and_then(Value::as_f64), Some(12.5));
    assert_eq!(
        node.pointer("/memory/attributedBytes")
            .and_then(Value::as_u64),
        Some(64 * 1024 * 1024)
    );
    assert_eq!(
        node.pointer("/memory/confidence").and_then(Value::as_str),
        Some("measured")
    );
    assert_eq!(
        node.pointer("/memory/source").and_then(Value::as_str),
        Some("child_process_rss")
    );
}

#[test]
fn resource_map_options_new_clamps_top_n_to_valid_range() {
    assert_eq!(
        ResourceMapOptions::new(ResourceMapView::Grouped, None).top_n,
        DEFAULT_TOP_N
    );
    assert_eq!(
        ResourceMapOptions::new(ResourceMapView::Grouped, Some(0)).top_n,
        1,
        "zero must clamp up to the minimum, not produce an empty view"
    );
    assert_eq!(
        ResourceMapOptions::new(ResourceMapView::Grouped, Some(MAX_TOP_N + 1_000)).top_n,
        MAX_TOP_N,
        "oversized requests must clamp down rather than allocate unbounded nodes"
    );
    assert_eq!(
        ResourceMapOptions::new(ResourceMapView::Grouped, Some(50)).top_n,
        50
    );
}

#[test]
fn number_field_defaults_to_zero_for_missing_or_non_integer_values() {
    assert_eq!(number_field(&json!({}), "missing"), 0);
    assert_eq!(number_field(&json!({"n": "not a number"}), "n"), 0);
    assert_eq!(
        number_field(&json!({"n": -1}), "n"),
        0,
        "negative is not a u64"
    );
    assert_eq!(
        number_field(&json!({"n": 1.5}), "n"),
        0,
        "fractional is not a u64"
    );
    assert_eq!(number_field(&json!({"n": u64::MAX}), "n"), u64::MAX);
}

#[test]
fn group_key_falls_back_to_defaults_on_missing_fields() {
    assert_eq!(group_key(&json!({})), "unknown");
    assert_eq!(
        group_key(&json!({"kind": "egress"})),
        "egress:unknown:shared"
    );
    assert_eq!(
        group_key(&json!({"kind": "egress", "execution": "os_thread", "label": "RTMP output"})),
        "egress:RTMP:os_thread"
    );
    assert_eq!(
        group_key(&json!({"kind": "egress", "label": "   "})),
        "egress:unknown:shared",
        "an all-whitespace label has no first word, so it falls back to unknown"
    );
    assert_eq!(group_key(&json!({"kind": "stage"})), "stage:shared");
    assert_eq!(group_key(&json!({"kind": "source_ring"})), "source_ring");
}

#[test]
fn group_label_formats_each_known_prefix_and_falls_back_for_unknown_keys() {
    assert_eq!(group_label("egress:rtmp:tokio_task", 3), "RTMP outputs (3)");
    assert_eq!(group_label("stage:os_thread", 2), "os_thread stages (2)");
    assert_eq!(group_label("source_ring", 1), "Source rings (1)");
    assert_eq!(group_label("runtime_process", 1), "restream");
    assert_eq!(group_label("child_process_group", 1), "External FFmpeg");
    assert_eq!(group_label("unknown", 5), "unknown (5)");
    assert_eq!(
        group_label("", 1),
        " (1)",
        "an empty key still yields a single empty part, not a panic"
    );
}

#[test]
fn merge_thread_counts_accumulates_and_ignores_non_numeric_entries() {
    let mut target = serde_json::Map::new();
    merge_thread_counts(
        &mut target,
        &json!({"threads": {"appOwned": 1, "childProcess": 2}}),
    );
    merge_thread_counts(
        &mut target,
        &json!({"threads": {"appOwned": 3, "bogus": "not a number"}}),
    );
    merge_thread_counts(&mut target, &json!({}));

    assert_eq!(target.get("appOwned").and_then(Value::as_u64), Some(4));
    assert_eq!(target.get("childProcess").and_then(Value::as_u64), Some(2));
    assert_eq!(target.get("bogus"), None);
}

#[test]
fn append_hotspots_deduplicates_and_ignores_non_string_entries() {
    let mut target = vec!["processing".to_string()];
    append_hotspots(
        &mut target,
        &json!({"hotspots": ["processing", "cpu", 42, "cpu"]}),
    );
    assert_eq!(target, vec!["processing", "cpu"]);
}

#[test]
fn queue_hotspots_high_watermark_is_inclusive_at_75_percent() {
    assert_eq!(queue_hotspots(75, 100, 0), vec!["queue_high"]);
    assert_eq!(queue_hotspots(74, 100, 0), Vec::<&str>::new());
    assert_eq!(
        queue_hotspots(u64::MAX, 0, 0),
        Vec::<&str>::new(),
        "zero capacity must not report a hotspot even for a huge queue length"
    );
    assert_eq!(queue_hotspots(0, 100, 1), vec!["backpressure"]);
    assert_eq!(
        queue_hotspots(u64::MAX, u64::MAX, u64::MAX),
        vec!["queue_high", "backpressure"]
    );
}

#[test]
fn execution_for_stage_maps_every_known_backend_and_defaults_to_shared() {
    let backend = |name: &str| json!({"lifecycle": {"backend": name}});
    assert_eq!(
        execution_for_stage(&backend("externalFfmpeg")),
        "child_process"
    );
    assert_eq!(
        execution_for_stage(&backend("ExternalFfmpeg")),
        "child_process"
    );
    assert_eq!(execution_for_stage(&backend("internalFfmpeg")), "os_thread");
    assert_eq!(execution_for_stage(&backend("recording")), "os_thread");
    assert_eq!(execution_for_stage(&backend("Recording")), "os_thread");
    assert_eq!(execution_for_stage(&backend("audioRouter")), "tokio_task");
    assert_eq!(execution_for_stage(&backend("hlsSegmenter")), "tokio_task");
    assert_eq!(execution_for_stage(&backend("somethingElse")), "shared");
    assert_eq!(execution_for_stage(&json!({})), "shared");
}

#[test]
fn stage_backend_pid_rejects_values_that_overflow_u32() {
    assert_eq!(
        stage_backend_pid(&json!({"lifecycle": {"backendPid": 4242}})),
        Some(4242)
    );
    assert_eq!(stage_backend_pid(&json!({})), None);
    assert_eq!(
        stage_backend_pid(&json!({"lifecycle": {"backendPid": u64::MAX}})),
        None,
        "a pid that does not fit in u32 must not silently truncate"
    );
}

#[test]
fn node_score_weighs_cpu_percent_far_above_a_single_byte_of_memory() {
    let cpu_heavy = node_score(&json!({"cpuPercent": 1.0}));
    let memory_heavy = node_score(&json!({"memory": {"attributedBytes": 1024 * 1024 - 1}}));
    assert!(cpu_heavy > memory_heavy);
}

#[test]
fn top_nodes_sorts_descending_by_score_and_truncates() {
    let nodes = vec![
        json!({"id": "low", "cpuPercent": 1.0}),
        json!({"id": "high", "cpuPercent": 90.0}),
        json!({"id": "mid", "cpuPercent": 40.0}),
    ];
    let top = top_nodes(nodes, 2);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0]["id"], "high");
    assert_eq!(top[1]["id"], "mid");
}

#[test]
fn top_nodes_truncate_to_zero_yields_empty_without_panicking() {
    let nodes = vec![json!({"id": "only"})];
    assert!(top_nodes(nodes, 0).is_empty());
}

#[test]
fn egress_node_uses_os_thread_only_for_srt_protocol() {
    let srt = egress_node(&json!({"outputId": "o1", "protocol": "srt"}), &[]);
    assert_eq!(srt["execution"], "os_thread");
    assert_eq!(srt["threads"]["appOwned"], 1);

    let rtmp = egress_node(&json!({"outputId": "o2", "protocol": "rtmp"}), &[]);
    assert_eq!(rtmp["execution"], "tokio_task");
    assert_eq!(rtmp["threads"]["appOwned"], 0);
}

#[test]
fn egress_node_uses_shard_thread_for_fabric_owned_outputs_regardless_of_protocol() {
    let fabric_srt = egress_node(
        &json!({"outputId": "o3", "protocol": "srt", "fabric": true, "shardId": 2}),
        &[],
    );
    assert_eq!(fabric_srt["execution"], "shard_thread");
    // A fabric-owned output shares a fixed shard thread — it must not also
    // be counted as an app-owned OS thread the way legacy SRT is, or the
    // resource map double-counts the same shared thread once per output.
    assert_eq!(fabric_srt["threads"]["appOwned"], 0);
    assert_eq!(fabric_srt["fabric"], true);
    assert_eq!(fabric_srt["shardId"], 2);

    let fabric_rtmp = egress_node(
        &json!({"outputId": "o4", "protocol": "rtmp", "fabric": true, "shardId": 0}),
        &[],
    );
    assert_eq!(fabric_rtmp["execution"], "shard_thread");
    assert_eq!(fabric_rtmp["threads"]["appOwned"], 0);
}

#[test]
fn egress_node_with_no_matching_queue_reports_zeroed_stats_and_no_hotspots() {
    let node = egress_node(
        &json!({"outputId": "missing-queue", "protocol": "rtmp"}),
        &[],
    );
    assert_eq!(node["memory"]["attributedBytes"], 0);
    assert_eq!(node["hotspots"], json!([]));
    assert_eq!(node["queue"], Value::Null);
}

#[test]
fn source_ring_node_reports_retained_payload_hotspot_only_when_bytes_present() {
    let empty = source_ring_node(&json!({"pipelineId": "pipe-1"}));
    assert_eq!(empty["hotspots"], json!([]));

    let retaining = source_ring_node(&json!({
        "pipelineId": "pipe-1",
        "payloadStats": {"payloadBytes": 100}
    }));
    assert_eq!(retaining["hotspots"], json!(["retained_payload"]));
}
