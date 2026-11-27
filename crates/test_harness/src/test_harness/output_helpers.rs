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

pub(crate) async fn stop_mixed_outputs(api: &RampApi, pipeline_id: &str, output_ids: &[String]) {
    for output_id in output_ids {
        let _ = api
            .post_null(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
            ))
            .await;
    }
}

pub(crate) async fn wait_for_outputs_stopped(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let config = api.get_json("/api/v1/settings").await?;
        let all_stopped = output_ids.iter().all(|output_id| {
            config["jobs"]
                .as_array()
                .and_then(|jobs| {
                    jobs.iter().find(|job| {
                        job["pipelineId"] == pipeline_id && job["outputId"] == output_id.as_str()
                    })
                })
                .and_then(|job| job["status"].as_str())
                .is_none_or(|status| matches!(status, "stopped" | "failed"))
        });
        if all_stopped {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("lifecycle: outputs did not all stop within 60 s".to_string());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub(crate) fn output_config_from_harness_label(label: &str) -> OutputConfig {
    let mut parts = label.trim().splitn(2, '+');
    let first = parts.next().unwrap_or("source");
    let second = parts.next().filter(|value| !value.is_empty());
    let (video, audio_operation) = if is_audio_operation(first) {
        (
            OutputVideoConfig::Source {
                codec: OutputVideoCodec::Auto,
            },
            Some(first),
        )
    } else {
        let video = match first {
            "" | "source" => OutputVideoConfig::Source {
                codec: OutputVideoCodec::Auto,
            },
            "custom" => OutputVideoConfig::Custom,
            preset => OutputVideoConfig::Preset {
                preset: preset.to_string(),
                codec: OutputVideoCodec::Auto,
            },
        };
        (video, second)
    };

    OutputConfig {
        video,
        audio: audio_operation
            .map(parse_audio_operation)
            .unwrap_or(AudioRouting::Passthrough),
        ..OutputConfig::default()
    }
}

pub(crate) fn output_create_payload(name: &str, url: &str, encoding: &str) -> Value {
    output_create_payload_with_rtmp_mode(name, url, encoding, RtmpOutputMode::Legacy)
}

pub(crate) fn output_create_payload_with_rtmp_mode(
    name: &str,
    url: &str,
    encoding: &str,
    rtmp_mode: RtmpOutputMode,
) -> Value {
    let mut config = output_config_from_harness_label(encoding);
    if OutputUrlScheme::from_url(url).is_rtmp_family() {
        config = config.with_rtmp_mode(rtmp_mode);
    }
    json!({
        "name": name,
        "url": url,
        "config": config,
    })
}
