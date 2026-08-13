use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroUsize};

use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ShardId};
use crate::media::egress::shard::{EgressShardGroup, EgressShardGroupError};

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
    desired: HashMap<OutputId, DesiredOutput>,
    desired_specs: HashMap<OutputId, OutputSpec>,
    command_depths: Vec<usize>,
    draining_shards: Vec<bool>,
    shutting_down: bool,
}

impl EgressManager {
    pub fn new(config: EgressManagerConfig) -> Self {
        Self {
            command_depths: vec![0; config.shard_count.get() as usize],
            config,
            desired: HashMap::new(),
            desired_specs: HashMap::new(),
            draining_shards: vec![false; config.shard_count.get() as usize],
            shutting_down: false,
        }
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

    pub fn desired_output(&self, output_id: &OutputId) -> Option<&DesiredOutput> {
        self.desired.get(output_id)
    }

    pub fn command_depth(&self, shard_id: ShardId) -> usize {
        self.command_depths
            .get(shard_id.index() as usize)
            .copied()
            .unwrap_or(0)
    }

    pub fn complete_one_command(&mut self, shard_id: ShardId) {
        if let Some(depth) = self.command_depths.get_mut(shard_id.index() as usize) {
            *depth = depth.saturating_sub(1);
        }
    }

    pub fn apply_command(
        &mut self,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, EgressManagerCommandError> {
        self.dispatch_command(command, |_, _| Ok(())).map_err(
            |error: EgressManagerDispatchError<()>| match error {
                EgressManagerDispatchError::Command(command_error) => command_error,
                EgressManagerDispatchError::Dispatch { .. } => {
                    unreachable!("infallible dispatch failed")
                }
            },
        )
    }

    pub fn dispatch_command<E, F>(
        &mut self,
        command: EgressCommand,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        match command {
            EgressCommand::Add(spec) => self.dispatch_spec(spec, false, dispatch),
            EgressCommand::Update(spec) => self.dispatch_spec(spec, true, dispatch),
            EgressCommand::Remove(output_id) => self.dispatch_remove(output_id, dispatch),
            // Feed wakes are delivered per shard by the feed watcher, not
            // routed through manager assignment.
            EgressCommand::FeedWake => Ok(ManagerCommandOutcome::Ignored),
            EgressCommand::DrainShard(shard_id) => {
                self.check_command_slot(shard_id)
                    .map_err(EgressManagerDispatchError::Command)?;
                dispatch(shard_id, EgressCommand::DrainShard(shard_id))
                    .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
                self.reserve_command_slot(shard_id)
                    .map_err(EgressManagerDispatchError::Command)?;
                self.mark_draining(shard_id)
                    .map_err(EgressManagerDispatchError::Command)?;
                Ok(ManagerCommandOutcome::Enqueued { shard_id })
            }
            EgressCommand::Shutdown => self.dispatch_shutdown(dispatch),
        }
    }

    pub fn dispatch_to_group(
        &mut self,
        command: EgressCommand,
        group: &EgressShardGroup,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<EgressShardGroupError>> {
        self.dispatch_command(command, |shard_id, command| {
            group.try_send_to(shard_id, command)
        })
    }

    pub fn dispatch_recreate_shard<E, F>(
        &mut self,
        shard_id: ShardId,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        if self.shutting_down {
            return Ok(ManagerCommandOutcome::AlreadyShuttingDown);
        }
        let mut specs = self
            .specs_for_shard(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        self.check_command_slots(shard_id, specs.len())
            .map_err(EgressManagerDispatchError::Command)?;
        for spec in specs {
            dispatch(shard_id, EgressCommand::Add(spec))
                .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
            self.reserve_command_slot(shard_id)
                .map_err(EgressManagerDispatchError::Command)?;
        }
        Ok(ManagerCommandOutcome::Replayed {
            shard_id,
            output_count: self.desired_count_for_shard(shard_id),
        })
    }

    fn dispatch_spec<E, F>(
        &mut self,
        spec: OutputSpec,
        is_update: bool,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        let shard_id = self.assign_spec(&spec);
        if let Some(current) = self.desired.get(&spec.id) {
            if spec.generation < current.generation {
                return Ok(ManagerCommandOutcome::IgnoredStale {
                    shard_id: current.shard_id,
                });
            }
            if spec.generation == current.generation {
                return Ok(ManagerCommandOutcome::AlreadyCurrent {
                    shard_id: current.shard_id,
                });
            }
        }

        if self
            .is_draining(shard_id)
            .map_err(EgressManagerDispatchError::Command)?
        {
            return Err(EgressManagerDispatchError::Command(
                EgressManagerCommandError::ShardDraining { shard_id },
            ));
        }
        // Check slot availability BEFORE cloning the OutputSpec for the
        // dispatch. When the channel is full, this avoids the spec clone
        // (heap-allocated Strings, Arc bump, LeafPolicy clone) entirely.
        self.check_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        let command = if is_update {
            EgressCommand::Update(spec.clone())
        } else {
            EgressCommand::Add(spec.clone())
        };
        dispatch(shard_id, command)
            .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        self.reserve_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        let desired = DesiredOutput {
            id: spec.id.clone(),
            generation: spec.generation,
            shard_id,
        };
        let output_id = spec.id.clone();
        self.desired.insert(output_id.clone(), desired.clone());
        self.desired_specs.insert(output_id, spec);
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    }

    fn dispatch_remove<E, F>(
        &mut self,
        output_id: OutputId,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        let Some(current) = self.desired.get(&output_id) else {
            return Ok(ManagerCommandOutcome::AlreadyRemoved);
        };
        let shard_id = current.shard_id;
        self.check_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        dispatch(shard_id, EgressCommand::Remove(output_id.clone()))
            .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        self.reserve_command_slot(shard_id)
            .map_err(EgressManagerDispatchError::Command)?;
        self.desired.remove(&output_id);
        self.desired_specs.remove(&output_id);
        Ok(ManagerCommandOutcome::Enqueued { shard_id })
    }

    fn dispatch_shutdown<E, F>(
        &mut self,
        mut dispatch: F,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        if self.shutting_down {
            return Ok(ManagerCommandOutcome::AlreadyShuttingDown);
        }
        for shard_index in 0..self.config.shard_count.get() {
            self.check_command_slot(ShardId::new(shard_index))
                .map_err(EgressManagerDispatchError::Command)?;
        }
        for shard_index in 0..self.config.shard_count.get() {
            let shard_id = ShardId::new(shard_index);
            dispatch(shard_id, EgressCommand::Shutdown)
                .map_err(|source| EgressManagerDispatchError::Dispatch { shard_id, source })?;
        }
        for shard_index in 0..self.config.shard_count.get() {
            self.reserve_command_slot(ShardId::new(shard_index))
                .map_err(EgressManagerDispatchError::Command)?;
        }
        self.shutting_down = true;
        Ok(ManagerCommandOutcome::Broadcast {
            shard_count: self.config.shard_count,
        })
    }

    fn reserve_command_slot(&mut self, shard_id: ShardId) -> Result<(), EgressManagerCommandError> {
        self.check_command_slot(shard_id)?;
        let depth = &mut self.command_depths[shard_id.index() as usize];
        *depth += 1;
        Ok(())
    }

    fn check_command_slot(&self, shard_id: ShardId) -> Result<(), EgressManagerCommandError> {
        self.check_command_slots(shard_id, 1)
    }

    fn check_command_slots(
        &self,
        shard_id: ShardId,
        additional: usize,
    ) -> Result<(), EgressManagerCommandError> {
        let Some(depth) = self.command_depths.get(shard_id.index() as usize) else {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        };
        if depth.saturating_add(additional) > self.config.command_channel_capacity.get() {
            return Err(EgressManagerCommandError::CommandChannelFull { shard_id });
        }
        Ok(())
    }

    fn is_draining(&self, shard_id: ShardId) -> Result<bool, EgressManagerCommandError> {
        self.draining_shards
            .get(shard_id.index() as usize)
            .copied()
            .ok_or(EgressManagerCommandError::UnknownShard { shard_id })
    }

    fn mark_draining(&mut self, shard_id: ShardId) -> Result<(), EgressManagerCommandError> {
        let Some(draining) = self.draining_shards.get_mut(shard_id.index() as usize) else {
            return Err(EgressManagerCommandError::UnknownShard { shard_id });
        };
        *draining = true;
        Ok(())
    }

    fn specs_for_shard(
        &self,
        shard_id: ShardId,
    ) -> Result<Vec<OutputSpec>, EgressManagerCommandError> {
        self.check_command_slots(shard_id, 0)?;
        Ok(self
            .desired_specs
            .iter()
            .filter_map(|(output_id, spec)| {
                let desired = self.desired.get(output_id)?;
                (desired.shard_id == shard_id).then(|| spec.clone())
            })
            .collect())
    }

    fn desired_count_for_shard(&self, shard_id: ShardId) -> usize {
        self.desired
            .values()
            .filter(|desired| desired.shard_id == shard_id)
            .count()
    }

    /// Live output count this manager currently owns — the input to
    /// `target_egress_fabric_shards` (`src/config.rs`) for dynamic shard
    /// scaling.
    pub fn output_count(&self) -> usize {
        self.desired.len()
    }

    /// Re-derive every output's shard assignment under `new_shard_count`
    /// and dispatch `Remove`+`Add` for exactly the outputs whose
    /// assignment actually changed (see `assign_output_to_shard`'s doc
    /// comment for why that's a small fraction, not all of them).
    /// Updates `self.config`'s shard count and the per-shard bookkeeping
    /// vecs to match. Returns the output ids that moved, for logging.
    ///
    /// Callers are expected to have already resized the underlying
    /// `EgressShardGroup` to `new_shard_count` (grow before rehoming a
    /// shard *onto*, shrink after rehoming everything *off* — see
    /// `EgressFabricRuntime::rescale`) — this method only touches
    /// `EgressManager`'s own view of shard count and assignment.
    pub fn rehome<E, F>(
        &mut self,
        new_shard_count: NonZeroU32,
        mut dispatch: F,
    ) -> Result<Vec<OutputId>, EgressManagerDispatchError<E>>
    where
        F: FnMut(ShardId, EgressCommand) -> Result<(), E>,
    {
        if new_shard_count == self.config.shard_count {
            return Ok(Vec::new());
        }
        let old_len = self.command_depths.len();
        let new_len = new_shard_count.get() as usize;
        self.config.shard_count = new_shard_count;
        // Grow the bookkeeping vecs up front so newly targeted shards have
        // a command-slot/draining entry before anything is dispatched to
        // them. Shrinking is deferred to the end of this call (see below).
        if new_len > old_len {
            self.command_depths.resize(new_len, 0);
            self.draining_shards.resize(new_len, false);
        }

        let to_move: Vec<(OutputId, OutputSpec, ShardId)> = self
            .desired
            .iter()
            .filter_map(|(output_id, desired)| {
                let new_shard = assign_output_to_shard(output_id, new_shard_count);
                if new_shard == desired.shard_id {
                    return None;
                }
                let spec = self.desired_specs.get(output_id)?.clone();
                Some((output_id.clone(), spec, desired.shard_id))
            })
            .collect();

        let mut moved = Vec::with_capacity(to_move.len());
        for (output_id, spec, old_shard_id) in to_move {
            // The caller shrinks the physical shard group before calling
            // `rehome` (see `EgressFabricRuntime::rescale`), so an output
            // whose old shard index is now >= `new_len` has already had
            // its shard shut down and drained -- there is no live handle
            // left to send `Remove` to, and the leaf state is already
            // gone. Only outputs still on a surviving shard need an
            // actual `Remove` dispatched there.
            if (old_shard_id.index() as usize) < new_len {
                self.dispatch_remove(output_id.clone(), &mut dispatch)?;
            } else {
                self.desired.remove(&output_id);
                self.desired_specs.remove(&output_id);
            }
            self.dispatch_spec(spec, false, &mut dispatch)?;
            moved.push(output_id);
        }

        if new_len < old_len {
            self.command_depths.truncate(new_len);
            self.draining_shards.truncate(new_len);
        }
        Ok(moved)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredOutput {
    pub id: OutputId,
    pub generation: u64,
    pub shard_id: ShardId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagerCommandOutcome {
    Enqueued {
        shard_id: ShardId,
    },
    Broadcast {
        shard_count: NonZeroU32,
    },
    IgnoredStale {
        shard_id: ShardId,
    },
    AlreadyCurrent {
        shard_id: ShardId,
    },
    AlreadyRemoved,
    AlreadyShuttingDown,
    /// The command is not routed through manager assignment (feed wakes are
    /// delivered per shard by the feed watcher).
    Ignored,
    Replayed {
        shard_id: ShardId,
        output_count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressManagerCommandError {
    UnknownShard { shard_id: ShardId },
    CommandChannelFull { shard_id: ShardId },
    ShardDraining { shard_id: ShardId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressManagerDispatchError<E> {
    Command(EgressManagerCommandError),
    Dispatch { shard_id: ShardId, source: E },
}

/// Rendezvous (highest-random-weight) hashing: score every shard by
/// hashing `(output_id, shard_index)` together and pick the max. Unlike
/// `hash % shard_count`, changing `shard_count` only changes the winner
/// for the outputs whose arg-max shard was affected — roughly
/// `1/shard_count` of them — instead of remapping nearly everything.
/// That property is what makes `EgressManager::rehome` (dynamic shard
/// scaling) cheap: a resize only needs to move the outputs that actually
/// changed shard, not replay every output on every shard. `shard_count`
/// is always small (see `default_egress_fabric_shards`, capped at 8), so
/// this stays a cheap `O(shard_count)` scan.
pub fn assign_output_to_shard(output_id: &OutputId, shard_count: NonZeroU32) -> ShardId {
    let bytes = output_id.as_str().as_bytes();
    (0..shard_count.get())
        .max_by_key(|&shard_index| stable_output_hash_pair(bytes, shard_index))
        .map(ShardId::new)
        .unwrap_or_else(|| ShardId::new(0))
}

fn stable_output_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// `stable_output_hash` extended with a shard index folded into the same
/// FNV chain, so each shard gets an independent score for the same output
/// id.
fn stable_output_hash_pair(bytes: &[u8], shard_index: u32) -> u64 {
    let mut hash = stable_output_hash(bytes);
    for byte in shard_index.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests;
