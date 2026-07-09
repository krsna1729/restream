use std::sync::Arc;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::application::ports::SqliteMetaStore;
use crate::application::recording::{
    load_recording_settings, recording_enabled_meta_key, spawn_recording_task,
};
use crate::db;
use crate::media::engine::MediaEngine;
use crate::types::Pipeline;

use super::error::{ApiError, ApiResult};
use super::pipeline_service::PipelineService;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRecordingMetadata {
    pub recording_id: String,
    pub pipeline_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub codec_summary: Option<String>,
    pub error: Option<String>,
}

pub struct MediaLibraryService {
    db: SqlitePool,
    pipeline_service: PipelineService,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRenamePlanError {
    ConvertedExists,
    ConversionStateExists,
}

impl MediaLibraryService {
    pub fn new(db: SqlitePool, pipeline_service: PipelineService) -> Self {
        Self {
            db,
            pipeline_service,
        }
    }

    pub async fn get_pipeline(&self, id: &str) -> ApiResult<Pipeline> {
        self.pipeline_service.get_by_id(id).await
    }

    pub async fn recording_start(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
        pipeline_name: String,
        input_source: Option<String>,
        media_dir: &str,
    ) -> ApiResult<bool> {
        let meta_key = recording_enabled_meta_key(pipeline_id);
        let _ = db::set_meta(&self.db, &meta_key, "1").await;

        let has_ingest = engine.ingests.active.read().await.contains_key(pipeline_id);
        if has_ingest && !engine.is_recording_active(pipeline_id).await {
            let recording_settings =
                load_recording_settings(&SqliteMetaStore::new(self.db.clone())).await;
            spawn_recording_task(
                engine.clone(),
                pipeline_name,
                pipeline_id.to_string(),
                input_source,
                media_dir.to_string(),
                recording_settings,
            )
            .await;
        }

        Ok(engine.is_recording_active(pipeline_id).await)
    }

    pub async fn recording_stop(
        &self,
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
    ) -> ApiResult<()> {
        let meta_key = recording_enabled_meta_key(pipeline_id);
        let _ = db::set_meta(&self.db, &meta_key, "0").await;
        engine.unregister_recording(pipeline_id).await;
        Ok(())
    }

    pub async fn recording_metadata_by_filename(
        &self,
        filenames: impl IntoIterator<Item = String>,
    ) -> ApiResult<HashMap<String, MediaRecordingMetadata>> {
        let requested = filenames.into_iter().collect::<HashSet<_>>();
        if requested.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = db::list_recordings(&self.db)
            .await
            .map_err(|e| ApiError::internal(format!("list recordings: {e}")))?;
        let mut metadata = HashMap::new();
        for row in rows {
            for path in [row.final_path.as_deref(), row.temp_path.as_deref()]
                .into_iter()
                .flatten()
            {
                let Some(name) = std::path::Path::new(path)
                    .file_name()
                    .map(|value| value.to_string_lossy().to_string())
                else {
                    continue;
                };
                if requested.contains(&name) && !metadata.contains_key(&name) {
                    metadata.insert(
                        name,
                        MediaRecordingMetadata {
                            recording_id: row.recording_id.clone(),
                            pipeline_id: row.pipeline_id.clone(),
                            started_at: row.started_at.clone(),
                            ended_at: row.ended_at.clone(),
                            status: row.status.clone(),
                            codec_summary: row.codec_summary.clone(),
                            error: row.error.clone(),
                        },
                    );
                }
            }
        }
        Ok(metadata)
    }

    pub fn delete_paths_for_media(&self, filename: &str, canonical_path: &Path) -> Vec<PathBuf> {
        let mut delete_paths = vec![canonical_path.to_path_buf()];
        if crate::media::recording::is_recording_source_filename(filename) {
            let converted_path = crate::media::recording::build_mp4_path(canonical_path);
            if converted_path.exists() {
                delete_paths.push(converted_path);
            }
            let state_path = crate::media::recording::build_conversion_state_path(canonical_path);
            if state_path.exists() {
                delete_paths.push(state_path);
            }
        }
        delete_paths
    }

