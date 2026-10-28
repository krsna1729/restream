//! Result-JSON assembly helpers for the `hls-pull-duty` mode.
//!
//! The checks are pure functions over the API and probe snapshots, so they are
//! unit tested without a live engine. The probe shape is the shared one
//! `hls-push-duty` reads (`/api/v1/pipelines/{id}/probe`); the pull-specific
//! difference is that the rendition pulled from Restream's own preview master
//! is the video variant, so these checks never require audio.

use serde_json::{Value, json};

/// The pull input row, read back from `/api/v1/pipelines/{id}/inputs`.
pub(crate) fn find_input<'a>(inputs: &'a Value, input_id: &str) -> Option<&'a Value> {
    inputs
        .get("inputs")?
        .as_array()?
        .iter()
        .find(|input| input.get("id").and_then(Value::as_str) == Some(input_id))
}

/// The pipeline's primary input id, from its inputs listing. The API refuses
/// to disable a selected input, so the lifecycle must select the primary
/// before the pull input can become a backup and be disabled.
pub(crate) fn primary_input_id(inputs: &Value) -> Option<&str> {
    inputs
        .get("inputs")?
        .as_array()?
        .iter()
        .find(|input| input.get("role").and_then(Value::as_str) == Some("primary"))?
        .get("id")
        .and_then(Value::as_str)
}

/// The protocol label of the input's live ingest session, or `None` while no
/// session is connected. The engine records `hls-pull` for objects the puller
/// fetched itself and `hls` for an upload.
pub(crate) fn session_protocol(input: &Value) -> Option<&str> {
    let runtime = input.get("runtime")?;
    if runtime.get("connected").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    runtime.get("protocol").and_then(Value::as_str)
}

fn keyframe_count(probe: &Value) -> Option<u64> {
    probe.pointer("/gop/keyframeCount").and_then(Value::as_u64)
}

fn average_interval_sec(probe: &Value) -> Option<f64> {
    probe
        .pointer("/gop/averageIntervalSec")
        .and_then(Value::as_f64)
}

/// The media time the ingest's keyframe timeline covers, in seconds. It is
/// frozen by a stalled pull while the wall clock keeps moving — and capped
/// with the timeline itself, which is why keep-up is measured on the sink's
/// monotonic media clock instead.
fn media_seconds(probe: &Value) -> Option<f64> {
    let count = keyframe_count(probe)?;
    let interval = average_interval_sec(probe)?;
    (count >= 1).then(|| count.saturating_sub(1) as f64 * interval)
}

/// Media-side view of the pull pipeline's probe snapshot, kept for the report.
pub(crate) fn pull_media_summary(probe: &Value) -> Value {
    json!({
        "protocol": probe.pointer("/ingest/protocol").cloned().unwrap_or(Value::Null),
        "bytesReceived": probe.pointer("/ingest/bytesReceived").cloned().unwrap_or(Value::Null),
        "uptimeSeconds": probe.pointer("/ingest/uptimeSeconds").cloned().unwrap_or(Value::Null),
        "videoPresent": probe.get("video").is_some_and(|video| !video.is_null()),
        "audioTracks": probe
            .get("audioTracks")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        "keyframeCount": keyframe_count(probe),
        "averageIntervalSec": average_interval_sec(probe),
        "mediaSeconds": media_seconds(probe),
    })
}

/// Media reached the pull pipeline's ring: bytes counted, video and audio
/// probed, and at least one keyframe released. The preview master carries its
/// audio as a separate `EXT-X-MEDIA` rendition, which the pull follows.
pub(crate) fn pull_published_ok(probe: &Value) -> bool {
    let bytes = probe
        .pointer("/ingest/bytesReceived")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    bytes > 0
        && probe.get("video").is_some_and(|video| !video.is_null())
        && probe
            .get("audioTracks")
            .and_then(Value::as_array)
            .is_some_and(|tracks| !tracks.is_empty())
        && keyframe_count(probe).is_some_and(|count| count >= 1)
}

/// The pull pipeline's sink delivered video and audio with monotone decode
/// timestamps.
pub(crate) fn pull_delivered_ok(sink_summary: &Value) -> bool {
    let count = |name: &str| sink_summary.get(name).and_then(Value::as_u64).unwrap_or(0);
    count("videoCount") > 0
        && count("audioCount") > 0
        && count("keyframeCount") > 0
        && count("bytes") > 0
        && sink_summary.get("dtsMonotone").and_then(Value::as_bool) == Some(true)
}

/// The pulled media clock keeps up with the wall clock **inside the window**:
/// the media time the sink received advanced by at least half the wall time
/// the window took.
///
/// Progress, not lifetime totals. The probe's keyframe timeline is capped at
/// 30 entries, so its media seconds stop advancing once the cap is reached
/// while `uptimeSeconds` keeps growing — a lifetime comparison fails a healthy
/// pull on a window longer than about two minutes. `mediaProgressMs` is the
/// sink's accumulated forward media time, so the difference between two samples
/// is exactly the media time the window carried, and a re-anchor inside the
/// window neither freezes it nor makes it go backwards.
pub(crate) fn pull_keeps_up_over_window(start: &Value, end: &Value, wall_secs: f64) -> bool {
    let progress_ms = |summary: &Value| {
        summary
            .get("mediaProgressMs")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let media_secs = progress_ms(end).saturating_sub(progress_ms(start)) as f64 / 1000.0;
    wall_secs > 0.0 && media_secs >= 0.5 * wall_secs
}

/// The engine log names the failing input on a retry line: the observable that
/// a misconfigured pull reports its failure instead of idling silently.
pub(crate) fn log_reports_pull_failure(log: &str, input_id: &str) -> bool {
    let needle = format!("input={input_id}");
    log.lines()
        .any(|line| line.contains("HLS pull failed") && line.contains(&needle))
}

/// Whether a `/api/v1/pipelines` listing still carries the pipeline.
pub(crate) fn pipeline_present(list: &Value, pipeline_id: &str) -> bool {
    list.get("pipelines")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            rows.iter()
                .any(|row| row.get("id").and_then(Value::as_str) == Some(pipeline_id))
        })
}
