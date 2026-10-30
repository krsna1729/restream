//! What the resolved `ffmpeg` and `ffprobe` can do, checked once against what
//! each FFmpeg-backed feature needs.
//!
//! The engine runs every codec in these executables, and any build may lack a
//! component (a distro package without libx265, a minimal build without the
//! `unix` protocol). The check lists the build's encoders, decoders, muxers,
//! demuxers, filters, protocols and bitstream filters, and marks each feature
//! available or names what it is missing. A missing piece never stops the
//! server: startup logs it, `/api/v1/engine` reports it, and the feature that
//! needs it fails clearly when used.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::OnceCell;
use tracing::{info, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Encoder,
    Decoder,
    Muxer,
    Demuxer,
    Filter,
    Protocol,
    BitstreamFilter,
}

impl Kind {
    const ALL: [Kind; 7] = [
        Kind::Encoder,
        Kind::Decoder,
        Kind::Muxer,
        Kind::Demuxer,
        Kind::Filter,
        Kind::Protocol,
        Kind::BitstreamFilter,
    ];

    fn flag(self) -> &'static str {
        match self {
            Kind::Encoder => "-encoders",
            Kind::Decoder => "-decoders",
            Kind::Muxer => "-muxers",
            Kind::Demuxer => "-demuxers",
            Kind::Filter => "-filters",
            Kind::Protocol => "-protocols",
            Kind::BitstreamFilter => "-bsfs",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Encoder => "encoder",
            Kind::Decoder => "decoder",
            Kind::Muxer => "muxer",
            Kind::Demuxer => "demuxer",
            Kind::Filter => "filter",
            Kind::Protocol => "protocol",
            Kind::BitstreamFilter => "bitstream filter",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    Transcoding,
    FileIngest,
    RecordingRemux,
    Posters,
    PictureAndSoundChecks,
    MediaAnalysis,
}

impl Feature {
    pub const ALL: [Feature; 6] = [
        Feature::Transcoding,
        Feature::FileIngest,
        Feature::RecordingRemux,
        Feature::Posters,
        Feature::PictureAndSoundChecks,
        Feature::MediaAnalysis,
    ];

    /// The components the engine's own arguments for this feature use
    /// (`external_transcoder`, `external_file_ingest`, `recording`, `poster`,
    /// `stream_qa`). Keep in step with those argument builders.
    fn needs(self) -> &'static [(Kind, &'static str)] {
        use Kind::*;
        match self {
            Feature::Transcoding => &[
                (Demuxer, "mpegts"),
                (Decoder, "h264"),
                (Decoder, "hevc"),
                (Decoder, "aac"),
                (Encoder, "libx264"),
                (Encoder, "libx265"),
                (Encoder, "aac"),
                (Filter, "scale"),
                (Filter, "pan"),
                (Filter, "aresample"),
                (Muxer, "mpegts"),
                (Protocol, "pipe"),
            ],
            Feature::FileIngest => &[
                (Demuxer, "mp4"),
                (Demuxer, "mpegts"),
                (Decoder, "h264"),
                (Decoder, "aac"),
                (Encoder, "libx264"),
                (Encoder, "aac"),
                (Muxer, "mpegts"),
                (Protocol, "file"),
                (Protocol, "pipe"),
            ],
            Feature::RecordingRemux => &[
                (Demuxer, "mpegts"),
                (Muxer, "mov"),
                (BitstreamFilter, "aac_adtstoasc"),
                (Protocol, "file"),
            ],
            Feature::Posters => &[
                (Demuxer, "mp4"),
                (Decoder, "h264"),
                (Filter, "scale"),
                (Encoder, "png"),
                (Muxer, "image2pipe"),
                (Protocol, "file"),
                (Protocol, "pipe"),
            ],
            Feature::PictureAndSoundChecks => &[
                (Demuxer, "mpegts"),
                (Decoder, "h264"),
                (Decoder, "hevc"),
                (Decoder, "aac"),
                (Filter, "scale"),
                (Filter, "format"),
                (Filter, "aresample"),
                (Filter, "aformat"),
                (Filter, "amerge"),
                (Muxer, "rawvideo"),
                (Muxer, "f32le"),
                (Protocol, "pipe"),
                (Protocol, "unix"),
            ],
            // ffprobe ships with the same libraries; only its presence is checked.
            Feature::MediaAnalysis => &[],
        }
    }
}

/// The names one `ffmpeg -<kind>s` listing reports, by kind.
#[derive(Debug, Default)]
pub struct Components {
    names: [BTreeSet<String>; 7],
}

impl Components {
    fn set(&mut self, kind: Kind) -> &mut BTreeSet<String> {
        &mut self.names[kind as usize]
    }

    pub fn has(&self, kind: Kind, name: &str) -> bool {
        self.names[kind as usize].contains(name)
    }

    /// Reads one listing in FFmpeg's `-encoders`/`-muxers`/`-protocols`/...
    /// format. Formats list comma-separated aliases (`mov,mp4,m4a,...`), each
    /// recorded; muxers only count when flagged `E`, demuxers when `D`.
    pub fn read(&mut self, kind: Kind, listing: &str) {
        let names = parse_listing(kind, listing);
        self.set(kind).extend(names);
    }
}

