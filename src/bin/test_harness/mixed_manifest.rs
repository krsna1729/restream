//! Manifest-shaped mixed-matrix axes, rows, and coverage helpers.

use std::path::PathBuf;
use std::sync::OnceLock;

use restream::test_fixtures::AvMarkerBframeMode;
use serde::Deserialize;

/// Input transport family for mixed-matrix source streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixedInputProtocol {
    File,
    Rtmp,
    Srt,
}

impl MixedInputProtocol {
    pub(crate) const fn source_name(self) -> &'static str {
        match self {
            Self::File => "asset",
            Self::Rtmp | Self::Srt => "live",
        }
    }

    pub(crate) const fn ingest_name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
        }
    }

    pub(crate) fn from_ingest_name(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "rtmp" => Some(Self::Rtmp),
            "srt" => Some(Self::Srt),
            _ => None,
        }
    }
}

/// Video codec axis for mixed-matrix source streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixedVideoCodec {
    H264,
    H265,
}

impl MixedVideoCodec {
    pub(crate) const fn scenario_token(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }

    pub(crate) const fn expected_video_codec(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
        }
    }

    pub(crate) const fn hls_preview_expected_dimensions(self) -> &'static str {
        match self {
            // HEVC preview is browser-compat today: HEVC input is converted to the
            // 720p H.264 preview ring before the MPEG-TS HLS segmenter sees it.
            Self::H264 => "1920x1080",
            Self::H265 => "1280x720",
        }
    }

    pub(crate) fn from_scenario_token(value: &str) -> Option<Self> {
        match value {
            "h264" => Some(Self::H264),
            "h265" => Some(Self::H265),
            _ => None,
        }
    }
}

/// Source audio-track layout axis for mixed-matrix scenarios.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixedInputAudioLayout {
    A1,
    A2,
}

impl MixedInputAudioLayout {
    pub(crate) const fn scenario_token(self) -> &'static str {
        match self {
            Self::A1 => "a1",
            Self::A2 => "a2",
        }
    }

    pub(crate) const fn track_layout_name(self) -> &'static str {
        match self {
            Self::A1 => "single",
            Self::A2 => "multi",
        }
    }

    pub(crate) const fn expected_audio_tracks(self) -> usize {
        match self {
            Self::A1 => 1,
            Self::A2 => 2,
        }
    }

    pub(crate) const fn is_multi_track(self) -> bool {
        matches!(self, Self::A2)
    }

    pub(crate) fn from_scenario_token(value: &str) -> Option<Self> {
        match value {
            "a1" => Some(Self::A1),
            "a2" => Some(Self::A2),
            _ => None,
        }
    }
}

/// Source frame-reordering axis used to distinguish BF0 from B-frame fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixedInputReorder {
    Bf0,
    Bf2,
}

impl MixedInputReorder {
    pub(crate) const fn scenario_token(self) -> &'static str {
        match self {
            Self::Bf0 => "bf0",
            Self::Bf2 => "bf2",
        }
    }

    pub(crate) const fn has_b_frames(self) -> bool {
        matches!(self, Self::Bf2)
    }

    pub(crate) const fn fixture_mode(self) -> AvMarkerBframeMode {
        match self {
            Self::Bf0 => AvMarkerBframeMode::Bf0,
            Self::Bf2 => AvMarkerBframeMode::Bf2,
        }
    }

    pub(crate) fn from_scenario_token(value: &str) -> Option<Self> {
        match value {
            "bf0" => Some(Self::Bf0),
            "bf2" => Some(Self::Bf2),
            _ => None,
        }
    }
}

/// Complete input-side scenario key for the mixed matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MixedInputCase {
    protocol: MixedInputProtocol,
    codec: MixedVideoCodec,
    audio_layout: MixedInputAudioLayout,
    reorder: MixedInputReorder,
}

impl MixedInputCase {
    pub(crate) const fn new(
        protocol: MixedInputProtocol,
        codec: MixedVideoCodec,
        audio_layout: MixedInputAudioLayout,
        reorder: MixedInputReorder,
    ) -> Self {
        Self {
            protocol,
            codec,
            audio_layout,
            reorder,
        }
    }

