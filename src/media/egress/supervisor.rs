use crate::media::egress::command::ShardId;
use crate::media::egress::manager::{
    EgressManager, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardGroup, EgressShardGroupError,
    EgressShardHeartbeat,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct EgressSupervisor {
    config: EgressSupervisorConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct EgressSupervisorConfig {
    shard_config: EgressShardConfig,
    stall_after: Duration,
}

impl EgressSupervisorConfig {
    pub fn new(shard_config: EgressShardConfig, stall_after: Duration) -> Self {
        Self {
            shard_config,
            stall_after,
        }
    }
}

impl EgressSupervisor {
    pub fn new(config: EgressSupervisorConfig) -> Self {
        Self { config }
    }

    pub fn observe_shards(
        &self,
        group: &EgressShardGroup,
        now: Instant,
    ) -> Vec<EgressShardHeartbeat> {
        group.heartbeat(now, self.config.stall_after)
    }

    pub fn recover_panicked_shards<B, F>(
        &self,
        manager: &mut EgressManager,
        group: &mut EgressShardGroup,
        backend_for: F,
    ) -> Result<EgressSupervisorRecovery, EgressSupervisorError>
    where
        B: EgressShardBackend,
        F: FnMut(ShardId) -> B,
    {
        let replaced = group.replace_panicked(self.config.shard_config, backend_for);
        let mut recoveries = Vec::with_capacity(replaced.len());
        for shard_id in replaced {
            let outcome = manager.dispatch_recreate_shard(shard_id, |shard_id, command| {
                group.try_send_to(shard_id, command)
            });
            match outcome {
                Ok(ManagerCommandOutcome::Replayed {
                    shard_id,
                    output_count,
                }) => recoveries.push(EgressShardRecovery::Replayed {
                    shard_id,
                    output_count,
                }),
                Ok(ManagerCommandOutcome::AlreadyShuttingDown) => {
                    recoveries.push(EgressShardRecovery::SkippedShutdown { shard_id });
                }
                Ok(outcome) => {
                    return Err(EgressSupervisorError::UnexpectedRecoveryOutcome {
                        shard_id,
                        outcome,
                    });
                }
                Err(source) => return Err(EgressSupervisorError::Replay(source)),
            }
        }
        Ok(EgressSupervisorRecovery { recoveries })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressSupervisorRecovery {
    pub recoveries: Vec<EgressShardRecovery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressShardRecovery {
    Replayed {
        shard_id: ShardId,
        output_count: usize,
    },
    SkippedShutdown {
        shard_id: ShardId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressSupervisorError {
    Replay(EgressManagerDispatchError<EgressShardGroupError>),
    UnexpectedRecoveryOutcome {
        shard_id: ShardId,
        outcome: ManagerCommandOutcome,
    },
}

#[cfg(test)]
mod tests;
