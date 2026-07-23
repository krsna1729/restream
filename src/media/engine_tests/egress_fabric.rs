use super::*;
use std::num::NonZeroU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::media::egress::command::{
    EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId,
};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::manager::{EgressManagerConfig, ManagerCommandOutcome};
use crate::media::egress::policy::LeafPolicy;
use crate::media::egress::runtime::EgressFabricRuntime;
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardConfig, EgressShardGroup,
};
use crate::media::ts_chunk_ring::TsChunkRing;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct ProbeState {
    commands: Vec<String>,
    shutdowns: u64,
}

#[derive(Clone, Debug, Default)]
struct Probe {
    inner: Arc<(Mutex<ProbeState>, Condvar)>,
}

impl Probe {
    fn wait_for_commands(&self, target: usize) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.commands.len() < target
            })
            .unwrap();
        assert!(result.0.commands.len() >= target);
    }

    fn state(&self) -> ProbeState {
        let state = self.inner.0.lock().unwrap();
        ProbeState {
            commands: state.commands.clone(),
            shutdowns: state.shutdowns,
        }
    }
}

#[derive(Debug)]
struct ProbeBackend {
    probe: Probe,
}

impl EgressShardBackend for ProbeBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let label = match command {
            EgressCommand::Add(spec) => format!("add:{}", spec.id),
            EgressCommand::Update(spec) => format!("update:{}", spec.id),
            EgressCommand::Remove(output_id) => format!("remove:{output_id}"),
            EgressCommand::FeedWake => "feed-wake".to_string(),
            EgressCommand::DrainShard(shard_id) => format!("drain:{shard_id}"),
            EgressCommand::Shutdown => "shutdown".to_string(),
        };
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.commands.push(label);
        condvar.notify_all();
        EgressShardCommandEffect::Continue
    }

    fn on_shutdown(&mut self) {
        let (lock, condvar) = &*self.probe.inner;
        let mut state = lock.lock().unwrap();
        state.shutdowns = state.shutdowns.saturating_add(1);
        condvar.notify_all();
    }
}

fn shard_config() -> EgressShardConfig {
    EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(1)).unwrap()
}

fn runtime(probe: Probe) -> EgressFabricRuntime {
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(1).unwrap(),
        shard_config(),
        vec![ProbeBackend {
            probe: probe.clone(),
        }],
    )
    .unwrap();
    EgressFabricRuntime::new(EgressManagerConfig::new(1, 16).unwrap(), group).unwrap()
}

fn output_spec(id: &str, feed: &FeedId) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation: 1,
        feed: feed.clone(),
        protocol: ProtocolSpec::Sink,
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[tokio::test]
async fn srt_fabric_registry_dispatches_to_feed_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-srt");
    let probe = Probe::default();
    engine
        .insert_srt_fabric_runtime_for_test(feed_id.clone(), runtime(probe.clone()))
        .await;

    let outcome = engine
        .dispatch_srt_fabric_command(&feed_id, EgressCommand::Add(output_spec("out-1", &feed_id)))
        .await;

    assert_eq!(
        outcome,
        Ok(ManagerCommandOutcome::Enqueued {
            shard_id: ShardId::new(0)
        })
    );
    probe.wait_for_commands(1);
    assert_eq!(probe.state().commands, vec!["add:out-1"]);
}

#[tokio::test]
async fn srt_fabric_registry_retains_native_runtime_once_per_feed() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-srt");
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));

    let first = engine
        .retain_srt_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let second = engine
        .retain_srt_fabric_runtime(feed_id.clone(), &feed)
        .await;
    let snapshots = engine.srt_fabric_runtime_snapshots(&feed_id).await;

    assert_eq!(first, Ok(true));
    assert_eq!(second, Ok(false));
    assert_eq!(snapshots.map(|snapshots| snapshots.len()), Some(4));
    assert!(!engine.release_srt_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .srt_fabric_runtime_snapshots(&feed_id)
            .await
            .is_some()
    );
    assert!(engine.release_srt_fabric_runtime(&feed_id).await);
    assert!(
        engine
            .srt_fabric_runtime_snapshots(&feed_id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn srt_fabric_registry_shutdown_helper_removes_and_joins_retained_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-native-srt-helper");
    let ts_ring = TsChunkRing::new(8, CancellationToken::new());
    let feed = TsFeed::new(&ts_ring, Arc::new(FeedEpoch::new()));

    assert_eq!(
        engine
            .retain_srt_fabric_runtime(feed_id.clone(), &feed)
            .await,
        Ok(true)
    );
    assert_eq!(
        engine
            .shutdown_srt_fabric_runtime(&feed_id)
            .await
            .map(|snapshots| snapshots.len()),
        Some(4)
    );
}

#[tokio::test]
async fn srt_fabric_registry_shutdown_removes_and_joins_feed_runtime() {
    let engine = MediaEngine::new();
    let feed_id = FeedId::new("feed-engine-srt-shutdown");
    let probe = Probe::default();
    engine
        .insert_srt_fabric_runtime_for_test(feed_id.clone(), runtime(probe.clone()))
        .await;

    let snapshots = engine.shutdown_srt_fabric_runtime(&feed_id).await;
    let missing = engine
        .dispatch_srt_fabric_command(&feed_id, EgressCommand::Add(output_spec("out-1", &feed_id)))
        .await;

    assert_eq!(snapshots.map(|snapshots| snapshots.len()), Some(1));
    assert_eq!(probe.state().shutdowns, 1);
    assert!(missing.is_err());
}
