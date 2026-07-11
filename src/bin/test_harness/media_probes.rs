use super::*;

/// Stream-selection policy for FFmpeg publishers spawned by the harness.
#[derive(Clone, Copy)]
pub(crate) enum PublishTrackSelection {
    PrimaryAv,
    AllStreams,
    /// MSR hero-scenario topology: one H.264 video, 29 stereo AAC language
    /// tracks, and one 5.1 AAC language track. The checked-in 2v16a fixture
    /// supplies both source layouts; FFmpeg only maps/copies them onto the
    /// exact transport shape required by the live scenario.
    MsrThirtyAudio,
}

pub(crate) const MSR_LANGUAGE_CODES: [&str; 30] = [
    "eng", "hin", "spa", "fra", "deu", "ita", "por", "rus", "jpn", "kor", "zho", "ara", "ben",
    "urd", "ind", "tur", "vie", "tha", "tam", "tel", "mar", "guj", "kan", "mal", "pan", "nld",
    "pol", "ukr", "swe", "fil",
];

pub(crate) fn sweep_fixture(config: SweepConfig, bitrate_label: &str) -> Result<PathBuf, String> {
    restream::test_fixtures::bench_transport_fixture(
        config.video_codec,
        bitrate_label,
        config.multi_audio,
    )
}

pub(crate) fn ramp_fixture() -> Result<PathBuf, String> {
    restream::test_fixtures::bench_transport_fixture("h264", "4M", false)
}

pub(crate) fn checked_h264_fixture() -> Result<PathBuf, String> {
    restream::test_fixtures::canonical_h264_ts_fixture()
}

