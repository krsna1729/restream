use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use crate::media::egress::backends::pipeline_shard::{
    PipelineShardBackend, SharedPipelineTargetSource,
};
use crate::media::egress::backends::rtmp_shard::{
    RtmpReadinessPoller, SharedRtmpPublishStartupSource,
};
use crate::media::egress::backends::rtmp_shard_resolve_runtime::{
    ResolvingRtmpShardBackendWithPoller, resolving_rtmp_shard_backend,
};
use crate::media::egress::backends::sink_shard::SinkShardBackend;
use crate::media::egress::backends::srt::SrtReadinessPoller;
use crate::media::egress::backends::srt::resolve_runtime::{
    ResolvingSrtShardBackendWithPoller, resolving_srt_shard_backend,
};
use crate::media::egress::backends::tcp::{TcpEgressPollError, TcpEgressPoller};
use crate::media::egress::command::ShardId;
use crate::media::egress::journal::{RingFeed, TsFeed};
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{EgressShardConfig, EgressShardGroup, EgressShardGroupError};
use crate::media::srt::{SrtEgressPollError, SrtFabricPoller};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SrtFabricShardGroupError<E> {
    Backend(E),
    Group(EgressShardGroupError),
}

pub(crate) fn spawn_srt_fabric_shard_group<F>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    poller_max_events: usize,
    budget: WorkBudget,
    feed_for: F,
    srt_egress_muxer_port_reuse: Option<Arc<Mutex<Option<u16>>>>,
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
        srt_egress_muxer_port_reuse,
    )
}

fn spawn_srt_fabric_shard_group_with_poller<P, E, F, G>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    feed_for: F,
    poller_for: G,
    srt_egress_muxer_port_reuse: Option<Arc<Mutex<Option<u16>>>>,
) -> Result<EgressShardGroup, SrtFabricShardGroupError<E>>
where
    P: SrtReadinessPoller + Send + 'static,
    F: FnMut(ShardId) -> TsFeed,
    G: FnMut(ShardId) -> Result<P, E>,
{
    let backends = srt_fabric_shard_backends_with_poller(
        shard_count,
        budget,
        feed_for,
        poller_for,
        srt_egress_muxer_port_reuse,
        shard_config.drain_timeout(),
    )
    .map_err(SrtFabricShardGroupError::Backend)?;
    EgressShardGroup::spawn(shard_count, shard_config, backends)
        .map_err(SrtFabricShardGroupError::Group)
}

#[allow(clippy::too_many_arguments)]
fn srt_fabric_shard_backends_with_poller<P, E, F, G>(
    shard_count: NonZeroU32,
    budget: WorkBudget,
    mut feed_for: F,
    mut poller_for: G,
    srt_egress_muxer_port_reuse: Option<Arc<Mutex<Option<u16>>>>,
    drain_timeout: std::time::Duration,
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
            srt_egress_muxer_port_reuse.clone(),
            drain_timeout,
        ));
    }
    Ok(backends)
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
    feed_for: F,
) -> Result<EgressShardGroup, RtmpFabricShardGroupError<TcpEgressPollError>>
where
    F: FnMut(ShardId) -> RingFeed,
{
    spawn_rtmp_fabric_shard_group_with_poller(
        shard_count,
        shard_config,
        budget,
        chunk_size,
        rtmps_client_config,
        startup_source,
        feed_for,
        |shard_id| {
            let _ = shard_id;
            TcpEgressPoller::new(poller_max_events)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_rtmp_fabric_shard_group_with_poller<P, E, F, G>(
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    chunk_size: u32,
    rtmps_client_config: Arc<tokio_rustls::rustls::ClientConfig>,
    startup_source: SharedRtmpPublishStartupSource,
    feed_for: F,
    poller_for: G,
) -> Result<EgressShardGroup, RtmpFabricShardGroupError<E>>
where
    P: RtmpReadinessPoller + Send + 'static,
    F: FnMut(ShardId) -> RingFeed,
    G: FnMut(ShardId) -> Result<P, E>,
{
    let backends = rtmp_fabric_shard_backends_with_poller(
        shard_count,
        budget,
        chunk_size,
        rtmps_client_config,
        startup_source,
        shard_config.drain_timeout(),
        feed_for,
        poller_for,
    )
    .map_err(RtmpFabricShardGroupError::Backend)?;
    EgressShardGroup::spawn(shard_count, shard_config, backends)
        .map_err(RtmpFabricShardGroupError::Group)
}

#[allow(clippy::too_many_arguments)]
fn rtmp_fabric_shard_backends_with_poller<P, E, F, G>(
    shard_count: NonZeroU32,
    budget: WorkBudget,
    chunk_size: u32,
    rtmps_client_config: Arc<tokio_rustls::rustls::ClientConfig>,
    startup_source: SharedRtmpPublishStartupSource,
    drain_timeout: std::time::Duration,
    mut feed_for: F,
    mut poller_for: G,
) -> Result<Vec<ResolvingRtmpShardBackendWithPoller<P, SharedRtmpPublishStartupSource>>, E>
where
    P: RtmpReadinessPoller,
    F: FnMut(ShardId) -> RingFeed,
    G: FnMut(ShardId) -> Result<P, E>,
{
    let mut backends = Vec::with_capacity(shard_count.get() as usize);
    for shard_index in 0..shard_count.get() {
        let shard_id = ShardId::new(shard_index);
        let poller = poller_for(shard_id)?;
        backends.push(resolving_rtmp_shard_backend(
            poller,
            feed_for(shard_id),
            budget,
            chunk_size,
            rtmps_client_config.clone(),
            startup_source.clone(),
            drain_timeout,
        ));
    }
    Ok(backends)
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
    let mut backends = Vec::with_capacity(shard_count.get() as usize);
    for shard_index in 0..shard_count.get() {
        let shard_id = ShardId::new(shard_index);
        backends.push(SinkShardBackend::new(feed_for(shard_id), budget));
    }
    EgressShardGroup::spawn(shard_count, shard_config, backends)
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
    let mut backends = Vec::with_capacity(shard_count.get() as usize);
    for shard_index in 0..shard_count.get() {
        let shard_id = ShardId::new(shard_index);
        backends.push(PipelineShardBackend::new(
            feed_for(shard_id),
            budget,
            target_source.clone(),
        ));
    }
    EgressShardGroup::spawn(shard_count, shard_config, backends)
}

#[cfg(test)]
mod tests;
