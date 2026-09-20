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

fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
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