    pub(crate) fn scenario_id(self) -> &'static str {
        match (self.protocol, self.codec, self.audio_layout, self.reorder) {
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ) => "mixed.asset.file.h264.a1.bf0",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf2,
            ) => "mixed.asset.file.h264.a1.bf2",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf0,
            ) => "mixed.asset.file.h264.a2.bf0",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf2,
            ) => "mixed.asset.file.h264.a2.bf2",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ) => "mixed.asset.file.h265.a1.bf0",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf2,
            ) => "mixed.asset.file.h265.a1.bf2",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf0,
            ) => "mixed.asset.file.h265.a2.bf0",
            (
                MixedInputProtocol::File,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf2,
            ) => "mixed.asset.file.h265.a2.bf2",
            (
                MixedInputProtocol::Rtmp,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ) => "mixed.live.rtmp.h264.a1.bf0",
            (
                MixedInputProtocol::Rtmp,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf2,
            ) => "mixed.live.rtmp.h264.a1.bf2",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ) => "mixed.live.srt.h264.a1.bf0",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf2,
            ) => "mixed.live.srt.h264.a1.bf2",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf0,
            ) => "mixed.live.srt.h264.a2.bf0",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf2,
            ) => "mixed.live.srt.h264.a2.bf2",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ) => "mixed.live.srt.h265.a1.bf0",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf2,
            ) => "mixed.live.srt.h265.a1.bf2",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf0,
            ) => "mixed.live.srt.h265.a2.bf0",
            (
                MixedInputProtocol::Srt,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf2,
            ) => "mixed.live.srt.h265.a2.bf2",
            _ => unreachable!("unsupported mixed input case"),
        }
    }

    pub(crate) const fn protocol(self) -> MixedInputProtocol {
        self.protocol
    }

    pub(crate) const fn codec(self) -> MixedVideoCodec {
        self.codec
    }

    pub(crate) const fn audio_layout(self) -> MixedInputAudioLayout {
        self.audio_layout
    }

    pub(crate) const fn reorder(self) -> MixedInputReorder {
        self.reorder
    }

    pub(crate) const fn source_name(self) -> &'static str {
        self.protocol().source_name()
    }

    pub(crate) const fn ingest_name(self) -> &'static str {
        self.protocol().ingest_name()
    }

    pub(crate) const fn codec_name(self) -> &'static str {
        self.codec().scenario_token()
    }

    pub(crate) const fn audio_layout_name(self) -> &'static str {
        self.audio_layout().scenario_token()
    }

    pub(crate) const fn reorder_name(self) -> &'static str {
        self.reorder().scenario_token()
    }

    pub(crate) const fn track_layout_name(self) -> &'static str {
        self.audio_layout().track_layout_name()
    }

    pub(crate) const fn is_multi_track(self) -> bool {
        self.audio_layout().is_multi_track()
    }

    pub(crate) const fn expected_video_codec(self) -> &'static str {
        self.codec().expected_video_codec()
    }

    pub(crate) const fn expected_audio_tracks(self) -> usize {
        self.audio_layout().expected_audio_tracks()
    }

    pub(crate) const fn hls_preview_expected_dimensions(self) -> &'static str {
        self.codec().hls_preview_expected_dimensions()
    }

    pub(crate) const fn source_has_b_frames(self) -> bool {
        self.reorder().has_b_frames()
    }

    pub(crate) const fn fixture_bframe_mode(self) -> AvMarkerBframeMode {
        self.reorder().fixture_mode()
    }

    pub(crate) fn artifact_rel_dir(self) -> PathBuf {
        PathBuf::from(self.source_name())
            .join(self.ingest_name())
            .join(self.codec_name())
            .join(self.audio_layout_name())
            .join(self.reorder_name())
    }

    /// Shared-stack family used by the breadth sweeps.
    ///
    /// Fast mode uses this today, and the full matrix can reuse the same
    /// grouping once we promote shared-stack waves there too.
    pub(crate) const fn shared_batch_group(self) -> MixedSharedBatchGroup {
        match self.protocol() {
            MixedInputProtocol::Rtmp => MixedSharedBatchGroup::LiveRtmp,
            MixedInputProtocol::Srt => MixedSharedBatchGroup::LiveSrt,
            MixedInputProtocol::File => MixedSharedBatchGroup::FileIngest,
        }
    }
}

