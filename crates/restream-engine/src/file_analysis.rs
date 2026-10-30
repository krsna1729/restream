//! Offline media-file inspection helpers used to validate ingest suitability
//! and expose operator diagnostics for stored media assets. The external
//! `ffprobe` reads the file (its JSON and CSV output are a stable
//! interface); nothing here decodes or demuxes in the engine.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// A probe that takes longer than this is abandoned (a long file's packet
/// list reads in well under it).
const PROBE_WITHIN: Duration = Duration::from_secs(60);

pub const LIVE_GOP_WARNING_THRESHOLD_SECS: f64 = 2.0;
pub const DEFAULT_LIVE_GOP_TARGET_SECONDS: u32 = 2;
const LIVE_GOP_WARNING_TOLERANCE_SECS: f64 = 0.05;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFileAnalysis {
    pub video_codec: Option<String>,
    pub fps: Option<f64>,
    pub duration_sec: Option<f64>,
    pub keyframe_count: usize,
    pub average_keyframe_interval_sec: Option<f64>,
    pub max_keyframe_interval_sec: Option<f64>,
    pub sparse_for_live: bool,
    pub live_gop_target_seconds: u32,
}

fn round_metric(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn is_sparse_gop_interval(max_interval_sec: f64) -> bool {
    max_interval_sec > LIVE_GOP_WARNING_THRESHOLD_SECS + LIVE_GOP_WARNING_TOLERANCE_SECS
}

/// Reduces a sequence of keyframe timestamps (seconds) into (average
/// interval, max interval, sparse-for-live). Fewer than two keyframes yields
/// no interval data. Non-monotonic timestamps (a corrupt or reordered
/// stream) clamp negative intervals to zero rather than skewing the average
/// or max downward with a negative contribution.
fn gop_stats_from_keyframe_times(keyframe_times: &[f64]) -> (Option<f64>, Option<f64>, bool) {
    if keyframe_times.len() < 2 {
        return (None, None, false);
    }
    let intervals: Vec<f64> = keyframe_times
        .windows(2)
        .map(|window| (window[1] - window[0]).max(0.0))
        .collect();
    let avg = intervals.iter().sum::<f64>() / intervals.len() as f64;
    let max = intervals.iter().copied().fold(0.0f64, f64::max);
    (
        Some(round_metric(avg)),
        Some(round_metric(max)),
        is_sparse_gop_interval(max),
    )
}

/// `ffprobe`'s view of the first video stream and the container.
#[derive(Debug, Default, Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Default, Deserialize)]
struct ProbeStream {
    codec_name: Option<String>,
    avg_frame_rate: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

/// "30000/1001" as a number; `None` for "0/0" or anything unreadable.
fn frame_rate(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let (num, den) = (num.parse::<f64>().ok()?, den.parse::<f64>().ok()?);
    (num > 0.0 && den > 0.0).then(|| round_metric(num / den))
}

/// Keyframe times from `ffprobe`'s packet CSV (`dts_time,pts_time,flags`
/// per line): the decode time of each packet flagged `K`, else its
/// presentation time.
fn keyframe_times(csv: &str) -> Vec<f64> {
    csv.lines()
        .filter_map(|line| {
            let mut fields = line.trim().split(',');
            let dts = fields.next()?;
            let pts = fields.next()?;
            let flags = fields.next()?;
            if !flags.starts_with('K') {
                return None;
            }
            dts.parse().ok().or_else(|| pts.parse().ok())
        })
        .collect()
}

fn analysis_from(probe: Probe, keyframe_times: &[f64]) -> MediaFileAnalysis {
    let stream = probe.streams.into_iter().next();
    let (average_keyframe_interval_sec, max_keyframe_interval_sec, sparse_for_live) =
        gop_stats_from_keyframe_times(keyframe_times);
    MediaFileAnalysis {
        video_codec: stream.as_ref().and_then(|stream| stream.codec_name.clone()),
        fps: stream
            .as_ref()
            .and_then(|stream| stream.avg_frame_rate.as_deref())
            .and_then(frame_rate),
        duration_sec: probe
            .format
            .and_then(|format| format.duration)
            .and_then(|duration| duration.parse::<f64>().ok())
            .filter(|duration| *duration > 0.0)
            .map(round_metric),
        keyframe_count: keyframe_times.len(),
        average_keyframe_interval_sec,
        max_keyframe_interval_sec,
        sparse_for_live,
        live_gop_target_seconds: DEFAULT_LIVE_GOP_TARGET_SECONDS,
    }
}

pub async fn analyze_media_file(path: &Path) -> Result<MediaFileAnalysis, String> {
    let ffprobe = crate::ffmpeg_binary::ffprobe_bin_path();
    let file = path.display().to_string();
    let header = crate::ffmpeg_binary::run_tool(
        ffprobe,
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,avg_frame_rate:format=duration",
            "-of",
            "json",
            &file,
        ]
        .map(String::from),
        PROBE_WITHIN,
    )
    .await
    .map_err(|error| format!("Failed to open media file: {error}"))?;
    let probe: Probe = serde_json::from_slice(&header)
        .map_err(|error| format!("Failed to open media file: unreadable probe ({error})"))?;
    if probe.streams.is_empty() {
        return Ok(analysis_from(probe, &[]));
    }
    let packets = crate::ffmpeg_binary::run_tool(
        ffprobe,
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=dts_time,pts_time,flags",
            "-of",
            "csv=p=0",
            &file,
        ]
        .map(String::from),
        PROBE_WITHIN,
    )
    .await
    .map_err(|error| format!("Failed to read media packets: {error}"))?;
    Ok(analysis_from(
        probe,
        &keyframe_times(&String::from_utf8_lossy(&packets)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canonical_fixture_reports_2_second_gop() {
        let fixture =
            crate::test_fixtures::canonical_h264_ts_fixture().expect("fixture should exist");
        let analysis = analyze_media_file(&fixture)
            .await
            .expect("analysis should succeed");

        assert_eq!(analysis.video_codec.as_deref(), Some("h264"));
        assert_eq!(analysis.keyframe_count, 4);
        assert_eq!(analysis.average_keyframe_interval_sec, Some(2.0));
        assert_eq!(analysis.max_keyframe_interval_sec, Some(2.0));
        assert!(!analysis.sparse_for_live);
    }

    #[tokio::test]
    async fn sparse_fixture_reports_sparse_gop() {
        let fixture = crate::test_fixtures::sparse_gop_mp4_fixture().expect("fixture should exist");
        let analysis = analyze_media_file(&fixture)
            .await
            .expect("analysis should succeed");

        assert_eq!(analysis.video_codec.as_deref(), Some("h264"));
        assert_eq!(analysis.keyframe_count, 3);
        assert_eq!(analysis.average_keyframe_interval_sec, Some(5.0));
        assert_eq!(analysis.max_keyframe_interval_sec, Some(5.0));
        assert!(analysis.sparse_for_live);
    }

    #[test]
    fn sparse_gop_threshold_uses_small_tolerance() {
        assert!(!is_sparse_gop_interval(2.0));
        assert!(!is_sparse_gop_interval(2.03));
        assert!(is_sparse_gop_interval(2.25));
        assert!(is_sparse_gop_interval(5.0));
    }

    #[tokio::test]
    async fn analyze_media_file_returns_err_for_nonexistent_path() {
        let missing = Path::new("/nonexistent/does-not-exist-analysis-fixture.ts");
        let result = analyze_media_file(missing).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to open media file"));
    }

    #[tokio::test]
    async fn analyze_media_file_returns_err_for_unparseable_content() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "restream-file-analysis-garbage-{}.ts",
            std::process::id()
        ));
        std::fs::write(&path, [0xFFu8; 4096]).expect("write garbage fixture");

        let result = analyze_media_file(&path).await;
        let _ = std::fs::remove_file(&path);

        assert!(
            result.is_err(),
            "a file that exists but isn't parseable media must surface an error, not panic"
        );
    }

    #[test]
    fn gop_stats_yields_no_interval_data_below_two_keyframes() {
        assert_eq!(gop_stats_from_keyframe_times(&[]), (None, None, false));
        assert_eq!(gop_stats_from_keyframe_times(&[3.0]), (None, None, false));
    }

    #[test]
    fn gop_stats_clamps_non_monotonic_intervals_to_zero() {
        // A corrupt or reordered stream can report keyframe timestamps out
        // of order; the resulting negative interval must be clamped to 0
        // rather than dragging the average or max below the true spacing.
        let (avg, max, sparse) = gop_stats_from_keyframe_times(&[0.0, 2.0, 1.0, 3.0]);
        // intervals: 2.0, -1.0 -> 0.0, 2.0 => avg = (2.0 + 0.0 + 2.0) / 3
        assert_eq!(avg, Some(round_metric(4.0 / 3.0)));
        assert_eq!(max, Some(2.0));
        assert!(!sparse);
    }

    #[test]
    fn gop_stats_sparse_boundary_is_strictly_greater_than_tolerance() {
        let boundary = LIVE_GOP_WARNING_THRESHOLD_SECS + LIVE_GOP_WARNING_TOLERANCE_SECS;
        let (_, _, at_boundary) = gop_stats_from_keyframe_times(&[0.0, boundary]);
        assert!(
            !at_boundary,
            "exactly at the tolerance boundary is not sparse"
        );

        let (_, _, just_over) = gop_stats_from_keyframe_times(&[0.0, boundary + 0.001]);
        assert!(just_over, "just past the tolerance boundary is sparse");
    }

    #[test]
    fn probe_output_reads_into_an_analysis() {
        let probe: Probe = serde_json::from_str(
            r#"{"programs":[],"streams":[{"codec_name":"hevc","avg_frame_rate":"30000/1001"}],
                "format":{"duration":"12.500000"}}"#,
        )
        .unwrap();
        let csv = "0.000000,0.066733,K__\n0.033367,0.033367,___\n2.002000,2.002000,K_\nN/A,4.004000,K__\n";
        let analysis = analysis_from(probe, &keyframe_times(csv));
        assert_eq!(analysis.video_codec.as_deref(), Some("hevc"));
        assert_eq!(analysis.fps, Some(29.97));
        assert_eq!(analysis.duration_sec, Some(12.5));
        assert_eq!(
            analysis.keyframe_count, 3,
            "non-key packets skipped; pts when dts is N/A"
        );
        assert_eq!(analysis.max_keyframe_interval_sec, Some(2.002));
        assert_eq!(frame_rate("0/0"), None);
        assert_eq!(frame_rate("nonsense"), None);
    }

    #[test]
    fn round_metric_rounds_to_three_decimal_places() {
        assert_eq!(round_metric(1.0004), 1.0);
        assert_eq!(round_metric(1.0005), 1.001);
        assert_eq!(round_metric(1.0006), 1.001);
        assert_eq!(round_metric(-1.0006), -1.001);
        assert_eq!(round_metric(0.0), 0.0);
    }
}
