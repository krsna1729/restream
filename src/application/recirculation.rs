use crate::application::models::Output;
use crate::application::pipeline_inputs::PipelineInputService;
use crate::application::services::{OutputService, ServiceError, ServiceResult};
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

#[derive(Clone)]
pub struct RecirculationService {
    output_service: OutputService,
    pipeline_input_service: PipelineInputService,
}

impl RecirculationService {
    pub fn with_services(
        output_service: OutputService,
        pipeline_input_service: PipelineInputService,
    ) -> Self {
        Self {
            output_service,
            pipeline_input_service,
        }
    }

    pub async fn validate_output_candidate(
        &self,
        source_pipeline_id: &str,
        target: &RecirculationTarget,
    ) -> ServiceResult<()> {
        let outputs = self.output_service.list_outputs().await?;
        validate_recirculation_topology(source_pipeline_id, target, &outputs)
            .map_err(recirculation_topology_service_error)?;

        let target_input = self
            .pipeline_input_service
            .get(target.pipeline_id(), target.input_id())
            .await
            .map(Some)
            .or_else(|error| match error {
                ServiceError::NotFound(_) => Ok(None),
                other => Err(other),
            })?;
        validate_recirculation_target_input(target, target_input.as_ref())
            .map_err(recirculation_target_input_service_error)
    }
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

fn recirculation_topology_service_error(error: RecirculationTopologyError) -> ServiceError {
    ServiceError::conflict(match error {
        RecirculationTopologyError::DirectCycle => {
            "pipeline recirculation cannot target an input on the same pipeline"
        }
        RecirculationTopologyError::IndirectCycle => {
            "pipeline recirculation would create a pipeline cycle"
        }
    })
}

fn recirculation_target_input_service_error(error: RecirculationTargetInputError) -> ServiceError {
    match error {
        RecirculationTargetInputError::Missing => {
            ServiceError::not_found("pipeline recirculation target input not found")
        }
        RecirculationTargetInputError::WrongPipeline => ServiceError::conflict(
            "pipeline recirculation target input belongs to a different pipeline",
        ),
        RecirculationTargetInputError::Disabled => {
            ServiceError::conflict("pipeline recirculation target input must be enabled")
        }
        RecirculationTargetInputError::Selected => {
            ServiceError::conflict("pipeline recirculation target input must not be selected")
        }
    }
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
    use crate::application::models::Pipeline;
    use crate::application::ports::{
        OutputCreateFuture, OutputDeleteFuture, OutputListFuture, OutputLookupFuture, OutputStore,
        OutputStoreError, OutputUpdateFuture, PipelineCreateFuture, PipelineDeleteFuture,
        PipelineIngestHostFuture, PipelineListFuture, PipelineLookupFuture, PipelineStore,
        PipelineStoreError, PipelineUpdateFuture,
    };
    use crate::application::services::PipelineService;
    use crate::domain::output_spec::OutputConfig;
    use crate::domain::pipeline_input::{PipelineInput, PipelineInputRole};
    use crate::domain::state::DesiredOutputState;
    use std::sync::Arc;

    struct ReadOnlyOutputStore {
        outputs: Vec<Output>,
    }

    impl OutputStore for ReadOnlyOutputStore {
        fn list_outputs<'a>(&'a self) -> OutputListFuture<'a> {
            Box::pin(async move { Ok(self.outputs.clone()) })
        }

