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
    id: &'static str,
    protocol: MixedInputProtocol,
    codec: MixedVideoCodec,
    audio_layout: MixedInputAudioLayout,
    reorder: MixedInputReorder,
    buffered_standby: bool,
}

impl MixedInputCase {
    pub(crate) const fn new(
        id: &'static str,
        protocol: MixedInputProtocol,
        codec: MixedVideoCodec,
        audio_layout: MixedInputAudioLayout,
        reorder: MixedInputReorder,
        buffered_standby: bool,
    ) -> Self {
        Self {
            id,
            protocol,
            codec,
            audio_layout,
            reorder,
            buffered_standby,
        }
    }

    pub(crate) const fn scenario_id(self) -> &'static str {
        self.id
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

    pub(crate) const fn has_buffered_standby(self) -> bool {
        self.buffered_standby
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
pub(crate) const MIXED_SIGNAL_MODE: &str = "mixed.signal";
pub(crate) const MIXED_FAST_BREADTH_MODE: &str = "mixed.fast-breadth";
const MIXED_ARTIFACT_ROOT: &str = ".local/artifacts/mixed";

/// When the mixed harness attaches and verifies the product HLS preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HlsPreviewTiming {
    BeforeFanout,
    AfterProgress,
    Disabled,
}

impl HlsPreviewTiming {
    const ALL: [Self; 3] = [Self::BeforeFanout, Self::AfterProgress, Self::Disabled];

    pub(crate) const fn for_input(_case: MixedInputCase) -> Self {
        Self::BeforeFanout
    }

    pub(crate) fn supported_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|timing| timing.as_str()).collect()
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeFanout => "before-fanout",
            Self::AfterProgress => "after-progress",
            Self::Disabled => "disabled",
        }
    }
}

/// Which duplicate output in a repeated cell is probed by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeSamplingPolicy {
    AllDuplicates,
    FirstDuplicate,
    LastDuplicate,
    Representative { index: usize },
}

impl ProbeSamplingPolicy {
    const ALL: [Self; 4] = [
        Self::AllDuplicates,
        Self::FirstDuplicate,
        Self::LastDuplicate,
        Self::Representative { index: 1 },
    ];

    pub(crate) const fn for_input(_case: MixedInputCase) -> Self {
        Self::LastDuplicate
    }

    pub(crate) fn supported_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|policy| policy.as_str()).collect()
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AllDuplicates => "all-duplicates",
            Self::FirstDuplicate => "first-duplicate",
            Self::LastDuplicate => "last-duplicate",
            Self::Representative { .. } => "representative",
        }
    }

    pub(crate) const fn duplicate_index(self, n_per_group: usize) -> usize {
        match self {
            Self::AllDuplicates | Self::FirstDuplicate => 1,
            Self::LastDuplicate => n_per_group,
            Self::Representative { index } => index,
        }
    }
}

/// One selected input row in the fast breadth sweep, with its minimal checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MixedFastBreadthCase {
    pub(crate) case: MixedInputCase,
    pub(crate) rationale: String,
    pub(crate) checks: Vec<MixedCheck>,
}

impl MixedFastBreadthCase {
    pub(crate) fn check_names(&self) -> Vec<&'static str> {
        self.checks.iter().map(|check| check.as_str()).collect()
    }
}

