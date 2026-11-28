//! Application-layer recording control that owns persisted recording settings
//! and translates them into engine-facing recording commands.

use crate::application::housekeeping_keys::{
    RECORDING_ENABLED_META_PREFIX, RECORDING_SETTINGS_META_KEY,
};
use crate::application::reconcile::RecordingCommand;
use crate::db::DeadlineDb;
use restream_domain::domain::recording::RecordingSettings;
use restream_engine::engine::MediaEngine;
use restream_engine::recording::{RecordingMetadataReporter, RecordingStart};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub fn recording_enabled_meta_key(pipeline_id: &str) -> String {
    format!("{RECORDING_ENABLED_META_PREFIX}{pipeline_id}")
}

/// Whether recording is switched on for `pipeline_id`; `None` when the
/// switch could not be read, which is neither on nor off.
pub async fn load_recording_enabled(db: &DeadlineDb, pipeline_id: &str) -> Option<bool> {
    let key = recording_enabled_meta_key(pipeline_id);
    db.read(|pool| crate::db::get_meta(pool, &key))
        .await
        .ok()
        .map(|value| value.is_some_and(|value| value == "1"))
}

/// Each pipeline's recording switch. One whose switch cannot be read keeps
/// its current state: on while its recording runs, so a pool timeout never
/// shows a running recording as switched off.
pub async fn load_recording_enabled_map(
    db: &DeadlineDb,
    engine: &MediaEngine,
    pipeline_ids: &[String],
) -> HashMap<String, bool> {
    let mut enabled = HashMap::with_capacity(pipeline_ids.len());
    for pipeline_id in pipeline_ids {
        let on = match load_recording_enabled(db, pipeline_id).await {
            Some(on) => on,
            None => engine.is_recording_active(pipeline_id).await,
        };
        enabled.insert(pipeline_id.clone(), on);
    }
    enabled
}

pub async fn load_recording_settings(db: &DeadlineDb) -> RecordingSettings {
    db.read(|pool| crate::db::get_meta(pool, RECORDING_SETTINGS_META_KEY))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<RecordingSettings>(&raw).ok())
        .unwrap_or_default()
}

pub async fn save_recording_settings(
    db: &DeadlineDb,
    settings: &RecordingSettings,
) -> Result<(), String> {
    let raw = serde_json::to_string(settings).map_err(|error| error.to_string())?;
    db.write(move |pool| async move {
        crate::db::set_meta(&pool, RECORDING_SETTINGS_META_KEY, &raw).await
    })
    .await
    .map(|_| ())
    .map_err(|error| error.to_string())
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
    use crate::infrastructure::bootstrap::test_support::memory_pool;
    use restream_domain::domain::stage::StageKey;
    use std::path::PathBuf;
    use std::time::Duration;

    /// An in-memory database holding `values` in its meta table.
    async fn meta_db(values: &[(&str, &str)]) -> (DeadlineDb, sqlx::SqlitePool) {
        let pool = memory_pool().await;
        for (key, value) in values {
            crate::db::set_meta(&pool, key, value).await.unwrap();
        }
        (DeadlineDb::api(pool.clone()), pool)
    }

    /// Stores a value the meta read cannot decode as text, so reading this
    /// one key fails while the others still read.
    async fn make_unreadable(pool: &sqlx::SqlitePool, key: &str) {
        sqlx::query("INSERT INTO meta (key, value) VALUES (?, X'FF')")
            .bind(key)
            .execute(pool)
            .await
            .unwrap();
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
        let (db, _pool) = meta_db(&[("recording_enabled:pipeline-a", "1")]).await;

        assert!(load_recording_enabled(&db, "pipeline-a").await.unwrap());
        assert!(!load_recording_enabled(&db, "pipeline-b").await.unwrap());
    }

    /// A switch that cannot be read keeps the pipeline's current state: a
    /// running recording reads as on, an idle pipeline as off.
    #[tokio::test]
    async fn an_unreadable_recording_switch_keeps_the_current_state() {
        let (db, pool) = meta_db(&[("recording_enabled:pipeline-a", "1")]).await;
        make_unreadable(&pool, "recording_enabled:pipeline-b").await;
        make_unreadable(&pool, "recording_enabled:pipeline-c").await;
        let engine = MediaEngine::new();
        let _recording = engine.register_recording("pipeline-b").await;

        let enabled = load_recording_enabled_map(
            &db,
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
        let (db, _pool) = meta_db(&[]).await;

        assert_eq!(
            load_recording_settings(&db).await,
            RecordingSettings {
                retain_source_ts: false,
            }
        );
    }

    #[tokio::test]
    async fn load_recording_settings_falls_back_to_default_on_malformed_json() {
        let (db, _pool) = meta_db(&[(RECORDING_SETTINGS_META_KEY, "{not valid json")]).await;

        assert_eq!(
            load_recording_settings(&db).await,
            RecordingSettings::default()
        );
    }

    #[tokio::test]
    async fn save_recording_settings_serializes_to_meta_store() {
        let (db, pool) = meta_db(&[]).await;

        save_recording_settings(
            &db,
            &RecordingSettings {
                retain_source_ts: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            crate::db::get_meta(&pool, RECORDING_SETTINGS_META_KEY)
                .await
                .unwrap(),
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
        engine
            .stages
            .with_runtimes(|runtimes| {
                let runtime = runtimes
                    .get(&planned_key)
                    .expect("recording stage should be runtime-backed");
                assert!(
                    runtime.ring.is_none(),
                    "recording writer is a non-ring protocol stage"
                );
            })
            .await;
        assert!(
            !engine
                .stages
                .with_metrics(|metrics| metrics.contains_key(&planned_key))
                .await,
            "recording metrics should be owned by StageRuntime, not the side map"
        );
        assert!(
            !engine
                .stages
                .has_side_lifecycle_for_test(&planned_key)
                .await,
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
            if !engine
                .stages
                .with_runtimes(|runtimes| runtimes.contains_key(key))
                .await
            {
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
}
