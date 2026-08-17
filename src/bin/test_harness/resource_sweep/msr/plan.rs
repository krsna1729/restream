use super::*;

pub(super) const MSR_RANK_COUNTS: [usize; 30] = [
    300, 150, 100, 75, 60, 50, 43, 38, 33, 30, 27, 25, 23, 21, 20, 19, 18, 17, 16, 15, 14, 14, 13,
    13, 12, 12, 11, 11, 10, 10,
];
pub(super) const MSR_TOTAL_OUTPUTS: usize = 1_200;
#[cfg(test)]
pub(super) const MSR_RTMP_OUTPUTS: usize = 1_140;
#[cfg(test)]
pub(super) const MSR_SRT_OUTPUTS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MsrProtocol {
    Rtmp,
    Srt,
}

impl MsrProtocol {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
        }
    }

    const fn default_rtmp_mode(self) -> RtmpOutputMode {
        match self {
            Self::Rtmp => RtmpOutputMode::Enhanced,
            Self::Srt => RtmpOutputMode::Legacy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MsrProtocolMix {
    Canonical,
    RtmpOnly,
    SrtOnly,
    SrtEvery(usize),
}

impl MsrProtocolMix {
    pub(super) fn from_env() -> Result<Self, String> {
        let raw = match std::env::var("MSR_PROTOCOL_MIX") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(Self::Canonical),
        };
        Self::parse(&raw)
    }

    pub(super) fn parse(raw: &str) -> Result<Self, String> {
        let value = raw.trim().to_ascii_lowercase();
        match value.as_str() {
            "canonical" | "default" | "95/5" | "95-5" | "rtmp95-srt5" => Ok(Self::Canonical),
            "rtmp" | "rtmp-only" | "rtmp_only" => Ok(Self::RtmpOnly),
            "srt" | "srt-only" | "srt_only" => Ok(Self::SrtOnly),
            _ => {
                let Some(step) = value
                    .strip_prefix("srt-every:")
                    .or_else(|| value.strip_prefix("srt_every:"))
                    .or_else(|| value.strip_prefix("every:"))
                else {
                    return Err(format!(
                        "MSR_PROTOCOL_MIX must be canonical, rtmp-only, srt-only, or srt-every:N (got {raw:?})"
                    ));
                };
                let every = step.parse::<usize>().map_err(|_| {
                    format!("MSR_PROTOCOL_MIX has invalid srt-every value {step:?}")
                })?;
                if every == 0 {
                    return Err(
                        "MSR_PROTOCOL_MIX srt-every value must be greater than zero".to_string()
                    );
                }
                Ok(Self::SrtEvery(every))
            }
        }
    }

    fn protocol_for_ordinal(self, ordinal: usize) -> MsrProtocol {
        match self {
            Self::Canonical => {
                if ordinal.is_multiple_of(20) {
                    MsrProtocol::Srt
                } else {
                    MsrProtocol::Rtmp
                }
            }
            Self::RtmpOnly => MsrProtocol::Rtmp,
            Self::SrtOnly => MsrProtocol::Srt,
            Self::SrtEvery(every) => {
                if ordinal.is_multiple_of(every) {
                    MsrProtocol::Srt
                } else {
                    MsrProtocol::Rtmp
                }
            }
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Canonical => "canonical-95-5",
            Self::RtmpOnly => "rtmp-only",
            Self::SrtOnly => "srt-only",
            Self::SrtEvery(_) => "custom-srt-every",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MsrOutputSpec {
    pub(super) ordinal: usize,
    pub(super) rank: usize,
    pub(super) language_code: &'static str,
    pub(super) language_name: &'static str,
    pub(super) protocol: MsrProtocol,
    pub(super) rtmp_mode: RtmpOutputMode,
    pub(super) encoding: String,
    pub(super) name: String,
}

impl MsrOutputSpec {
    pub(super) fn rtmp_mode_name(&self) -> Option<&'static str> {
        matches!(self.protocol, MsrProtocol::Rtmp).then(|| self.rtmp_mode.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MsrRunProfile {
    Canonical,
    SignalCalibration,
}

impl MsrRunProfile {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::SignalCalibration => "signal-calibration",
        }
    }

    pub(super) const fn scenario(self) -> &'static str {
        match self {
            Self::Canonical => "mahashivratri",
            Self::SignalCalibration => "mahashivratri-signal-calibration",
        }
    }

    pub(super) const fn output_prefix(self) -> &'static str {
        match self {
            Self::Canonical => "msr",
            Self::SignalCalibration => "msr-signal",
        }
    }

    pub(super) const fn stream_key(self) -> &'static str {
        match self {
            Self::Canonical => "msr-hero",
            Self::SignalCalibration => "msr-signal-hero",
        }
    }

    pub(super) const fn pipeline_name(self) -> &'static str {
        match self {
            Self::Canonical => "MSR hero scenario",
            Self::SignalCalibration => "MSR signal calibration",
        }
    }

    pub(super) const fn ingest_types(self) -> &'static str {
        match self {
            Self::Canonical => "h264-srt-30a",
            Self::SignalCalibration => "h264-srt-av-marker-2a",
        }
    }

    pub(super) const fn audio_tracks(self) -> usize {
        match self {
            Self::Canonical => 30,
            Self::SignalCalibration => 2,
        }
    }

    pub(super) const fn stereo_tracks(self) -> usize {
        match self {
            Self::Canonical => 29,
            Self::SignalCalibration => 2,
        }
    }

    pub(super) const fn surround_tracks(self) -> usize {
        match self {
            Self::Canonical => 1,
            Self::SignalCalibration => 0,
        }
    }

    fn output_encoding(self, rank_index: usize) -> String {
        match self {
            Self::Canonical => format!("source+atrack:{rank_index}"),
            Self::SignalCalibration => format!("source+atrack:{}", rank_index % 2),
        }
    }

    fn fixture(self) -> Result<PathBuf, String> {
        // Lets this mode run against a real high-bitrate source instead of
        // the checked-in low-bitrate fixture, without changing
        // REQUIRED_CHECKED_IN_FIXTURES or committing external media --
        // large real-media fixtures are reproducible via
        // scripts/fixtures/generate-msr-1080p-fixture.sh rather than
        // checked in.
        if let Self::Canonical = self
            && let Ok(path) = std::env::var("RESTREAM_MSR_FIXTURE_OVERRIDE")
        {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!(
                "RESTREAM_MSR_FIXTURE_OVERRIDE set but not a file: {}",
                path.display()
            ));
        }
        match self {
            Self::Canonical => restream::test_fixtures::checked_in_fixture(
                "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
            ),
            Self::SignalCalibration => {
                restream::test_fixtures::av_marker_transport_fixture("h264", true)
            }
        }
    }

    const fn publisher_selection(self) -> PublishTrackSelection {
        match self {
            Self::Canonical => PublishTrackSelection::MsrThirtyAudio,
            Self::SignalCalibration => PublishTrackSelection::AllStreams,
        }
    }
}

