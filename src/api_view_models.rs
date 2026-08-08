use crate::application::models::{Ingest, Job, Output, Pipeline};
use crate::application::srt_ingest::parse_persisted_srt_ingest_policy;

pub(crate) use crate::api_runtime_views::probe_snapshot;

pub(crate) fn pipeline_response_json(
    pipeline: &Pipeline,
    effective_ingest_host: &str,
    rtmp_port: u16,
    srt_port: u16,
) -> serde_json::Value {
    serde_json::json!({
        "id": pipeline.id,
        "name": pipeline.name,
        "streamKey": pipeline.stream_key,
        "inputSource": pipeline.input_source,
        "srtIngestPolicy": parse_persisted_srt_ingest_policy(
            pipeline.srt_ingest_policy.as_deref()
        ),
        "ingestUrls": {
            "rtmp": format!("rtmp://{}:{}/live/{}", effective_ingest_host, rtmp_port, pipeline.stream_key),
            "srt": format!("srt://{}:{}?streamid=publish:{}", effective_ingest_host, srt_port, pipeline.stream_key)
        }
    })
}

pub(crate) fn pipeline_response_json_with_file_ingest(
    pipeline: &Pipeline,
    effective_ingest_host: &str,
    rtmp_port: u16,
    srt_port: u16,
    ingest: Option<Ingest>,
    running: bool,
) -> serde_json::Value {
    let mut value = pipeline_response_json(pipeline, effective_ingest_host, rtmp_port, srt_port);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "fileIngest".to_string(),
            file_ingest_response(ingest, running),
        );
    }
    value
}

pub(crate) fn file_ingest_response(ingest: Option<Ingest>, running: bool) -> serde_json::Value {
    match ingest {
        Some(ingest) => serde_json::json!({
            "configured": true,
            "id": ingest.id,
            "filename": ingest.filename,
            "streamKey": ingest.stream_key,
            "loop": ingest.loop_flag,
            "startTime": ingest.start_time,
            "liveOptimized": ingest.live_optimized,
            "targetGopSeconds": ingest.target_gop_seconds,
            "running": running
        }),
        None => serde_json::json!({
            "configured": false,
            "running": false
        }),
    }
}

pub(crate) fn output_response_json(output: &Output) -> serde_json::Value {
    serde_json::json!({
        "id": output.id,
        "pipelineId": output.pipeline_id,
        "name": output.name,
        "url": output.url,
        "monitoringUrl": output.monitoring_url,
        "desiredState": output.desired_state,
        "config": output.config,
    })
}

pub(crate) fn output_response_json_list(outputs: &[Output]) -> Vec<serde_json::Value> {
    outputs.iter().map(output_response_json).collect()
}

pub(crate) fn job_response_json(job: &Job) -> serde_json::Value {
    serde_json::json!({
        "id": job.id,
        "pipelineId": job.pipeline_id,
        "outputId": job.output_id,
        "pid": job.pid,
        "status": job.status,
        "startedAt": job.started_at,
        "endedAt": job.ended_at,
        "exitCode": job.exit_code,
        "exitSignal": job.exit_signal,
    })
}

pub(crate) fn job_response_json_list(jobs: &[Job]) -> Vec<serde_json::Value> {
    jobs.iter().map(job_response_json).collect()
}

