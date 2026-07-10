//! SRT egress adapter entry points.

pub use crate::media::srt::{
    parse_pipeline_srt_ingest_policy, serialize_pipeline_srt_ingest_policy, start_srt_egress,
    teardown_srt,
};