pub(super) fn msr_output_plan_for_mix_and_profile(
    mix: MsrProtocolMix,
    profile: MsrRunProfile,
) -> Vec<MsrOutputSpec> {
    let mut plan = Vec::with_capacity(MSR_TOTAL_OUTPUTS);
    for (rank_index, count) in MSR_RANK_COUNTS.iter().copied().enumerate() {
        for within_rank in 0..count {
            let ordinal = plan.len() + 1;
            let protocol = mix.protocol_for_ordinal(ordinal);
            plan.push(MsrOutputSpec {
                ordinal,
                rank: rank_index + 1,
                language_code: MSR_LANGUAGE_CODES[rank_index],
                language_name: MSR_LANGUAGE_NAMES[rank_index],
                protocol,
                rtmp_mode: protocol.default_rtmp_mode(),
                encoding: profile.output_encoding(rank_index),
                name: format!(
                    "{}-rank{:02}-{}-{:04}",
                    profile.output_prefix(),
                    rank_index + 1,
                    protocol.label(),
                    within_rank + 1
                ),
            });
        }
    }
    plan
}

pub(super) fn msr_output_plan_for_mix(mix: MsrProtocolMix) -> Vec<MsrOutputSpec> {
    msr_output_plan_for_mix_and_profile(mix, MsrRunProfile::Canonical)
}

