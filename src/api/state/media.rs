//! Media library operations: recordings, stored files, posters, and analysis.

use std::path::PathBuf;

use super::AppState;
use crate::application::services::media_library_service::{MediaDeleteError, MediaRenameError};

impl AppState {
    /// Lists the media files under the configured library directory.
    pub(crate) async fn media_files(
        &self,
    ) -> Vec<crate::application::services::media_library_service::MediaLibraryFile> {
        self.media_library_service
            .list_media_files(&self.media_dir)
            .await
    }

    /// Starts recording one pipeline into the media library.
    pub(crate) async fn start_recording(
        &self,
        pipeline_id: &str,
        pipeline_name: String,
        input_source: Option<String>,
    ) -> crate::application::services::ServiceResult<bool> {
        self.media_library_service
            .recording_start(
                &self.engine,
                pipeline_id,
                pipeline_name,
                input_source,
                &self.media_dir,
            )
            .await
    }

    /// Stops recording one pipeline.
    pub(crate) async fn stop_recording(
        &self,
        pipeline_id: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.media_library_service
            .recording_stop(&self.engine, pipeline_id)
            .await
    }

    /// Extracts one poster frame from a stored file.
    pub(crate) async fn media_poster(&self, path: PathBuf) -> Result<Vec<u8>, String> {
        self.media_library_service.media_poster(path).await
    }

    /// Analyzes one stored media file.
    pub(crate) async fn analyze_media(
        &self,
        path: PathBuf,
    ) -> Result<restream_engine::file_analysis::MediaFileAnalysis, String> {
        self.media_library_service.analyze_media_file(path).await
    }

    /// Deletes one stored media file.
    pub(crate) async fn delete_media(
        &self,
        filename: &str,
        canonical_path: &std::path::Path,
    ) -> Result<(), MediaDeleteError> {
        self.media_library_service
            .delete_media_file(filename, canonical_path)
            .await
    }

    /// Renames one stored media file and returns the updated ingest count.
    pub(crate) async fn rename_media(
        &self,
        filename: &str,
        new_name: &str,
        source_path: &std::path::Path,
        destination_path: &std::path::Path,
    ) -> Result<usize, MediaRenameError> {
        self.media_library_service
            .rename_media_file(filename, new_name, source_path, destination_path)
            .await
    }
}