        fn list_outputs_for_pipeline<'a>(&'a self, pipeline_id: &'a str) -> OutputListFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .outputs
                    .iter()
                    .filter(|output| output.pipeline_id == pipeline_id)
                    .cloned()
                    .collect())
            })
        }

        fn get_output<'a>(&'a self, pipeline_id: &'a str, id: &'a str) -> OutputLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .outputs
                    .iter()
                    .find(|output| output.pipeline_id == pipeline_id && output.id == id)
                    .cloned())
            })
        }

        fn create_output<'a>(
            &'a self,
            _id: &'a str,
            _pipeline_id: &'a str,
            _name: &'a str,
            _url: &'a str,
            _monitoring_url: Option<&'a str>,
            _desired_state: DesiredOutputState,
            _config: &'a OutputConfig,
        ) -> OutputCreateFuture<'a> {
            Box::pin(async move { Err(OutputStoreError::new("read-only output store")) })
        }

        fn update_output<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _id: &'a str,
            _name: &'a str,
            _url: &'a str,
            _monitoring_url: Option<&'a str>,
            _config: &'a OutputConfig,
        ) -> OutputUpdateFuture<'a> {
            Box::pin(async move { Err(OutputStoreError::new("read-only output store")) })
        }

        fn delete_output<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _id: &'a str,
        ) -> OutputDeleteFuture<'a> {
            Box::pin(async move { Err(OutputStoreError::new("read-only output store")) })
        }

        fn set_output_desired_state<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _id: &'a str,
            _desired_state: DesiredOutputState,
        ) -> OutputCreateFuture<'a> {
            Box::pin(async move { Err(OutputStoreError::new("read-only output store")) })
        }
    }

    struct ReadOnlyInputStore {
        inputs: Vec<PipelineInput>,
    }

    impl crate::application::pipeline_inputs::PipelineInputStore for ReadOnlyInputStore {
        fn get<'a>(
            &'a self,
            pipeline_id: &'a str,
            input_id: &'a str,
        ) -> crate::application::pipeline_inputs::InputLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .inputs
                    .iter()
                    .find(|input| input.pipeline_id == pipeline_id && input.id == input_id)
                    .cloned())
            })
        }

        fn get_by_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> crate::application::pipeline_inputs::InputLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .inputs
                    .iter()
                    .find(|input| input.stream_key == stream_key)
                    .cloned())
            })
        }

        fn list<'a>(
            &'a self,
            pipeline_id: &'a str,
        ) -> crate::application::pipeline_inputs::InputListFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .inputs
                    .iter()
                    .filter(|input| input.pipeline_id == pipeline_id)
                    .cloned()
                    .collect())
            })
        }

        fn create<'a>(
            &'a self,
            _id: &'a str,
            _pipeline_id: &'a str,
            _label: &'a str,
            _stream_key: &'a str,
        ) -> crate::application::pipeline_inputs::InputWriteFuture<'a> {
            Box::pin(async move {
                Err(
                    crate::application::pipeline_inputs::PipelineInputStoreError::Internal(
                        "read-only input store".to_string(),
                    ),
                )
            })
        }

        fn update<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _input_id: &'a str,
            _label: &'a str,
            _enabled: bool,
        ) -> crate::application::pipeline_inputs::InputUpdateFuture<'a> {
            Box::pin(async move {
                Err(
                    crate::application::pipeline_inputs::PipelineInputStoreError::Internal(
                        "read-only input store".to_string(),
                    ),
                )
            })
        }

        fn delete<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _input_id: &'a str,
        ) -> crate::application::pipeline_inputs::InputDeleteFuture<'a> {
            Box::pin(async move {
                Err(
                    crate::application::pipeline_inputs::PipelineInputStoreError::Internal(
                        "read-only input store".to_string(),
                    ),
                )
            })
        }

        fn promote<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _input_id: &'a str,
        ) -> crate::application::pipeline_inputs::InputUpdateFuture<'a> {
            Box::pin(async move {
                Err(
                    crate::application::pipeline_inputs::PipelineInputStoreError::Internal(
                        "read-only input store".to_string(),
                    ),
                )
            })
        }
    }

    struct PipelineCatalogStore;

    impl PipelineStore for PipelineCatalogStore {
        fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a> {
            Box::pin(async move {
                Ok(Some(Pipeline {
                    id: id.to_string(),
                    name: id.to_string(),
                    stream_key: format!("sk-{id}"),
                    input_source: None,
                    srt_ingest_policy: None,
                }))
            })
        }

        fn get_pipeline_by_stream_key<'a>(
            &'a self,
            _stream_key: &'a str,
        ) -> PipelineLookupFuture<'a> {
            Box::pin(async move { Ok(None) })
        }

        fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
            Box::pin(async move { Ok(Vec::new()) })
        }

        fn create_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> PipelineCreateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("read-only pipeline store")) })
        }

        fn update_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> PipelineUpdateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("read-only pipeline store")) })
        }

        fn delete_pipeline<'a>(&'a self, _id: &'a str) -> PipelineDeleteFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("read-only pipeline store")) })
        }

        fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
            Box::pin(async move { Ok(None) })
        }

        fn update_pipeline_input_source<'a>(
            &'a self,
            _pipeline: &'a Pipeline,
            _input_source: Option<&'a str>,
        ) -> PipelineUpdateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("read-only pipeline store")) })
        }
    }

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

    fn service(outputs: Vec<Output>, inputs: Vec<PipelineInput>) -> RecirculationService {
        let output_service = OutputService::with_store(Arc::new(ReadOnlyOutputStore { outputs }));
        let pipeline_service = PipelineService::with_store(Arc::new(PipelineCatalogStore));
        let pipeline_input_service = PipelineInputService::with_store(
            Arc::new(ReadOnlyInputStore { inputs }),
            pipeline_service,
        );
        RecirculationService::with_services(output_service, pipeline_input_service)
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

    #[tokio::test]
    async fn recirculation_service_accepts_valid_candidate() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let service = service(Vec::new(), vec![input("pipe-b", "input-backup")]);

        let result = service.validate_output_candidate("pipe-a", &target).await;

        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn recirculation_service_rejects_candidate_cycle() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let service = service(
            vec![output("pipe-b", "b-to-a", "pipeline://pipe-a/input-backup")],
            vec![input("pipe-b", "input-backup")],
        );

        let error = service
            .validate_output_candidate("pipe-a", &target)
            .await
            .unwrap_err();

        assert_eq!(
            error,
            ServiceError::conflict("pipeline recirculation would create a pipeline cycle")
        );
    }

    #[tokio::test]
    async fn recirculation_service_rejects_missing_target_input() {
        let target = RecirculationTarget::parse("pipeline://pipe-b/input-backup").unwrap();
        let service = service(Vec::new(), Vec::new());

        let error = service
            .validate_output_candidate("pipe-a", &target)
            .await
            .unwrap_err();

        assert_eq!(
            error,
            ServiceError::not_found("pipeline recirculation target input not found")
        );
    }
}
