use super::*;
use crate::domain::audio_routing::AudioRouting;

#[test]
fn plan_rejects_unknown_pipeline_and_bad_url() {
    let req = PlanRequest {
        intent: "add output".to_string(),
        pipeline_id: Some("missing".to_string()),
        proposed_changes: vec![ProposedChange {
            kind: "addOutput".to_string(),
            pipeline_id: None,
            output_id: None,
            name: Some("CDN".to_string()),
            url: Some("ftp://example".to_string()),
            monitoring_url: None,
            config: Some(OutputConfig::preset("720p")),
            desired_state: None,
        }],
    };

    let validation = validate_plan(&req, &[], &[]);
    assert!(!validation.valid);
    assert!(
        validation
            .errors
            .iter()
            .any(|issue| issue.code == "pipelineNotFound")
    );
    assert!(
        validation
            .errors
            .iter()
            .any(|issue| issue.code == "unsupportedOutputUrl")
    );
}

#[test]
fn graph_preview_identifies_shared_stage_candidates() {
    let req = PlanRequest {
        intent: "add 720p rtmp output".to_string(),
        pipeline_id: Some("pipe-a".to_string()),
        proposed_changes: vec![ProposedChange {
            kind: "addOutput".to_string(),
            pipeline_id: None,
            output_id: None,
            name: None,
            url: Some("rtmp://example/live/key".to_string()),
            monitoring_url: None,
            config: Some(
                OutputConfig::preset("720p")
                    .with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            ),
            desired_state: None,
        }],
    };

    let preview = graph_preview(&req, None);
    let stages: Vec<_> = preview
        .added_nodes
        .iter()
        .filter_map(|node| node["stageKey"].as_str())
        .collect();
    assert!(stages.contains(&"video:720p:codec:h264"));
    assert!(
        stages
            .iter()
            .any(|stage| stage.starts_with("audio:atrack:0"))
    );
    assert!(!stages.iter().any(|stage| stage.starts_with("hevc_to_h264")));
}

#[test]
fn graph_preview_uses_planner_stage_keys_when_filtering_existing_graph() {
    let req = PlanRequest {
        intent: "add 720p rtmp output".to_string(),
        pipeline_id: Some("pipe-a".to_string()),
        proposed_changes: vec![ProposedChange {
            kind: "addOutput".to_string(),
            pipeline_id: None,
            output_id: Some("out-a".to_string()),
            name: None,
            url: Some("rtmp://example/live/key".to_string()),
            monitoring_url: None,
            config: Some(OutputConfig::preset("720p")),
            desired_state: Some("running".to_string()),
        }],
    };
    let current_graph = serde_json::json!({
        "nodes": [
            {"stageKey": "pipe-a:video:720p:codec:h264"}
        ]
    });

    let preview = graph_preview(&req, Some(&current_graph));
    let stages = preview
        .added_nodes
        .iter()
        .filter_map(|node| node["stageKey"].as_str())
        .collect::<Vec<_>>();

    assert!(!stages.contains(&"video:720p:codec:h264"));
    assert!(!stages.iter().any(|stage| stage.starts_with("hevc_to_h264")));
}

#[test]
fn plan_rejects_unsupported_output_codec_for_legacy_rtmp() {
    let req = PlanRequest {
        intent: "add h265 legacy rtmp output".to_string(),
        pipeline_id: Some("pipe-a".to_string()),
        proposed_changes: vec![ProposedChange {
            kind: "addOutput".to_string(),
            pipeline_id: None,
            output_id: None,
            name: Some("Legacy RTMP".to_string()),
            url: Some("rtmp://example/live/key".to_string()),
            monitoring_url: None,
            config: Some(
                OutputConfig::preset("720p")
                    .with_video_codec(crate::domain::output_spec::OutputVideoCodec::Hevc),
            ),
            desired_state: Some("running".to_string()),
        }],
    };
    let pipelines = [Pipeline {
        id: "pipe-a".to_string(),
        name: "Pipeline A".to_string(),
        stream_key: "stream-key".to_string(),
        input_source: None,
        srt_ingest_policy: None,
    }];

    let validation = validate_plan(&req, &pipelines, &[]);

    assert!(!validation.valid);
    assert!(validation.errors.iter().any(|issue| {
        issue.code == "unsupportedOutputCodec" && issue.field == Some("config.video.codec")
    }));
}