/// Observation/assertion axis for mixed scenarios.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MixedCheck {
    Ffprobe,
    AudioRoute,
    DecodeScan,
    RuntimeLog,
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
            Self::RuntimeLog => "runtime-log",
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
            "runtime-log" => Some(Self::RuntimeLog),
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
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct MixedDslManifest<'a> {
    #[allow(dead_code)]
    pub(crate) version: u32,
    pub(crate) mixed: MixedDslMatrix<'a>,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct MixedDslMatrix<'a> {
    pub(crate) inputs: Vec<MixedDslInput<'a>>,
    pub(crate) outputs: MixedDslOutputMatrices<'a>,
    #[serde(rename = "defaultChecks")]
    pub(crate) default_checks: Vec<&'a str>,
    #[serde(rename = "signalSentinels")]
    pub(crate) signal_sentinels: Vec<MixedDslFastBreadth<'a>>,
    #[serde(rename = "signalBatches")]
    pub(crate) signal_batches: Vec<MixedDslFastBreadthBatch<'a>>,
    #[serde(rename = "fastBreadth")]
    pub(crate) fast_breadth: Vec<MixedDslFastBreadth<'a>>,
    #[serde(rename = "fastBreadthBatches")]
    pub(crate) fast_breadth_batches: Vec<MixedDslFastBreadthBatch<'a>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslInput<'a> {
    pub(crate) id: &'a str,
    pub(crate) ingest: &'a str,
    pub(crate) video: &'a str,
    pub(crate) audio: &'a str,
    pub(crate) reorder: &'a str,
    #[serde(default, rename = "bufferedStandby")]
    pub(crate) buffered_standby: bool,
}

impl MixedDslInput<'static> {
    pub(crate) fn to_case(&self) -> Result<MixedInputCase, String> {
        let protocol = MixedInputProtocol::from_ingest_name(self.ingest)
            .ok_or_else(|| format!("{} has unknown ingest {}", self.id, self.ingest))?;
        let codec = MixedVideoCodec::from_scenario_token(self.video)
            .ok_or_else(|| format!("{} has unknown video {}", self.id, self.video))?;
        let audio_layout = MixedInputAudioLayout::from_scenario_token(self.audio)
            .ok_or_else(|| format!("{} has unknown audio {}", self.id, self.audio))?;
        let reorder = MixedInputReorder::from_scenario_token(self.reorder)
            .ok_or_else(|| format!("{} has unknown reorder {}", self.id, self.reorder))?;
        let expected = format!(
            "mixed.{}.{}.{}.{}.{}",
            protocol.source_name(),
            protocol.ingest_name(),
            codec.scenario_token(),
            audio_layout.scenario_token(),
            reorder.scenario_token()
        );
        if expected != self.id {
            return Err(format!("DSL input {} expands to {}", self.id, expected));
        }
        Ok(MixedInputCase::new(
            self.id,
            protocol,
            codec,
            audio_layout,
            reorder,
            self.buffered_standby,
        ))
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslFastBreadth<'a> {
    pub(crate) id: &'a str,
    pub(crate) rationale: String,
    pub(crate) checks: Vec<&'a str>,
}

impl MixedDslFastBreadth<'_> {
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
pub(crate) struct MixedDslFastBreadthBatch<'a> {
    pub(crate) group: &'a str,
    pub(crate) cases: Vec<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct MixedDslOutputMatrices<'a> {
    #[serde(rename = "singleTrack")]
    pub(crate) single_track: Vec<MixedDslOutputCase<'a>>,
    #[serde(rename = "multiTrack")]
    pub(crate) multi_track: Vec<MixedDslOutputCase<'a>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MixedDslOutputCase<'a> {
    pub(crate) id: &'a str,
    pub(crate) protocol: &'a str,
    pub(crate) encoding: &'a str,
    #[serde(rename = "expectedDimensions")]
    pub(crate) expected_dimensions: &'a str,
    #[serde(rename = "expectedAudioTracks")]
    pub(crate) expected_audio_tracks: usize,
    #[serde(rename = "selectedAudioTrack")]
    pub(crate) selected_audio_track: Option<usize>,
}

impl MixedDslOutputCase<'_> {
    pub(crate) fn to_output_case(&self) -> Result<MixedOutputCase, String> {
        Ok(MixedOutputCase {
            id: self.id.to_string(),
            protocol: MixedOutputProtocol::from_name(self.protocol).ok_or_else(|| {
                format!("{} has unknown output protocol {}", self.id, self.protocol)
            })?,
            encoding: self.encoding.to_string(),
            expected_dimensions: self.expected_dimensions.to_string(),
            expected_audio_tracks: self.expected_audio_tracks,
            selected_audio_track: self.selected_audio_track,
        })
    }
}

impl MixedDslManifest<'static> {
    pub(crate) fn input_cases(&self) -> Result<Vec<MixedInputCase>, String> {
        self.mixed
            .inputs
            .iter()
            .map(MixedDslInput::to_case)
            .collect()
    }
}

pub(crate) fn mixed_dsl_manifest() -> Result<MixedDslManifest<'static>, String> {
    serde_json::from_str(include_str!("mixed_matrix.json")).map_err(|error| error.to_string())
}

static MIXED_INPUT_CASES_FROM_DSL: OnceLock<Vec<MixedInputCase>> = OnceLock::new();
static MIXED_SIGNAL_SENTINELS_FROM_DSL: OnceLock<Vec<MixedFastBreadthCase>> = OnceLock::new();
static MIXED_SIGNAL_BATCHES_FROM_DSL: OnceLock<Vec<MixedFastBreadthBatch>> = OnceLock::new();
static MIXED_FAST_BREADTH_CASES_FROM_DSL: OnceLock<Vec<MixedFastBreadthCase>> = OnceLock::new();
static MIXED_FAST_BREADTH_BATCHES_FROM_DSL: OnceLock<Vec<MixedFastBreadthBatch>> = OnceLock::new();
static SINGLE_TRACK_MIXED_OUTPUT_CASES_FROM_DSL: OnceLock<Vec<MixedOutputCase>> = OnceLock::new();
static MULTI_TRACK_MIXED_OUTPUT_CASES_FROM_DSL: OnceLock<Vec<MixedOutputCase>> = OnceLock::new();
static MIXED_DEFAULT_CHECKS_FROM_DSL: OnceLock<Vec<MixedCheck>> = OnceLock::new();

