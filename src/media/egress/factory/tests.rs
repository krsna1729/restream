use super::*;
use crate::media::egress::command::ShardId;
use crate::media::egress::journal::FeedEpoch;
use crate::media::ts_chunk_ring::TsChunkRing;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn feed() -> TsFeed {
    let ring = TsChunkRing::new(8, CancellationToken::new());
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

fn budget() -> WorkBudgetConfig {
    WorkBudgetConfig::new(8, 1024, Duration::from_millis(1))
}

fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(1)).unwrap()
}

fn owner_settings() -> SrtOwnerSettings {
    SrtOwnerSettings::new(4)
}

/// The factory runs on each shard's own thread and builds a Compio runtime
/// there, so a started group proves the runtime was constructible off the
/// caller's thread and that every shard came up.
#[test]
fn spawn_srt_fabric_shard_group_starts_requested_shards() {
    let group = spawn_srt_fabric_shard_group(
        NonZeroU32::new(2).unwrap(),
        shard_config(),
        budget(),
        |_: ShardId| feed(),
        owner_settings(),
    )
    .unwrap();

    assert_eq!(group.shard_count(), 2);
    assert_eq!(group.snapshots().len(), 2);
    let snapshots = group.shutdown_and_join();
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().all(|snapshot| !snapshot.panicked));
}

fn denied_runtime(
    _: srt_transport::compio::ProductionRuntimeConfig,
) -> Result<compio::runtime::Runtime, String> {
    Err(
        "SRT egress Compio runtime failed to build: Operation not permitted (os error 1)"
            .to_string(),
    )
}

/// The forced-io_uring runtime cannot be created (a seccomp-restricted
/// container is the canonical cause): the whole shard group fails closed with
/// the typed backend error, leaves no shard running, and never switches to
/// another runtime or backend -- retrying fails the same way.
#[test]
fn a_runtime_construction_failure_is_a_typed_backend_error_and_leaves_no_shard() {
    let settings = owner_settings().with_runtime_builder(denied_runtime);
    for _ in 0..2 {
        let error = spawn_srt_fabric_shard_group(
            NonZeroU32::new(3).unwrap(),
            shard_config(),
            budget(),
            |_: ShardId| feed(),
            settings,
        )
        .map(|_| ())
        .expect_err("no group is created without an io_uring runtime");
        match error {
            SrtFabricShardGroupError::Backend(message) => {
                assert!(message.contains("Operation not permitted"), "{message}");
                assert!(
                    message.contains("Compio runtime failed to build"),
                    "{message}"
                );
            }
            other => panic!("expected the typed Backend error, got {other:?}"),
        }
    }
}