fn parse_listing(kind: Kind, listing: &str) -> Vec<String> {
    match kind {
        // `Input:` then `Output:` sections of bare names; a protocol in
        // either counts (the engine reads files and writes pipes and sockets).
        Kind::Protocol => listing
            .lines()
            .skip_while(|line| line.trim() != "Input:")
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.ends_with(':'))
            .map(str::to_string)
            .collect(),
        Kind::BitstreamFilter => listing
            .lines()
            .skip_while(|line| !line.starts_with("Bitstream filters:"))
            .skip(1)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
        _ => listing
            .lines()
            // The legend ends at a dash rule: `--` (6.x formats), `---` (8.x
            // formats) or ` ------` (codecs and filters).
            .skip_while(|line| !line.trim_start().starts_with("--"))
            .skip(1)
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let flags = fields.next()?;
                let names = fields.next()?;
                let wanted = match kind {
                    Kind::Muxer => flags.contains('E'),
                    Kind::Demuxer => flags.contains('D'),
                    _ => true,
                };
                wanted.then(|| names.split(',').map(str::to_string).collect::<Vec<_>>())
            })
            .flatten()
            .collect(),
    }
}

/// One executable as found at startup.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub path: String,
    /// The `-version` banner's version, `None` when it did not run.
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureStatus {
    pub feature: Feature,
    pub available: bool,
    /// What it lacks, e.g. "encoder libx265".
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub ffmpeg: Tool,
    pub ffprobe: Tool,
    pub features: Vec<FeatureStatus>,
}

impl Report {
    pub fn available(&self, feature: Feature) -> bool {
        self.features
            .iter()
            .any(|status| status.feature == feature && status.available)
    }
}

/// Judges each feature against what was found. `components` is `None` when
/// `ffmpeg` itself did not run.
fn judge(components: Option<&Components>, ffprobe_found: bool) -> Vec<FeatureStatus> {
    Feature::ALL
        .iter()
        .map(|&feature| {
            let missing: Vec<String> = match (feature, components) {
                (Feature::MediaAnalysis, _) if !ffprobe_found => vec!["ffprobe".to_string()],
                (Feature::MediaAnalysis, _) => Vec::new(),
                (_, None) => vec!["ffmpeg".to_string()],
                (_, Some(components)) => feature
                    .needs()
                    .iter()
                    .filter(|(kind, name)| !components.has(*kind, name))
                    .map(|(kind, name)| format!("{} {name}", kind.label()))
                    .collect(),
            };
            FeatureStatus {
                feature,
                available: missing.is_empty(),
                missing,
            }
        })
        .collect()
}

/// The first line of `-version`: `ffmpeg version n8.1.3-... Copyright ...`.
fn version_from_banner(banner: &str) -> Option<String> {
    let first = banner.lines().next()?;
    let rest = first.split_once(" version ")?.1;
    rest.split_whitespace().next().map(str::to_string)
}

const PROBE_WITHIN: Duration = Duration::from_secs(10);

async fn tool(path: &Path) -> Tool {
    let run = crate::media::ffmpeg_binary::run_tool(
        path,
        &["-hide_banner".into(), "-version".into()],
        PROBE_WITHIN,
    )
    .await;
    let display = path.display().to_string();
    match run {
        Ok(stdout) => Tool {
            path: display,
            version: version_from_banner(&String::from_utf8_lossy(&stdout)),
            error: None,
        },
        Err(error) => Tool {
            path: display,
            version: None,
            error: Some(error),
        },
    }
}

async fn probe(ffmpeg: &Path, ffprobe: &Path) -> Report {
    let ffmpeg_tool = tool(ffmpeg).await;
    let ffprobe_tool = tool(ffprobe).await;
    let components = if ffmpeg_tool.error.is_none() {
        let mut components = Components::default();
        for kind in Kind::ALL {
            let args = ["-hide_banner".to_string(), kind.flag().to_string()];
            match crate::media::ffmpeg_binary::run_tool(ffmpeg, &args, PROBE_WITHIN).await {
                Ok(stdout) => components.read(kind, &String::from_utf8_lossy(&stdout)),
                Err(error) => warn!(err = %error, "ffmpeg {} listing failed", kind.flag()),
            }
        }
        Some(components)
    } else {
        None
    };
    Report {
        features: judge(components.as_ref(), ffprobe_tool.error.is_none()),
        ffmpeg: ffmpeg_tool,
        ffprobe: ffprobe_tool,
    }
}

static REPORT: OnceCell<Report> = OnceCell::const_new();

/// The check's result, run on first use (startup calls [`check_at_startup`]).
pub async fn report() -> &'static Report {
    REPORT
        .get_or_init(|| {
            probe(
                crate::media::ffmpeg_binary::ffmpeg_bin_path(),
                crate::media::ffmpeg_binary::ffprobe_bin_path(),
            )
        })
        .await
}

/// The result if the check has finished; for synchronous status views.
pub fn finished() -> Option<&'static Report> {
    REPORT.get()
}

/// Runs the check and logs one line per unavailable feature.
pub async fn check_at_startup() {
    let report = report().await;
    let unavailable: Vec<&FeatureStatus> = report
        .features
        .iter()
        .filter(|status| !status.available)
        .collect();
    if unavailable.is_empty() {
        info!(
            ffmpeg = report.ffmpeg.version.as_deref().unwrap_or("unknown"),
            "[startup] FFmpeg provides every feature"
        );
        return;
    }
    for status in unavailable {
        warn!(
            feature = ?status.feature,
            missing = %status.missing.join(", "),
            ffmpeg = %report.ffmpeg.path,
            "[startup] FFmpeg lacks what this feature needs; it will fail until FFmpeg is replaced \
             (`restream ffmpeg-fetch` installs a complete build)",
        );
    }
}

#[cfg(test)]
#[path = "ffmpeg_capabilities_tests.rs"]
mod tests;
