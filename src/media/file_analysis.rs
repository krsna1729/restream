//! Offline media-file inspection helpers used to validate ingest suitability
//! and expose operator diagnostics for stored media assets.

use ffmpeg_next::{format, media};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

fn codec_name(id: ffmpeg_next::codec::Id) -> String {
    match id {
        ffmpeg_next::codec::Id::H264 => "h264",
        ffmpeg_next::codec::Id::HEVC => "hevc",
        ffmpeg_next::codec::Id::AAC => "aac",
        other => return format!("{other:?}").to_ascii_lowercase(),
    }
    .to_string()
}

fn timestamp_seconds(
    stream: &ffmpeg_next::Stream<'_>,
    packet: &ffmpeg_next::Packet,
) -> Option<f64> {
    let ts = packet.dts().or_else(|| packet.pts())?;
    let tb = stream.time_base();
    if tb.1 == 0 {
        return Some(ts as f64);
    }
    Some(ts as f64 * tb.0 as f64 / tb.1 as f64)
}

pub fn analyze_media_file(path: &Path) -> Result<MediaFileAnalysis, String> {
    let mut ictx =
        format::input(path).map_err(|error| format!("Failed to open media file: {error}"))?;
    let Some(video_stream) = ictx.streams().best(media::Type::Video).or_else(|| {
        ictx.streams()
            .find(|stream| stream.parameters().medium() == media::Type::Video)
    }) else {
        return Ok(MediaFileAnalysis {
            video_codec: None,
            fps: None,
            duration_sec: None,
            keyframe_count: 0,
            average_keyframe_interval_sec: None,
            max_keyframe_interval_sec: None,
            sparse_for_live: false,
            live_gop_target_seconds: DEFAULT_LIVE_GOP_TARGET_SECONDS,
        });
    };

    let video_index = video_stream.index();
    let video_codec = Some(codec_name(video_stream.parameters().id()));
    let frame_rate = video_stream.avg_frame_rate();
    let fps = if frame_rate.0 > 0 && frame_rate.1 > 0 {
        Some(round_metric(frame_rate.0 as f64 / frame_rate.1 as f64))
    } else {
        None
    };
    let duration_sec = if ictx.duration() > 0 {
        Some(round_metric(ictx.duration() as f64 / 1_000_000.0))
    } else {
        None
    };

    let mut keyframe_times = Vec::new();
    for (stream, packet) in ictx.packets() {
        if stream.index() != video_index || !packet.is_key() {
            continue;
        }
        if let Some(timestamp) = timestamp_seconds(&stream, &packet) {
            keyframe_times.push(timestamp);
        }
    }

    let (average_keyframe_interval_sec, max_keyframe_interval_sec, sparse_for_live) =
        gop_stats_from_keyframe_times(&keyframe_times);

    Ok(MediaFileAnalysis {
        video_codec,
        fps,
        duration_sec,
        keyframe_count: keyframe_times.len(),
        average_keyframe_interval_sec,
        max_keyframe_interval_sec,
        sparse_for_live,
        live_gop_target_seconds: DEFAULT_LIVE_GOP_TARGET_SECONDS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_fixture_reports_2_second_gop() {
        let fixture =
            crate::test_fixtures::canonical_h264_ts_fixture().expect("fixture should exist");
        let analysis = analyze_media_file(&fixture).expect("analysis should succeed");

        assert_eq!(analysis.video_codec.as_deref(), Some("h264"));
        assert_eq!(analysis.keyframe_count, 4);
        assert_eq!(analysis.average_keyframe_interval_sec, Some(2.0));
        assert_eq!(analysis.max_keyframe_interval_sec, Some(2.0));
        assert!(!analysis.sparse_for_live);
    }

    #[test]
    fn sparse_fixture_reports_sparse_gop() {
        let fixture = crate::test_fixtures::sparse_gop_mp4_fixture().expect("fixture should exist");
        let analysis = analyze_media_file(&fixture).expect("analysis should succeed");

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

    #[test]
    fn analyze_media_file_returns_err_for_nonexistent_path() {
        let missing = Path::new("/nonexistent/does-not-exist-analysis-fixture.ts");
        let result = analyze_media_file(missing);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to open media file"));
    }

    #[test]
    fn analyze_media_file_returns_err_for_unparseable_content() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "restream-file-analysis-garbage-{}.ts",
            std::process::id()
        ));
        std::fs::write(&path, [0xFFu8; 4096]).expect("write garbage fixture");

        let result = analyze_media_file(&path);
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
    fn codec_name_maps_known_codecs_and_falls_back_for_unknown() {
        assert_eq!(codec_name(ffmpeg_next::codec::Id::H264), "h264");
        assert_eq!(codec_name(ffmpeg_next::codec::Id::HEVC), "hevc");
        assert_eq!(codec_name(ffmpeg_next::codec::Id::AAC), "aac");
        // Any codec outside the explicit match falls back to the
        // lowercased Debug representation rather than panicking.
        assert_eq!(codec_name(ffmpeg_next::codec::Id::VP8), "vp8");
        assert_eq!(codec_name(ffmpeg_next::codec::Id::None), "none");
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
