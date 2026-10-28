//! Pure verdicts over what MediaMTX, FFprobe and the RTMP sink report.
//!
//! Everything here is a function of captured JSON or samples, so each guard
//! is unit-tested on its own; `run.rs` only gathers the inputs.

use super::*;

use sha2::{Digest, Sha256};

/// MediaMTX's label (`tracks2[].codec`) for the H.264 track Restream sends.
pub(crate) const RELAY_VIDEO_LABEL: &str = "H264";
/// MediaMTX's label for the AAC track Restream sends.
pub(crate) const RELAY_AUDIO_LABEL: &str = "MPEG-4 Audio";
/// Largest relative difference between the delivered video frame count and
/// the input frame rate over the same media span.
pub(crate) const FRAME_COUNT_TOLERANCE: f64 = 0.05;

/// Codec labels of a MediaMTX `/v3/paths/get/<name>` body, in track order.
pub(crate) fn relay_path_codecs(path: &Value) -> Vec<String> {
    path.get("tracks2")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|track| track.get("codec").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Whether a MediaMTX path body shows the path online, published by a
/// source of `source_type` (`moqSession` for Restream's MoQ egress), with an
/// H.264 and an AAC track.
pub(crate) fn relay_path_published(path: &Value, source_type: &str) -> bool {
    let online = path.get("online").and_then(Value::as_bool) == Some(true);
    let source = path.pointer("/source/type").and_then(Value::as_str);
    let codecs = relay_path_codecs(path);
    online
        && source == Some(source_type)
        && codecs.iter().any(|codec| codec == RELAY_VIDEO_LABEL)
        && codecs.iter().any(|codec| codec == RELAY_AUDIO_LABEL)
}

/// An FFprobe `avg_frame_rate`/`r_frame_rate` value (`"30/1"`,
/// `"30000/1001"`) as frames per second; `None` for `0/0` or garbage.
pub(crate) fn parse_frame_rate(value: &str) -> Option<f64> {
    let (num, den) = value.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    let fps = num / den;
    (fps.is_finite() && fps > 0.0).then_some(fps)
}

/// What one FFprobe read of a relay path delivered, per stream.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RelaySample {
    pub(crate) video_codec: Option<String>,
    pub(crate) audio_codec: Option<String>,
    pub(crate) video_frames: usize,
    pub(crate) audio_frames: usize,
    pub(crate) video_span_secs: f64,
    pub(crate) audio_span_secs: f64,
    /// Every video packet has a PTS greater than the one before. The input is
    /// B-frame free, so decode order is presentation order and a repeat or a
    /// step back is a timestamp fault.
    pub(crate) video_pts_increasing: bool,
    /// Every audio packet has a PTS at least the one before. Equal values are
    /// allowed: FFmpeg's RTP depacketizer gives every AAC access unit of one
    /// aggregated RTP packet that packet's timestamp.
    pub(crate) audio_pts_nondecreasing: bool,
}

/// The PTS series of one stream, in demux order.
struct PtsSeries {
    frames: usize,
    first: Option<f64>,
    last: Option<f64>,
    increasing: bool,
    nondecreasing: bool,
}

impl PtsSeries {
    fn new() -> Self {
        Self {
            frames: 0,
            first: None,
            last: None,
            increasing: true,
            nondecreasing: true,
        }
    }

    fn push(&mut self, pts: Option<f64>) {
        self.frames += 1;
        let Some(pts) = pts else {
            // Every MoQ object carries a timestamp, so a packet without one
            // is a fault on the path, not a gap to skip.
            self.increasing = false;
            self.nondecreasing = false;
            return;
        };
        if let Some(last) = self.last {
            if pts <= last {
                self.increasing = false;
            }
            if pts < last {
                self.nondecreasing = false;
            }
        }
        self.first.get_or_insert(pts);
        self.last = Some(pts);
    }

    fn span_secs(&self) -> f64 {
        match (self.first, self.last) {
            (Some(first), Some(last)) => (last - first).max(0.0),
            _ => 0.0,
        }
    }
}

