use std::num::{NonZeroU32, NonZeroUsize};

use crate::media::egress::command::{OutputId, OutputSpec, ShardId};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressManagerConfigError {
    ZeroShardCount,
    ZeroCommandCapacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressManagerConfig {
    shard_count: NonZeroU32,
    command_channel_capacity: NonZeroUsize,
}

impl EgressManagerConfig {
    pub fn new(
        shard_count: u32,
        command_channel_capacity: usize,
    ) -> Result<Self, EgressManagerConfigError> {
        let shard_count =
            NonZeroU32::new(shard_count).ok_or(EgressManagerConfigError::ZeroShardCount)?;
        let command_channel_capacity = NonZeroUsize::new(command_channel_capacity)
            .ok_or(EgressManagerConfigError::ZeroCommandCapacity)?;
        Ok(Self {
            shard_count,
            command_channel_capacity,
        })
    }

    pub fn shard_count(self) -> NonZeroU32 {
        self.shard_count
    }

    pub fn command_channel_capacity(self) -> NonZeroUsize {
        self.command_channel_capacity
    }
}

#[derive(Debug, Clone)]
pub struct EgressManager {
    config: EgressManagerConfig,
}

impl EgressManager {
    pub fn new(config: EgressManagerConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> EgressManagerConfig {
        self.config
    }

    pub fn assign_output(&self, output_id: &OutputId) -> ShardId {
        assign_output_to_shard(output_id, self.config.shard_count)
    }

    pub fn assign_spec(&self, spec: &OutputSpec) -> ShardId {
        self.assign_output(&spec.id)
    }
}

pub fn assign_output_to_shard(output_id: &OutputId, shard_count: NonZeroU32) -> ShardId {
    let hash = stable_output_hash(output_id.as_str().as_bytes());
    ShardId::new((hash % u64::from(shard_count.get())) as u32)
}

fn stable_output_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::command::{FeedId, ProtocolSpec};
    use crate::media::egress::policy::LeafPolicy;

    fn manager(shards: u32) -> EgressManager {
        EgressManager::new(EgressManagerConfig::new(shards, 128).unwrap())
    }

    fn spec(id: &str) -> OutputSpec {
        OutputSpec {
            id: OutputId::new(id),
            generation: 1,
            feed: FeedId::new("feed-1"),
            protocol: ProtocolSpec::Rtmp {
                url: "rtmp://localhost/live".to_string(),
                tls: false,
            },
            policy: LeafPolicy::default(),
        }
    }

    #[test]
    fn config_rejects_zero_shards() {
        assert_eq!(
            EgressManagerConfig::new(0, 128),
            Err(EgressManagerConfigError::ZeroShardCount)
        );
    }

    #[test]
    fn config_rejects_zero_command_capacity() {
        assert_eq!(
            EgressManagerConfig::new(1, 0),
            Err(EgressManagerConfigError::ZeroCommandCapacity)
        );
    }

    #[test]
    fn same_output_is_assigned_to_the_same_shard() {
        let manager = manager(8);
        let first = manager.assign_output(&OutputId::new("out-a"));
        let second = manager.assign_output(&OutputId::new("out-a"));

        assert_eq!(first, second);
    }

    #[test]
    fn assignment_is_bounded_by_configured_shard_count() {
        let manager = manager(3);

        for i in 0..1_000 {
            let shard = manager.assign_output(&OutputId::new(format!("out-{i}")));
            assert!(shard.index() < 3);
        }
    }

    #[test]
    fn assignment_uses_output_identity_not_feed_identity() {
        let manager = manager(16);
        let first = manager.assign_output(&OutputId::new("pipeline-a/out-1"));
        let second = manager.assign_output(&OutputId::new("pipeline-a/out-2"));

        assert_ne!(first, second);
    }

    #[test]
    fn spec_assignment_uses_spec_output_id() {
        let manager = manager(8);
        let output_spec = spec("out-from-spec");

        assert_eq!(
            manager.assign_spec(&output_spec),
            manager.assign_output(&output_spec.id)
        );
    }
}