#[cfg(test)]
pub(super) fn msr_output_plan() -> Vec<MsrOutputSpec> {
    msr_output_plan_for_mix(MsrProtocolMix::Canonical)
}

pub(super) fn msr_checkpoints() -> Result<Vec<usize>, String> {
    let default = if std::env::var("MSR_FULL").ok().as_deref() == Some("1") {
        "30,120,300,600,900,1200"
    } else {
        // Safe representative default. Full certification is opt-in because
        // 1,200 live outputs can exceed ordinary workstation resources.
        "30"
    };
    let mut checkpoints = parse_usize_list("MSR_OUTPUT_COUNTS", default);
    checkpoints.sort_unstable();
    checkpoints.dedup();
    if checkpoints.is_empty() {
        return Err("MSR_OUTPUT_COUNTS produced no checkpoints".to_string());
    }
    if checkpoints
        .iter()
        .any(|count| *count == 0 || *count > MSR_TOTAL_OUTPUTS)
    {
        return Err(format!(
            "MSR_OUTPUT_COUNTS entries must be in 1..={MSR_TOTAL_OUTPUTS}"
        ));
    }
    Ok(checkpoints)
}

pub(super) fn msr_plan_json(
    plan: &[MsrOutputSpec],
    checkpoints: &[usize],
    mix: MsrProtocolMix,
    profile: MsrRunProfile,
) -> Value {
    let rtmp = plan
        .iter()
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
        .count();
    let enhanced_rtmp = plan
        .iter()
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
        .filter(|output| output.rtmp_mode == RtmpOutputMode::Enhanced)
        .count();
    let srt = plan.len().saturating_sub(rtmp);
    json!({
        "mode": MSR_MODE,
        "scenario": profile.scenario(),
        "profile": profile.label(),
        "zipf": {
            "exponent": 1.0,
            "hotCount": 300,
            "rankCounts": MSR_RANK_COUNTS,
        },
        "ingest": {
            "protocol": "srt",
            "video": { "codec": "h264", "width": 1920, "height": 1080, "fps": 30 },
            "audioTracks": profile.audio_tracks(),
            "stereoTracks": profile.stereo_tracks(),
            "surroundTracks": profile.surround_tracks(),
            "surroundLayout": if profile.surround_tracks() > 0 { "5.1" } else { "none" },
        },
        "outputs": {
            "total": plan.len(),
            "rtmp": rtmp,
            "enhancedRtmp": enhanced_rtmp,
            "srt": srt,
            "protocolMix": mix.label(),
            "rtmpPercent": (rtmp * 100) / plan.len().max(1),
            "srtPercent": (srt * 100) / plan.len().max(1),
            "checkpoints": checkpoints,
        },
        "languages": MSR_LANGUAGE_NAMES,
        "languageTracks": MSR_LANGUAGE_CODES
            .iter()
            .zip(MSR_LANGUAGE_NAMES.iter())
            .enumerate()
            .map(|(index, (code, name))| json!({
                "rank": index + 1,
                "code": code,
                "name": name,
            }))
            .collect::<Vec<_>>(),
    })
}

