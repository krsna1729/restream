use std::num::NonZeroU32;

use crate::media::egress::command::{EgressCommand, ShardId};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardHandle, EgressShardSendError,
    EgressShardSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressShardGroupError {
    ShardCountTooLarge,
    BackendCountMismatch {
        expected: usize,
        actual: usize,
    },
    UnknownShard {
        shard_id: ShardId,
    },
    SendFailed {
        shard_id: ShardId,
        source: EgressShardSendError,
    },
}

#[derive(Debug)]
pub struct EgressShardGroup {
    handles: Vec<EgressShardHandle>,
}

impl EgressShardGroup {
    pub fn spawn<B: EgressShardBackend>(
        shard_count: NonZeroU32,
        config: EgressShardConfig,
        backends: Vec<B>,
    ) -> Result<Self, EgressShardGroupError> {
        let expected = usize::try_from(shard_count.get())
            .map_err(|_| EgressShardGroupError::ShardCountTooLarge)?;
        if backends.len() != expected {
            return Err(EgressShardGroupError::BackendCountMismatch {
                expected,
                actual: backends.len(),
            });
        }
        let mut handles = Vec::with_capacity(expected);
        for (index, backend) in backends.into_iter().enumerate() {
            let shard_index =
                u32::try_from(index).map_err(|_| EgressShardGroupError::ShardCountTooLarge)?;
            handles.push(EgressShardHandle::spawn(
                ShardId::new(shard_index),
                config,
                backend,
            ));
        }
        Ok(Self { handles })
    }

    pub fn shard_count(&self) -> usize {
        self.handles.len()
    }

    pub fn try_send_to(
        &self,
        shard_id: ShardId,
        command: EgressCommand,
    ) -> Result<(), EgressShardGroupError> {
        let Ok(index) = usize::try_from(shard_id.index()) else {
            return Err(EgressShardGroupError::UnknownShard { shard_id });
        };
        let Some(handle) = self.handles.get(index) else {
            return Err(EgressShardGroupError::UnknownShard { shard_id });
        };
        handle
            .try_send(command)
            .map_err(|source| EgressShardGroupError::SendFailed { shard_id, source })
    }

    pub fn snapshots(&self) -> Vec<EgressShardSnapshot> {
        self.handles
            .iter()
            .map(EgressShardHandle::snapshot)
            .collect()
    }

    pub fn shutdown_and_join(self) -> Vec<EgressShardSnapshot> {
        self.handles
            .into_iter()
            .map(EgressShardHandle::shutdown_and_join)
            .collect()
    }
}
