use super::*;
use crate::media::egress::command::ShardId;
use crate::media::egress::journal::FeedEpoch;
use crate::media::srt::{SRTSOCKET, SrtEgressInterest, SrtEgressPollError, SrtReadyLeaf};
use crate::media::ts_chunk_ring::TsChunkRing;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct FakeSrtPoller;

impl SrtReadinessPoller for FakeSrtPoller {
    fn register_leaf(
        &mut self,
        _socket: SRTSOCKET,
        _key: crate::media::egress::scheduler::LeafKey,
        _generation: u64,
        _interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        Ok(())
    }

    fn remove(&mut self, _socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        Ok(())
    }

    fn poll_leaves(
        &mut self,
        _timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        ready.clear();
        Ok(0)
    }
}

fn feed() -> TsFeed {
    let ring = TsChunkRing::new(8, CancellationToken::new());
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
}

fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(1)).unwrap()
}

#[test]
fn srt_fabric_shard_backends_build_one_backend_per_shard() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_poller = Arc::clone(&seen);

    let backends = srt_fabric_shard_backends_with_poller(
        "pipeline-a",
        NonZeroU32::new(3).unwrap(),
        budget(),
        |_| feed(),
        move |shard_id| {
            seen_for_poller.lock().unwrap().push(shard_id.index());
            Ok::<_, &'static str>(FakeSrtPoller)
        },
        None,
        EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
    )
    .unwrap();

    assert_eq!(backends.len(), 3);
    assert_eq!(*seen.lock().unwrap(), vec![0, 1, 2]);
}

#[test]
fn srt_fabric_shard_backends_give_each_shard_its_own_muxer_port_state() {
    // libsrt runs exactly one `CSndQueue` worker thread per bound local UDP
    // port, so handing every shard the same reuse state funnels all egress
    // sockets through one libsrt sender thread. Each shard must get its own.
    let ports = SrtEgressMuxerPorts::default();

    let backends = srt_fabric_shard_backends_with_poller(
        "pipeline-a",
        NonZeroU32::new(3).unwrap(),
        budget(),
        |_| feed(),
        |_| Ok::<_, &'static str>(FakeSrtPoller),
        Some(ports.clone()),
        EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
    )
    .unwrap();

    assert_eq!(
        ports.tracked_shards(),
        3,
        "one libsrt multiplexer port per shard"
    );
    let states = backends
        .iter()
        .map(|backend| {
            backend
                .inner_backend()
                .srt_egress_muxer_port_state()
                .clone()
        })
        .collect::<Vec<_>>();
    for (index, state) in states.iter().enumerate() {
        // Within a shard, reuse still works: a later leaf on this shard
        // resolves the same state and therefore binds the same port.
        assert!(
            Arc::ptr_eq(
                state,
                &ports.shard("pipeline-a", ShardId::new(index as u32))
            ),
            "shard {index} must keep claiming its own port"
        );
        for other in states.iter().skip(index + 1) {
            assert!(
                !Arc::ptr_eq(state, other),
                "shards must not share one libsrt multiplexer"
            );
        }
    }
}

#[test]
fn srt_fabric_shard_backends_leave_muxer_port_reuse_off_without_a_registry() {
    let backends = srt_fabric_shard_backends_with_poller(
        "pipeline-a",
        NonZeroU32::new(2).unwrap(),
        budget(),
        |_| feed(),
        |_| Ok::<_, &'static str>(FakeSrtPoller),
        None,
        EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
    )
    .unwrap();

    // Reuse disabled still means backend-local, never-shared state.
    let first = backends[0]
        .inner_backend()
        .srt_egress_muxer_port_state()
        .clone();
    let second = backends[1]
        .inner_backend()
        .srt_egress_muxer_port_state()
        .clone();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(*first.lock().unwrap(), None);
}

#[test]
fn spawn_srt_fabric_shard_group_starts_requested_shards() {
    let group = spawn_srt_fabric_shard_group_with_poller(
        "pipeline-a",
        NonZeroU32::new(2).unwrap(),
        shard_config(),
        budget(),
        |_| feed(),
        |_| Ok::<_, &'static str>(FakeSrtPoller),
        None,
    )
    .unwrap();

    assert_eq!(group.shard_count(), 2);
    assert_eq!(group.snapshots().len(), 2);
    let snapshots = group.shutdown_and_join();
    assert_eq!(snapshots.len(), 2);
}

#[test]
fn spawn_srt_fabric_shard_group_reports_poller_creation_error() {
    let result = spawn_srt_fabric_shard_group_with_poller(
        "pipeline-a",
        NonZeroU32::new(2).unwrap(),
        shard_config(),
        budget(),
        |_| feed(),
        |shard_id| {
            if shard_id.index() == 1 {
                return Err("poller failed");
            }
            Ok(FakeSrtPoller)
        },
        None,
    );

    assert!(matches!(
        result,
        Err(SrtFabricShardGroupError::Backend("poller failed"))
    ));
}
