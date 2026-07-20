use super::*;
use crate::media::engine_hls::HlsConsumers;
use crate::media::engine_registries::SrtMuxerAssignment;
use crate::media::ts_chunk_ring::TsChunkRing;
use std::collections::{HashMap, HashSet};

fn engine_with_srt_muxer_caps(max_outputs_per_shard: usize, max_shards: usize) -> MediaEngine {
    MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        srt_egress_muxer_max_outputs_per_shard: max_outputs_per_shard,
        srt_egress_muxer_max_shards: max_shards,
        ..crate::AppConfig::default()
    }))
}

#[path = "engine_stage_tests/hls.rs"]
mod hls_tests;
#[path = "engine_stage_tests/routing.rs"]
mod routing_tests;
#[path = "engine_stage_tests/runtime.rs"]
mod runtime_tests;
#[path = "engine_stage_tests/srt_muxer.rs"]
mod srt_muxer_tests;
#[path = "engine_stage_tests/transcoder.rs"]
mod transcoder_tests;
