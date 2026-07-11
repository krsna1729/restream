use crate::domain::srt_ingest::SrtPipelineIngestConfig;

pub fn parse_pipeline_srt_ingest_policy(raw: Option<&str>) -> Option<SrtPipelineIngestConfig> {
    raw.and_then(|value| serde_json::from_str::<SrtPipelineIngestConfig>(value).ok())
}

pub fn serialize_pipeline_srt_ingest_policy(
    config: &SrtPipelineIngestConfig,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(config)
}
