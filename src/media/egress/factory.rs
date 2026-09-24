use std::num::NonZeroU32;
use std::sync::Arc;

use crate::media::egress::backends::compio_tcp::CompioTcpPoller;
use crate::media::egress::backends::pipeline_shard::{
    PipelineShardBackend, SharedPipelineTargetSource,
};
use crate::media::egress::backends::rtmp_shard::SharedRtmpPublishStartupSource;
use crate::media::egress::backends::rtmp_shard_resolve_runtime::resolving_rtmp_shard_backend;
use crate::media::egress::backends::sink_shard::SinkShardBackend;
use crate::media::egress::backends::srt::SrtOwnerSettings;
use crate::media::egress::backends::srt::resolve_runtime::resolving_srt_shard_backend;
use crate::media::egress::backends::tcp::TcpEgressPollError;
use crate::media::egress::command::ShardId;
use crate::media::egress::journal::{RingFeed, TsFeed};
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{
    EgressShardConfig, EgressShardGroup, EgressShardGroupError, EgressShardGroupSpawnError,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SrtFabricShardGroupError<E> {
    Backend(E),
    Group(EgressShardGroupError),
}

/// Spawn the SRT shard group. Each shard's backend -- including its Compio
/// runtime and Owners -- is constructed by the factory on that shard's own OS
/// thread; a runtime that cannot be built surfaces here as
/// `SrtFabricShardGroupError::Backend` and no shard is left running.
pub(crate) fn spawn_srt_fabric_shard_group<F>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    mut feed_for: F,
    owner_settings: SrtOwnerSettings,
) -> Result<EgressShardGroup, SrtFabricShardGroupError<String>>
where
    F: FnMut(ShardId) -> TsFeed,
{
    let drain_timeout = shard_config.drain_timeout();
    let leaf_capacity = shard_config.leaf_capacity().get();
    EgressShardGroup::try_spawn_with(shard_count, shard_config, |shard_id| {
        let feed = feed_for(shard_id);
        move || {
            resolving_srt_shard_backend(feed, budget, drain_timeout, leaf_capacity, owner_settings)
        }
    })
    .map_err(|error| match error {
        EgressShardGroupSpawnError::Backend(error) => SrtFabricShardGroupError::Backend(error),
        EgressShardGroupSpawnError::Group(error) => SrtFabricShardGroupError::Group(error),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RtmpFabricShardGroupError<E> {
    Backend(E),
    Group(EgressShardGroupError),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_rtmp_fabric_shard_group<F>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    poller_max_events: usize,
    budget: WorkBudget,
    chunk_size: u32,
    rtmps_client_config: Arc<tokio_rustls::rustls::ClientConfig>,
    startup_source: SharedRtmpPublishStartupSource,
    mut feed_for: F,
) -> Result<EgressShardGroup, RtmpFabricShardGroupError<TcpEgressPollError>>
where
    F: FnMut(ShardId) -> RingFeed,
{
    let drain_timeout = shard_config.drain_timeout();
    let leaf_capacity = shard_config.leaf_capacity().get();
    EgressShardGroup::try_spawn_with(shard_count, shard_config, |shard_id| {
        let feed = feed_for(shard_id);
        let rtmps_client_config = rtmps_client_config.clone();
        let startup_source = startup_source.clone();
        // The Compio runtime is created on the shard thread that drives it,
        // so a creation failure surfaces here as `Backend`.
        move || {
            let poller = CompioTcpPoller::new(poller_max_events)?;
            Ok(resolving_rtmp_shard_backend(
                poller,
                feed,
                budget,
                chunk_size,
                rtmps_client_config,
                startup_source,
                drain_timeout,
                leaf_capacity,
            ))
        }
    })
    .map_err(|error| match error {
        EgressShardGroupSpawnError::Backend(error) => RtmpFabricShardGroupError::Backend(error),
        EgressShardGroupSpawnError::Group(error) => RtmpFabricShardGroupError::Group(error),
    })
}

/// Spawns one [`SinkShardBackend`] per shard, all bound to the same feed —
/// mirrors `spawn_rtmp_fabric_shard_group`/`spawn_srt_fabric_shard_group`,
/// simplified: sink leaves have no socket and no poller (see
/// `sink_shard.rs`'s module doc), so there is no per-shard readiness poller
/// to construct or thread through.
pub(crate) fn spawn_sink_fabric_shard_group<F>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    mut feed_for: F,
) -> Result<EgressShardGroup, EgressShardGroupError>
where
    F: FnMut(ShardId) -> RingFeed,
{
    let leaf_capacity = shard_config.leaf_capacity().get();
    EgressShardGroup::spawn_with(shard_count, shard_config, |shard_id| {
        let feed = feed_for(shard_id);
        move || SinkShardBackend::new(feed, budget).with_leaf_capacity(leaf_capacity)
    })
}

/// Spawns one [`PipelineShardBackend`] per shard, all bound to the same
/// feed and sharing one [`SharedPipelineTargetSource`] — mirrors
/// `spawn_sink_fabric_shard_group`, with the target source threaded
/// through the same way RTMP threads its publish-startup source.
pub(crate) fn spawn_pipeline_fabric_shard_group<F>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    target_source: SharedPipelineTargetSource,
    mut feed_for: F,
) -> Result<EgressShardGroup, EgressShardGroupError>
where
    F: FnMut(ShardId) -> RingFeed,
{
    let leaf_capacity = shard_config.leaf_capacity().get();
    EgressShardGroup::spawn_with(shard_count, shard_config, |shard_id| {
        let feed = feed_for(shard_id);
        let target_source = target_source.clone();
        move || {
            PipelineShardBackend::new(feed, budget, target_source).with_leaf_capacity(leaf_capacity)
        }
    })
}

#[cfg(test)]
mod tests;