pub(crate) fn mixed_input_cases() -> &'static [MixedInputCase] {
    MIXED_INPUT_CASES_FROM_DSL.get_or_init(|| {
        mixed_dsl_manifest()
            .and_then(|manifest| manifest.input_cases())
            .expect("embedded mixed_matrix.json should define valid input cases")
    })
}

pub(crate) fn mixed_fast_breadth_cases() -> &'static [MixedFastBreadthCase] {
    MIXED_FAST_BREADTH_CASES_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        manifest
            .mixed
            .fast_breadth
            .iter()
            .map(|row| MixedFastBreadthCase {
                case: mixed_input_case_from_manifest(row.id),
                rationale: row.rationale.clone(),
                checks: row
                    .check_specs()
                    .unwrap_or_else(|error| panic!("invalid fast-breadth checks: {error}")),
            })
            .collect()
    })
}

pub(crate) fn mixed_signal_sentinels() -> &'static [MixedFastBreadthCase] {
    MIXED_SIGNAL_SENTINELS_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        manifest
            .mixed
            .signal_sentinels
            .iter()
            .map(|row| MixedFastBreadthCase {
                case: mixed_input_case_from_manifest(row.id),
                rationale: row.rationale.clone(),
                checks: row
                    .check_specs()
                    .unwrap_or_else(|error| panic!("invalid signal-sentinel checks: {error}")),
            })
            .collect()
    })
}

pub(crate) fn mixed_signal_batches() -> &'static [MixedFastBreadthBatch] {
    MIXED_SIGNAL_BATCHES_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        manifest
            .mixed
            .signal_batches
            .iter()
            .map(|batch| MixedFastBreadthBatch {
                group: MixedSharedBatchGroup::from_str(batch.group)
                    .unwrap_or_else(|| panic!("{} is not a signal batch group", batch.group)),
                cases: batch
                    .cases
                    .iter()
                    .map(|case| mixed_input_case_from_manifest(case))
                    .collect(),
            })
            .collect()
    })
}

pub(crate) fn mixed_fast_breadth_batches() -> &'static [MixedFastBreadthBatch] {
    MIXED_FAST_BREADTH_BATCHES_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        manifest
            .mixed
            .fast_breadth_batches
            .iter()
            .map(|batch| MixedFastBreadthBatch {
                group: MixedSharedBatchGroup::from_str(batch.group)
                    .unwrap_or_else(|| panic!("{} is not a fast-breadth batch group", batch.group)),
                cases: batch
                    .cases
                    .iter()
                    .map(|case| mixed_input_case_from_manifest(case))
                    .collect(),
            })
            .collect()
    })
}

fn output_cases_from_dsl(rows: &[MixedDslOutputCase<'_>], label: &str) -> Vec<MixedOutputCase> {
    rows.iter()
        .map(|row| {
            row.to_output_case()
                .unwrap_or_else(|error| panic!("invalid {label} output row: {error}"))
        })
        .collect()
}

pub(crate) fn single_track_mixed_output_cases() -> &'static [MixedOutputCase] {
    SINGLE_TRACK_MIXED_OUTPUT_CASES_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        output_cases_from_dsl(&manifest.mixed.outputs.single_track, "single-track")
    })
}

pub(crate) fn multi_track_mixed_output_cases() -> &'static [MixedOutputCase] {
    MULTI_TRACK_MIXED_OUTPUT_CASES_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        output_cases_from_dsl(&manifest.mixed.outputs.multi_track, "multi-track")
    })
}

pub(crate) fn mixed_default_checks() -> &'static [MixedCheck] {
    MIXED_DEFAULT_CHECKS_FROM_DSL.get_or_init(|| {
        let manifest = mixed_dsl_manifest().expect("embedded mixed_matrix.json should parse");
        manifest
            .mixed
            .default_checks
            .iter()
            .map(|check| {
                MixedCheck::from_name(check)
                    .unwrap_or_else(|| panic!("{check} is not a known mixed check"))
            })
            .collect()
    })
}

pub(crate) fn mixed_output_cases_for_input(case: MixedInputCase) -> &'static [MixedOutputCase] {
    if case.is_multi_track() {
        multi_track_mixed_output_cases()
    } else {
        single_track_mixed_output_cases()
    }
}