impl RelaySample {
    /// Read an FFprobe `-show_streams -show_packets` JSON body (the shape
    /// `ffprobe_live_sample` captures). The first video and the first audio
    /// stream are the ones Restream publishes.
    pub(crate) fn from_probe(probe: &Value) -> Self {
        let streams = probe
            .get("streams")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let first_of = |kind: &str| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some(kind))
        };
        let index_of = |stream: Option<&Value>| stream.and_then(|s| s.get("index")?.as_u64());
        let codec_of = |stream: Option<&Value>| {
            stream
                .and_then(|s| s.get("codec_name")?.as_str())
                .map(str::to_string)
        };
        let video = first_of("video");
        let audio = first_of("audio");
        let (video_index, audio_index) = (index_of(video), index_of(audio));

        let mut video_pts = PtsSeries::new();
        let mut audio_pts = PtsSeries::new();
        for packet in probe
            .get("packets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = packet.get("stream_index").and_then(Value::as_u64);
            let pts = packet
                .get("pts_time")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<f64>().ok());
            if index.is_some() && index == video_index {
                video_pts.push(pts);
            } else if index.is_some() && index == audio_index {
                audio_pts.push(pts);
            }
        }

        Self {
            video_codec: codec_of(video),
            audio_codec: codec_of(audio),
            video_frames: video_pts.frames,
            audio_frames: audio_pts.frames,
            video_span_secs: video_pts.span_secs(),
            audio_span_secs: audio_pts.span_secs(),
            video_pts_increasing: video_pts.frames > 0 && video_pts.increasing,
            audio_pts_nondecreasing: audio_pts.frames > 0 && audio_pts.nondecreasing,
        }
    }

    /// Video frames the input produces over the sampled span: both ends of
    /// the span are frames, hence the `+ 1`.
    pub(crate) fn expected_video_frames(&self, input_fps: f64) -> f64 {
        self.video_span_secs * input_fps + 1.0
    }

    /// Delivered video frames over expected ones.
    pub(crate) fn frame_ratio(&self, input_fps: f64) -> f64 {
        let expected = self.expected_video_frames(input_fps);
        if expected > 0.0 {
            self.video_frames as f64 / expected
        } else {
            0.0
        }
    }

    /// Every reason this sample fails the relay contract: H.264 + AAC, both
    /// spanning at least `min_span_secs`, monotonic timestamps, and a video
    /// frame count within [`FRAME_COUNT_TOLERANCE`] of the input rate.
    pub(crate) fn failures(&self, input_fps: f64, min_span_secs: f64) -> Vec<String> {
        let mut failures = Vec::new();
        if self.video_codec.as_deref() != Some("h264") {
            failures.push(format!("video codec {:?}, expected h264", self.video_codec));
        }
        if self.audio_codec.as_deref() != Some("aac") {
            failures.push(format!("audio codec {:?}, expected aac", self.audio_codec));
        }
        if self.video_span_secs < min_span_secs {
            failures.push(format!(
                "video spans {:.2}s, expected at least {min_span_secs}s",
                self.video_span_secs
            ));
        }
        if self.audio_span_secs < min_span_secs {
            failures.push(format!(
                "audio spans {:.2}s, expected at least {min_span_secs}s",
                self.audio_span_secs
            ));
        }
        if !self.video_pts_increasing {
            failures.push("video PTS not strictly increasing".to_string());
        }
        if !self.audio_pts_nondecreasing {
            failures.push("audio PTS went backwards or is missing".to_string());
        }
        let ratio = self.frame_ratio(input_fps);
        if (ratio - 1.0).abs() > FRAME_COUNT_TOLERANCE {
            failures.push(format!(
                "{} video frames over {:.2}s, expected {:.0} at {input_fps} fps (ratio {ratio:.3})",
                self.video_frames,
                self.video_span_secs,
                self.expected_video_frames(input_fps)
            ));
        }
        failures
    }

    pub(crate) fn to_json(&self, input_fps: f64) -> Value {
        json!({
            "videoCodec": self.video_codec,
            "audioCodec": self.audio_codec,
            "videoFrames": self.video_frames,
            "audioFrames": self.audio_frames,
            "videoSpanSecs": round3(self.video_span_secs),
            "audioSpanSecs": round3(self.audio_span_secs),
            "videoPtsIncreasing": self.video_pts_increasing,
            "audioPtsNondecreasing": self.audio_pts_nondecreasing,
            "inputFps": input_fps,
            "expectedVideoFrames": round3(self.expected_video_frames(input_fps)),
            "frameRatio": round3(self.frame_ratio(input_fps)),
        })
    }
}

/// Longest wall-clock stretch, over time-ordered `(elapsed, counter)`
/// samples, in which the counter did not advance. The stretch from the last
/// advance to the final sample counts too, so a counter that stops for good
/// is caught rather than ignored.
pub(crate) fn longest_counter_stall(samples: &[(Duration, u64)]) -> Duration {
    let mut longest = Duration::ZERO;
    let mut last_advance: Option<(Duration, u64)> = None;
    for &(at, value) in samples {
        match last_advance {
            None => last_advance = Some((at, value)),
            Some((since, seen)) => {
                longest = longest.max(at.saturating_sub(since));
                if value > seen {
                    last_advance = Some((at, value));
                }
            }
        }
    }
    longest
}

/// SHA-256 of a DER certificate as lowercase hex without separators: the
/// form MediaMTX compares `destFingerprint` against.
pub(crate) fn certificate_sha256_hex(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}
