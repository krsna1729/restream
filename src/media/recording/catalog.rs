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

#[cfg(test)]
mod tests {
    use super::*;

    // `is_recording_source_filename` has call sites in
    // `application::services::media_library_service` but, before this test,
    // no direct coverage anywhere in the tree.
    #[test]
    fn recording_source_filename_requires_both_ts_extension_and_recording_substring() {
        assert!(is_recording_source_filename("recording_20260629.ts"));
        assert!(is_recording_source_filename("some/dir/recording_x.ts"));
        assert!(!is_recording_source_filename("video.ts"));
        assert!(!is_recording_source_filename("recording_20260629.mp4"));
        assert!(!is_recording_source_filename(""));
        assert!(!is_recording_source_filename(".ts"));
    }

    #[test]
    fn recording_source_filename_substring_match_is_case_insensitive() {
        assert!(is_recording_source_filename("MyRECORDINGfile.ts"));
        assert!(is_recording_source_filename("RECORDING.ts"));
    }

    #[test]
    fn recording_source_filename_extension_match_is_case_sensitive() {
        // `.ends_with(".ts")` runs on the original-case filename while the
        // "recording" substring check lowercases first — an uppercase `.TS`
        // extension is therefore rejected even though it would pass the
        // substring check. Locks in this asymmetry rather than letting it
        // drift unnoticed.
        assert!(!is_recording_source_filename("recording_20260629.TS"));
    }

    #[test]
    fn recording_source_filename_rejects_sidecar_json_despite_ts_in_path() {
        assert!(!is_recording_source_filename(
            "recording_20260629.ts.conversion.json"
        ));
    }

    #[test]
    fn load_conversion_state_returns_none_for_missing_file() {
        let ts_path = std::env::temp_dir().join(format!(
            "catalog-missing-{}-recording.ts",
            rand::random::<u64>()
        ));
        assert!(load_conversion_state(&ts_path).is_none());
    }

    #[test]
    fn load_conversion_state_returns_none_for_malformed_json() {
        let ts_path = std::env::temp_dir().join(format!(
            "catalog-malformed-{}-recording.ts",
            rand::random::<u64>()
        ));
        let state_path = build_conversion_state_path(&ts_path);
        std::fs::write(&state_path, b"not json").expect("temp sidecar should write");

        assert!(load_conversion_state(&ts_path).is_none());

        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn write_then_load_conversion_state_round_trips_failed_status_and_error() {
        let ts_path = std::env::temp_dir().join(format!(
            "catalog-roundtrip-{}-recording.ts",
            rand::random::<u64>()
        ));

        write_conversion_state(
            &ts_path,
            RecordingConversionStatus::Failed,
            Some("ffmpeg exited with status 1".to_string()),
        )
        .await;

        let state =
            load_conversion_state(&ts_path).expect("just-written conversion state should load");
        assert_eq!(state.status, RecordingConversionStatus::Failed);
        assert_eq!(state.error.as_deref(), Some("ffmpeg exited with status 1"));

        let _ = std::fs::remove_file(build_conversion_state_path(&ts_path));
    }
}