/// Fast-mode shared-stack batches.
///
/// Each batch reuses one restream + mediamtx setup and runs up to two input
/// pipelines concurrently inside that stack. This keeps setup cost low while
/// preserving enough isolation to attribute failures by transport family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MixedFastBreadthBatch {
    pub(crate) group: MixedSharedBatchGroup,
    pub(crate) cases: Vec<MixedInputCase>,
}

pub(crate) fn mixed_input_mode_name(case: MixedInputCase) -> String {
    case.scenario_id().to_string()
}

pub(crate) fn mixed_input_case_for_command(command: &str) -> Option<MixedInputCase> {
    mixed_input_cases()
        .iter()
        .copied()
        .find(|case| case.scenario_id() == command)
}

fn mixed_input_case_from_manifest(id: &str) -> MixedInputCase {
    mixed_input_case_for_command(id).unwrap_or_else(|| panic!("{id} is not a mixed input case"))
}

pub(crate) fn mixed_fast_breadth_selected(case: MixedInputCase) -> &'static MixedFastBreadthCase {
    mixed_fast_breadth_cases()
        .iter()
        .find(|selected| selected.case == case)
        .unwrap_or_else(|| panic!("missing fast-breadth selection for {}", case.scenario_id()))
}

pub(crate) fn mixed_signal_selected(case: MixedInputCase) -> &'static MixedFastBreadthCase {
    mixed_signal_sentinels()
        .iter()
        .find(|selected| selected.case == case)
        .unwrap_or_else(|| panic!("missing signal sentinel for {}", case.scenario_id()))
}

fn parse_mixed_shared_batch_groups(
    value: &str,
    env_name: &str,
) -> Result<Vec<MixedSharedBatchGroup>, String> {
    let mut groups = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let group = MixedSharedBatchGroup::from_str(item).ok_or_else(|| {
            format!(
                "unknown {env_name} entry '{item}'; expected one of: live-rtmp, live-srt, file-ingest"
            )
        })?;
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    if groups.is_empty() {
        return Err(format!("{env_name} must select at least one batch group"));
    }
    Ok(groups)
}

pub(crate) fn parse_mixed_fast_breadth_groups(
    value: &str,
) -> Result<Vec<MixedSharedBatchGroup>, String> {
    parse_mixed_shared_batch_groups(value, "MIXED_FAST_BREADTH_GROUPS")
}

pub(crate) fn parse_mixed_signal_groups(value: &str) -> Result<Vec<MixedSharedBatchGroup>, String> {
    parse_mixed_shared_batch_groups(value, "MIXED_SIGNAL_GROUPS")
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
                    mixed_fast_breadth_batches()
                        .iter()
                        .find(|batch| batch.group == group)
                        .expect("every fast-breadth group should have one batch")
                })
                .collect())
        }
        None => Ok(mixed_fast_breadth_batches().iter().collect()),
    }
}

pub(crate) fn selected_mixed_signal_batches() -> Result<Vec<&'static MixedFastBreadthBatch>, String>
{
    let requested = std::env::var("MIXED_SIGNAL_GROUPS")
        .ok()
        .filter(|value| !value.trim().is_empty());
    match requested {
        Some(value) => {
            let groups = parse_mixed_signal_groups(&value)?;
            Ok(groups
                .into_iter()
                .map(|group| {
                    mixed_signal_batches()
                        .iter()
                        .find(|batch| batch.group == group)
                        .expect("every signal batch group should have one batch")
                })
                .collect())
        }
        None => Ok(mixed_signal_batches().iter().collect()),
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

pub(crate) fn mixed_signal_default_work_dir() -> PathBuf {
    PathBuf::from(MIXED_ARTIFACT_ROOT).join("signal")
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

impl MixedOutputProtocol {
    pub(crate) fn from_name(value: &str) -> Option<Self> {
        match value {
            "rtmp" => Some(Self::Rtmp),
            "srt" => Some(Self::Srt),
            _ => None,
        }
    }
}

/// Complete output-side row in the mixed matrix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MixedOutputCase {
    id: String,
    protocol: MixedOutputProtocol,
    encoding: String,
    expected_dimensions: String,
    expected_audio_tracks: usize,
    selected_audio_track: Option<usize>,
}

impl MixedOutputCase {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) const fn protocol(&self) -> MixedOutputProtocol {
        self.protocol
    }

    pub(crate) fn encoding(&self) -> &str {
        &self.encoding
    }

    pub(crate) fn expected_dimensions(&self) -> &str {
        &self.expected_dimensions
    }

    pub(crate) const fn expected_audio_tracks(&self) -> usize {
        self.expected_audio_tracks
    }

    pub(crate) const fn selected_audio_track(&self) -> Option<usize> {
        self.selected_audio_track
    }
}

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
    pub(crate) hls_preview_timing: HlsPreviewTiming,
    pub(crate) probe_sampling_policy: ProbeSamplingPolicy,
    pub(crate) expected_stages: MixedStageCount,
}

