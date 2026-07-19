//! Generic output lifecycle helpers shared across harness modes.

use super::*;

pub(crate) async fn create_output(
    api: &RampApi,
    pipeline_id: &str,
    name: &str,
    url: &str,
    encoding: &str,
) -> Result<String, String> {
    create_output_with_rtmp_mode(
        api,
        pipeline_id,
        name,
        url,
        encoding,
        RtmpOutputMode::Legacy,
    )
    .await
}

pub(crate) async fn create_output_with_rtmp_mode(
    api: &RampApi,
    pipeline_id: &str,
    name: &str,
    url: &str,
    encoding: &str,
    rtmp_mode: RtmpOutputMode,
) -> Result<String, String> {
    let output = api
        .post_json(
            &format!("/api/v1/pipelines/{pipeline_id}/outputs"),
            output_create_payload_with_rtmp_mode(name, url, encoding, rtmp_mode),
        )
        .await?;
    output["output"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or("output create response missing output.id".to_string())
}

pub(crate) async fn start_output(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
) -> Result<(), String> {
    api.post_null(&format!(
        "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/start"
    ))
    .await
    .map(|_| ())
}