/// Shared-stack family for mixed breadth waves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixedSharedBatchGroup {
    LiveRtmp,
    LiveSrt,
    FileIngest,
}

impl MixedSharedBatchGroup {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LiveRtmp => "live-rtmp",
            Self::LiveSrt => "live-srt",
            Self::FileIngest => "file-ingest",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "live-rtmp" => Some(Self::LiveRtmp),
            "live-srt" => Some(Self::LiveSrt),
            "file-ingest" => Some(Self::FileIngest),
            _ => None,
        }
    }
}

pub(crate) const MIXED_MATRIX_MODE: &str = "mixed.matrix";
pub(crate) const MIXED_FAST_BREADTH_MODE: &str = "mixed.fast-breadth";
const MIXED_ARTIFACT_ROOT: &str = "test/artifacts/mixed";

/// One selected input row in the fast breadth sweep, with its minimal checks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MixedFastBreadthCase {
    pub(crate) case: MixedInputCase,
    pub(crate) rationale: &'static str,
    pub(crate) checks: &'static [MixedCheck],
}

impl MixedFastBreadthCase {
    pub(crate) fn check_names(self) -> Vec<&'static str> {
        self.checks.iter().map(|check| check.as_str()).collect()
    }
}

/// Observation/assertion axis for mixed scenarios.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixedCheck {
    Ffprobe,
    AudioRoute,
    DecodeScan,
    Signal,
    StageSharing,
    Hls,
    Recording,
    Load,
    Smoke,
    Lifecycle,
    SinkProbe,
    HlsPutProbe,
    BurstGraph,
    SoakDrift,
}

