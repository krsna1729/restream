use std::num::NonZeroU32;
use std::sync::Arc;

use crate::media::egress::backends::pipeline_shard::{
    PipelineShardBackend, SharedPipelineTargetSource,
};
use crate::media::egress::backends::rtmp_shard::SharedRtmpPublishStartupSource;
use crate::media::egress::backends::rtmp_shard_resolve_runtime::resolving_rtmp_shard_backend;
use crate::media::egress::backends::sink_shard::SinkShardBackend;
use crate::media::egress::backends::srt::muxer_ports::SrtEgressMuxerPorts;
use crate::media::egress::backends::srt::resolve_runtime::{
    ResolvingSrtShardBackendDefault, resolving_srt_shard_backend,
};
use crate::media::egress::backends::tcp::{IoUringTcpPoller, TcpEgressPollError};
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

pub(crate) fn spawn_srt_fabric_shard_group<F>(
    pipeline_id: &str,
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    feed_for: F,
    srt_egress_muxer_port_reuse: Option<SrtEgressMuxerPorts>,
    connect_admission: Option<Arc<tokio::sync::Semaphore>>,
) -> Result<EgressShardGroup, SrtFabricShardGroupError<String>>
where
    F: FnMut(ShardId) -> TsFeed,
{
    spawn_srt_fabric_shard_group_with_runtime_check(
        pipeline_id,
        shard_count,
        shard_config,
        budget,
        feed_for,
        crate::media::srt::ensure_srt_native,
        srt_egress_muxer_port_reuse,
        connect_admission,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_srt_fabric_shard_group_with_runtime_check<F, E>(
    pipeline_id: &str,
    shard_count: NonZeroU32,
    shard_config: EgressShardConfig,
    budget: WorkBudget,
    feed_for: F,
    runtime_check: impl FnOnce() -> Result<(), E>,
    srt_egress_muxer_port_reuse: Option<SrtEgressMuxerPorts>,
    connect_admission: Option<Arc<tokio::sync::Semaphore>>,
) -> Result<EgressShardGroup, SrtFabricShardGroupError<E>>
where
    F: FnMut(ShardId) -> TsFeed,
{
    runtime_check().map_err(SrtFabricShardGroupError::Backend)?;
    let mut factories = srt_fabric_shard_factories(
        pipeline_id,
        shard_count,
        budget,
        feed_for,
        srt_egress_muxer_port_reuse,
        shard_config.drain_timeout(),
        shard_config.leaf_capacity().get(),
        connect_admission,
    )
    .into_iter();
    EgressShardGroup::spawn_with(shard_count, shard_config, |_| {
        factories.next().expect("one SRT shard factory per shard")
    })
    .map_err(SrtFabricShardGroupError::Group)
}

/// One factory per shard. The `Send` inputs (feed reader, the per-shard
/// muxer port state, the shared admission handle) are captured here on the
/// caller thread; the backend itself is built when the factory runs on its
/// shard thread.
#[allow(clippy::too_many_arguments)]
fn srt_fabric_shard_factories<F>(
    pipeline_id: &str,
    shard_count: NonZeroU32,
    budget: WorkBudget,
    mut feed_for: F,
    srt_egress_muxer_port_reuse: Option<SrtEgressMuxerPorts>,
    drain_timeout: std::time::Duration,
    leaf_capacity: usize,
    connect_admission: Option<Arc<tokio::sync::Semaphore>>,
) -> Vec<impl FnOnce() -> ResolvingSrtShardBackendDefault + Send + 'static>
where
    F: FnMut(ShardId) -> TsFeed,
{
    let mut factories = Vec::with_capacity(shard_count.get() as usize);
    for shard_index in 0..shard_count.get() {
        let shard_id = ShardId::new(shard_index);
        let feed = feed_for(shard_id);
        // Per (pipeline, shard), not one state shared engine-wide or
        // shared across pipelines: libsrt gives each bound local port
        // exactly one `CSndQueue` worker thread, so a group-wide port
        // would funnel every leaf on every shard through a single
        // libsrt sender thread, and a pipeline-agnostic port would let
        // unrelated pipelines share that thread (see `muxer_ports.rs`).
        let muxer_ports = srt_egress_muxer_port_reuse
            .as_ref()
            .map(|ports| ports.shard(pipeline_id, shard_id));
        // Shared engine-wide, not per shard: this bounds total
        // in-flight SRT connect concurrency, independent of shard
        // count (see `srt_connect_admission.rs`).
        let connect_admission = connect_admission.clone();
        factories.push(move || {
            resolving_srt_shard_backend(
                feed,
                budget,
                muxer_ports,
                drain_timeout,
                leaf_capacity,
                connect_admission,
            )
        });
    }
    factories
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
        // The io_uring poller is created on the shard thread that will drive
        // it, so a creation failure surfaces here as `Backend`.
        move || {
            let poller = IoUringTcpPoller::new(poller_max_events)?;
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
