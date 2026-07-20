//! Embedded mixed-matrix manifest loading, selection, and expected coverage.

use std::path::PathBuf;
use std::sync::OnceLock;

use restream::domain::stage::StageKind;
use restream::planner::{BackendPolicy, PlannedOutput, plan_pipeline_graph};
use serde::Deserialize;

use super::*;

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
    #[serde(rename = "rtmpMode", default)]
    pub(crate) rtmp_mode: RtmpOutputMode,
    #[serde(rename = "expectedDimensions")]
    pub(crate) expected_dimensions: &'a str,
    #[serde(rename = "expectedAudioTracks")]
    pub(crate) expected_audio_tracks: usize,
    #[serde(rename = "selectedAudioTrack")]
    pub(crate) selected_audio_track: Option<usize>,
}

impl MixedDslOutputCase<'_> {
    pub(crate) fn to_output_case(&self) -> Result<MixedOutputCase, String> {
        let protocol = MixedOutputProtocol::from_name(self.protocol)
            .ok_or_else(|| format!("{} has unknown output protocol {}", self.id, self.protocol))?;
        if !matches!(protocol, MixedOutputProtocol::Rtmp)
            && !matches!(self.rtmp_mode, RtmpOutputMode::Legacy)
        {
            return Err(format!(
                "{} sets rtmpMode for non-RTMP output protocol {}",
                self.id, self.protocol
            ));
        }
        Ok(MixedOutputCase {
            id: self.id.to_string(),
            protocol,
            encoding: self.encoding.to_string(),
            rtmp_mode: self.rtmp_mode,
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
    expected_mixed_stage_count_for_outputs(case, mixed_output_cases_for_input(case))
}

pub(crate) fn expected_mixed_stage_count_for_outputs(
    case: MixedInputCase,
    output_cases: &[MixedOutputCase],
) -> MixedStageCount {
    let outputs = output_cases
        .iter()
        .map(|output_case| {
            let url = match output_case.protocol() {
                MixedOutputProtocol::Rtmp => "rtmp://example/live/out",
                MixedOutputProtocol::Srt => "srt://example:9000?streamid=publish:out",
            };
            PlannedOutput::new(
                output_case.id().to_string(),
                output_case.output_config(),
                url,
            )
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