impl MixedScenarioPlan {
    pub(crate) fn for_input(input: MixedInputCase) -> Self {
        Self {
            input,
            source: MixedSourcePlan {
                adapter: MixedSourceAdapter::for_input(input),
                input,
            },
            outputs: mixed_output_cases_for_input(input),
            checks: mixed_default_checks(),
            hls_preview_timing: HlsPreviewTiming::for_input(input),
            probe_sampling_policy: ProbeSamplingPolicy::for_input(input),
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

#[cfg(test)]
mod tests {
    use super::*;
    use restream::domain::stage::StageKind;
    use restream::planner::backend_policy::BackendPolicy;
    use restream::planner::graph_plan::{PlannedOutput, plan_pipeline_graph};

    fn planned_stage_count_from_graph(
        case: MixedInputCase,
        duplicates_per_output: usize,
    ) -> MixedStageCount {
        let outputs = mixed_output_cases_for_input(case)
            .iter()
            .flat_map(|output_case| {
                (0..duplicates_per_output).map(move |duplicate| {
                    let url = match output_case.protocol() {
                        MixedOutputProtocol::Rtmp => "rtmp://example/live/out",
                        MixedOutputProtocol::Srt => "srt://example:9000?streamid=publish:out",
                    };
                    PlannedOutput::new(
                        format!("{}-{duplicate}", output_case.id()),
                        output_case.encoding(),
                        url,
                    )
                })
            })
            .collect::<Vec<_>>();
        let plan = plan_pipeline_graph(
            "pipe",
            Some(case.expected_video_codec()),
            &outputs,
            false,
            &BackendPolicy::default(),
        );

        let mut counts = MixedStageCount {
            video: 0,
            audio: 0,
            codec_edge: 0,
        };
        for stage in plan.stages {
            match stage.kind {
                StageKind::VideoPreset { .. } => counts.video += 1,
                StageKind::AudioRoute { .. } => counts.audio += 1,
                StageKind::CodecEdge { .. } => counts.codec_edge += 1,
                StageKind::Source
                | StageKind::Hls
                | StageKind::HlsSegmenter { .. }
                | StageKind::Recording
                | StageKind::Preview { .. } => {}
            }
        }
        counts
    }

    #[test]
    fn mixed_expected_stage_counts_match_graph_planner() {
        for case in mixed_input_cases() {
            let expected = expected_mixed_stage_count(*case);
            let single = planned_stage_count_from_graph(*case, 1);
            let duplicated = planned_stage_count_from_graph(*case, 2);

            assert_eq!(
                single,
                expected,
                "{} expected stage count should match StageGraphPlan",
                case.scenario_id()
            );
            assert_eq!(
                duplicated,
                expected,
                "{} duplicate output rows should not add unique planned stages",
                case.scenario_id()
            );
        }
    }

    #[test]
    fn mixed_scenario_plan_names_phase_f_execution_policies() {
        for case in mixed_input_cases() {
            let plan = MixedScenarioPlan::for_input(*case);
            assert_eq!(
                plan.hls_preview_timing,
                HlsPreviewTiming::BeforeFanout,
                "{} should attach HLS preview before output fanout by default",
                case.scenario_id()
            );
            assert_eq!(
                plan.probe_sampling_policy,
                ProbeSamplingPolicy::LastDuplicate,
                "{} should report the duplicated output sampled by probes",
                case.scenario_id()
            );
        }
    }

    #[test]
    fn mixed_matrix_and_fast_breadth_cover_rtmp_and_srt_buffered_standbys() {
        let matrix_protocols = mixed_input_cases()
            .iter()
            .filter(|case| case.has_buffered_standby())
            .map(|case| case.protocol())
            .collect::<Vec<_>>();
        let breadth_protocols = mixed_fast_breadth_cases()
            .iter()
            .filter(|row| row.case.has_buffered_standby())
            .map(|row| row.case.protocol())
            .collect::<Vec<_>>();

        assert_eq!(
            matrix_protocols,
            vec![MixedInputProtocol::Rtmp, MixedInputProtocol::Srt]
        );
        assert_eq!(breadth_protocols, matrix_protocols);
    }
}
