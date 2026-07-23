use std::num::NonZeroU32;

use crate::media::egress::backends::srt::SrtReadinessPoller;
use crate::media::egress::backends::srt::resolve_runtime::{
    ResolvingNativeSrtShardBackend, ResolvingSrtShardBackendWithPoller, resolving_srt_shard_backend,
};
use crate::media::egress::command::ShardId;
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{EgressShardConfig, EgressShardGroup, EgressShardGroupError};
use crate::media::srt::{SrtEgressPollError, SrtFabricPoller};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SrtFabricShardGroupError<E> {
    Backend(E),
    Group(EgressShardGroupError),
}

#[allow(dead_code)]
pub(crate) fn spawn_srt_fabric_shard_group<F>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    poller_max_events: usize,
    budget: WorkBudget,
    feed_for: F,
) -> Result<EgressShardGroup, SrtFabricShardGroupError<SrtEgressPollError>>
where
    F: FnMut(ShardId) -> TsFeed,
{
    spawn_srt_fabric_shard_group_with_poller(
        shard_count,
        shard_config,
        budget,
        feed_for,
        |shard_id| {
            let _ = shard_id;
            SrtFabricPoller::new(poller_max_events)
        },
    )
}

fn spawn_srt_fabric_shard_group_with_poller<P, E, F, G>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    feed_for: F,
    poller_for: G,
) -> Result<EgressShardGroup, SrtFabricShardGroupError<E>>
where
    P: SrtReadinessPoller + Send + 'static,
    F: FnMut(ShardId) -> TsFeed,
    G: FnMut(ShardId) -> Result<P, E>,
{
    let backends = srt_fabric_shard_backends_with_poller(shard_count, budget, feed_for, poller_for)
        .map_err(SrtFabricShardGroupError::Backend)?;
    EgressShardGroup::spawn(shard_count, shard_config, backends)
        .map_err(SrtFabricShardGroupError::Group)
}

#[allow(dead_code)]
pub(crate) fn srt_fabric_shard_backends<F>(
    shard_count: NonZeroU32,
    poller_max_events: usize,
    budget: WorkBudget,
    feed_for: F,
) -> Result<Vec<ResolvingNativeSrtShardBackend>, SrtEgressPollError>
where
    F: FnMut(ShardId) -> TsFeed,
{
    srt_fabric_shard_backends_with_poller(shard_count, budget, feed_for, |shard_id| {
        let _ = shard_id;
        SrtFabricPoller::new(poller_max_events)
    })
}

fn srt_fabric_shard_backends_with_poller<P, E, F, G>(
    shard_count: NonZeroU32,
    budget: WorkBudget,
    mut feed_for: F,
    mut poller_for: G,
) -> Result<Vec<ResolvingSrtShardBackendWithPoller<P>>, E>
where
    P: SrtReadinessPoller,
    F: FnMut(ShardId) -> TsFeed,
    G: FnMut(ShardId) -> Result<P, E>,
{
    let mut backends = Vec::with_capacity(shard_count.get() as usize);
    for shard_index in 0..shard_count.get() {
        let shard_id = ShardId::new(shard_index);
        let poller = poller_for(shard_id)?;
        backends.push(resolving_srt_shard_backend(
            poller,
            feed_for(shard_id),
            budget,
        ));
    }
    Ok(backends)
}

#[cfg(test)]
mod tests;
