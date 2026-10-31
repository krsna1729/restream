use super::*;
use crate::engine_registries::SrtMuxerAssignment;
use std::collections::{HashMap, HashSet};

fn engine_with_srt_muxer_caps(max_outputs_per_shard: usize, max_shards: usize) -> MediaEngine {
    MediaEngine::new_with_config(Arc::new(crate::config::EngineConfig {
        srt_egress_muxer_max_outputs_per_shard: max_outputs_per_shard,
        srt_egress_muxer_max_shards: max_shards,
        ..crate::config::EngineConfig::default()
    }))
}

mod hls;
mod routing;
mod runtime;
mod srt_muxer;
mod transcoder;