pub(crate) fn spawn_publisher_with_selection(
    path: &Path,
    url: &str,
    format: &str,
    selection: PublishTrackSelection,
    log_path: Option<&Path>,
) -> Result<Child, String> {
    let ffmpeg_threads = std::env::var("HARNESS_FFMPEG_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .max(1);
    let mut cmd = command_with_optional_cgroup("ffmpeg", "publisher");
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-threads"]);
    cmd.arg(ffmpeg_threads.to_string());
    cmd.args(["-re", "-stream_loop", "-1", "-i"]);
    cmd.arg(path);
    match selection {
        PublishTrackSelection::AllStreams => {
            cmd.args(["-map", "0"]);
        }
        PublishTrackSelection::PrimaryAv => {
            cmd.args(["-map", "0:v", "-map", "0:a:0"]);
        }
        PublishTrackSelection::MsrThirtyAudio => {
            // The fixture has two video streams and sixteen audio streams.
            // Use only the primary H.264 video, duplicate a checked-in stereo
            // AAC stream for ranks 1..29, and place the fixture's 5.1 AAC
            // stream at rank 30. Stream copy preserves representative AAC
            // packet cadence without adding encoder cost to the publisher.
            cmd.args(["-map", "0:v:0"]);
            for _ in 0..29 {
                cmd.args(["-map", "0:a:1"]);
            }
            cmd.args(["-map", "0:a:2"]);
            for (index, language) in MSR_LANGUAGE_CODES.iter().enumerate() {
                cmd.arg(format!("-metadata:s:a:{index}"));
                cmd.arg(format!("language={language}"));
            }
        }
    }
    if format == "mpegts" {
        cmd.args(["-mpegts_flags", "+resend_headers"]);
        if matches!(selection, PublishTrackSelection::MsrThirtyAudio) {
            // The MSR source fixture is MP4/AVCC; convert its H.264 payload to
            // Annex B before the MPEG-TS muxer. Existing transport fixtures
            // retain their established dump-extra path.
            cmd.args(["-bsf:v", "h264_mp4toannexb"]);
        } else {
            cmd.args(["-bsf:v", "dump_extra=freq=keyframe"]);
        }
    }
    cmd.args(["-c", "copy", "-f", format]).arg(url);
    if let Some(log_path) = log_path {
        let log = std::fs::File::create(log_path).map_err(|e| e.to_string())?;
        let stderr = log.try_clone().map_err(|e| e.to_string())?;
        cmd.stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
    } else {
        // stderr must not be piped without a consumer — the 64KB pipe buffer
        // fills and blocks ffmpeg, hanging the test. Discard it when a fixture
        // publisher does not need a dedicated log file.
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
    }
    cmd.spawn().map_err(|e| e.to_string())
}

pub(crate) async fn spawn_publisher(
    path: &Path,
    url: &str,
    format: &str,
    map_all: bool,
) -> Result<Child, String> {
    spawn_publisher_with_selection(
        path,
        url,
        format,
        if map_all {
            PublishTrackSelection::AllStreams
        } else {
            PublishTrackSelection::PrimaryAv
        },
        None,
    )
}

/// Probe a live stream URL without buffering its contents into the harness.
pub(crate) async fn ffprobe(url: &str) -> Result<Value, String> {
    // kill_on_drop(true) ensures the subprocess is killed when the timeout
    // drops the future, preventing orphan ffprobe processes (T2 fix).
    let child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-probesize",
            "2M",
            "-analyzeduration",
            "2M",
            "-show_entries",
            "stream=index,codec_name,codec_type,width,height,sample_rate,channels",
            "-of",
            "json",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;

    let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
        .await
        .map_err(|_| format!("ffprobe timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

pub(crate) async fn ffprobe_video_packets(url: &str, output_path: &Path) -> Result<Value, String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-read_intervals",
            "%+5",
            "-select_streams",
            "v:0",
            "-show_packets",
            "-show_entries",
            "packet=pts_time,dts_time",
            "-of",
            "json",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;

    let output = tokio::time::timeout(Duration::from_secs(25), child.wait_with_output())
        .await
        .map_err(|_| format!("ffprobe packet capture timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    std::fs::write(output_path, &output.stdout).map_err(|e| e.to_string())?;
    let stderr_path = artifact_path("bframe-ffprobe.log");
    if let Some(parent) = stderr_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&stderr_path, &output.stderr).map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe packet capture failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

fn packet_times(packet_probe: &Value) -> impl Iterator<Item = (Option<f64>, Option<f64>)> + '_ {
    packet_probe["packets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|packet| {
            (
                packet["pts_time"].as_str().and_then(parse_probe_time),
                packet["dts_time"].as_str().and_then(parse_probe_time),
            )
        })
}

fn parse_probe_time(value: &str) -> Option<f64> {
    if value == "N/A" {
        None
    } else {
        value.parse().ok()
    }
}

pub(crate) fn count_video_packets(packet_probe: &Value) -> usize {
    packet_times(packet_probe)
        .filter(|(_, dts)| dts.is_some())
        .count()
}

pub(crate) fn count_bframe_packets(packet_probe: &Value) -> usize {
    packet_times(packet_probe)
        .filter(|(pts, dts)| matches!((pts, dts), (Some(pts), Some(dts)) if pts > dts))
        .count()
}

pub(crate) fn video_dts_monotone(packet_probe: &Value) -> bool {
    let mut last = None;
    for (_, dts) in packet_times(packet_probe) {
        let Some(dts) = dts else {
            continue;
        };
        if last.is_some_and(|last| dts < last) {
            return false;
        }
        last = Some(dts);
    }
    true
}

pub(crate) fn normalized_streams(probe: &Value) -> Result<Value, String> {
    let streams = probe["streams"]
        .as_array()
        .ok_or("ffprobe output has no streams")?;
    let mut normalized: Vec<Value> = streams
        .iter()
        .filter_map(|stream| match stream["codec_type"].as_str() {
            Some("video") => Some(json!({
                "type": "video",
                "codec": stream["codec_name"],
                "width": stream["width"],
                "height": stream["height"],
            })),
            Some("audio") => Some(json!({
                "type": "audio",
                "codec": stream["codec_name"],
                "sampleRate": stream["sample_rate"],
                "channels": stream["channels"],
            })),
            _ => None,
        })
        .collect();
    normalized.sort_by_key(|entry| entry["type"].as_str().unwrap_or("").to_string());
    Ok(Value::Array(normalized))
}

pub(crate) fn assert_media_only(probe: &Value, label: &str) -> Result<(), String> {
    let streams = probe["streams"]
        .as_array()
        .ok_or_else(|| format!("{label}: ffprobe output has no streams"))?;
    let non_media: Vec<&str> = streams
        .iter()
        .filter_map(|stream| stream["codec_type"].as_str())
        .filter(|kind| !matches!(*kind, "video" | "audio"))
        .collect();
    let video_count = streams
        .iter()
        .filter(|stream| stream["codec_type"] == "video")
        .count();
    let audio_count = streams
        .iter()
        .filter(|stream| stream["codec_type"] == "audio")
        .count();
    if !non_media.is_empty() || video_count != 1 || audio_count < 1 {
        return Err(format!(
            "{label}: expected 1 video + >=1 audio, got video={video_count} \
             audio={audio_count} non_media={non_media:?}"
        ));
    }
    Ok(())
}

pub(crate) fn media_dir_entries(path: &Path) -> Result<HashSet<String>, String> {
    let mut files = HashSet::new();
    if !path.exists() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() {
            files.insert(entry.file_name().to_string_lossy().to_string());
        }
    }
    Ok(files)
}

pub(crate) async fn wait_for_new_media_file(
    media_dir: &Path,
    before: &HashSet<String>,
    extension: &str,
    timeout: Duration,
) -> Result<PathBuf, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let files = media_dir_entries(media_dir)?;
        if let Some(name) = files
            .iter()
            .find(|name| !before.contains(*name) && name.ends_with(extension))
        {
            return Ok(media_dir.join(name));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no new {extension} media file appeared in {} within {}s",
                media_dir.display(),
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(crate) fn absolute_delta_secs(actual: f64, expected: f64) -> f64 {
    (actual - expected).abs()
}
