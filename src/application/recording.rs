//! Application-layer recording control that owns persisted recording settings
//! and translates them into engine-facing recording commands.

use crate::application::ports::{MetaLookupError, MetaStore, MetaStoreWriter};
use crate::application::reconcile::RecordingCommand;
use restream_domain::domain::recording::RecordingSettings;
use restream_engine::engine::MediaEngine;
use restream_engine::recording::{RecordingMetadataReporter, RecordingStart};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const RECORDING_SETTINGS_META_KEY: &str = "recording_settings";
pub const RECORDING_ENABLED_META_PREFIX: &str = "recording_enabled:";

pub fn recording_enabled_meta_key(pipeline_id: &str) -> String {
    format!("{RECORDING_ENABLED_META_PREFIX}{pipeline_id}")
}

/// Whether recording is switched on for `pipeline_id`; an error when the
/// switch could not be read, which is neither on nor off.
pub async fn load_recording_enabled(
    meta_store: &dyn MetaStore,
    pipeline_id: &str,
) -> Result<bool, MetaLookupError> {
    Ok(meta_store
        .get_meta(&recording_enabled_meta_key(pipeline_id))
        .await?
        .is_some_and(|value| value == "1"))
}

/// Each pipeline's recording switch. One whose switch cannot be read keeps
/// its current state: on while its recording runs, so a pool timeout never
/// shows a running recording as switched off.
pub async fn load_recording_enabled_map(
    meta_store: &dyn MetaStore,
    engine: &MediaEngine,
    pipeline_ids: &[String],
) -> HashMap<String, bool> {
    let mut enabled = HashMap::with_capacity(pipeline_ids.len());
    for pipeline_id in pipeline_ids {
        let on = match load_recording_enabled(meta_store, pipeline_id).await {
            Ok(on) => on,
            Err(_) => engine.is_recording_active(pipeline_id).await,
        };
        enabled.insert(pipeline_id.clone(), on);
    }
    enabled
}

pub async fn load_recording_settings(meta_store: &dyn MetaStore) -> RecordingSettings {
    meta_store
        .get_meta(RECORDING_SETTINGS_META_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<RecordingSettings>(&raw).ok())
        .unwrap_or_default()
}

pub async fn save_recording_settings(
    meta_store: &dyn MetaStoreWriter,
    settings: &RecordingSettings,
) -> Result<(), MetaLookupError> {
    let raw =
        serde_json::to_string(settings).map_err(|error| MetaLookupError::new(error.to_string()))?;
    meta_store
        .set_meta(RECORDING_SETTINGS_META_KEY, &raw)
        .await
        .map(|_| ())
}

/// Spawns a recorder for `pipeline_id`, or returns `None` if a recording is
/// already active for it.
///
/// `register_recording` is the atomic serialization point: only one of two
/// racing callers for the same pipeline can receive `Some`, so at most one
/// recorder task is ever spawned per pipeline at a time. This prevents both
/// an orphaned cancellation token (leaking the loser's writer thread/file
/// handle) and a filename collision between two recorders racing
/// `std::fs::File::create` on the same output path.
pub async fn spawn_recording_task(
    engine: Arc<MediaEngine>,
    pipeline_name: String,
    pipeline_id: String,
    input_source: Option<String>,
    media_dir: String,
    recording_settings: RecordingSettings,
    metadata: Option<RecordingMetadataReporter>,
) -> Option<CancellationToken> {
    let ring_buffer = engine.get_or_create_pipeline(&pipeline_id).await;
    let cancel_token = engine.register_recording(&pipeline_id).await?;
    let cancel_token_for_task = cancel_token.clone();
    let engine_for_task = engine.clone();
    let pipeline_id_for_cleanup = pipeline_id.clone();
    let recording_plan = restream_domain::planner::plan_recording_graph(&pipeline_id);
    let stage_key = recording_plan.terminal_stage;
    let recording_id = format!("recording_{:016x}", rand::random::<u64>());

    tokio::spawn(async move {
        restream_engine::recording::start_recording(
            RecordingStart {
                recording_id,
                pipeline_name,
                pipeline_id: pipeline_id.clone(),
                input_source,
                media_dir,
                settings: recording_settings,
                stage_key,
                metadata,
            },
            ring_buffer,
            engine_for_task.clone(),
            cancel_token_for_task,
        )
        .await;
        engine_for_task
            .unregister_recording(&pipeline_id_for_cleanup)
            .await;
    });

    Some(cancel_token)
}

