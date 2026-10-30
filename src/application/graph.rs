//! Application-owned desired graph planning for API and diagnostics read models.

use crate::application::models::Output;
use restream_domain::domain::output_spec::OutputUrlScheme;
use restream_domain::planner::PlannedOutput;
use restream_domain::runtime::graph::StageGraphPlan;

pub struct DesiredPipelineGraphs {
    pub aggregate: StageGraphPlan,
    pub outputs: Vec<StageGraphPlan>,
}

pub fn desired_pipeline_graphs(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    outputs: &[Output],
) -> DesiredPipelineGraphs {
    let planned_outputs = outputs.iter().map(planned_output).collect::<Vec<_>>();
    let aggregate = restream_domain::planner::plan_pipeline_graph(
        pipeline_id,
        ingest_codec,
        &planned_outputs,
        false,
    );
    let outputs = outputs
        .iter()
        .map(|output| {
            let planned = planned_output(output);
            if OutputUrlScheme::from_url(&output.url).is_hls_family() {
                restream_domain::planner::plan_hls_output_graph(pipeline_id, ingest_codec, &planned)
            } else {
                restream_domain::planner::plan_pipeline_graph(
                    pipeline_id,
                    ingest_codec,
                    std::slice::from_ref(&planned),
                    false,
                )
            }
        })
        .collect();

    DesiredPipelineGraphs { aggregate, outputs }
}

fn planned_output(output: &Output) -> PlannedOutput {
    PlannedOutput::new(
        output.id.as_str(),
        output.config.clone(),
        output.url.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_domain::domain::ids::OutputId;
    use restream_domain::domain::state::DesiredOutputState;
    use restream_domain::runtime::graph::GraphRole;

    fn test_output(id: &str, url: &str) -> Output {
        Output {
            id: id.to_string(),
            pipeline_id: "pipe-1".to_string(),
            name: id.to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            retries_exhausted: false,
            config: restream_domain::domain::output_spec::OutputConfig::source(),
        }
    }

    #[test]
    fn desired_pipeline_graphs_preserves_hls_output_roles_per_output() {
        let outputs = vec![
            test_output("rtmp-out", "rtmp://example.test/live/key"),
            test_output("hls-out", "https://upload.example.test/live/out.m3u8"),
        ];

        let graphs = desired_pipeline_graphs("pipe-1", Some("h264"), &outputs);

        assert_eq!(graphs.outputs.len(), 2);
        assert!(graphs.outputs.iter().any(|graph| {
            graph.role
                == GraphRole::HlsOutput {
                    output_id: OutputId::new("hls-out"),
                }
        }));
        assert!(graphs.outputs.iter().any(|graph| {
            graph.role
                == GraphRole::Output {
                    output_id: OutputId::new("rtmp-out"),
                }
        }));
    }
}
