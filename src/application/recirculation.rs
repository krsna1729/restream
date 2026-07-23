use crate::application::models::Output;
use crate::domain::output_spec::RecirculationTarget;
use crate::domain::pipeline_input::PipelineInput;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecirculationTopologyError {
    DirectCycle,
    IndirectCycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecirculationTargetInputError {
    Missing,
    WrongPipeline,
    Disabled,
    Selected,
}

pub fn validate_recirculation_target_input(
    target: &RecirculationTarget,
    input: Option<&PipelineInput>,
) -> Result<(), RecirculationTargetInputError> {
    let Some(input) = input else {
        return Err(RecirculationTargetInputError::Missing);
    };
    if input.pipeline_id != target.pipeline_id() {
        return Err(RecirculationTargetInputError::WrongPipeline);
    }
    if !input.enabled {
        return Err(RecirculationTargetInputError::Disabled);
    }
    if input.selected {
        return Err(RecirculationTargetInputError::Selected);
    }
    Ok(())
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
    use crate::domain::pipeline_input::{PipelineInput, PipelineInputRole};
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

    fn input(pipeline_id: &str, input_id: &str) -> PipelineInput {
        PipelineInput {
            id: input_id.to_string(),
            pipeline_id: pipeline_id.to_string(),
            label: input_id.to_string(),
            stream_key: format!("sk_{input_id}"),
            role: PipelineInputRole::Backup,
            enabled: true,
            selected: false,
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

    #[test]
    fn recirculation_target_input_rejects_missing_input() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();

        let result = validate_recirculation_target_input(&target, None);

        assert_eq!(result, Err(RecirculationTargetInputError::Missing));
    }

    #[test]
    fn recirculation_target_input_rejects_cross_pipeline_input() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let input = input("pipe-c", "input-backup");

        let result = validate_recirculation_target_input(&target, Some(&input));

        assert_eq!(result, Err(RecirculationTargetInputError::WrongPipeline));
    }

    #[test]
    fn recirculation_target_input_rejects_disabled_input() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let input = PipelineInput {
            enabled: false,
            ..input("pipe-b", "input-backup")
        };

        let result = validate_recirculation_target_input(&target, Some(&input));

        assert_eq!(result, Err(RecirculationTargetInputError::Disabled));
    }

    #[test]
    fn recirculation_target_input_rejects_selected_input() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let input = PipelineInput {
            selected: true,
            ..input("pipe-b", "input-backup")
        };

        let result = validate_recirculation_target_input(&target, Some(&input));

        assert_eq!(result, Err(RecirculationTargetInputError::Selected));
    }

    #[test]
    fn recirculation_target_input_accepts_enabled_unselected_target() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let input = input("pipe-b", "input-backup");

        let result = validate_recirculation_target_input(&target, Some(&input));

        assert_eq!(result, Ok(()));
    }
}
