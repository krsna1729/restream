use crate::media::egress::command::EgressCommand;
use crate::media::egress::manager::{
    EgressManager, EgressManagerConfig, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::shard::{EgressShardGroup, EgressShardGroupError, EgressShardSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EgressFabricRuntimeError {
    ShardCountMismatch { expected: usize, actual: usize },
}

#[derive(Debug)]
pub(crate) struct EgressFabricRuntime {
    manager: EgressManager,
    group: EgressShardGroup,
}

impl EgressFabricRuntime {
    pub(crate) fn new(
        manager_config: EgressManagerConfig,
        group: EgressShardGroup,
    ) -> Result<Self, EgressFabricRuntimeError> {
        let expected = manager_config.shard_count().get() as usize;
        let actual = group.shard_count();
        if actual != expected {
            return Err(EgressFabricRuntimeError::ShardCountMismatch { expected, actual });
        }
        Ok(Self {
            manager: EgressManager::new(manager_config),
            group,
        })
    }

    pub(crate) fn dispatch(
        &mut self,
        command: EgressCommand,
    ) -> Result<ManagerCommandOutcome, EgressManagerDispatchError<EgressShardGroupError>> {
        self.manager.dispatch_to_group(command, &self.group)
    }

    #[cfg(test)]
    pub(crate) fn snapshots(&self) -> Vec<EgressShardSnapshot> {
        self.group.snapshots()
    }

    pub(crate) fn shutdown(self) -> Vec<EgressShardSnapshot> {
        self.group.shutdown_and_join()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use crate::media::egress::command::{
        EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec, ShardId,
    };
    use crate::media::egress::manager::{EgressManagerConfigError, ManagerCommandOutcome};
    use crate::media::egress::policy::LeafPolicy;
    use crate::media::egress::shard::{
        EgressShardBackend, EgressShardCommandEffect, EgressShardConfig,
    };

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

    fn manager_config(shards: u32) -> Result<EgressManagerConfig, EgressManagerConfigError> {
        EgressManagerConfig::new(shards, 16)
    }

    fn group(shards: u32, probes: &[Probe]) -> EgressShardGroup {
        let backends = probes
            .iter()
            .cloned()
            .map(|probe| ProbeBackend { probe })
            .collect::<Vec<_>>();
        EgressShardGroup::spawn(NonZeroU32::new(shards).unwrap(), shard_config(), backends).unwrap()
    }

    fn output_spec(id: &str) -> OutputSpec {
        OutputSpec {
            id: OutputId::new(id),
            generation: 1,
            feed: FeedId::new("feed-1"),
            protocol: ProtocolSpec::Sink,
            policy: LeafPolicy::default(),
        }
    }

    #[test]
    fn runtime_dispatches_commands_to_owned_shard_group() {
        let probe = Probe::default();
        let mut runtime = EgressFabricRuntime::new(
            manager_config(1).unwrap(),
            group(1, std::slice::from_ref(&probe)),
        )
        .unwrap();

        let outcome = runtime.dispatch(EgressCommand::Add(output_spec("out-1")));

        assert_eq!(
            outcome,
            Ok(ManagerCommandOutcome::Enqueued {
                shard_id: ShardId::new(0)
            })
        );
        probe.wait_for_commands(1);
        assert_eq!(probe.state().commands, vec!["add:out-1"]);
        let snapshots = runtime.shutdown();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(probe.state().shutdowns, 1);
    }

    #[test]
    fn runtime_rejects_group_with_wrong_shard_count() {
        let probes = vec![Probe::default(), Probe::default()];
        let result = EgressFabricRuntime::new(manager_config(1).unwrap(), group(2, &probes));

        assert!(matches!(
            result,
            Err(EgressFabricRuntimeError::ShardCountMismatch {
                expected: 1,
                actual: 2
            })
        ));
    }
}
