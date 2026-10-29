//! The MoQ fabric shard backend: `moqt://` leaves on one egress shard
//! thread, each a local task on the shard's own Compio runtime.
//!
//! The runtime is entered only from this backend: [`EgressShardBackend::wait_idle`]
//! parks in it (commands, leaf state changes and QUIC I/O awaited
//! together), and each ready batch ends with one non-blocking service of
//! it, so frames written during the batch reach the wire even on a shard
//! that never idles. Visits run through the fabric's [`EngineVisit`] under
//! the shard's [`WorkBudget`]: cursor priming, overrun resynchronization,
//! progress accounting and panic containment are the fabric's own.

use std::collections::HashMap;
use std::pin::pin;
use std::rc::Rc;
use std::time::Duration;

use compio::driver::{DriverType, ProactorBuilder};
use compio::runtime::{JoinHandle, Runtime, RuntimeBuilder};
use futures_util::future::{Either, select};

use crate::media::egress::backend::{CloseReason, EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::leaf_arena::{LeafArena, LeafKey};
use crate::media::egress::policy::{LeafLimits, WorkBudgetConfig};
use crate::media::egress::scheduler::{ReadyQueue, VisitDecision};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardIdleWake,
};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};

use super::leaf::{
    Activity, LeafLink, LinkPlan, MoqConnector, MoqEngine, QuicConnector, spawn_link,
};
use super::publish::{HangBroadcast, RingPublisher};
use super::startup::SharedMoqStartupSource;
use crate::media::moq::egress::media::MediaConverter;
use restream_moq::url::parse_moq_url;

struct MoqFabricLeaf {
    common: LeafCommon,
    engine: MoqEngine,
    link: Rc<LeafLink>,
    /// The connection task; dropping it cancels the connection.
    task: Option<JoinHandle<()>>,
}

pub(crate) struct MoqShardBackend<C: MoqConnector> {
    runtime: Runtime,
    connector: Rc<C>,
    activity: Rc<Activity>,
    feed: RingFeed,
    budget_config: WorkBudgetConfig,
    startup_source: SharedMoqStartupSource,
    drain_timeout: Duration,
    leaves: LeafArena<MoqFabricLeaf>,
    output_leaves: HashMap<OutputId, LeafKey>,
    ready: ReadyQueue,
    changed: Vec<LeafKey>,
    resyncs: u64,
    budget_exhaustions: u64,
}

/// The production backend: a forced-io_uring runtime and native-QUIC
/// connections.
pub(crate) fn moq_shard_backend(
    feed: RingFeed,
    budget: WorkBudgetConfig,
    startup_source: SharedMoqStartupSource,
    drain_timeout: Duration,
    leaf_capacity: usize,
) -> Result<MoqShardBackend<QuicConnector>, String> {
    let mut proactor = ProactorBuilder::new();
    proactor.driver_type(DriverType::IoUring);
    let runtime = RuntimeBuilder::new()
        .with_proactor(proactor)
        .build()
        .map_err(|error| format!("MoQ egress Compio runtime failed to build: {error}"))?;
    if !runtime.driver_type().is_iouring() {
        return Err(format!(
            "MoQ egress requires io_uring, got {:?}",
            runtime.driver_type()
        ));
    }
    Ok(MoqShardBackend::new(
        runtime,
        Rc::new(QuicConnector::default()),
        feed,
        budget,
        startup_source,
        drain_timeout,
        leaf_capacity,
    ))
}

