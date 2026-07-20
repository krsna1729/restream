use std::path::PathBuf;

use serde::Serialize;

use crate::media::file_analysis::MediaFileAnalysis;

#[derive(Debug, Clone)]
pub struct FileDiagnosticsContext {
    pub ingest_id: String,
    pub filename: String,
    pub path: PathBuf,
    pub file_exists: bool,
    pub file_size_bytes: Option<u64>,
    pub file_modified_at: Option<String>,
    pub loop_enabled: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
    pub analysis: Option<MediaFileAnalysis>,
    pub analysis_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagResult {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub issues: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl DiagResult {
    pub(super) fn ok(
        index: u32,
        name: &str,
        description: &str,
        command: &str,
        stdout: String,
        duration_ms: u64,
    ) -> Self {
        Self {
            index,
            name: name.into(),
            description: description.into(),
            command: command.into(),
            exit_code: 0,
            stdout,
            stderr: String::new(),
            duration_ms,
            issues: vec![],
            help: None,
        }
    }

    pub(super) fn with_issues(mut self, issues: Vec<String>) -> Self {
        self.issues = issues;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub protocol: String,
    pub total_duration_ms: u64,
    pub checks: Vec<DiagResult>,
}