impl MixedCheck {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ffprobe => "ffprobe",
            Self::AudioRoute => "audio-route",
            Self::DecodeScan => "decode-scan",
            Self::Signal => "signal",
            Self::StageSharing => "stage-sharing",
            Self::Hls => "hls",
            Self::Recording => "recording",
            Self::Load => "load",
            Self::Smoke => "smoke",
            Self::Lifecycle => "lifecycle",
            Self::SinkProbe => "sink-probe",
            Self::HlsPutProbe => "hls-put-probe",
            Self::BurstGraph => "burst-graph",
            Self::SoakDrift => "soak-drift",
        }
    }

    pub(crate) fn from_name(value: &str) -> Option<Self> {
        match value {
            "ffprobe" => Some(Self::Ffprobe),
            "audio-route" => Some(Self::AudioRoute),
            "decode-scan" => Some(Self::DecodeScan),
            "signal" => Some(Self::Signal),
            "stage-sharing" => Some(Self::StageSharing),
            "hls" => Some(Self::Hls),
            "recording" => Some(Self::Recording),
            "load" => Some(Self::Load),
            "smoke" => Some(Self::Smoke),
            "lifecycle" => Some(Self::Lifecycle),
            "sink-probe" => Some(Self::SinkProbe),
            "hls-put-probe" => Some(Self::HlsPutProbe),
            "burst-graph" => Some(Self::BurstGraph),
            "soak-drift" => Some(Self::SoakDrift),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslManifest {
    pub(crate) version: u32,
    pub(crate) mixed: MixedDslMatrix,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslMatrix {
    pub(crate) inputs: Vec<MixedDslInput>,
    #[serde(rename = "fastBreadth")]
    pub(crate) fast_breadth: Vec<MixedDslFastBreadth>,
    #[serde(rename = "fastBreadthBatches")]
    pub(crate) fast_breadth_batches: Vec<MixedDslFastBreadthBatch>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslInput {
    pub(crate) id: String,
    pub(crate) ingest: String,
    pub(crate) video: String,
    pub(crate) audio: String,
    pub(crate) reorder: String,
}

impl MixedDslInput {
    pub(crate) fn to_case(&self) -> Result<MixedInputCase, String> {
        let case = MixedInputCase::new(
            MixedInputProtocol::from_ingest_name(&self.ingest)
                .ok_or_else(|| format!("{} has unknown ingest {}", self.id, self.ingest))?,
            MixedVideoCodec::from_scenario_token(&self.video)
                .ok_or_else(|| format!("{} has unknown video {}", self.id, self.video))?,
            MixedInputAudioLayout::from_scenario_token(&self.audio)
                .ok_or_else(|| format!("{} has unknown audio {}", self.id, self.audio))?,
            MixedInputReorder::from_scenario_token(&self.reorder)
                .ok_or_else(|| format!("{} has unknown reorder {}", self.id, self.reorder))?,
        );
        if case.scenario_id() != self.id {
            return Err(format!(
                "DSL input {} expands to {}",
                self.id,
                case.scenario_id()
            ));
        }
        Ok(case)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslFastBreadth {
    pub(crate) id: String,
    pub(crate) rationale: String,
    pub(crate) checks: Vec<String>,
}

impl MixedDslFastBreadth {
    pub(crate) fn check_specs(&self) -> Result<Vec<MixedCheck>, String> {
        self.checks
            .iter()
            .map(|check| {
                MixedCheck::from_name(check)
                    .ok_or_else(|| format!("{} has unknown check {}", self.id, check))
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslFastBreadthBatch {
    pub(crate) group: String,
    pub(crate) cases: Vec<String>,
}

impl MixedDslManifest {
    pub(crate) fn input_cases(&self) -> Result<Vec<MixedInputCase>, String> {
        self.mixed
            .inputs
            .iter()
            .map(MixedDslInput::to_case)
            .collect()
    }
}

pub(crate) fn mixed_dsl_manifest() -> Result<MixedDslManifest, String> {
    serde_json::from_str(include_str!("mixed_matrix.json")).map_err(|error| error.to_string())
}

static MIXED_INPUT_CASES_FROM_DSL: OnceLock<Vec<MixedInputCase>> = OnceLock::new();

pub(crate) fn mixed_input_cases() -> &'static [MixedInputCase] {
    MIXED_INPUT_CASES_FROM_DSL.get_or_init(|| {
        mixed_dsl_manifest()
            .and_then(|manifest| manifest.input_cases())
            .expect("embedded mixed_matrix.json should define valid input cases")
    })
}

// Fast breadth is the "find the broad failure shape quickly without pretending
// to be exhaustive" sweep.
// It samples the 180 input/output cells by risk axes rather than by row count:
// - file ingest: BF0 startup/liveness plus BF2+HEVC+multi-audio stress
// - RTMP ingest: both sender BF0 and BF2, because RTMP timestamp/sequence-header
//   behavior differs from SRT and file ingest
// - SRT ingest: multi-audio BF0 plus HEVC BF2 codec-edge/transcode pressure
// - output matrix: each selected A1 row covers 6 RTMP/SRT source/720p/1080p
//   outputs; each selected A2 row covers the 15 all-audio/atrack variants.
//
// Keep this list small enough for a quick WSL-safe sweep. When a new input or
// output axis is added, update the rationale and the coverage unit tests below
// before relying on this mode as the first diagnostic pass. The runner defaults
// to N_PER_GROUP=1, SKIP_LOAD=1, COLLECT_FAILURES=1, and the per-row `checks`
// below so it reports the failure shape across selected cells; set those env
// vars explicitly when scale/load/signal depth is the point. `mixed.matrix`
// remains the exhaustive proof gate.
pub(crate) const MIXED_FAST_BREADTH_CASES: &[MixedFastBreadthCase] = &[
    MixedFastBreadthCase {
        case: MixedInputCase::new(
            MixedInputProtocol::File,
            MixedVideoCodec::H264,
            MixedInputAudioLayout::A1,
            MixedInputReorder::Bf0,
        ),
        rationale: "file H.264 BF0 single-audio startup hero row",
        checks: &[
            MixedCheck::Ffprobe,
            MixedCheck::StageSharing,
            MixedCheck::Hls,
        ],
    },
    MixedFastBreadthCase {
        case: MixedInputCase::new(
            MixedInputProtocol::File,
            MixedVideoCodec::H265,
            MixedInputAudioLayout::A2,
            MixedInputReorder::Bf2,
        ),
        rationale: "file HEVC BF2 multi-audio plus codec-edge outputs",
        checks: &[MixedCheck::Ffprobe, MixedCheck::StageSharing],
    },
    MixedFastBreadthCase {
        case: MixedInputCase::new(
            MixedInputProtocol::Rtmp,
            MixedVideoCodec::H264,
            MixedInputAudioLayout::A1,
            MixedInputReorder::Bf0,
        ),
        rationale: "RTMP publisher without B-frames",
        checks: &[MixedCheck::Ffprobe],
    },
    MixedFastBreadthCase {
        case: MixedInputCase::new(
            MixedInputProtocol::Rtmp,
            MixedVideoCodec::H264,
            MixedInputAudioLayout::A1,
            MixedInputReorder::Bf2,
        ),
        rationale: "RTMP publisher with B-frames",
        checks: &[MixedCheck::Ffprobe],
    },
    MixedFastBreadthCase {
        case: MixedInputCase::new(
            MixedInputProtocol::Srt,
            MixedVideoCodec::H264,
            MixedInputAudioLayout::A2,
            MixedInputReorder::Bf0,
        ),
        rationale: "SRT H.264 BF0 multi-audio adaptive-ring row",
        checks: &[MixedCheck::Ffprobe, MixedCheck::StageSharing],
    },
    MixedFastBreadthCase {
        case: MixedInputCase::new(
            MixedInputProtocol::Srt,
            MixedVideoCodec::H265,
            MixedInputAudioLayout::A2,
            MixedInputReorder::Bf2,
        ),
        rationale: "SRT HEVC BF2 multi-audio codec-edge stress row",
        checks: &[
            MixedCheck::Ffprobe,
            MixedCheck::StageSharing,
            MixedCheck::Hls,
        ],
    },
];

/// Fast-mode shared-stack batches.
///
/// Each batch reuses one restream + mediamtx setup and runs up to two input
/// pipelines concurrently inside that stack. This keeps setup cost low while
/// preserving enough isolation to attribute failures by transport family.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MixedFastBreadthBatch {
    pub(crate) group: MixedSharedBatchGroup,
    pub(crate) cases: &'static [MixedInputCase],
}

pub(crate) const MIXED_FAST_BREADTH_BATCHES: &[MixedFastBreadthBatch] = &[
    MixedFastBreadthBatch {
        group: MixedSharedBatchGroup::LiveRtmp,
        cases: &[
            MixedInputCase::new(
                MixedInputProtocol::Rtmp,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ),
            MixedInputCase::new(
                MixedInputProtocol::Rtmp,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf2,
            ),
        ],
    },
    MixedFastBreadthBatch {
        group: MixedSharedBatchGroup::LiveSrt,
        cases: &[
            MixedInputCase::new(
                MixedInputProtocol::Srt,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf0,
            ),
            MixedInputCase::new(
                MixedInputProtocol::Srt,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf2,
            ),
        ],
    },
    MixedFastBreadthBatch {
        group: MixedSharedBatchGroup::FileIngest,
        cases: &[
            MixedInputCase::new(
                MixedInputProtocol::File,
                MixedVideoCodec::H264,
                MixedInputAudioLayout::A1,
                MixedInputReorder::Bf0,
            ),
            MixedInputCase::new(
                MixedInputProtocol::File,
                MixedVideoCodec::H265,
                MixedInputAudioLayout::A2,
                MixedInputReorder::Bf2,
            ),
        ],
    },
];

pub(crate) fn mixed_input_mode_name(case: MixedInputCase) -> String {
    case.scenario_id().to_string()
}

pub(crate) fn mixed_input_case_for_command(command: &str) -> Option<MixedInputCase> {
    mixed_input_cases()
        .iter()
        .copied()
        .find(|case| case.scenario_id() == command)
}

pub(crate) fn mixed_fast_breadth_selected(case: MixedInputCase) -> &'static MixedFastBreadthCase {
    MIXED_FAST_BREADTH_CASES
        .iter()
        .find(|selected| selected.case == case)
        .unwrap_or_else(|| panic!("missing fast-breadth selection for {}", case.scenario_id()))
}

pub(crate) fn parse_mixed_fast_breadth_groups(
    value: &str,
) -> Result<Vec<MixedSharedBatchGroup>, String> {
    let mut groups = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let group = MixedSharedBatchGroup::from_str(item).ok_or_else(|| {
            format!(
                "unknown MIXED_FAST_BREADTH_GROUPS entry '{item}'; expected one of: live-rtmp, live-srt, file-ingest"
            )
        })?;
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    if groups.is_empty() {
        return Err("MIXED_FAST_BREADTH_GROUPS must select at least one batch group".to_string());
    }
    Ok(groups)
}

pub(crate) fn selected_mixed_fast_breadth_batches()
-> Result<Vec<&'static MixedFastBreadthBatch>, String> {
    let requested = std::env::var("MIXED_FAST_BREADTH_GROUPS")
        .ok()
        .filter(|value| !value.trim().is_empty());
    match requested {
        Some(value) => {
            let groups = parse_mixed_fast_breadth_groups(&value)?;
            Ok(groups
                .into_iter()
                .map(|group| {
                    MIXED_FAST_BREADTH_BATCHES
                        .iter()
                        .find(|batch| batch.group == group)
                        .expect("every fast-breadth group should have one batch")
                })
                .collect())
        }
        None => Ok(MIXED_FAST_BREADTH_BATCHES.iter().collect()),
    }
}

pub(crate) fn mixed_input_default_work_dir(case: MixedInputCase) -> PathBuf {
    PathBuf::from(MIXED_ARTIFACT_ROOT).join(case.artifact_rel_dir())
}

pub(crate) fn mixed_matrix_default_work_dir() -> PathBuf {
    PathBuf::from(MIXED_ARTIFACT_ROOT).join("matrix")
}

pub(crate) fn mixed_fast_breadth_default_work_dir() -> PathBuf {
    PathBuf::from(MIXED_ARTIFACT_ROOT).join("fast-breadth")
}

pub(crate) fn mixed_output_cases_for_input(case: MixedInputCase) -> &'static [MixedOutputCase] {
    if case.is_multi_track() {
        MULTI_TRACK_MIXED_OUTPUT_CASES
    } else {
        SINGLE_TRACK_MIXED_OUTPUT_CASES
    }
}

/// Expected unique processing-stage counts for a mixed scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MixedStageCount {
    pub(crate) video: usize,
    pub(crate) audio: usize,
    pub(crate) codec_edge: usize,
}

pub(crate) fn expected_mixed_stage_count(case: MixedInputCase) -> MixedStageCount {
    match (case.codec(), case.is_multi_track()) {
        // H.265 multi deliberately has more audio routes than H.264 multi:
        // SRT selected-track outputs preserve HEVC while RTMP selected-track
        // outputs select audio after the shared H.264 compatibility edge.
        // The important expensive-stage invariant is codec_edge=3, one per
        // video shape (source, 720p, 1080p), not one per selected audio track.
        (MixedVideoCodec::H265, true) => MixedStageCount {
            video: 2,
            audio: 12,
            codec_edge: 3,
        },
        (MixedVideoCodec::H265, false) => MixedStageCount {
            video: 2,
            audio: 0,
            codec_edge: 3,
        },
        (_, true) => MixedStageCount {
            video: 2,
            audio: 6,
            codec_edge: 0,
        },
        (_, false) => MixedStageCount {
            video: 2,
            audio: 0,
            codec_edge: 0,
        },
    }
}

/// Output transport protocol axis for mixed-matrix output rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixedOutputProtocol {
    Rtmp,
    Srt,
}

/// Complete output-side row in the mixed matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixedOutputCase {
    SingleRtmpSrcA0,
    SingleRtmp720pA0,
    SingleRtmp1080pA0,
    SingleSrtSrcA0,
    SingleSrt720pA0,
    SingleSrt1080pA0,
    MultiRtmpSrcA0,
    MultiRtmpSrcA1,
    MultiRtmp720pA0,
    MultiRtmp720pA1,
    MultiRtmp1080pA0,
    MultiRtmp1080pA1,
    MultiSrtSrcAll,
    MultiSrtSrcA0,
    MultiSrtSrcA1,
    MultiSrt720pAll,
    MultiSrt720pA0,
    MultiSrt720pA1,
    MultiSrt1080pAll,
    MultiSrt1080pA0,
    MultiSrt1080pA1,
}

impl MixedOutputCase {
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::SingleRtmpSrcA0 | Self::MultiRtmpSrcA0 => "rtmp.src.a0",
            Self::MultiRtmpSrcA1 => "rtmp.src.a1",
            Self::SingleRtmp720pA0 | Self::MultiRtmp720pA0 => "rtmp.720p.a0",
            Self::MultiRtmp720pA1 => "rtmp.720p.a1",
            Self::SingleRtmp1080pA0 | Self::MultiRtmp1080pA0 => "rtmp.1080p.a0",
            Self::MultiRtmp1080pA1 => "rtmp.1080p.a1",
            Self::SingleSrtSrcA0 => "srt.src.a0",
            Self::MultiSrtSrcAll => "srt.src.all",
            Self::MultiSrtSrcA0 => "srt.src.a0",
            Self::MultiSrtSrcA1 => "srt.src.a1",
            Self::SingleSrt720pA0 => "srt.720p.a0",
            Self::MultiSrt720pAll => "srt.720p.all",
            Self::MultiSrt720pA0 => "srt.720p.a0",
            Self::MultiSrt720pA1 => "srt.720p.a1",
            Self::SingleSrt1080pA0 => "srt.1080p.a0",
            Self::MultiSrt1080pAll => "srt.1080p.all",
            Self::MultiSrt1080pA0 => "srt.1080p.a0",
            Self::MultiSrt1080pA1 => "srt.1080p.a1",
        }
    }

    pub(crate) const fn protocol(self) -> MixedOutputProtocol {
        match self {
            Self::SingleRtmpSrcA0
            | Self::SingleRtmp720pA0
            | Self::SingleRtmp1080pA0
            | Self::MultiRtmpSrcA0
            | Self::MultiRtmpSrcA1
            | Self::MultiRtmp720pA0
            | Self::MultiRtmp720pA1
            | Self::MultiRtmp1080pA0
            | Self::MultiRtmp1080pA1 => MixedOutputProtocol::Rtmp,
            _ => MixedOutputProtocol::Srt,
        }
    }

    pub(crate) const fn encoding(self) -> &'static str {
        match self {
            Self::SingleRtmpSrcA0 | Self::SingleSrtSrcA0 | Self::MultiSrtSrcAll => "source",
            Self::MultiRtmpSrcA0 | Self::MultiSrtSrcA0 => "source+atrack:0",
            Self::MultiRtmpSrcA1 | Self::MultiSrtSrcA1 => "source+atrack:1",
            Self::SingleRtmp720pA0 | Self::SingleSrt720pA0 | Self::MultiSrt720pAll => "720p",
            Self::MultiRtmp720pA0 | Self::MultiSrt720pA0 => "720p+atrack:0",
            Self::MultiRtmp720pA1 | Self::MultiSrt720pA1 => "720p+atrack:1",
            Self::SingleRtmp1080pA0 | Self::SingleSrt1080pA0 | Self::MultiSrt1080pAll => "1080p",
            Self::MultiRtmp1080pA0 | Self::MultiSrt1080pA0 => "1080p+atrack:0",
            Self::MultiRtmp1080pA1 | Self::MultiSrt1080pA1 => "1080p+atrack:1",
        }
    }

    pub(crate) const fn expected_dimensions(self) -> &'static str {
        match self {
            Self::SingleRtmp720pA0
            | Self::SingleSrt720pA0
            | Self::MultiRtmp720pA0
            | Self::MultiRtmp720pA1
            | Self::MultiSrt720pAll
            | Self::MultiSrt720pA0
            | Self::MultiSrt720pA1 => "1280x720",
            _ => "1920x1080",
        }
    }

    pub(crate) const fn expected_audio_tracks(self) -> usize {
        match self {
            Self::MultiSrtSrcAll | Self::MultiSrt720pAll | Self::MultiSrt1080pAll => 2,
            _ => 1,
        }
    }

    pub(crate) const fn selected_audio_track(self) -> Option<usize> {
        match self {
            Self::MultiRtmpSrcA0
            | Self::MultiRtmp720pA0
            | Self::MultiRtmp1080pA0
            | Self::MultiSrtSrcA0
            | Self::MultiSrt720pA0
            | Self::MultiSrt1080pA0 => Some(0),
            Self::MultiRtmpSrcA1
            | Self::MultiRtmp720pA1
            | Self::MultiRtmp1080pA1
            | Self::MultiSrtSrcA1
            | Self::MultiSrt720pA1
            | Self::MultiSrt1080pA1 => Some(1),
            _ => None,
        }
    }
}

