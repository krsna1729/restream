use restream_domain::models::{Ingest, Pipeline};

pub(super) fn pipeline_model(record: crate::db::PipelineRecord) -> Pipeline {
    Pipeline {
        id: record.id,
        name: record.name,
        stream_key: record.stream_key,
        input_source: record.input_source,
        srt_ingest_policy: record.srt_ingest_policy,
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
        playlist: record
            .playlist
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default(),
    }
}
