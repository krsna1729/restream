//! Per-`(pipeline, shard)` local-UDP-port reuse state for libsrt egress
//! multiplexers.
//!
//! libsrt does not run a worker pool: `CUDTUnited::updateMux`
//! (`srtcore/api.cpp`) creates exactly one `CMultiplexer` per bound local UDP
//! endpoint, and each multiplexer gets exactly one `CSndQueue` worker thread
//! plus one `CRcvQueue` worker thread (`CSndQueue::init` / `CRcvQueue::init`
//! in `srtcore/queue.cpp`; the `SRT:SndQ:wN` / `SRT:RcvQ:wN` thread names are
//! numbered from a process-global counter, so `wN` means "the Nth multiplexer
//! this process ever created", not "worker N of a pool"). Every socket that
//! binds the same local port shares that one multiplexer by refcount, which
//! means it also shares that one sender thread.
//!
//! `srt_egress_reuse_local_port` exists so egress sockets do *not* get one
//! multiplexer (and one thread pair) each. Scoping the learned port
//! engine-wide took that too far: every SRT egress socket across every feed
//! collapsed onto a single multiplexer, so one `CSndQueue` thread serialized
//! the packet-send and TSBPD deadline work for every outbound connection. At
//! ~120 concurrent egress connections that thread became the bottleneck and
//! libsrt's TLPKTDROP started discarding packets that missed their
//! `SRTO_PEERLATENCY` deadline.
//!
//! Scoping the state per [`ShardId`] instead gives each egress-fabric shard
//! its own multiplexer, so libsrt protocol work is spread across as many
//! independent sender/receiver threads as there are shards (already sized to
//! the CPU count via `EgressFabricConfig::shard_count`).
//!
//! The reuse key is `(pipeline id, ShardId)`, not `ShardId` alone: sharding
//! by shard id alone means shard *N* of every feed *and every pipeline*
//! shares one multiplexer, which is the right tradeoff within one pipeline's
//! own feeds (they're the same publisher's own language/quality tracks, not
//! a tenancy boundary) but leaks a native-library-level failure/contention
//! domain across independent pipelines — two unrelated customers' SRT
//! egress could otherwise share one libsrt sender thread purely because
//! their shard-assignment formulas happened to both produce `ShardId(0)`.
//! Keying by pipeline id closes that: multiplexer count becomes
//! `cpu_max_shards × active_pipeline_count` instead of a single
//! `cpu_max_shards` shared engine-wide, while still sharing across one
//! pipeline's own feeds exactly as before (so a single-pipeline deployment,
//! including every MSR measurement to date, sees no change in multiplexer
//! count from this).
//!
//! Each per-`(pipeline, shard)` `Arc<Mutex<Option<u16>>>` is only ever
//! claimed from that shard's own thread (`SrtShardBackend::complete_pending_connect`),
//! so the inner mutex is uncontended in production; it stays a mutex because
//! `SrtEgressMuxerPortClaim::First` must hold it across the blocking connect
//! that learns the port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::media::egress::command::ShardId;

/// One `(pipeline, shard)`'s learned local UDP port: `None` until that
/// shard's first egress connect records the port libsrt autoselected,
/// `Some` afterwards. This is exactly the state `claim_srt_egress_muxer_port`
/// takes.
pub(crate) type SrtEgressMuxerPortState = Arc<Mutex<Option<u16>>>;

/// Engine-wide registry of per-`(pipeline, shard)` libsrt egress multiplexer
/// ports.
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
        *self
            .shard(pipeline_id, shard_id)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
            "each shard must claim its own libsrt multiplexer port"
        );
        assert_eq!(ports.tracked_shards(), 2);
    }

    #[test]
    fn repeated_lookups_for_one_shard_share_reuse_state() {
        let ports = SrtEgressMuxerPorts::default();

        let first = ports.shard("pipeline-a", ShardId::new(3));
        *first.lock().unwrap() = Some(41_000);
        let again = ports.shard("pipeline-a", ShardId::new(3));

        assert!(
            Arc::ptr_eq(&first, &again),
            "leaves on one shard must keep sharing that shard's local port"
        );
        assert_eq!(
            ports.recorded_port("pipeline-a", ShardId::new(3)),
            Some(41_000)
        );
        assert_eq!(ports.recorded_port("pipeline-a", ShardId::new(4)), None);
        assert_eq!(ports.tracked_shards(), 2);
    }

    #[test]
    fn clones_share_one_registry() {
        let ports = SrtEgressMuxerPorts::default();
        let clone = ports.clone();

        let from_clone = clone.shard("pipeline-a", ShardId::new(2));
        *from_clone.lock().unwrap() = Some(42_000);

        assert_eq!(
            ports.recorded_port("pipeline-a", ShardId::new(2)),
            Some(42_000)
        );
        assert_eq!(ports.tracked_shards(), 1);
    }

    #[test]
    fn distinct_pipelines_get_distinct_reuse_state_for_the_same_shard_id() {
        let ports = SrtEgressMuxerPorts::default();

        let a0 = ports.shard("pipeline-a", ShardId::new(0));
        let b0 = ports.shard("pipeline-b", ShardId::new(0));

        assert!(
            !Arc::ptr_eq(&a0, &b0),
            "two pipelines must not share one libsrt multiplexer for the same shard id \
             -- that would be a cross-tenant native-thread coupling"
        );
        assert_eq!(ports.tracked_shards(), 2);
    }
}
