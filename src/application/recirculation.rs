use crate::application::models::Output;
use crate::domain::output_spec::RecirculationTarget;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecirculationTopologyError {
    DirectCycle,
    IndirectCycle,
}

pub fn validate_recirculation_topology(
    source_pipeline_id: &str,
    target: &RecirculationTarget,
    outputs: &[Output],
) -> Result<(), RecirculationTopologyError> {
    if source_pipeline_id == target.pipeline_id() {
        return Err(RecirculationTopologyError::DirectCycle);
    }

    let edges = recirculation_edges(outputs);
    let mut stack = vec![target.pipeline_id().to_string()];
    let mut visited = HashSet::new();
    while let Some(pipeline_id) = stack.pop() {
        if !visited.insert(pipeline_id.clone()) {
            continue;
        }
        let Some(targets) = edges.get(pipeline_id.as_str()) else {
            continue;
        };
        if targets
            .iter()
            .any(|next_pipeline_id| next_pipeline_id == source_pipeline_id)
        {
            return Err(RecirculationTopologyError::IndirectCycle);
        }
        stack.extend(targets.iter().cloned());
    }

    Ok(())
}

fn recirculation_edges(outputs: &[Output]) -> HashMap<&str, Vec<String>> {
    let mut edges = HashMap::<&str, Vec<String>>::new();
    for output in outputs {
        let Ok(target) = RecirculationTarget::parse(&output.url) else {
            continue;
        };
        edges
            .entry(output.pipeline_id.as_str())
            .or_default()
            .push(target.pipeline_id().to_string());
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::state::DesiredOutputState;

    fn output(source_pipeline: &str, id: &str, url: &str) -> Output {
        Output {
            id: id.to_string(),
            pipeline_id: source_pipeline.to_string(),
            name: id.to_string(),
            url: url.to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: OutputConfig::source(),
        }
    }

    #[test]
    fn recirculation_topology_rejects_direct_cycle() {
        let target = RecirculationTarget::parse("pipeline://pipe-a/input-backup").unwrap();

        let result = validate_recirculation_topology("pipe-a", &target, &[]);

        assert_eq!(result, Err(RecirculationTopologyError::DirectCycle));
    }

    #[test]
    fn recirculation_topology_rejects_indirect_cycle() {
        let outputs = vec![
            output("pipe-b", "b-to-c", "pipeline://pipe-c/input-backup"),
            output("pipe-c", "c-to-a", "recirculate://pipe-a/input-backup"),
        ];
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();

        let result = validate_recirculation_topology("pipe-a", &target, &outputs);

        assert_eq!(result, Err(RecirculationTopologyError::IndirectCycle));
    }

    #[test]
    fn recirculation_topology_accepts_acyclic_chain() {
        let outputs = vec![output("pipe-b", "b-to-c", "pipeline://pipe-c/input-backup")];
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();

        let result = validate_recirculation_topology("pipe-a", &target, &outputs);

        assert_eq!(result, Ok(()));
    }
}
