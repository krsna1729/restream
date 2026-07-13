//! Application-owned desired graph planning for API and diagnostics read models.

use crate::application::models::Output;
use crate::domain::output_spec::OutputUrlScheme;
use crate::planner::backend_policy::BackendPolicy;
use crate::planner::graph_plan::PlannedOutput;
use crate::runtime::graph::StageGraphPlan;

pub struct DesiredPipelineGraphs {
    pub aggregate: StageGraphPlan,
    pub outputs: Vec<StageGraphPlan>,
}

pub fn desired_pipeline_graphs(
    pipeline_id: &str,
    ingest_codec: Option<&str>,
    outputs: &[Output],
    policy: &BackendPolicy,
) -> DesiredPipelineGraphs {
    let planned_outputs = outputs.iter().map(planned_output).collect::<Vec<_>>();
    let aggregate = crate::planner::graph_plan::plan_pipeline_graph(
        pipeline_id,
        ingest_codec,
        &planned_outputs,
        false,
        policy,
    );
    let outputs = outputs
        .iter()
        .map(|output| {
            let planned = planned_output(output);
            if OutputUrlScheme::from_url(&output.url).is_hls_family() {
                crate::planner::graph_plan::plan_hls_output_graph(
                    pipeline_id,
                    ingest_codec,
                    &planned,
                    policy,
                )
            } else {
                crate::planner::graph_plan::plan_pipeline_graph(
                    pipeline_id,
                    ingest_codec,
                    std::slice::from_ref(&planned),
                    false,
                    policy,
                )
            }
        })
        .collect();

    DesiredPipelineGraphs { aggregate, outputs }
}

fn planned_output(output: &Output) -> PlannedOutput {
    PlannedOutput::new(
        output.id.as_str(),
        output.encoding_string(),
        output.url.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::OutputId;
    use crate::domain::state::DesiredOutputState;
    use crate::planner::backend_policy::BackendPolicy;
    use crate::runtime::graph::GraphRole;

    fn test_output(id: &str, url: &str) -> Output {
        Output {
            id: id.to_string(),
            pipeline_id: "pipe-1".to_string(),
            name: id.to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: crate::domain::output_spec::OutputConfig::parse("source"),
        }
    }

    #[test]
    fn desired_pipeline_graphs_preserves_hls_output_roles_per_output() {
        let policy = BackendPolicy::default();
        let outputs = vec![
            test_output("rtmp-out", "rtmp://example.test/live/key"),
            test_output("hls-out", "https://upload.example.test/live/out.m3u8"),
        ];

        let graphs = desired_pipeline_graphs("pipe-1", Some("h264"), &outputs, &policy);

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
