use crate::media::egress::command::ShardId;
use crate::media::egress::manager::{
    EgressManager, EgressManagerDispatchError, ManagerCommandOutcome,
};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardConfig, EgressShardGroup, EgressShardGroupError,
};

#[derive(Debug, Clone, Copy)]
pub struct EgressSupervisor {
    shard_config: EgressShardConfig,
}

impl EgressSupervisor {
    pub fn new(shard_config: EgressShardConfig) -> Self {
        Self { shard_config }
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
        let replaced = group.replace_panicked(self.shard_config, backend_for);
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
mod tests {
    use super::*;
    use crate::media::egress::command::{
        EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec,
    };
    use crate::media::egress::manager::{EgressManager, EgressManagerConfig};
    use crate::media::egress::policy::LeafPolicy;
    use std::num::NonZeroU32;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Debug, Clone, Default)]
    struct Probe {
        inner: Arc<(Mutex<Vec<String>>, Condvar)>,
    }

    impl Probe {
        fn wait_for_commands(&self, target: usize) {
            let (lock, condvar) = &*self.inner;
            let commands = lock.lock().unwrap();
            let result = condvar
                .wait_timeout_while(commands, Duration::from_secs(2), |commands| {
                    commands.len() < target
                })
                .unwrap();
            assert!(result.0.len() >= target);
        }

        fn commands(&self) -> Vec<String> {
            self.inner.0.lock().unwrap().clone()
        }
    }

    #[derive(Debug)]
    enum TestBackend {
        Panic,
        Probe(Probe),
    }

    impl EgressShardBackend for TestBackend {
        fn on_command(
            &mut self,
            command: EgressCommand,
        ) -> crate::media::egress::shard::EgressShardCommandEffect {
            match self {
                Self::Panic => panic!("scripted shard panic"),
                Self::Probe(probe) => {
                    let (lock, condvar) = &*probe.inner;
                    let mut commands = lock.lock().unwrap();
                    commands.push(command_label(&command));
                    condvar.notify_all();
                    crate::media::egress::shard::EgressShardCommandEffect::Continue
                }
            }
        }
    }

    #[test]
    fn supervisor_replaces_panicked_shard_and_replays_only_its_outputs() {
        let mut manager = EgressManager::new(EgressManagerConfig::new(2, 16).unwrap());
        let survivor = Probe::default();
        let replacement = Probe::default();
        let panicked_output = spec_for_shard(&manager, ShardId::new(0));
        let survivor_output = spec_for_shard(&manager, ShardId::new(1));
        let panicked_output_id = panicked_output.id.clone();
        let survivor_output_id = survivor_output.id.clone();
        let mut group = EgressShardGroup::spawn(
            NonZeroU32::new(2).unwrap(),
            config(),
            vec![TestBackend::Panic, TestBackend::Probe(survivor.clone())],
        )
        .unwrap();

        assert!(matches!(
            manager.dispatch_to_group(EgressCommand::Add(panicked_output), &group),
            Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(0)
        ));
        assert!(matches!(
            manager.dispatch_to_group(EgressCommand::Add(survivor_output), &group),
            Ok(ManagerCommandOutcome::Enqueued { shard_id }) if shard_id == ShardId::new(1)
        ));
        survivor.wait_for_commands(1);
        wait_for_panicked(&group, ShardId::new(0));

        let recovery = EgressSupervisor::new(config())
            .recover_panicked_shards(&mut manager, &mut group, |_| {
                TestBackend::Probe(replacement.clone())
            })
            .unwrap();

        assert_eq!(
            recovery,
            EgressSupervisorRecovery {
                recoveries: vec![EgressShardRecovery::Replayed {
                    shard_id: ShardId::new(0),
                    output_count: 1,
                }],
            }
        );
        replacement.wait_for_commands(1);
        let snapshots = group.shutdown_and_join();

        assert_eq!(
            replacement.commands(),
            vec![format!("add:{panicked_output_id}")]
        );
        assert_eq!(
            survivor.commands(),
            vec![format!("add:{survivor_output_id}")]
        );
        assert!(
            snapshots
                .iter()
                .all(|snapshot| snapshot.stopped && !snapshot.panicked)
        );
    }

    fn config() -> EgressShardConfig {
        EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(10)).unwrap()
    }

    fn output_spec(id: &str) -> OutputSpec {
        OutputSpec {
            id: OutputId::new(id),
            generation: 1,
            feed: FeedId::new("feed-1"),
            protocol: ProtocolSpec::Rtmp {
                url: "rtmp://localhost/live".into(),
                tls: false,
            },
            policy: LeafPolicy::default(),
        }
    }

    fn spec_for_shard(manager: &EgressManager, target: ShardId) -> OutputSpec {
        for index in 0..1_000 {
            let candidate = output_spec(&format!("out-target-{index}"));
            if manager.assign_spec(&candidate) == target {
                return candidate;
            }
        }
        panic!("test fixture could not find output for {target}");
    }

    fn wait_for_panicked(group: &EgressShardGroup, shard_id: ShardId) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if group
                .snapshots()
                .iter()
                .any(|snapshot| snapshot.shard_id == shard_id && snapshot.panicked)
            {
                return;
            }
            std::thread::yield_now();
        }
        panic!("timed out waiting for {shard_id:?} to report panic");
    }

    fn command_label(command: &EgressCommand) -> String {
        match command {
            EgressCommand::Add(spec) => format!("add:{}", spec.id.as_str()),
            EgressCommand::Update(spec) => format!("update:{}", spec.id.as_str()),
            EgressCommand::Remove(id) => format!("remove:{}", id.as_str()),
            EgressCommand::DrainShard(shard_id) => format!("drain:{}", shard_id.index()),
            EgressCommand::Shutdown => "shutdown".to_string(),
        }
    }
}