impl<C: MoqConnector> MoqShardBackend<C> {
    pub(crate) fn new(
        runtime: Runtime,
        connector: Rc<C>,
        feed: RingFeed,
        budget_config: WorkBudgetConfig,
        startup_source: SharedMoqStartupSource,
        drain_timeout: Duration,
        leaf_capacity: usize,
    ) -> Self {
        Self {
            runtime,
            connector,
            activity: Rc::default(),
            feed,
            budget_config,
            startup_source,
            drain_timeout,
            leaves: LeafArena::with_capacity(leaf_capacity),
            output_leaves: HashMap::new(),
            ready: ReadyQueue::with_capacity(leaf_capacity),
            changed: Vec::new(),
            resyncs: 0,
            budget_exhaustions: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn link_state(&self, output_id: &OutputId) -> Option<super::leaf::LinkState> {
        let key = self.output_leaves.get(output_id)?;
        self.leaves.get(*key).map(|leaf| leaf.link.state())
    }

    fn add_leaf(&mut self, spec: OutputSpec, url: &str) {
        let output_id = spec.id.clone();
        if let Some(previous) = self.output_leaves.remove(&output_id) {
            self.remove_leaf(previous, CloseReason::Removed);
        }
        let refuse = |why: &str| {
            tracing::warn!(output_id = %output_id, reason = why, "MoQ egress leaf refused");
            spec.progress.mark_terminated_unexpectedly();
        };
        if self.leaves.is_full() {
            return refuse("shard leaf capacity exhausted");
        }
        let url = match parse_moq_url(url) {
            Ok(url) => url,
            Err(error) => return refuse(error.message()),
        };
        let Some(startup) = self.startup_source.take_startup(&output_id) else {
            return refuse("no publish startup for the output");
        };
        let converter = MediaConverter::new(&startup);
        let (origin, origin_driver, budget) = restream_moq::budget::origin();
        let broadcast = match HangBroadcast::create(
            &origin,
            budget,
            &startup.broadcast,
            converter.has_video(),
            converter.has_audio(),
        ) {
            Ok(broadcast) => broadcast,
            Err(error) => return refuse(&format!("MoQ broadcast: {error}")),
        };
        let progress = spec.progress.clone();
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress);
        let activity = self.activity.clone();
        let Some(key) = self.leaves.insert_with(|key| MoqFabricLeaf {
            common,
            engine: MoqEngine::new(RingPublisher::new(converter, broadcast)),
            link: LeafLink::new(key, activity),
            task: None,
        }) else {
            progress.mark_terminated_unexpectedly();
            return;
        };
        if let Some(leaf) = self.leaves.get_mut(key) {
            let plan = LinkPlan {
                connector: self.connector.clone(),
                url,
                extra_ca_pem: startup.extra_ca_pem,
                origin,
                origin_driver,
                drain_timeout: self.drain_timeout,
            };
            let link = leaf.link.clone();
            leaf.task = Some(self.runtime.enter(|| spawn_link(plan, link)));
        }
        self.output_leaves.insert(output_id, key);
    }

    /// Close one leaf: finish its broadcast and let its task drain and
    /// close the session in the background (bounded by the drain timeout).
    fn remove_leaf(&mut self, key: LeafKey, reason: CloseReason) {
        self.ready.remove(key);
        if let Some(mut leaf) = self.leaves.remove(key) {
            leaf.engine.close(&mut leaf.link, reason);
            if let Some(task) = leaf.task.take() {
                task.detach();
            }
        }
    }

    fn remove_output(&mut self, output_id: &OutputId, reason: CloseReason) {
        if let Some(key) = self.output_leaves.remove(output_id) {
            self.remove_leaf(key, reason);
        }
    }

    /// Queue the leaves whose link changed state.
    fn collect_changed(&mut self) {
        self.activity.take(&mut self.changed);
        for key in self.changed.drain(..) {
            if self.leaves.get(key).is_some() {
                self.ready.push(key);
            }
        }
    }

    /// Run whatever the last batch woke (moq-net drivers with new frames)
    /// and submit/reap QUIC I/O, without blocking.
    fn service_runtime(&self) {
        self.runtime.enter(|| {
            self.runtime.run();
            self.runtime.poll_with(Some(Duration::ZERO));
            self.runtime.run();
        });
    }

    fn visit_one(&mut self) {
        let Some(key) = self.ready.pop() else {
            return;
        };
        let budget = self.budget_config.new_visit();
        let Some(leaf) = self.leaves.get_mut(key) else {
            return;
        };
        let result = EngineVisit {
            generation: leaf.common.generation,
            common: &mut leaf.common,
            engine: &mut leaf.engine,
            transport: &mut leaf.link,
            readiness: Readiness::WRITABLE,
            feed: &self.feed,
            budget,
        }
        .run();
        let decision = match result {
            EngineVisitResult::StaleGeneration => VisitDecision::Suspend,
            EngineVisitResult::Visited(outcome) => {
                match outcome.progress {
                    EngineProgress::Yield => self.budget_exhaustions += 1,
                    EngineProgress::FeedOverrun => self.resyncs += 1,
                    _ => {}
                }
                outcome.decision
            }
        };
        match decision {
            VisitDecision::Continue => {
                self.ready.push(key);
            }
            VisitDecision::Suspend => {}
            VisitDecision::Close => {
                // Only a session or broadcast that ended by itself closes
                // here; an explicit Remove never visits again.
                leaf.common.progress_sink.mark_terminated_unexpectedly();
                let output_id = leaf.common.output_id.clone();
                self.output_leaves.remove(&output_id);
                self.ready.remove(key);
                // Dropping the task cancels whatever is left of it.
                drop(self.leaves.remove(key));
            }
        }
    }

    fn ready_effect(&self) -> EgressShardCommandEffect {
        if self.ready.is_empty() {
            EgressShardCommandEffect::Continue
        } else {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        }
    }
}

impl<C: MoqConnector> EgressShardBackend for MoqShardBackend<C> {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                if let ProtocolSpec::Moq { url } = spec.protocol.clone() {
                    self.add_leaf(spec, &url);
                }
            }
            EgressCommand::Remove(output_id) | EgressCommand::Abandon(output_id) => {
                self.remove_output(&output_id, CloseReason::Removed);
            }
            EgressCommand::FeedWake => {
                let ready = &mut self.ready;
                for (key, leaf) in self.leaves.iter() {
                    if leaf.common.schedule.wants_feed_wake {
                        ready.push(key);
                    }
                }
            }
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {
                let reason = if matches!(command, EgressCommand::Shutdown) {
                    CloseReason::ShardShutdown
                } else {
                    CloseReason::Removed
                };
                let keys: Vec<LeafKey> = self.output_leaves.drain().map(|(_, key)| key).collect();
                for key in keys {
                    self.remove_leaf(key, reason);
                }
            }
        }
        self.ready_effect()
    }

    /// Park inside the shard's Compio runtime: commands, leaf state changes
    /// and every connection task's I/O and timers are awaited together.
    /// Commands win ties.
    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: Duration,
    ) -> EgressShardIdleWake {
        let activity = self.activity.clone();
        self.runtime.block_on(async {
            let command = pin!(async {
                match commands.recv_async().await {
                    Ok(command) => EgressShardIdleWake::Command(command),
                    Err(_) => EgressShardIdleWake::Disconnected,
                }
            });
            let changed = pin!(async {
                activity.wait().await;
                EgressShardIdleWake::BackendActivity
            });
            let timeout = pin!(async {
                compio::time::sleep(max_wait).await;
                EgressShardIdleWake::Timeout
            });
            match select(command, select(changed, timeout)).await {
                Either::Left((wake, _)) => wake,
                Either::Right((Either::Left((wake, _)) | Either::Right((wake, _)), _)) => wake,
            }
        })
    }

    /// One `on_ready` = the queued state changes plus one leaf visit, then
    /// one non-blocking service of the runtime: QUIC I/O and timers run even
    /// when the ready queue never empties (a backlogged leaf would otherwise
    /// stall every leaf on the shard).
    fn on_ready(&mut self) -> EgressShardCommandEffect {
        self.collect_changed();
        self.visit_one();
        self.service_runtime();
        self.collect_changed();
        self.ready_effect()
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        self.collect_changed();
        self.ready_effect()
    }

    fn on_shutdown(&mut self) {
        self.ready.clear();
        for mut leaf in self.leaves.drain() {
            leaf.engine
                .close(&mut leaf.link, CloseReason::ShardShutdown);
        }
        self.output_leaves.clear();
        // Let the closes reach the wire before the runtime goes away.
        self.service_runtime();
    }

    fn resync_count(&self) -> u64 {
        self.resyncs
    }

    fn budget_exhaustion_count(&self) -> u64 {
        self.budget_exhaustions
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
