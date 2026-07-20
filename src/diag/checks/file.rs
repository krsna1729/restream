use std::sync::Arc;
use std::time::Instant;

use crate::media::engine::MediaEngine;

use super::super::model::{DiagResult, FileDiagnosticsContext};

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.2} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub(in crate::diag) async fn check_file_source(
    idx: u32,
    file: Option<&FileDiagnosticsContext>,
) -> DiagResult {
    let start = Instant::now();
    let mut lines = vec![];
    let mut issues = vec![];

    if let Some(file) = file {
        lines.push(format!("Filename: {}", file.filename));
        lines.push(format!("Path: {}", file.path.display()));
        lines.push(format!("Exists: {}", yes_no(file.file_exists)));
        lines.push(format!("Loop enabled: {}", yes_no(file.loop_enabled)));
        lines.push(format!("Start time: {}", file.start_time));
        lines.push(format!(
            "Live optimized: {}",
            if file.live_optimized {
                format!("yes (target {}s GOP)", file.target_gop_seconds)
            } else {
                "no".to_string()
            }
        ));
        if let Some(size) = file.file_size_bytes {
            lines.push(format!("Size: {}", format_bytes(size)));
        }
        if let Some(modified_at) = &file.file_modified_at {
            lines.push(format!("Modified: {}", modified_at));
        }
        if !file.file_exists {
            issues.push("Configured source file is missing from the media directory.".to_string());
        }

        if let Some(analysis) = &file.analysis {
            lines.push(format!(
                "Container analysis: codec={}, fps={}, duration={}s",
                analysis.video_codec.as_deref().unwrap_or("unknown"),
                analysis
                    .fps
                    .map(|value| format!("{value:.2}"))
                    .unwrap_or_else(|| "unknown".to_string()),
                analysis
                    .duration_sec
                    .map(|value| format!("{value:.3}"))
                    .unwrap_or_else(|| "unknown".to_string())
            ));
            lines.push(format!("Keyframes found: {}", analysis.keyframe_count));
            if let Some(avg) = analysis.average_keyframe_interval_sec {
                lines.push(format!("Average source GOP: {:.2}s", avg));
            }
            if let Some(max) = analysis.max_keyframe_interval_sec {
                lines.push(format!("Max source GOP: {:.2}s", max));
            }
            if analysis.sparse_for_live {
                if file.live_optimized {
                    issues.push(format!(
                        "Source GOP is sparse (max {:.2}s), so playback depends on Live Optimized re-encoding toward a {}s GOP.",
                        analysis.max_keyframe_interval_sec.unwrap_or_default(),
                        file.target_gop_seconds
                    ));
                } else {
                    issues.push(format!(
                        "Source GOP is sparse (max {:.2}s) and Live Optimized is disabled. Preview and recording may stutter until the next source keyframe.",
                        analysis.max_keyframe_interval_sec.unwrap_or_default()
                    ));
                }
            }
        } else if let Some(error) = &file.analysis_error {
            lines.push("Container analysis: unavailable".to_string());
            issues.push(format!("Source file analysis failed: {error}"));
        } else if file.file_exists {
            lines.push("Container analysis: skipped".to_string());
        }
    } else {
        lines.push("No file-ingest metadata was found for the active pipeline.".to_string());
        issues.push("The pipeline is running in file-ingest mode, but its source-file configuration could not be loaded.".to_string());
    }

    DiagResult::ok(
        idx,
        "File Source",
        "Configured file-ingest source and live-stream suitability",
        "file_ingest config + media file analysis",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(in crate::diag) async fn check_file_ingest_runtime(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
    file: Option<&FileDiagnosticsContext>,
) -> DiagResult {
    let start = Instant::now();
    let mut lines = vec![];
    let mut issues = vec![];

    let ingest = engine.active_ingest_diag_snapshot(pipeline_id).await;
    if let Some(ingest) = &ingest {
        lines.push(format!("Protocol: {}", ingest.protocol));
        lines.push(format!("Ingest uptime: {:.1}s", ingest.uptime_secs));
        lines.push(format!("Bytes injected: {}", ingest.bytes_received));
        if ingest.uptime_secs >= 3.0 && ingest.bytes_received == 0 {
            issues.push(
                "File ingest has been active for several seconds but has not injected any bytes."
                    .to_string(),
            );
        }
    } else {
        lines.push("No active file ingest is registered in MediaEngine.".to_string());
        issues.push("File ingest runtime is missing from MediaEngine active state.".to_string());
    }

    if let Some(file) = file {
        lines.push(format!("Ingest id: {}", file.ingest_id));
        let dependency = engine
            .file_ingest_dependency_snapshot(&file.ingest_id)
            .await;
        lines.push(format!(
            "Marked active in registry: {}",
            yes_no(dependency.marked_active)
        ));
        lines.push(format!(
            "Subprocess registered: {}",
            yes_no(dependency.child_registered)
        ));
        if !dependency.marked_active {
            issues
                .push("File ingest is not marked active in the file-ingest registry.".to_string());
        }
        if !dependency.child_registered {
            issues.push(
                "No file-ingest subprocess is currently registered for this pipeline.".to_string(),
            );
        }
    } else {
        lines.push(
            "Ingest registry details are unavailable without a file-ingest record.".to_string(),
        );
    }

    DiagResult::ok(
        idx,
        "File Ingest Runtime",
        "Internal file-ingest registration and byte flow",
        "engine.file_ingest_dependency_snapshot()",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

pub(in crate::diag) async fn check_preview_recording_state(
    idx: u32,
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
) -> DiagResult {
    let start = Instant::now();
    let hls = engine.hls_dependency_snapshot(pipeline_id).await;
    let recording_active = engine.is_recording_active(pipeline_id).await;
    let mut lines = vec![];
    let mut issues = vec![];

    lines.push(format!("HLS store exists: {}", yes_no(hls.store_exists)));
    lines.push(format!("HLS segmenter active: {}", yes_no(hls.active)));
    lines.push(format!(
        "Persistent HLS consumers: {}",
        hls.persistent_consumers
    ));
    lines.push(format!("Segments in store: {}", hls.segments));
    lines.push(format!("Playlist bytes: {}", hls.playlist_bytes));
    lines.push(format!("Recording active: {}", yes_no(recording_active)));
    if let Some(age_ms) = hls.last_access_age_ms {
        lines.push(format!("Last HLS access: {}ms ago", age_ms));
    }

    if hls.active && hls.segments == 0 {
        issues.push("HLS preview is active but has not produced any segments yet.".to_string());
    }
    if hls.active && hls.playlist_bytes == 0 {
        issues.push("HLS preview is active but the playlist is currently empty.".to_string());
    }
    if recording_active && !hls.store_exists && !hls.active {
        lines.push("Recording can run without an active HLS preview session.".to_string());
    }

    DiagResult::ok(
        idx,
        "Preview & Recording",
        "Browser preview and recording readiness for file ingest",
        "engine.hls_dependency_snapshot() + engine.is_recording_active()",
        lines.join("\n"),
        start.elapsed().as_millis() as u64,
    )
    .with_issues(issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_zero_is_plain_bytes() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_just_below_kib_stays_in_bytes() {
        assert_eq!(format_bytes(1023), "1023 B");
    }

    #[test]
    fn format_bytes_exact_kib_boundary_switches_units() {
        assert_eq!(format_bytes(1024), "1.00 KiB");
    }

    #[test]
    fn format_bytes_just_below_mib_stays_in_kib() {
        assert_eq!(format_bytes(1024 * 1024 - 1), "1024.00 KiB");
    }

    #[test]
    fn format_bytes_exact_mib_boundary_switches_units() {
        assert_eq!(format_bytes(1024 * 1024), "1.00 MiB");
    }

    #[test]
    fn format_bytes_just_below_gib_stays_in_mib() {
        assert_eq!(format_bytes(1024 * 1024 * 1024 - 1), "1024.00 MiB");
    }

    #[test]
    fn format_bytes_exact_gib_boundary_switches_units() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GiB");
    }

    #[test]
    fn format_bytes_u64_max_does_not_panic_or_overflow() {
        let formatted = format_bytes(u64::MAX);
        assert!(formatted.ends_with(" GiB"), "got {formatted}");
    }

    #[test]
    fn yes_no_maps_bool_to_expected_strings() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
    }
}