pub(crate) const SINGLE_TRACK_MIXED_OUTPUT_CASES: &[MixedOutputCase] = &[
    MixedOutputCase::SingleRtmpSrcA0,
    MixedOutputCase::SingleRtmp720pA0,
    MixedOutputCase::SingleRtmp1080pA0,
    MixedOutputCase::SingleSrtSrcA0,
    MixedOutputCase::SingleSrt720pA0,
    MixedOutputCase::SingleSrt1080pA0,
];

pub(crate) const MULTI_TRACK_MIXED_OUTPUT_CASES: &[MixedOutputCase] = &[
    MixedOutputCase::MultiRtmpSrcA0,
    MixedOutputCase::MultiRtmpSrcA1,
    MixedOutputCase::MultiRtmp720pA0,
    MixedOutputCase::MultiRtmp720pA1,
    MixedOutputCase::MultiRtmp1080pA0,
    MixedOutputCase::MultiRtmp1080pA1,
    MixedOutputCase::MultiSrtSrcAll,
    MixedOutputCase::MultiSrtSrcA0,
    MixedOutputCase::MultiSrtSrcA1,
    MixedOutputCase::MultiSrt720pAll,
    MixedOutputCase::MultiSrt720pA0,
    MixedOutputCase::MultiSrt720pA1,
    MixedOutputCase::MultiSrt1080pAll,
    MixedOutputCase::MultiSrt1080pA0,
    MixedOutputCase::MultiSrt1080pA1,
];

