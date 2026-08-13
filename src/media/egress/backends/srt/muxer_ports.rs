//! Per-shard local-UDP-port reuse state for libsrt egress multiplexers.
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
//! the CPU count via `EgressFabricConfig::shard_count`). Sharding is keyed by
//! shard id alone, not by `(feed, shard)`: shard *N* of every feed shares one
//! multiplexer, which keeps the libsrt thread count bounded by shard count
//! rather than growing with the number of feeds.
//!
//! Each per-shard `Arc<Mutex<Option<u16>>>` is only ever claimed from that
//! shard's own thread (`SrtShardBackend::complete_pending_connect`), so the
//! inner mutex is uncontended in production; it stays a mutex because
//! `SrtEgressMuxerPortClaim::First` must hold it across the blocking connect
//! that learns the port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::media::egress::command::ShardId;

/// One shard's learned local UDP port: `None` until that shard's first
/// egress connect records the port libsrt autoselected, `Some` afterwards.
/// This is exactly the state `claim_srt_egress_muxer_port` takes.
pub(crate) type SrtEgressMuxerPortState = Arc<Mutex<Option<u16>>>;

/// Engine-wide registry of per-shard libsrt egress multiplexer ports.
///
/// Cloning shares the same registry; entries are created lazily on first
/// lookup and are deliberately never removed, so a shard that is shrunk away
/// by `EgressFabricRuntime::rescale` and later regrown reuses its previous
/// multiplexer port instead of stranding it.
#[derive(Clone, Default)]
pub(crate) struct SrtEgressMuxerPorts {
    shards: Arc<Mutex<HashMap<ShardId, SrtEgressMuxerPortState>>>,
}

impl SrtEgressMuxerPorts {
    /// The reuse state for `shard_id`, created empty on first request.
    pub(crate) fn shard(&self, shard_id: ShardId) -> SrtEgressMuxerPortState {
        self.shards
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(shard_id)
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
    pub(crate) fn recorded_port(&self, shard_id: ShardId) -> Option<u16> {
        *self
            .shard(shard_id)
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

        let first = ports.shard(ShardId::new(0));
        let second = ports.shard(ShardId::new(1));

        assert!(
            !Arc::ptr_eq(&first, &second),
            "each shard must claim its own libsrt multiplexer port"
        );
        assert_eq!(ports.tracked_shards(), 2);
    }

    #[test]
    fn repeated_lookups_for_one_shard_share_reuse_state() {
        let ports = SrtEgressMuxerPorts::default();

        let first = ports.shard(ShardId::new(3));
        *first.lock().unwrap() = Some(41_000);
        let again = ports.shard(ShardId::new(3));

        assert!(
            Arc::ptr_eq(&first, &again),
            "leaves on one shard must keep sharing that shard's local port"
        );
        assert_eq!(ports.recorded_port(ShardId::new(3)), Some(41_000));
        assert_eq!(ports.recorded_port(ShardId::new(4)), None);
        assert_eq!(ports.tracked_shards(), 2);
    }

    #[test]
    fn clones_share_one_registry() {
        let ports = SrtEgressMuxerPorts::default();
        let clone = ports.clone();

        let from_clone = clone.shard(ShardId::new(2));
        *from_clone.lock().unwrap() = Some(42_000);

        assert_eq!(ports.recorded_port(ShardId::new(2)), Some(42_000));
        assert_eq!(ports.tracked_shards(), 1);
    }
}