pub(crate) fn latest_job_response_json_list(jobs: &[Job]) -> Vec<serde_json::Value> {
    let mut latest_by_output: std::collections::HashSet<(&str, &str)> =
        std::collections::HashSet::new();
    let mut latest_jobs = Vec::new();

    for job in jobs {
        let key = (job.pipeline_id.as_str(), job.output_id.as_str());
        if latest_by_output.insert(key) {
            latest_jobs.push(job_response_json(job));
        }
    }

    latest_jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::models::JobStatus;
    use crate::application::srt_ingest::serialize_persisted_srt_ingest_policy;
    use crate::domain::srt_ingest::{SrtPipelineIngestConfig, SrtPipelineIngestMode};

    #[test]
    fn pipeline_responses_preserve_pipeline_and_file_ingest_shape() {
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Primary".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: Some("file:clip.mp4".to_string()),
            srt_ingest_policy: Some(
                serialize_persisted_srt_ingest_policy(&SrtPipelineIngestConfig {
                    mode: SrtPipelineIngestMode::Encrypted,
                    passphrase: Some("secret-pass".to_string()),
                    pbkeylen: Some(24),
                    latency_ms: None,
                })
                .unwrap(),
            ),
        };
        let ingest = Ingest {
            id: "ingest-1".to_string(),
            filename: "clip.mp4".to_string(),
            stream_key: pipeline.stream_key.clone(),
            loop_flag: true,
            start_time: "00:00:03".to_string(),
            live_optimized: true,
            target_gop_seconds: 3,
        };

        let pipeline_json = pipeline_response_json(&pipeline, "ingest.example", 1935, 10080);
        let pipeline_with_ingest = pipeline_response_json_with_file_ingest(
            &pipeline,
            "ingest.example",
            1935,
            10080,
            Some(ingest.clone()),
            true,
        );
        let ingest_json = file_ingest_response(Some(ingest), true);
        let missing_ingest_json = file_ingest_response(None, false);

        assert_eq!(pipeline_json["id"], "pipeline-1");
        assert_eq!(
            pipeline_json["ingestUrls"]["rtmp"],
            "rtmp://ingest.example:1935/live/stream-key"
        );
        assert_eq!(
            pipeline_json["ingestUrls"]["srt"],
            "srt://ingest.example:10080?streamid=publish:stream-key"
        );
        assert_eq!(pipeline_json["srtIngestPolicy"]["mode"], "encrypted");
        assert_eq!(pipeline_json["srtIngestPolicy"]["pbkeylen"], 24);
        assert_eq!(pipeline_with_ingest["fileIngest"]["configured"], true);
        assert_eq!(pipeline_with_ingest["fileIngest"]["filename"], "clip.mp4");
        assert_eq!(pipeline_with_ingest["fileIngest"]["running"], true);
        assert_eq!(ingest_json["configured"], true);
        assert_eq!(ingest_json["filename"], "clip.mp4");
        assert_eq!(ingest_json["loop"], true);
        assert_eq!(ingest_json["targetGopSeconds"], 3);
        assert_eq!(ingest_json["running"], true);
        assert_eq!(missing_ingest_json["configured"], false);
        assert_eq!(missing_ingest_json["running"], false);
    }

    #[test]
    fn latest_jobs_keep_only_newest_job_per_output() {
        let jobs = vec![
            Job {
                id: "job-newest".to_string(),
                pipeline_id: "pipe-1".to_string(),
                output_id: "out-1".to_string(),
                pid: Some(200),
                status: JobStatus::Running,
                started_at: "2026-06-30T12:00:00Z".to_string(),
                ended_at: None,
                exit_code: None,
                exit_signal: None,
            },
            Job {
                id: "job-older".to_string(),
                pipeline_id: "pipe-1".to_string(),
                output_id: "out-1".to_string(),
                pid: Some(100),
                status: JobStatus::Stopped,
                started_at: "2026-06-30T11:00:00Z".to_string(),
                ended_at: Some("2026-06-30T11:30:00Z".to_string()),
                exit_code: Some(0),
                exit_signal: None,
            },
            Job {
                id: "job-other-output".to_string(),
                pipeline_id: "pipe-1".to_string(),
                output_id: "out-2".to_string(),
                pid: Some(300),
                status: JobStatus::Failed,
                started_at: "2026-06-30T10:00:00Z".to_string(),
                ended_at: Some("2026-06-30T10:10:00Z".to_string()),
                exit_code: Some(1),
                exit_signal: None,
            },
        ];

        let response = latest_job_response_json_list(&jobs);

        assert_eq!(response.len(), 2);
        assert_eq!(response[0]["id"], "job-newest");
        assert_eq!(response[1]["id"], "job-other-output");
    }
}
