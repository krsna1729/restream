//! Per-`(pipeline, shard)` shared UDP state for srt-rs egress callers.
//!
//! One state owns one application UDP socket and a runtime-neutral
//! `CallerTable`. It is scoped by both pipeline and shard so independent
//! pipelines never share a failure or contention domain, while outputs on one
//! shard reuse the same socket and SRT Socket-ID demultiplexer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::media::egress::command::ShardId;
use crate::media::srt::SharedSrtEgress;

/// One `(pipeline, shard)`'s shared socket/table, initialized by its first
/// caller. `None` means the shard has not opened an SRT egress socket yet.
pub(crate) type SrtEgressMuxerPortState = Arc<Mutex<Option<SharedSrtEgress>>>;

/// Engine-wide registry of per-`(pipeline, shard)` shared SRT egress states.
///
/// Cloning shares the same registry; entries are created lazily on first
/// lookup and are deliberately never removed, so a shard that is shrunk away
/// by `EgressFabricRuntime::rescale` and later regrown reuses its previous
/// multiplexer port instead of stranding it.
#[derive(Clone, Default)]
pub(crate) struct SrtEgressMuxerPorts {
    shards: Arc<Mutex<HashMap<(String, ShardId), SrtEgressMuxerPortState>>>,
}

impl SrtEgressMuxerPorts {
    /// The reuse state for `(pipeline_id, shard_id)`, created empty on first
    /// request. `pipeline_id` is an opaque grouping key here (compared and
    /// hashed as a plain string), not a typed `PipelineId` — the fabric
    /// layer intentionally does not depend on `domain` ID types.
    pub(crate) fn shard(&self, pipeline_id: &str, shard_id: ShardId) -> SrtEgressMuxerPortState {
        self.shards
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry((pipeline_id.to_string(), shard_id))
            .or_default()
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn tracked_shards(&self) -> usize {
        self.shards
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    #[cfg(test)]
    pub(crate) fn recorded_port(&self, pipeline_id: &str, shard_id: ShardId) -> Option<u16> {
        self.shard(pipeline_id, shard_id)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(SharedSrtEgress::local_port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_shards_get_distinct_reuse_state() {
        let ports = SrtEgressMuxerPorts::default();

        let first = ports.shard("pipeline-a", ShardId::new(0));
        let second = ports.shard("pipeline-a", ShardId::new(1));

        assert!(
            !Arc::ptr_eq(&first, &second),
            "each shard must own distinct shared SRT state"
        );
        assert_eq!(ports.tracked_shards(), 2);
    }

    #[test]
    fn repeated_lookups_for_one_shard_share_reuse_state() {
        let ports = SrtEgressMuxerPorts::default();

        let first = ports.shard("pipeline-a", ShardId::new(3));
        let again = ports.shard("pipeline-a", ShardId::new(3));

        assert!(
            Arc::ptr_eq(&first, &again),
            "leaves on one shard must keep sharing that shard's local port"
        );
        assert_eq!(ports.recorded_port("pipeline-a", ShardId::new(3)), None);
        assert_eq!(ports.recorded_port("pipeline-a", ShardId::new(4)), None);
        assert_eq!(ports.tracked_shards(), 2);
    }

    #[test]
    fn clones_share_one_registry() {
        let ports = SrtEgressMuxerPorts::default();
        let clone = ports.clone();

        let from_clone = clone.shard("pipeline-a", ShardId::new(2));
        assert!(Arc::ptr_eq(
            &from_clone,
            &ports.shard("pipeline-a", ShardId::new(2))
        ));
        assert_eq!(ports.tracked_shards(), 1);
    }

    #[test]
    fn distinct_pipelines_get_distinct_reuse_state_for_the_same_shard_id() {
        let ports = SrtEgressMuxerPorts::default();

        let a0 = ports.shard("pipeline-a", ShardId::new(0));
        let b0 = ports.shard("pipeline-b", ShardId::new(0));

        assert!(
            !Arc::ptr_eq(&a0, &b0),
            "two pipelines must not share SRT state for the same shard id \
             -- that would be a cross-tenant contention coupling"
        );
        assert_eq!(ports.tracked_shards(), 2);
    }
}