    pub fn rename_pairs_for_media(
        &self,
        filename: &str,
        source_path: &Path,
        destination_path: &Path,
    ) -> Result<Vec<(PathBuf, PathBuf)>, MediaRenamePlanError> {
        let mut rename_pairs = vec![(source_path.to_path_buf(), destination_path.to_path_buf())];
        if crate::media::recording::is_recording_source_filename(filename) {
            let source_converted = crate::media::recording::build_mp4_path(source_path);
            let destination_converted = crate::media::recording::build_mp4_path(destination_path);
            if source_converted.exists() {
                if destination_converted.exists() {
                    return Err(MediaRenamePlanError::ConvertedExists);
                }
                rename_pairs.push((source_converted, destination_converted));
            }

            let source_state = crate::media::recording::build_conversion_state_path(source_path);
            let destination_state =
                crate::media::recording::build_conversion_state_path(destination_path);
            if source_state.exists() {
                if destination_state.exists() {
                    return Err(MediaRenamePlanError::ConversionStateExists);
                }
                rename_pairs.push((source_state, destination_state));
            }
        }
        Ok(rename_pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::RecordingId;

    async fn service_with_pipeline() -> MediaLibraryService {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();
        crate::db::create_recording(
            &pool,
            &RecordingId::from("rec-1"),
            "pipe-1",
            "2026-07-09T00:00:00Z",
            Some("/tmp/recording_1.ts"),
            Some("h264/aac"),
        )
        .await
        .unwrap();
        crate::db::finalize_recording(
            &pool,
            &RecordingId::from("rec-1"),
            "2026-07-09T00:01:00Z",
            "/media/finished.mp4",
        )
        .await
        .unwrap();

        MediaLibraryService::new(pool.clone(), PipelineService::new(pool))
    }

    #[tokio::test]
    async fn recording_metadata_by_filename_matches_final_and_temp_basenames() {
        let service = service_with_pipeline().await;
        let metadata = service
            .recording_metadata_by_filename(vec![
                "finished.mp4".to_string(),
                "recording_1.ts".to_string(),
                "other.mp4".to_string(),
            ])
            .await
            .unwrap();

        assert_eq!(metadata["finished.mp4"].recording_id, "rec-1");
        assert_eq!(metadata["recording_1.ts"].pipeline_id, "pipe-1");
        assert!(!metadata.contains_key("other.mp4"));
    }

    #[tokio::test]
    async fn delete_paths_for_media_includes_recording_companions() {
        let service = service_with_pipeline().await;
        let temp_dir = tempfile_dir("media-delete-plan");
        let source = temp_dir.join("recording_20260709T010203_demo.ts");
        let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
        let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&converted, b"converted").unwrap();
        std::fs::write(&state, b"state").unwrap();

        let paths = service.delete_paths_for_media(
            "recording_20260709T010203_demo.ts",
            &std::fs::canonicalize(&source).unwrap(),
        );

        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|path| path.ends_with(&converted)));
        assert!(paths.iter().any(|path| path.ends_with(&state)));
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn rename_pairs_for_media_includes_recording_companions() {
        let service = service_with_pipeline().await;
        let temp_dir = tempfile_dir("media-rename-plan");
        let source = temp_dir.join("recording_20260709T010203_demo.ts");
        let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
        let state = temp_dir.join("recording_20260709T010203_demo.ts.conversion.json");
        let destination = temp_dir.join("recording_20260709T010203_renamed.ts");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&converted, b"converted").unwrap();
        std::fs::write(&state, b"state").unwrap();

        let pairs = service
            .rename_pairs_for_media("recording_20260709T010203_demo.ts", &source, &destination)
            .unwrap();

        assert_eq!(pairs.len(), 3);
        assert!(
            pairs
                .iter()
                .any(|(_, to)| { to.ends_with("recording_20260709T010203_renamed.mp4") })
        );
        assert!(pairs.iter().any(|(_, to)| {
            to.ends_with("recording_20260709T010203_renamed.ts.conversion.json")
        }));
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn rename_pairs_for_media_reports_companion_conflict() {
        let service = service_with_pipeline().await;
        let temp_dir = tempfile_dir("media-rename-conflict");
        let source = temp_dir.join("recording_20260709T010203_demo.ts");
        let converted = temp_dir.join("recording_20260709T010203_demo.mp4");
        let destination = temp_dir.join("recording_20260709T010203_renamed.ts");
        let destination_converted = temp_dir.join("recording_20260709T010203_renamed.mp4");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&converted, b"converted").unwrap();
        std::fs::write(&destination_converted, b"existing").unwrap();

        let err = service
            .rename_pairs_for_media("recording_20260709T010203_demo.ts", &source, &destination)
            .unwrap_err();

        assert_eq!(err, MediaRenamePlanError::ConvertedExists);
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "restream-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
