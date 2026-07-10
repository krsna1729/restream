//! Application-layer recording control that owns persisted recording settings
//! and translates them into engine-facing recording commands.

use crate::application::ports::{MetaLookupError, MetaStore, MetaStoreWriter};
use crate::application::reconcile::RecordingCommand;
use crate::domain::ids::RecordingId;
use crate::domain::state::RecordingPhase;
use crate::media::engine::MediaEngine;
use crate::media::recording::{RecordingMetadataEvent, RecordingMetadataReporter, RecordingStart};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const RECORDING_SETTINGS_META_KEY: &str = "recording_settings";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RecordingSettings {
    pub retain_source_ts: bool,
}

pub fn recording_enabled_meta_key(pipeline_id: &str) -> String {
    format!("recording_enabled:{pipeline_id}")
}

pub async fn load_recording_enabled(meta_store: &dyn MetaStore, pipeline_id: &str) -> bool {
    meta_store
        .get_meta(&recording_enabled_meta_key(pipeline_id))
        .await
        .ok()
        .flatten()
        .is_some_and(|value| value == "1")
}

pub async fn load_recording_enabled_map(
    meta_store: &dyn MetaStore,
    pipeline_ids: &[String],
) -> HashMap<String, bool> {
    let mut enabled = HashMap::with_capacity(pipeline_ids.len());
    for pipeline_id in pipeline_ids {
        enabled.insert(
            pipeline_id.clone(),
            load_recording_enabled(meta_store, pipeline_id).await,
        );
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

pub async fn spawn_recording_task(
    engine: Arc<MediaEngine>,
    pipeline_name: String,
    pipeline_id: String,
    input_source: Option<String>,
    media_dir: String,
    recording_settings: RecordingSettings,
    metadata: Option<RecordingMetadataReporter>,
) -> CancellationToken {
    let ring_buffer = engine.get_or_create_pipeline(&pipeline_id).await;
    let cancel_token = engine.register_recording(&pipeline_id).await;
    let cancel_token_for_task = cancel_token.clone();
    let engine_for_task = engine.clone();
    let pipeline_id_for_cleanup = pipeline_id.clone();
    let recording_plan = crate::planner::graph_plan::plan_recording_graph(
        &pipeline_id,
        &engine.config.backend_policy,
    );
    let stage_key = recording_plan.terminal_stage;
    let recording_id = format!("recording_{:016x}", rand::random::<u64>());

    tokio::spawn(async move {
        crate::media::recording::start_recording(
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

    cancel_token
}

pub fn spawn_recording_metadata_reporter(pool: SqlitePool) -> RecordingMetadataReporter {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(event) = receiver.recv().await {
            if let Err(error) = persist_recording_metadata_event(&pool, event).await {
                tracing::warn!(
                    err = %error,
                    "failed to persist recording metadata event"
                );
            }
        }
    });
    RecordingMetadataReporter::new(sender)
}

async fn persist_recording_metadata_event(
    pool: &SqlitePool,
    event: RecordingMetadataEvent,
) -> Result<(), sqlx::Error> {
    match event {
        RecordingMetadataEvent::Started {
            recording_id,
            pipeline_id,
            started_at,
            temp_path,
        } => {
            crate::db::create_recording(
                pool,
                &RecordingId::new(recording_id),
                &pipeline_id,
                &started_at,
                Some(&temp_path),
                None,
            )
            .await?;
        }
        RecordingMetadataEvent::Finalized {
            recording_id,
            ended_at,
            final_path,
        } => {
            crate::db::finalize_recording(
                pool,
                &RecordingId::new(recording_id),
                &ended_at,
                &final_path,
            )
            .await?;
        }
        RecordingMetadataEvent::Failed {
            recording_id,
            error,
        } => {
            crate::db::update_recording_status(
                pool,
                &RecordingId::new(recording_id),
                RecordingPhase::Failed,
                Some(&error),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn apply_recording_commands(
    engine: Arc<MediaEngine>,
    meta_store: &dyn MetaStore,
    media_dir: &str,
    commands: Vec<RecordingCommand>,
    metadata: Option<RecordingMetadataReporter>,
) {
    let needs_settings = commands
        .iter()
        .any(|command| matches!(command, RecordingCommand::Start { .. }));
    let recording_settings = if needs_settings {
        Some(load_recording_settings(meta_store).await)
    } else {
        None
    };

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
                    recording_settings.clone().unwrap_or_default(),
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
    use crate::domain::stage::StageKey;
    use crate::media::recording::RecordingMetadataEvent;
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
                Ok(self
                    .values
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(key)
                    .cloned())
            })
        }
    }

    impl MetaStoreWriter for FakeMetaStore {
        fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a> {
            Box::pin(async move {
                self.values
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key.to_string(), value.to_string());
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

        assert!(load_recording_enabled(&store, "pipeline-a").await);
        assert!(!load_recording_enabled(&store, "pipeline-b").await);
    }

    #[tokio::test]
    async fn load_recording_enabled_map_treats_lookup_errors_as_disabled() {
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::from([(
                "recording_enabled:pipeline-a".to_string(),
                "1".to_string(),
            )])),
            fail_keys: HashMap::from([(
                "recording_enabled:pipeline-b".to_string(),
                "db unavailable".to_string(),
            )]),
        };

        let enabled = load_recording_enabled_map(
            &store,
            &["pipeline-a".to_string(), "pipeline-b".to_string()],
        )
        .await;

        assert_eq!(enabled.get("pipeline-a"), Some(&true));
        assert_eq!(enabled.get("pipeline-b"), Some(&false));
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
        .await;

        assert!(engine.is_recording_active("pipeline-launch").await);
        let planned_key = StageKey::new(
            "pipeline-launch",
            crate::domain::stage::StageKind::recording(),
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
        let store = FakeMetaStore {
            values: Mutex::new(HashMap::from([(
                RECORDING_SETTINGS_META_KEY.to_string(),
                "{\"retainSourceTs\":true}".to_string(),
            )])),
            fail_keys: HashMap::new(),
        };
        let _existing = engine.register_recording("pipeline-stop").await;

        apply_recording_commands(
            engine.clone(),
            &store,
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

    #[tokio::test]
    async fn recording_metadata_reporter_persists_lifecycle_events() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_pipeline(&pool, "pipeline-meta", "Pipeline", "stream-key", None, None)
            .await
            .unwrap();

        let reporter = spawn_recording_metadata_reporter(pool.clone());
        reporter.report(RecordingMetadataEvent::Started {
            recording_id: "recording-meta".to_string(),
            pipeline_id: "pipeline-meta".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            temp_path: "/tmp/recording-meta.ts".to_string(),
        });
        reporter.report(RecordingMetadataEvent::Finalized {
            recording_id: "recording-meta".to_string(),
            ended_at: "2026-01-01T00:00:10Z".to_string(),
            final_path: "/tmp/recording-meta.mp4".to_string(),
        });

        let row = wait_for_recording_final_path(&pool, "recording-meta").await;
        assert_eq!(row.pipeline_id, "pipeline-meta");
        assert_eq!(row.status, RecordingPhase::Ready.as_str());
        assert_eq!(row.temp_path.as_deref(), Some("/tmp/recording-meta.ts"));
        assert_eq!(row.final_path.as_deref(), Some("/tmp/recording-meta.mp4"));
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
                    crate::media::stage_lifecycle::StageBackendKind::Recording
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("recording stage snapshot did not appear for planned key");
    }

    async fn wait_for_recording_final_path(
        pool: &SqlitePool,
        recording_id: &str,
    ) -> crate::db::RecordingRow {
        let id = RecordingId::new(recording_id);
        for _ in 0..50 {
            if let Some(row) = crate::db::get_recording(pool, &id).await.unwrap()
                && row.final_path.is_some()
            {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("recording metadata reporter did not persist final path");
    }
}
