//! Recording catalog filenames and conversion sidecar state.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingConversionStatus {
    Converting,
    Ready,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingConversionState {
    pub status: RecordingConversionStatus,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) fn is_recording_source_filename(filename: &str) -> bool {
    filename.ends_with(".ts") && filename.to_ascii_lowercase().contains("recording")
}

pub(crate) fn build_mp4_path(ts_path: &Path) -> PathBuf {
    ts_path.with_extension("mp4")
}

pub(crate) fn build_conversion_state_path(ts_path: &Path) -> PathBuf {
    ts_path.with_extension("ts.conversion.json")
}

pub(super) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub(super) async fn write_conversion_state(
    ts_path: &Path,
    status: RecordingConversionStatus,
    error: Option<String>,
) {
    let state_path = build_conversion_state_path(ts_path);
    let state = RecordingConversionState {
        status,
        updated_at: now_rfc3339(),
        error,
    };
    match serde_json::to_vec(&state) {
        Ok(bytes) => {
            if let Err(write_error) = tokio::fs::write(&state_path, bytes).await {
                warn!(
                    state = %state_path.display(),
                    err = %write_error,
                    "failed to persist recording conversion state"
                );
            }
        }
        Err(serialize_error) => {
            warn!(
                state = %state_path.display(),
                err = %serialize_error,
                "failed to serialize recording conversion state"
            );
        }
    }
}

pub(crate) fn load_conversion_state(ts_path: &Path) -> Option<RecordingConversionState> {
    let state_path = build_conversion_state_path(ts_path);
    let bytes = std::fs::read(state_path).ok()?;
    serde_json::from_slice(&bytes).ok()
}