pub(super) fn spawn_msr_publisher(
    env: &ResourceSweepEnv,
    stream_key: &str,
    profile: MsrRunProfile,
    standby: bool,
) -> Result<Child, String> {
    let fixture = profile.fixture()?;
    let role = if standby { "-standby" } else { "" };
    let log_path = env
        .work_dir
        .join(format!("publisher-{}{}.log", profile.output_prefix(), role));
    let url = append_srt_crypto(
        harness_srt_ffmpeg_url(env.restream_srt, stream_key, HarnessSrtMode::Publish, None),
        &env.srt_crypto,
    );
    spawn_publisher_with_selection(
        &fixture,
        &url,
        "mpegts",
        profile.publisher_selection(),
        Some(&log_path),
    )
}

/// The peer instance (0-based) that receives output `ordinal`, matching the
/// round-robin distribution `msr_output_url` publishes with. Shared by
/// every place that needs to reach the same peer an output actually landed
/// on: publish URL, read-back URL, and per-instance path-health grouping.
pub(super) fn msr_peer_instance(ordinal: usize, peer_count: usize) -> usize {
    ordinal % peer_count.max(1)
}

pub(super) fn msr_output_url(env: &ResourceSweepEnv, output: &MsrOutputSpec) -> String {
    let instance = msr_peer_instance(output.ordinal, env.peer_count);
    let rtmp_port = env.mtx_rtmp + instance as u16;
    let srt_port = env.mtx_srt + instance as u16;
    match output.protocol {
        MsrProtocol::Rtmp => format!("rtmp://127.0.0.1:{rtmp_port}/live/{}", output.name),
        MsrProtocol::Srt => append_srt_crypto(
            harness_srt_standard_publish_url(srt_port, &output.name),
            &env.srt_crypto,
        ),
    }
}

pub(super) fn msr_mediamtx_path(output: &MsrOutputSpec) -> String {
    match output.protocol {
        MsrProtocol::Rtmp => format!("live/{}", output.name),
        MsrProtocol::Srt => output.name.clone(),
    }
}

/// Group `outputs`' expected mediamtx paths by the peer instance each
/// output actually publishes to (see `msr_peer_instance`), sorted by
/// instance index. With `peer_count == 1` this always yields exactly one
/// group covering every path, matching the pre-multi-instance behavior.
pub(super) fn msr_group_expected_paths_by_instance(
    outputs: &[MsrOutputSpec],
    peer_count: usize,
) -> Vec<(usize, Vec<String>)> {
    let peer_count = peer_count.max(1);
    let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
    for output in outputs {
        let instance = msr_peer_instance(output.ordinal, peer_count);
        match groups.iter_mut().find(|(index, _)| *index == instance) {
            Some((_, paths)) => paths.push(msr_mediamtx_path(output)),
            None => groups.push((instance, vec![msr_mediamtx_path(output)])),
        }
    }
    groups.sort_unstable_by_key(|(index, _)| *index);
    groups
}

pub(super) fn msr_read_url(env: &ResourceSweepEnv, output: &MsrOutputSpec) -> String {
    let instance = msr_peer_instance(output.ordinal, env.peer_count);
    let rtmp_port = env.mtx_rtmp + instance as u16;
    let srt_port = env.mtx_srt + instance as u16;
    match output.protocol {
        MsrProtocol::Rtmp => format!("rtmp://127.0.0.1:{rtmp_port}/live/{}", output.name),
        MsrProtocol::Srt => {
            let mut url =
                harness_srt_ffmpeg_url(srt_port, &output.name, HarnessSrtMode::Read, None);
            if let Some(secs) = std::env::var("MSR_SRT_READ_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
            {
                let timeout_us = secs.saturating_mul(1_000_000);
                url = url.replace("timeout=30000000", &format!("timeout={timeout_us}"));
            }
            url
        }
    }
}

pub(super) fn msr_progress_timeout(output_count: usize) -> Duration {
    scaled_output_progress_timeout(
        output_count,
        env_secs("MSR_PROGRESS_TIMEOUT_BASE_SECS", 60),
        env_secs("MSR_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 2),
        env_secs("MSR_PROGRESS_TIMEOUT_CAP_SECS", 900),
    )
}
