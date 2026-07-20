use crate::application::models::{Ingest, Job, JobStatus, Output, Pipeline};

impl From<JobStatus> for crate::db::JobStatusRecord {
    fn from(status: JobStatus) -> Self {
        match status {
            JobStatus::Running => Self::Running,
            JobStatus::Stopped => Self::Stopped,
            JobStatus::Failed => Self::Failed,
        }
    }
}

impl From<crate::db::JobStatusRecord> for JobStatus {
    fn from(status: crate::db::JobStatusRecord) -> Self {
        match status {
            crate::db::JobStatusRecord::Running => Self::Running,
            crate::db::JobStatusRecord::Stopped => Self::Stopped,
            crate::db::JobStatusRecord::Failed => Self::Failed,
        }
    }
}

impl PartialEq<JobStatus> for crate::db::JobStatusRecord {
    fn eq(&self, other: &JobStatus) -> bool {
        JobStatus::from(*self) == *other
    }
}

pub(super) fn pipeline_model(record: crate::db::PipelineRecord) -> Pipeline {
    Pipeline {
        id: record.id,
        name: record.name,
        stream_key: record.stream_key,
        input_source: record.input_source,
        srt_ingest_policy: record.srt_ingest_policy,
    }
}

pub(in crate::infrastructure) fn output_model(record: crate::db::OutputRecord) -> Output {
    Output {
        id: record.id,
        pipeline_id: record.pipeline_id,
        name: record.name,
        url: record.url,
        monitoring_url: record.monitoring_url,
        desired_state: record.desired_state,
        config: record.config,
    }
}

pub(super) fn ingest_model(record: crate::db::IngestRecord) -> Ingest {
    Ingest {
        id: record.id,
        filename: record.filename,
        stream_key: record.stream_key,
        loop_flag: record.loop_flag,
        start_time: record.start_time,
        live_optimized: record.live_optimized,
        target_gop_seconds: record.target_gop_seconds,
    }
}

pub(super) fn job_model(record: crate::db::JobRecord) -> Job {
    Job {
        id: record.id,
        pipeline_id: record.pipeline_id,
        output_id: record.output_id,
        pid: record.pid,
        status: record.status.into(),
        started_at: record.started_at,
        ended_at: record.ended_at,
        exit_code: record.exit_code,
        exit_signal: record.exit_signal,
    }
}