/// Source adapter used by the mixed runner for one input axis row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixedSourceAdapter {
    FileIngest,
    RtmpPublisher,
    SrtPublisher,
}

impl MixedSourceAdapter {
    pub(crate) const fn for_input(case: MixedInputCase) -> Self {
        match case.protocol() {
            MixedInputProtocol::File => Self::FileIngest,
            MixedInputProtocol::Rtmp => Self::RtmpPublisher,
            MixedInputProtocol::Srt => Self::SrtPublisher,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::FileIngest => "file-ingest",
            Self::RtmpPublisher => "rtmp-publisher",
            Self::SrtPublisher => "srt-publisher",
        }
    }
}

/// Typed source-side plan for one mixed scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MixedSourcePlan {
    pub(crate) adapter: MixedSourceAdapter,
    pub(crate) input: MixedInputCase,
}

/// Typed expansion of `source axis + output matrix + checks`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MixedScenarioPlan {
    pub(crate) input: MixedInputCase,
    pub(crate) source: MixedSourcePlan,
    pub(crate) outputs: &'static [MixedOutputCase],
    pub(crate) checks: &'static [MixedCheck],
    pub(crate) expected_stages: MixedStageCount,
}

pub(crate) const MIXED_DEFAULT_CHECKS: &[MixedCheck] = &[
    MixedCheck::Ffprobe,
    MixedCheck::AudioRoute,
    MixedCheck::DecodeScan,
    MixedCheck::Signal,
    MixedCheck::StageSharing,
    MixedCheck::Hls,
    MixedCheck::Recording,
    MixedCheck::Load,
    MixedCheck::Smoke,
    MixedCheck::Lifecycle,
    MixedCheck::SinkProbe,
    MixedCheck::HlsPutProbe,
    MixedCheck::BurstGraph,
    MixedCheck::SoakDrift,
];

impl MixedScenarioPlan {
    pub(crate) fn for_input(input: MixedInputCase) -> Self {
        Self {
            input,
            source: MixedSourcePlan {
                adapter: MixedSourceAdapter::for_input(input),
                input,
            },
            outputs: mixed_output_cases_for_input(input),
            checks: MIXED_DEFAULT_CHECKS,
            expected_stages: expected_mixed_stage_count(input),
        }
    }

    pub(crate) fn check_names(self) -> Vec<&'static str> {
        self.checks.iter().map(|check| check.as_str()).collect()
    }

    pub(crate) fn output_cells(self) -> usize {
        self.outputs.len()
    }
}

pub(crate) fn mixed_output_protocol_name(protocol: MixedOutputProtocol) -> &'static str {
    match protocol {
        MixedOutputProtocol::Rtmp => "rtmp",
        MixedOutputProtocol::Srt => "srt",
    }
}
