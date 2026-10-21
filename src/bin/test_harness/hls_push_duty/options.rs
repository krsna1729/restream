//! Parsed command-line options for the `hls-push-duty` mode.
//!
//! The mode is the executor-isolation harness for HLS push ingest: a baseline
//! arm of RTMP publishers and a treatment arm of extra HLS push publishers
//! whose only difference is the HLS addition. Every knob has a default, so a
//! bare `hls-push-duty` is a short, standalone smoke run.

use std::time::Duration;

/// Which HLS container the HLS-arm publishers push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsSegmentType {
    Ts,
    Fmp4,
}

impl HlsSegmentType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ts => "ts",
            Self::Fmp4 => "fmp4",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "ts" => Ok(Self::Ts),
            "fmp4" => Ok(Self::Fmp4),
            other => Err(format!("--segment-type must be ts or fmp4 (got {other:?})")),
        }
    }

    /// The `-f hls` arguments that select this segment container.
    ///
    /// fMP4 needs `aac_adtstoasc` when the source is ADTS AAC in MPEG-TS,
    /// because the mp4 muxer rejects a raw ADTS bitstream (see the HLS push
    /// ingest section of `docs/media-pipeline.md`).
    pub(crate) const fn ffmpeg_args(self) -> &'static [&'static str] {
        match self {
            Self::Ts => &[],
            Self::Fmp4 => &["-hls_segment_type", "fmp4", "-bsf:a", "aac_adtstoasc"],
        }
    }
}

/// Which HTTP method the HLS-arm publishers upload with (`-method`). The
/// ingest takes both; FFmpeg's default is POST, the docs' examples use PUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsMethod {
    Put,
    Post,
}

impl HlsMethod {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Put => "PUT",
            Self::Post => "POST",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "put" => Ok(Self::Put),
            "post" => Ok(Self::Post),
            _ => Err(format!("--method must be put or post (got {value:?})")),
        }
    }
}

/// Upper bounds keep a mistyped argument from spawning hundreds of FFmpeg
/// processes on a shared host.
const MAX_PUBLISHERS: usize = 64;
const MAX_SEGMENT_SECS: u64 = 30;
const MAX_DURATION_SECS: u64 = 3_600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlsPushOptions {
    /// RTMP publishers that make up the baseline arm.
    pub(crate) base_publishers: usize,
    /// Extra HLS push publishers: 0 is the baseline, >0 the treatment arm.
    pub(crate) hls_publishers: usize,
    /// HLS pull sessions: >0 adds one RTMP `pull-source` pipeline and this
    /// many pipelines pulling its preview.
    pub(crate) pull_sessions: usize,
    pub(crate) segment_type: HlsSegmentType,
    pub(crate) method: HlsMethod,
    /// Rated window, in seconds, measured after every input has published.
    pub(crate) duration_secs: u64,
    /// `-hls_time`: the HLS arm's target segment duration.
    pub(crate) segment_secs: u64,
    /// Start Restream with `RESTREAM_MEDIA_POLL_USAGE=1`, so the executor
    /// rows carry per-poll user/system time and page faults.
    pub(crate) poll_usage: bool,
    /// Run the host-stall probe beside the window and attribute a class's
    /// poll-CPU maximum against it (`host_probe`).
    pub(crate) host_probe: bool,
}

impl Default for HlsPushOptions {
    fn default() -> Self {
        Self {
            base_publishers: 2,
            hls_publishers: 0,
            pull_sessions: 0,
            segment_type: HlsSegmentType::Ts,
            method: HlsMethod::Put,
            duration_secs: 12,
            segment_secs: 2,
            poll_usage: false,
            host_probe: false,
        }
    }
}

pub(crate) const USAGE: &str = "hls-push-duty [--base-publishers N] [--hls-publishers N] \
[--pull-sessions N] [--segment-type ts|fmp4] [--method put|post] [--duration-secs N] \
[--segment-secs N] [--poll-usage] [--host-probe]";

impl HlsPushOptions {
    pub(crate) fn from_args(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut index = 0;
        while index < args.len() {
            let (name, inline) = match args[index].split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (args[index].as_str(), None),
            };
            let value = |index: &mut usize| -> Result<String, String> {
                if let Some(inline) = inline.clone() {
                    return Ok(inline);
                }
                *index += 1;
                args.get(*index)
                    .cloned()
                    .ok_or_else(|| format!("{name} requires a value"))
            };
            match name {
                "--base-publishers" => {
                    options.base_publishers = parse_usize(&value(&mut index)?, name)?
                }
                "--hls-publishers" => {
                    options.hls_publishers = parse_usize(&value(&mut index)?, name)?
                }
                "--pull-sessions" => {
                    options.pull_sessions = parse_usize(&value(&mut index)?, name)?
                }
                "--segment-type" => {
                    options.segment_type = HlsSegmentType::parse(&value(&mut index)?)?
                }
                "--method" => options.method = HlsMethod::parse(&value(&mut index)?)?,
                "--duration-secs" => options.duration_secs = parse_u64(&value(&mut index)?, name)?,
                "--segment-secs" => options.segment_secs = parse_u64(&value(&mut index)?, name)?,
                "--poll-usage" if inline.is_none() => options.poll_usage = true,
                "--host-probe" if inline.is_none() => options.host_probe = true,
                other => {
                    return Err(format!(
                        "unknown hls-push-duty option {other:?}; usage: {USAGE}"
                    ));
                }
            }
            index += 1;
        }
        options.validate()?;
        Ok(options)
    }

    /// Publishers this mode starts: base, HLS push, and the pull source.
    pub(crate) fn total_publishers(&self) -> usize {
        self.base_publishers + self.hls_publishers + usize::from(self.pull_sessions > 0)
    }

    /// Pipelines, each with its own sink: every publisher's plus the pulls.
    pub(crate) fn total_pipelines(&self) -> usize {
        self.total_publishers() + self.pull_sessions
    }

    pub(crate) fn window(&self) -> Duration {
        Duration::from_secs(self.duration_secs)
    }

    fn validate(&self) -> Result<(), String> {
        if self.base_publishers + self.hls_publishers == 0 {
            return Err(
                "hls-push-duty needs at least one publisher (--base-publishers or \
                 --hls-publishers)"
                    .to_string(),
            );
        }
        if self.total_pipelines() > MAX_PUBLISHERS {
            return Err(format!(
                "hls-push-duty supports at most {MAX_PUBLISHERS} pipelines (got {})",
                self.total_pipelines()
            ));
        }
        if self.segment_secs == 0 || self.segment_secs > MAX_SEGMENT_SECS {
            return Err(format!(
                "--segment-secs must be in 1..={MAX_SEGMENT_SECS} (got {})",
                self.segment_secs
            ));
        }
        if self.duration_secs == 0 || self.duration_secs > MAX_DURATION_SECS {
            return Err(format!(
                "--duration-secs must be in 1..={MAX_DURATION_SECS} (got {})",
                self.duration_secs
            ));
        }
        Ok(())
    }
}

fn parse_usize(value: &str, name: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("{name} needs a non-negative integer (got {value:?})"))
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} needs a positive integer (got {value:?})"))
}