pub async fn apply_recording_commands(
    engine: Arc<MediaEngine>,
    recording_settings: &RecordingSettings,
    media_dir: &str,
    commands: Vec<RecordingCommand>,
    metadata: Option<RecordingMetadataReporter>,
) {
    for command in commands {
        match command {
            RecordingCommand::Start {
                pipeline_name,
                pipeline_id,
                input_source,
            } => {
                spawn_recording_task(
                    engine.clone(),
                    pipeline_name,
                    pipeline_id,
                    input_source,
                    media_dir.to_string(),
                    recording_settings.clone(),
                    metadata.clone(),
                )
                .await;
            }
            RecordingCommand::Stop { pipeline_id } => {
                engine.unregister_recording(&pipeline_id).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{MetaLookupFuture, MetaWriteFuture};
    use restream_domain::domain::stage::StageKey;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    struct FakeMetaStore {
        values: Mutex<HashMap<String, String>>,
        fail_keys: HashMap<String, String>,
    }

    impl MetaStore for FakeMetaStore {
        fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.fail_keys.get(key) {
                    return Err(MetaLookupError::new(message.clone()));
                }
                Ok(restream_sync::lock::lock(&self.values).get(key).cloned())
            })
        }
    }

    impl MetaStoreWriter for FakeMetaStore {
        fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a> {
            Box::pin(async move {
                restream_sync::lock::lock(&self.values).insert(key.to_string(), value.to_string());
                Ok(value.to_string())
            })
        }
    }

    #[test]
    fn recording_enabled_meta_key_prefixes_pipeline_id() {
        assert_eq!(
            recording_enabled_meta_key("pipeline-a"),
            "recording_enabled:pipeline-a"
        );
    }

    #[tokio::test]
    async fn load_recording_enabled_reads_truthy_meta_value() {
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::from([(
                "recording_enabled:pipeline-a".to_string(),
                "1".to_string(),
            )])),
            fail_keys: HashMap::new(),
        };

        assert!(load_recording_enabled(&store, "pipeline-a").await.unwrap());
        assert!(!load_recording_enabled(&store, "pipeline-b").await.unwrap());
    }

    /// A switch that cannot be read keeps the pipeline's current state: a
    /// running recording reads as on, an idle pipeline as off.
    #[tokio::test]
    async fn an_unreadable_recording_switch_keeps_the_current_state() {
        let unreadable = |pipeline: &str| {
            (
                format!("recording_enabled:{pipeline}"),
                "db unavailable".to_string(),
            )
        };
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::from([(
                "recording_enabled:pipeline-a".to_string(),
                "1".to_string(),
            )])),
            fail_keys: HashMap::from([unreadable("pipeline-b"), unreadable("pipeline-c")]),
        };
        let engine = MediaEngine::new();
        let _recording = engine.register_recording("pipeline-b").await;

        let enabled = load_recording_enabled_map(
            &store,
            &engine,
            &[
                "pipeline-a".to_string(),
                "pipeline-b".to_string(),
                "pipeline-c".to_string(),
            ],
        )
        .await;

        assert_eq!(enabled.get("pipeline-a"), Some(&true));
        assert_eq!(
            enabled.get("pipeline-b"),
            Some(&true),
            "a running recording must not read as switched off"
        );
        assert_eq!(enabled.get("pipeline-c"), Some(&false));
    }

    #[tokio::test]
    async fn load_recording_settings_defaults_when_missing() {
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::new()),
            fail_keys: HashMap::new(),
        };

        assert_eq!(
            load_recording_settings(&store).await,
            RecordingSettings {
                retain_source_ts: false,
            }
        );
    }

    #[tokio::test]
    async fn load_recording_settings_falls_back_to_default_on_malformed_json() {
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::from([(
                RECORDING_SETTINGS_META_KEY.to_string(),
                "{not valid json".to_string(),
            )])),
            fail_keys: HashMap::new(),
        };

        assert_eq!(
            load_recording_settings(&store).await,
            RecordingSettings::default()
        );
    }

    #[tokio::test]
    async fn save_recording_settings_serializes_to_meta_store() {
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::new()),
            fail_keys: HashMap::new(),
        };

        save_recording_settings(
            &store,
            &RecordingSettings {
                retain_source_ts: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            store
                .values
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(RECORDING_SETTINGS_META_KEY)
                .cloned(),
            Some("{\"retainSourceTs\":true}".to_string())
        );
    }

    #[tokio::test]
    async fn spawn_recording_task_registers_and_cleans_up_recording() {
        let engine = Arc::new(MediaEngine::new());
        let media_dir = unique_test_media_dir("recording-launch");

        let cancel_token = spawn_recording_task(
            engine.clone(),
            "Launch Test".to_string(),
            "pipeline-launch".to_string(),
            None,
            media_dir.display().to_string(),
            RecordingSettings::default(),
            None,
        )
        .await
        .expect("first registration for a pipeline must succeed");

        assert!(engine.is_recording_active("pipeline-launch").await);
        let planned_key = StageKey::new(
            "pipeline-launch",
            restream_domain::domain::stage::StageKind::recording(),
        );
        wait_for_recording_stage_snapshot(&engine, &planned_key).await;
        let runtime = engine
            .stages
            .runtimes
            .read()
            .await
            .get(&planned_key)
            .cloned()
            .expect("recording stage should be runtime-backed");
        assert!(
            runtime.ring.is_none(),
            "recording writer is a non-ring protocol stage"
        );
        assert!(
            !engine
                .stages
                .metrics
                .read()
                .await
                .contains_key(&planned_key),
            "recording metrics should be owned by StageRuntime, not the side map"
        );
        assert!(
            !engine
                .stages
                .lifecycles
                .read()
                .await
                .contains_key(&planned_key),
            "recording lifecycle should be owned by StageRuntime, not the side map"
        );

        cancel_token.cancel();
        wait_for_recording_shutdown(&engine, "pipeline-launch").await;
        assert!(!engine.is_recording_active("pipeline-launch").await);
        wait_for_recording_runtime_removed(&engine, &planned_key).await;

        let _ = std::fs::remove_dir_all(media_dir);
    }

    #[tokio::test]
    async fn apply_recording_commands_starts_and_stops_recordings() {
        let engine = Arc::new(MediaEngine::new());
        let media_dir = unique_test_media_dir("recording-commands");
        let _existing = engine.register_recording("pipeline-stop").await;

        apply_recording_commands(
            engine.clone(),
            &RecordingSettings {
                retain_source_ts: true,
            },
            media_dir.to_str().unwrap_or_default(),
            vec![
                RecordingCommand::Start {
                    pipeline_name: "Start Me".to_string(),
                    pipeline_id: "pipeline-start".to_string(),
                    input_source: None,
                },
                RecordingCommand::Stop {
                    pipeline_id: "pipeline-stop".to_string(),
                },
            ],
            None,
        )
        .await;

        assert!(engine.is_recording_active("pipeline-start").await);
        assert!(!engine.is_recording_active("pipeline-stop").await);

        engine.unregister_recording("pipeline-start").await;
        wait_for_recording_shutdown(&engine, "pipeline-start").await;
        let _ = std::fs::remove_dir_all(media_dir);
    }

    fn unique_test_media_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&path).expect("test media dir should be created");
        path
    }

    async fn wait_for_recording_shutdown(engine: &Arc<MediaEngine>, pipeline_id: &str) {
        for _ in 0..50 {
            if !engine.is_recording_active(pipeline_id).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("recording task did not shut down in time");
    }

    async fn wait_for_recording_runtime_removed(engine: &Arc<MediaEngine>, key: &StageKey) {
        for _ in 0..50 {
            if !engine.stages.runtimes.read().await.contains_key(key) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("recording runtime was not removed on shutdown");
    }

    async fn wait_for_recording_stage_snapshot(engine: &Arc<MediaEngine>, key: &StageKey) {
        for _ in 0..50 {
            if let Some(snapshot) = engine.stage_runtime_snapshot(key).await {
                assert_eq!(snapshot.key, *key);
                assert_eq!(
                    snapshot.backend,
                    restream_engine::stage_lifecycle::StageBackendKind::Recording
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("recording stage snapshot did not appear for planned key");
    }

    #[tokio::test]
    async fn recording_settings_default_to_deleting_source_ts() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");
        crate::db::setup_database_schema(&pool)
            .await
            .expect("schema should initialize");
        let meta_store = crate::infrastructure::sqlite_ports::SqliteMetaStore::new(pool.clone());

        assert_eq!(
            crate::application::recording::load_recording_settings(&meta_store).await,
            RecordingSettings {
                retain_source_ts: false
            }
        );
    }
}
