use super::*;
use crate::mediamtx_probe::{
    MediaMtxPathHealth, mediamtx_path_health_json, verify_mediamtx_path_health,
};

pub(crate) const MSR_MODE: &str = "msr";
pub(crate) const MSR_DASHBOARD_MODE: &str = "msr.dashboard";
const MSR_RANK_COUNTS: [usize; 30] = [
    300, 150, 100, 75, 60, 50, 43, 38, 33, 30, 27, 25, 23, 21, 20, 19, 18, 17, 16, 15, 14, 14, 13,
    13, 12, 12, 11, 11, 10, 10,
];
const MSR_TOTAL_OUTPUTS: usize = 1_200;
#[cfg(test)]
const MSR_RTMP_OUTPUTS: usize = 1_140;
#[cfg(test)]
const MSR_SRT_OUTPUTS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MsrProtocol {
    Rtmp,
    Srt,
}

impl MsrProtocol {
    const fn label(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MsrProtocolMix {
    Canonical,
    RtmpOnly,
    SrtOnly,
    SrtEvery(usize),
}

impl MsrProtocolMix {
    fn from_env() -> Result<Self, String> {
        let raw = match std::env::var("MSR_PROTOCOL_MIX") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(Self::Canonical),
        };
        Self::parse(&raw)
    }

    fn parse(raw: &str) -> Result<Self, String> {
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

    const fn label(self) -> &'static str {
        match self {
            Self::Canonical => "canonical-95-5",
            Self::RtmpOnly => "rtmp-only",
            Self::SrtOnly => "srt-only",
            Self::SrtEvery(_) => "custom-srt-every",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MsrOutputSpec {
    ordinal: usize,
    rank: usize,
    language_code: &'static str,
    language_name: &'static str,
    protocol: MsrProtocol,
    encoding: String,
    name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MsrRunProfile {
    Canonical,
    SignalCalibration,
}

impl MsrRunProfile {
    const fn label(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::SignalCalibration => "signal-calibration",
        }
    }

    const fn scenario(self) -> &'static str {
        match self {
            Self::Canonical => "mahashivratri",
            Self::SignalCalibration => "mahashivratri-signal-calibration",
        }
    }

    const fn output_prefix(self) -> &'static str {
        match self {
            Self::Canonical => "msr",
            Self::SignalCalibration => "msr-signal",
        }
    }

    const fn stream_key(self) -> &'static str {
        match self {
            Self::Canonical => "msr-hero",
            Self::SignalCalibration => "msr-signal-hero",
        }
    }

    const fn pipeline_name(self) -> &'static str {
        match self {
            Self::Canonical => "MSR hero scenario",
            Self::SignalCalibration => "MSR signal calibration",
        }
    }

    const fn ingest_types(self) -> &'static str {
        match self {
            Self::Canonical => "h264-srt-30a",
            Self::SignalCalibration => "h264-srt-av-marker-2a",
        }
    }

    const fn audio_tracks(self) -> usize {
        match self {
            Self::Canonical => 30,
            Self::SignalCalibration => 2,
        }
    }

    const fn stereo_tracks(self) -> usize {
        match self {
            Self::Canonical => 29,
            Self::SignalCalibration => 2,
        }
    }

    const fn surround_tracks(self) -> usize {
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

struct MsrCheckpointAggregate {
    resource: ResourceAggregate,
    path_health: MediaMtxPathHealth,
    post_sample_path_health: MediaMtxPathHealth,
    ffprobe_checks: Vec<Value>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct MsrDashboardPipeline {
    id: String,
    name: String,
    stream_key: String,
    role: &'static str,
}

#[derive(Clone)]
struct MsrDashboardEnv {
    resource: ResourceSweepEnv,
    hero_seed_outputs: usize,
    playwright_runtime_secs: u64,
    churn_outputs_per_pipeline: usize,
    diagnostics_every_cycles: usize,
    playwright_log: PathBuf,
    playwright_summary_json: PathBuf,
}

impl MsrDashboardEnv {
    fn from_env() -> Result<Self, String> {
        let mut resource =
            ResourceSweepEnv::from_env_with_default_dir(".local/artifacts/msr-dashboard")?;
        resource.no_cleanup = std::env::var("MSR_DASHBOARD_NO_CLEANUP")
            .ok()
            .or_else(|| std::env::var("MSR_NO_CLEANUP").ok())
            .is_some_and(|value| value == "1");
        let default_seed_outputs = if std::env::var("MSR_FULL").ok().as_deref() == Some("1") {
            120
        } else {
            30
        };
        let hero_seed_outputs = env_usize("MSR_DASHBOARD_HERO_OUTPUTS", default_seed_outputs)
            .clamp(1, MSR_TOTAL_OUTPUTS);
        Ok(Self {
            playwright_runtime_secs: env_secs("MSR_DASHBOARD_RUNTIME_SECS", 1800),
            churn_outputs_per_pipeline: env_usize("MSR_DASHBOARD_CHURN_OUTPUTS_PER_PIPELINE", 3)
                .max(1),
            diagnostics_every_cycles: env_usize("MSR_DASHBOARD_DIAGNOSTICS_EVERY_CYCLES", 3).max(1),
            playwright_log: resource.work_dir.join("playwright.log"),
            playwright_summary_json: resource.work_dir.join("playwright-summary.json"),
            resource,
            hero_seed_outputs,
        })
    }
}

fn msr_output_plan_for_mix_and_profile(
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

fn msr_output_plan_for_mix(mix: MsrProtocolMix) -> Vec<MsrOutputSpec> {
    msr_output_plan_for_mix_and_profile(mix, MsrRunProfile::Canonical)
}

#[cfg(test)]
fn msr_output_plan() -> Vec<MsrOutputSpec> {
    msr_output_plan_for_mix(MsrProtocolMix::Canonical)
}

fn msr_checkpoints() -> Result<Vec<usize>, String> {
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

fn msr_plan_json(
    plan: &[MsrOutputSpec],
    checkpoints: &[usize],
    mix: MsrProtocolMix,
    profile: MsrRunProfile,
) -> Value {
    let rtmp = plan
        .iter()
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
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

fn spawn_msr_publisher(
    env: &ResourceSweepEnv,
    stream_key: &str,
    profile: MsrRunProfile,
) -> Result<Child, String> {
    let fixture = profile.fixture()?;
    let log_path = env
        .work_dir
        .join(format!("publisher-{}.log", profile.output_prefix()));
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

fn msr_output_url(env: &ResourceSweepEnv, output: &MsrOutputSpec) -> String {
    match output.protocol {
        MsrProtocol::Rtmp => format!("rtmp://127.0.0.1:{}/live/{}", env.mtx_rtmp, output.name),
        MsrProtocol::Srt => harness_srt_standard_publish_url(env.mtx_srt, &output.name),
    }
}

fn msr_mediamtx_path(output: &MsrOutputSpec) -> String {
    match output.protocol {
        MsrProtocol::Rtmp => format!("live/{}", output.name),
        MsrProtocol::Srt => output.name.clone(),
    }
}

fn msr_read_url(env: &ResourceSweepEnv, output: &MsrOutputSpec) -> String {
    match output.protocol {
        MsrProtocol::Rtmp => format!("rtmp://127.0.0.1:{}/live/{}", env.mtx_rtmp, output.name),
        MsrProtocol::Srt => {
            harness_srt_ffmpeg_url(env.mtx_srt, &output.name, HarnessSrtMode::Read, None)
        }
    }
}

fn msr_progress_timeout(output_count: usize) -> Duration {
    scaled_output_progress_timeout(
        output_count,
        env_secs("MSR_PROGRESS_TIMEOUT_BASE_SECS", 60),
        env_secs("MSR_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 2),
        env_secs("MSR_PROGRESS_TIMEOUT_CAP_SECS", 900),
    )
}

fn msr_checkpoint_aggregate_json(aggregate: &MsrCheckpointAggregate) -> Value {
    let mut value = resource_aggregate_json(&aggregate.resource);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "mediamtxPathHealth".to_string(),
            mediamtx_path_health_json(MSR_MODE, &aggregate.resource.label, &aggregate.path_health),
        );
        object.insert(
            "mediamtxPostSamplePathHealth".to_string(),
            mediamtx_path_health_json(
                MSR_MODE,
                &format!("{}-post-sample", aggregate.resource.label),
                &aggregate.post_sample_path_health,
            ),
        );
        object.insert(
            "ffprobeSamples".to_string(),
            Value::Array(aggregate.ffprobe_checks.clone()),
        );
    }
    value
}

fn human_kib(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.2} GB", kb as f64 / 1024.0 / 1024.0)
    } else if kb >= 1024 {
        format!("{:.0} MB", kb as f64 / 1024.0)
    } else {
        format!("{kb} KB")
    }
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_msr_report(
    executed_outputs: usize,
    audio_tracks: usize,
    rtmp_outputs: usize,
    srt_outputs: usize,
    aggregates: &[MsrCheckpointAggregate],
) -> String {
    let mut report = format!(
        "Status: PASS at every checkpoint including {executed_outputs} outputs \
         (1 SRT ingest, {audio_tracks} audio tracks, Zipf fan-out, {rtmp_outputs} RTMP / {srt_outputs} SRT, \
         1080p30 H.264 passthrough, loopback MediaMTX path API byte-growth proof).\n\n"
    );
    report.push_str("| Outputs | Egress mix | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |\n");
    report.push_str("|---:|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for aggregate in aggregates {
        let resource = &aggregate.resource;
        let path_health = &aggregate.path_health;
        report.push_str(&format!(
            "| {} | {} | {}/{} | {} | {:.1} | {:.1} | {} | {} | {} |\n",
            resource.outputs,
            resource.egress_mix,
            path_health.ready_paths,
            path_health.expected_paths,
            human_bytes(path_health.bytes_received_delta),
            resource.total_cpu_avg_pct,
            resource.total_cpu_peak_pct,
            human_kib(resource.rss_peak_kb),
            human_kib(resource.avio_hwm_peak_kb),
            resource.sample_count,
        ));
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    report.push_str(&format!(
        "\nCPU % is of a single core ({}% available on this host). MediaMTX proof is from `/v3/paths/list`: every expected path must be ready and `bytesReceived` must grow across the sample window before a checkpoint can pass.\n",
        cores * 100
    ));
    report
}

fn msr_ffprobe_sample_count(output_count: usize) -> usize {
    let default = if std::env::var("MSR_FULL").ok().as_deref() == Some("1") {
        "60"
    } else {
        "4"
    };
    env_usize("MSR_FFPROBE_SAMPLE_COUNT", default.parse().unwrap())
        .min(output_count)
        .max(usize::from(output_count > 0))
}

fn msr_ffprobe_seed() -> u64 {
    std::env::var("MSR_FFPROBE_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0x5eed_5eed_cafe_babe)
}

fn msr_ffprobe_detection_confidence(
    sample_count: usize,
    population: usize,
    defect_rate: f64,
) -> f64 {
    if population == 0 || sample_count == 0 || defect_rate <= 0.0 {
        return 0.0;
    }
    let bad = ((population as f64) * defect_rate)
        .ceil()
        .clamp(1.0, population as f64) as usize;
    let sample_count = sample_count.min(population);
    let mut miss_probability = 1.0f64;
    for draw in 0..sample_count {
        let remaining = population.saturating_sub(draw);
        let good_remaining = population.saturating_sub(bad).saturating_sub(draw);
        if remaining == 0 {
            break;
        }
        miss_probability *= good_remaining as f64 / remaining as f64;
    }
    (1.0 - miss_probability).clamp(0.0, 1.0)
}

fn msr_ffprobe_sample_outputs(
    outputs: &[MsrOutputSpec],
    sample_count: usize,
    seed: u64,
) -> Vec<MsrOutputSpec> {
    if outputs.is_empty() || sample_count == 0 {
        return Vec::new();
    }
    let mut indexes = Vec::with_capacity(outputs.len());
    let mut state = seed;
    for index in 0..outputs.len() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        indexes.push((state, index));
    }
    indexes.sort_unstable_by_key(|(rank, _)| *rank);
    let mut selected = indexes
        .into_iter()
        .take(sample_count.min(outputs.len()))
        .map(|(_, index)| index)
        .collect::<Vec<_>>();
    if !selected
        .iter()
        .any(|index| outputs[*index].protocol == MsrProtocol::Srt)
        && let Some(srt_index) = outputs
            .iter()
            .position(|output| output.protocol == MsrProtocol::Srt)
    {
        if selected.len() < sample_count.min(outputs.len()) {
            selected.push(srt_index);
        } else if let Some(last) = selected.last_mut() {
            *last = srt_index;
        }
    }
    selected.sort_unstable();
    selected.dedup();
    selected
        .into_iter()
        .map(|index| outputs[index].clone())
        .collect()
}

fn validate_msr_ffprobe_sample(output: &MsrOutputSpec, probe: &Value) -> Result<Value, String> {
    let streams = probe["streams"]
        .as_array()
        .ok_or_else(|| format!("{} ffprobe output has no streams array", output.name))?;
    let video_stream = streams
        .iter()
        .find(|stream| stream["codec_type"].as_str() == Some("video"))
        .ok_or_else(|| format!("{} ffprobe did not find a video stream", output.name))?;
    let video_codec = video_stream["codec_name"].as_str().unwrap_or("");
    if !matches!(video_codec, "h264" | "hevc") {
        return Err(format!(
            "{} ffprobe found unexpected video codec {video_codec:?}",
            output.name
        ));
    }
    let audio_streams = streams
        .iter()
        .filter(|stream| stream["codec_type"].as_str() == Some("audio"))
        .count();
    if audio_streams == 0 {
        return Err(format!(
            "{} ffprobe did not find an audio stream",
            output.name
        ));
    }
    let video_index = video_stream["index"].as_i64().unwrap_or(0);
    let mut video_packets = 0usize;
    let mut last_dts = None;
    for packet in probe["packets"].as_array().into_iter().flatten() {
        if packet["stream_index"].as_i64() != Some(video_index) {
            continue;
        }
        let Some(dts) = packet["dts_time"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok())
        else {
            continue;
        };
        if let Some(previous) = last_dts
            && dts < previous
        {
            return Err(format!(
                "{} ffprobe observed non-monotone video DTS ({previous} -> {dts})",
                output.name
            ));
        }
        last_dts = Some(dts);
        video_packets += 1;
    }
    if video_packets == 0 {
        return Err(format!(
            "{} ffprobe did not capture any video packets",
            output.name
        ));
    }
    Ok(json!({
        "videoCodec": video_codec,
        "audioStreams": audio_streams,
        "videoPackets": video_packets,
    }))
}

async fn seed_msr_dashboard_hero_outputs(
    env: &ResourceSweepEnv,
    stack: &RampApi,
    pipeline_id: &str,
    plan: &[MsrOutputSpec],
    output_count: usize,
) -> Result<Vec<String>, String> {
    let mut output_ids = Vec::with_capacity(output_count);
    for output in plan.iter().take(output_count) {
        let url = msr_output_url(env, output);
        let output_id =
            create_output(stack, pipeline_id, &output.name, &url, &output.encoding).await?;
        start_output(stack, pipeline_id, &output_id).await?;
        output_ids.push(output_id);
    }
    wait_for_outputs_progress(
        stack,
        pipeline_id,
        &output_ids,
        msr_progress_timeout(output_ids.len()),
    )
    .await?;
    Ok(output_ids)
}

fn msr_dashboard_sidecar_specs() -> [(&'static str, &'static str, SweepConfig); 2] {
    let configs = sweep_configs();
    [
        (
            "MSR dashboard sidecar RTMP",
            "msr-dashboard-sidecar-rtmp",
            configs
                .iter()
                .copied()
                .find(|config| config.name == "h264-rtmp")
                .expect("sweep config h264-rtmp should exist"),
        ),
        (
            "MSR dashboard sidecar multi-audio",
            "msr-dashboard-sidecar-srt",
            configs
                .iter()
                .copied()
                .find(|config| config.name == "mixed.live.srt.h264.a2.bf2")
                .expect("sweep config mixed.live.srt.h264.a2.bf2 should exist"),
        ),
    ]
}

async fn create_msr_dashboard_sidecars(
    env: &MsrDashboardEnv,
    stack: &mut ResourceSweepStack,
) -> Result<(Vec<MsrDashboardPipeline>, Vec<Child>), String> {
    let mut pipelines = Vec::new();
    let mut publishers = Vec::new();
    for (name, stream_key, config) in msr_dashboard_sidecar_specs() {
        let pipeline_id = create_resource_pipeline(&stack.api, name, stream_key).await?;
        let publisher = spawn_resource_publisher(&env.resource, config, stream_key)?;
        wait_for_api_input_live(&stack.api, &pipeline_id, Duration::from_secs(45)).await?;
        pipelines.push(MsrDashboardPipeline {
            id: pipeline_id,
            name: name.to_string(),
            stream_key: stream_key.to_string(),
            role: "sidecar",
        });
        publishers.push(publisher);
    }
    Ok((pipelines, publishers))
}

async fn run_msr_dashboard_playwright(
    env: &MsrDashboardEnv,
    pipelines: &[MsrDashboardPipeline],
) -> Result<(), String> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let log = std::fs::File::create(&env.playwright_log).map_err(|error| error.to_string())?;
    let log_err = log.try_clone().map_err(|error| error.to_string())?;
    let status = Command::new("npx")
        .arg("playwright")
        .arg("test")
        .arg("test/frontend/msr-dashboard-soak.spec.ts")
        .arg("--project=chromium")
        .current_dir(&repo_root)
        .env(
            "BASE_URL",
            format!("http://127.0.0.1:{}", env.resource.restream_http),
        )
        .env("RESTREAM_UI_PASSWORD", harness_admin_password())
        .env(
            "MSR_DASHBOARD_PIPELINES_JSON",
            serde_json::to_string(pipelines).map_err(|error| error.to_string())?,
        )
        .env(
            "MSR_DASHBOARD_ARTIFACT_DIR",
            env.resource.work_dir.to_string_lossy().to_string(),
        )
        .env(
            "MSR_DASHBOARD_SUMMARY_JSON",
            env.playwright_summary_json.to_string_lossy().to_string(),
        )
        .env(
            "MSR_DASHBOARD_RUNTIME_SECS",
            env.playwright_runtime_secs.to_string(),
        )
        .env(
            "MSR_DASHBOARD_CHURN_OUTPUTS_PER_PIPELINE",
            env.churn_outputs_per_pipeline.to_string(),
        )
        .env(
            "MSR_DASHBOARD_DIAGNOSTICS_EVERY_CYCLES",
            env.diagnostics_every_cycles.to_string(),
        )
        .env(
            "MSR_DASHBOARD_OUTPUT_RTMP_PORT",
            env.resource.mtx_rtmp.to_string(),
        )
        .env(
            "MSR_DASHBOARD_OUTPUT_SRT_PORT",
            env.resource.mtx_srt.to_string(),
        )
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .status()
        .await
        .map_err(|error| format!("failed to launch Playwright: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Playwright soak failed with status {status}; see {}",
            env.playwright_log.display()
        ))
    }
}

pub(crate) async fn msr_dashboard() -> Result<Value, String> {
    let env = MsrDashboardEnv::from_env()?;
    std::fs::create_dir_all(&env.resource.work_dir).map_err(|error| error.to_string())?;
    let _ = std::fs::remove_file(&env.resource.summary_json);
    let _ = std::fs::remove_file(&env.playwright_log);
    let _ = std::fs::remove_file(&env.playwright_summary_json);

    let protocol_mix = MsrProtocolMix::from_env()?;
    let hero_plan = msr_output_plan_for_mix(protocol_mix);
    let mut stack = start_resource_sweep_stack(&env.resource).await?;

    let hero_pipeline = MsrDashboardPipeline {
        id: create_resource_pipeline(
            &stack.api,
            MsrRunProfile::Canonical.pipeline_name(),
            MsrRunProfile::Canonical.stream_key(),
        )
        .await?,
        name: MsrRunProfile::Canonical.pipeline_name().to_string(),
        stream_key: MsrRunProfile::Canonical.stream_key().to_string(),
        role: "hero",
    };
    let mut hero_publisher = spawn_msr_publisher(
        &env.resource,
        MsrRunProfile::Canonical.stream_key(),
        MsrRunProfile::Canonical,
    )?;
    wait_for_api_input_live(&stack.api, &hero_pipeline.id, Duration::from_secs(60)).await?;
    let hero_outputs = seed_msr_dashboard_hero_outputs(
        &env.resource,
        &stack.api,
        &hero_pipeline.id,
        &hero_plan,
        env.hero_seed_outputs,
    )
    .await?;

    let (mut sidecars, mut sidecar_publishers) =
        create_msr_dashboard_sidecars(&env, &mut stack).await?;
    let mut pipelines = vec![hero_pipeline.clone()];
    pipelines.append(&mut sidecars);

    let playwright_result = run_msr_dashboard_playwright(&env, &pipelines).await;
    let status = if playwright_result.is_ok() {
        "PASS"
    } else {
        "FAIL"
    };
    let result = json!({
        "mode": MSR_DASHBOARD_MODE,
        "status": status,
        "heroSeedOutputs": hero_outputs.len(),
        "runtimeSecs": env.playwright_runtime_secs,
        "churnOutputsPerPipeline": env.churn_outputs_per_pipeline,
        "diagnosticsEveryCycles": env.diagnostics_every_cycles,
        "pipelines": pipelines,
        "artifacts": {
            "summaryJson": env.resource.summary_json.clone(),
            "playwrightSummaryJson": env.playwright_summary_json.clone(),
            "playwrightLog": env.playwright_log.clone(),
            "publisherLog": env.resource.work_dir.join("publisher-msr.log"),
            "restreamLog": env.resource.restream_log.clone(),
            "mediamtxLog": env.resource.mediamtx_log.clone(),
        },
    });
    std::fs::write(
        &env.resource.summary_json,
        serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    if env.resource.no_cleanup {
        println!("MSR dashboard no-cleanup: leaving the live stack running");
        std::mem::forget(hero_publisher);
        for publisher in sidecar_publishers.drain(..) {
            std::mem::forget(publisher);
        }
        std::mem::forget(stack);
    } else {
        stop_child(&mut hero_publisher).await;
        for publisher in &mut sidecar_publishers {
            stop_child(publisher).await;
        }
        for pipeline in &pipelines {
            delete_resource_pipeline(&stack.api, &pipeline.id).await;
        }
        stop_child(&mut stack.restream).await;
        stop_child(&mut stack.mediamtx).await;
    }

    playwright_result.map(|_| result)
}

async fn run_msr_ffprobe_checkpoint(
    env: &ResourceSweepEnv,
    checkpoint: usize,
    outputs: &[MsrOutputSpec],
) -> Result<Vec<Value>, String> {
    let sample_count = msr_ffprobe_sample_count(outputs.len());
    let seed = msr_ffprobe_seed();
    let duration = env_secs("MSR_FFPROBE_SAMPLE_SECS", 5);
    let probesize = std::env::var("MSR_FFPROBE_PROBESIZE").unwrap_or_else(|_| "10M".to_string());
    let analyzeduration =
        std::env::var("MSR_FFPROBE_ANALYZEDURATION").unwrap_or_else(|_| "10M".to_string());
    let defect_rate = std::env::var("MSR_FFPROBE_DEFECT_RATE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.05);
    let samples = msr_ffprobe_sample_outputs(outputs, sample_count, seed);
    let confidence = msr_ffprobe_detection_confidence(samples.len(), outputs.len(), defect_rate);
    let mut checks = Vec::new();
    for output in samples {
        let url = msr_read_url(env, &output);
        let label = format!("{}-{}-{}", MSR_MODE, checkpoint, output.name);
        let probe_path = env
            .work_dir
            .join(format!("{}.ffprobe.json", safe_artifact_stem(&label)));
        let probe =
            ffprobe_live_sample(&url, &probe_path, duration, &probesize, &analyzeduration).await?;
        let shape = validate_msr_ffprobe_sample(&output, &probe)?;
        let check = json!({
            "kind": "msrFfprobeSample",
            "mode": MSR_MODE,
            "checkpoint": checkpoint,
            "sampleCount": sample_count,
            "population": outputs.len(),
            "seed": seed,
            "durationSecs": duration,
            "probesize": probesize,
            "analyzeduration": analyzeduration,
            "defectRateAssumption": defect_rate,
            "detectionConfidence": confidence,
            "output": {
                "name": output.name,
                "ordinal": output.ordinal,
                "rank": output.rank,
                "protocol": output.protocol.label(),
                "encoding": output.encoding,
            },
            "url": url,
            "artifact": probe_path,
            "shape": shape,
        });
        append_line(
            &env.samples_jsonl,
            &format!("{}\n", serde_json::to_string(&check).unwrap()),
        )?;
        checks.push(check);
    }
    Ok(checks)
}

fn msr_signal_calibration_enabled() -> bool {
    match std::env::var("MSR_SIGNAL_CALIBRATION") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => std::env::var("MSR_FULL").ok().as_deref() == Some("1"),
    }
}

fn configure_msr_env(mut env: ResourceSweepEnv, profile: MsrRunProfile) -> ResourceSweepEnv {
    env.lifecycle = ResourceSweepLifecycle::Continuous;
    env.sample_secs = env_secs("MSR_SAMPLE_SECS", 6);
    env.sample_interval_ms = env_secs("MSR_SAMPLE_INTERVAL_MS", 1000);
    env.settle_secs = env_secs("MSR_SETTLE_SECS", 4);
    env.summary_json = env
        .work_dir
        .join(format!("{}-results.json", profile.output_prefix()));
    env.summary_csv = env
        .work_dir
        .join(format!("{}-results.csv", profile.output_prefix()));
    env.samples_jsonl = env
        .work_dir
        .join(format!("{}-samples.jsonl", profile.output_prefix()));
    env.restream_log = env
        .work_dir
        .join(format!("{}-restream.log", profile.output_prefix()));
    env.mediamtx_log = env
        .work_dir
        .join(format!("{}-mediamtx.log", profile.output_prefix()));
    env.mediamtx_config = env
        .work_dir
        .join(format!("{}-mediamtx.yml", profile.output_prefix()));
    if std::env::var_os("RESTREAM_DB_PATH").is_none() {
        env.restream_db_path = env.work_dir.join(format!("{}.db", profile.output_prefix()));
    }
    env
}

struct MsrPhaseResult {
    env: ResourceSweepEnv,
    report_md: PathBuf,
    plan_json: Value,
    executed_outputs: usize,
    aggregates: Vec<MsrCheckpointAggregate>,
    signal_checks: Vec<Value>,
}

fn msr_signal_sample_outputs(outputs: &[MsrOutputSpec], limit: usize) -> Vec<MsrOutputSpec> {
    if limit == 0 || outputs.is_empty() {
        return Vec::new();
    }
    let mut indexes = Vec::new();
    fn push_unique(indexes: &mut Vec<usize>, output_count: usize, index: usize) {
        if index < output_count && !indexes.contains(&index) {
            indexes.push(index);
        }
    }

    push_unique(&mut indexes, outputs.len(), 0);
    if let Some(index) = outputs
        .iter()
        .position(|output| output.protocol == MsrProtocol::Srt)
    {
        push_unique(&mut indexes, outputs.len(), index);
    }
    push_unique(&mut indexes, outputs.len(), outputs.len() / 2);
    push_unique(&mut indexes, outputs.len(), outputs.len().saturating_sub(1));
    if indexes.len() < limit {
        for index in (0..outputs.len()).step_by((outputs.len() / limit.max(1)).max(1)) {
            push_unique(&mut indexes, outputs.len(), index);
            if indexes.len() >= limit {
                break;
            }
        }
    }
    indexes
        .into_iter()
        .take(limit)
        .map(|index| outputs[index].clone())
        .collect()
}

async fn run_msr_signal_check(
    env: &ResourceSweepEnv,
    checkpoint: usize,
    output: &MsrOutputSpec,
    duration: u64,
) -> Result<Value, String> {
    let url = msr_read_url(env, output);
    let label = format!("{}-{}-{}", MSR_MODE, checkpoint, output.name);
    let stem = safe_artifact_stem(&label);
    let capture_path = env.work_dir.join(format!("{stem}.signal.mkv"));
    let blackdetect_log = env.work_dir.join(format!("{stem}.blackdetect.log"));
    let silencedetect_log = env.work_dir.join(format!("{stem}.silencedetect.log"));
    let ashowinfo_log = env.work_dir.join(format!("{stem}.ashowinfo.log"));
    let astats_log = env.work_dir.join(format!("{stem}.astats.log"));

    capture_signal_sample(&url, &capture_path, duration).await?;
    let black = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &[
            "-vf",
            "blackdetect=d=0.05:pix_th=0.10",
            "-an",
            "-f",
            "null",
            "-",
        ],
        &blackdetect_log,
    )
    .await?;
    let silence = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &[
            "-af",
            "silencedetect=n=-35dB:d=0.05",
            "-vn",
            "-f",
            "null",
            "-",
        ],
        &silencedetect_log,
    )
    .await?;
    let ashow = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &["-af", "ashowinfo", "-vn", "-f", "null", "-"],
        &ashowinfo_log,
    )
    .await?;
    let astats = run_ffmpeg_filter_log(
        &capture_path,
        duration,
        &["-af", "astats=metadata=1:reset=1", "-vn", "-f", "null", "-"],
        &astats_log,
    )
    .await?;
    let pcm = decode_pcm_quality(&capture_path, duration).await?;
    let report = validate_signal_quality_with_tolerances(
        &black,
        &silence,
        &ashow,
        &astats,
        pcm,
        &SignalTolerances::default(),
    )
    .map_err(|error| format!("{} signal validation failed: {error}", output.name))?;
    Ok(json!({
        "kind": "msrSignalQuality",
        "mode": MSR_MODE,
        "checkpoint": checkpoint,
        "output": {
            "name": output.name,
            "ordinal": output.ordinal,
            "rank": output.rank,
            "protocol": output.protocol.label(),
            "encoding": output.encoding,
        },
        "quality": signal_report_json(
            &label,
            &url,
            duration,
            &capture_path,
            &blackdetect_log,
            &silencedetect_log,
            &ashowinfo_log,
            &astats_log,
            &report,
        ),
    }))
}

async fn run_msr_signal_checkpoint(
    env: &ResourceSweepEnv,
    checkpoint: usize,
    outputs: &[MsrOutputSpec],
) -> Result<Vec<Value>, String> {
    let sample_limit = env_usize("MSR_SIGNAL_SAMPLES_PER_CHECKPOINT", 4);
    let duration = env_secs("MSR_SIGNAL_SAMPLE_SECS", 20);
    let mut checks = Vec::new();
    for output in msr_signal_sample_outputs(outputs, sample_limit) {
        let check = run_msr_signal_check(env, checkpoint, &output, duration).await?;
        append_line(
            &env.samples_jsonl,
            &format!("{}\n", serde_json::to_string(&check).unwrap()),
        )?;
        checks.push(check);
    }
    Ok(checks)
}

async fn run_msr_phase(
    env: ResourceSweepEnv,
    protocol_mix: MsrProtocolMix,
    checkpoints: &[usize],
    profile: MsrRunProfile,
) -> Result<MsrPhaseResult, String> {
    let plan = msr_output_plan_for_mix_and_profile(protocol_mix, profile);
    let plan_json = msr_plan_json(&plan, checkpoints, protocol_mix, profile);

    std::fs::create_dir_all(&env.work_dir).map_err(|error| error.to_string())?;
    let report_md = env
        .work_dir
        .join(format!("{}-report.md", profile.output_prefix()));
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.samples_jsonl);
    let _ = std::fs::remove_file(&report_md);

    let mut stack = start_resource_sweep_stack(&env).await?;
    let stream_key = profile.stream_key();
    let pipeline_id =
        create_resource_pipeline(&stack.api, profile.pipeline_name(), stream_key).await?;
    let mut publisher = spawn_msr_publisher(&env, stream_key, profile)?;
    wait_for_api_input_live(&stack.api, &pipeline_id, Duration::from_secs(60)).await?;

    let max_outputs = *checkpoints
        .last()
        .ok_or("MSR checkpoint list unexpectedly empty".to_string())?;
    let mut output_ids = Vec::with_capacity(max_outputs);
    let mut aggregates = Vec::with_capacity(checkpoints.len());
    let mut signal_checks = Vec::new();

    for output in plan.iter().take(max_outputs) {
        let url = msr_output_url(&env, output);
        let output_id = create_output(
            &stack.api,
            &pipeline_id,
            &output.name,
            &url,
            &output.encoding,
        )
        .await?;
        start_output(&stack.api, &pipeline_id, &output_id).await?;
        output_ids.push(output_id);

        if checkpoints.binary_search(&output.ordinal).is_ok() {
            wait_for_outputs_progress(
                &stack.api,
                &pipeline_id,
                &output_ids,
                msr_progress_timeout(output_ids.len()),
            )
            .await?;
            let expected_mediamtx_paths = plan[..output.ordinal]
                .iter()
                .map(msr_mediamtx_path)
                .collect::<Vec<_>>();
            let path_health = verify_mediamtx_path_health(
                env.mtx_api,
                &expected_mediamtx_paths,
                env_secs("MSR_SINK_SAMPLE_SECS", 3),
                Duration::from_secs(env_secs("MSR_SINK_TIMEOUT_SECS", 60)),
            )
            .await?;
            let rtmp_count = plan
                .iter()
                .take(output.ordinal)
                .filter(|spec| spec.protocol == MsrProtocol::Rtmp)
                .count();
            let srt_count = output.ordinal - rtmp_count;
            let label = format!("{}-outputs", output.ordinal);
            append_line(
                &env.samples_jsonl,
                &format!(
                    "{}\n",
                    serde_json::to_string(&mediamtx_path_health_json(
                        MSR_MODE,
                        &label,
                        &path_health
                    ))
                    .unwrap()
                ),
            )?;
            let ffprobe_checks =
                run_msr_ffprobe_checkpoint(&env, output.ordinal, &plan[..output.ordinal]).await?;
            if profile == MsrRunProfile::SignalCalibration {
                signal_checks.extend(
                    run_msr_signal_checkpoint(&env, output.ordinal, &plan[..output.ordinal])
                        .await?,
                );
            }
            let resource = sample_resource_window(
                &env,
                &mut stack,
                ResourceScenarioMeta {
                    scenario: MSR_MODE,
                    label,
                    pipelines: 1,
                    outputs: output.ordinal,
                    ingest_types: profile.ingest_types().to_string(),
                    egress_mix: format!("rtmp:{rtmp_count},srt:{srt_count}"),
                    transcode: "no",
                },
            )
            .await?;
            let post_sample_path_health = verify_mediamtx_path_health(
                env.mtx_api,
                &expected_mediamtx_paths,
                env_secs("MSR_SINK_POST_SAMPLE_SECS", 2),
                Duration::from_secs(env_secs("MSR_SINK_TIMEOUT_SECS", 60)),
            )
            .await?;
            append_line(
                &env.samples_jsonl,
                &format!(
                    "{}\n",
                    serde_json::to_string(&mediamtx_path_health_json(
                        MSR_MODE,
                        &format!("{}-post-sample", output.ordinal),
                        &post_sample_path_health
                    ))
                    .unwrap()
                ),
            )?;
            aggregates.push(MsrCheckpointAggregate {
                resource,
                path_health,
                post_sample_path_health,
                ffprobe_checks,
            });
        }
    }

    let resource_aggregates = aggregates
        .iter()
        .map(|aggregate| aggregate.resource.clone())
        .collect::<Vec<_>>();
    write_resource_sweep_csv(&env.summary_csv, &resource_aggregates)?;
    let rtmp_outputs = plan
        .iter()
        .take(output_ids.len())
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
        .count();
    let srt_outputs = output_ids.len().saturating_sub(rtmp_outputs);
    std::fs::write(
        &report_md,
        format_msr_report(
            output_ids.len(),
            profile.audio_tracks(),
            rtmp_outputs,
            srt_outputs,
            &aggregates,
        ),
    )
    .map_err(|error| error.to_string())?;
    let result = json!({
        "mode": MSR_MODE,
        "status": "PASS",
        "profile": profile.label(),
        "plan": plan_json.clone(),
        "executedOutputs": output_ids.len(),
        "artifacts": {
            "summaryJson": env.summary_json.clone(),
            "summaryCsv": env.summary_csv.clone(),
            "reportMd": report_md.clone(),
            "samplesJsonl": env.samples_jsonl.clone(),
            "publisherLog": env.work_dir.join(format!("publisher-{}.log", profile.output_prefix())),
            "restreamLog": env.restream_log.clone(),
            "mediamtxLog": env.mediamtx_log.clone(),
        },
        "aggregates": aggregates.iter().map(msr_checkpoint_aggregate_json).collect::<Vec<_>>(),
        "signalChecks": signal_checks.clone(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    if env.no_cleanup {
        println!("MSR no-cleanup: leaving the live stack running");
        std::mem::forget(publisher);
        std::mem::forget(stack);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&stack.api, &pipeline_id).await;
        stop_child(&mut stack.restream).await;
        stop_child(&mut stack.mediamtx).await;
    }
    Ok(MsrPhaseResult {
        env,
        report_md,
        plan_json,
        executed_outputs: output_ids.len(),
        aggregates,
        signal_checks,
    })
}

pub(crate) async fn msr() -> Result<Value, String> {
    let protocol_mix = MsrProtocolMix::from_env()?;
    let checkpoints = msr_checkpoints()?;
    let canonical_plan = msr_output_plan_for_mix(protocol_mix);
    let canonical_plan_json = msr_plan_json(
        &canonical_plan,
        &checkpoints,
        protocol_mix,
        MsrRunProfile::Canonical,
    );
    let signal_calibration = msr_signal_calibration_enabled();
    if std::env::var("MSR_PLAN_ONLY").ok().as_deref() == Some("1") {
        return Ok(json!({
            "status": "PLAN",
            "plan": canonical_plan_json,
            "signalCalibration": signal_calibration.then(|| {
                let signal_plan = msr_output_plan_for_mix_and_profile(
                    protocol_mix,
                    MsrRunProfile::SignalCalibration,
                );
                msr_plan_json(
                    &signal_plan,
                    &checkpoints,
                    protocol_mix,
                    MsrRunProfile::SignalCalibration,
                )
            }),
        }));
    }

    let mut base_env = ResourceSweepEnv::from_env_with_default_dir(".local/artifacts/msr")?;
    base_env.no_cleanup = std::env::var("MSR_NO_CLEANUP")
        .ok()
        .is_some_and(|value| value == "1");

    let calibration = if signal_calibration {
        let mut signal_env = base_env.clone();
        signal_env.work_dir = base_env.work_dir.join("signal-calibration");
        signal_env.no_cleanup = false;
        let signal_env = configure_msr_env(signal_env, MsrRunProfile::SignalCalibration);
        Some(
            run_msr_phase(
                signal_env,
                protocol_mix,
                &checkpoints,
                MsrRunProfile::SignalCalibration,
            )
            .await?,
        )
    } else {
        None
    };

    let canonical_env = configure_msr_env(base_env, MsrRunProfile::Canonical);
    let canonical = run_msr_phase(
        canonical_env,
        protocol_mix,
        &checkpoints,
        MsrRunProfile::Canonical,
    )
    .await?;

    let final_summary_json = canonical.env.summary_json.clone();
    let result = json!({
        "mode": MSR_MODE,
        "status": "PASS",
        "plan": canonical.plan_json.clone(),
        "executedOutputs": canonical.executed_outputs,
        "artifacts": {
            "summaryJson": canonical.env.summary_json.clone(),
            "summaryCsv": canonical.env.summary_csv.clone(),
            "reportMd": canonical.report_md.clone(),
            "samplesJsonl": canonical.env.samples_jsonl.clone(),
            "publisherLog": canonical.env.work_dir.join("publisher-msr.log"),
            "restreamLog": canonical.env.restream_log.clone(),
            "mediamtxLog": canonical.env.mediamtx_log.clone(),
            "signalCalibration": calibration.as_ref().map(|phase| json!({
                "summaryJson": phase.env.summary_json.clone(),
                "summaryCsv": phase.env.summary_csv.clone(),
                "reportMd": phase.report_md.clone(),
                "samplesJsonl": phase.env.samples_jsonl.clone(),
                "publisherLog": phase.env.work_dir.join("publisher-msr-signal.log"),
                "restreamLog": phase.env.restream_log.clone(),
                "mediamtxLog": phase.env.mediamtx_log.clone(),
            })),
        },
        "aggregates": canonical
            .aggregates
            .iter()
            .map(msr_checkpoint_aggregate_json)
            .collect::<Vec<_>>(),
        "signalCalibration": calibration.as_ref().map(|phase| json!({
            "profile": MsrRunProfile::SignalCalibration.label(),
            "plan": phase.plan_json.clone(),
            "executedOutputs": phase.executed_outputs,
            "signalChecks": phase.signal_checks.clone(),
            "aggregates": phase
                .aggregates
                .iter()
                .map(msr_checkpoint_aggregate_json)
                .collect::<Vec<_>>(),
        })),
    });
    std::fs::write(
        &final_summary_json,
        serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_plan_has_exact_zipf_and_protocol_totals() {
        let plan = msr_output_plan();
        assert_eq!(MSR_RANK_COUNTS.iter().sum::<usize>(), MSR_TOTAL_OUTPUTS);
        assert_eq!(plan.len(), MSR_TOTAL_OUTPUTS);
        assert_eq!(
            plan.iter()
                .filter(|output| output.protocol == MsrProtocol::Rtmp)
                .count(),
            MSR_RTMP_OUTPUTS
        );
        assert_eq!(
            plan.iter()
                .filter(|output| output.protocol == MsrProtocol::Srt)
                .count(),
            MSR_SRT_OUTPUTS
        );
    }

    #[test]
    fn protocol_mix_can_generate_isolated_plans() {
        let rtmp_plan = msr_output_plan_for_mix(MsrProtocolMix::RtmpOnly);
        assert_eq!(
            rtmp_plan
                .iter()
                .filter(|output| output.protocol == MsrProtocol::Rtmp)
                .count(),
            MSR_TOTAL_OUTPUTS
        );
        assert_eq!(
            rtmp_plan
                .iter()
                .filter(|output| output.protocol == MsrProtocol::Srt)
                .count(),
            0
        );

        let srt_plan = msr_output_plan_for_mix(MsrProtocolMix::SrtOnly);
        assert_eq!(
            srt_plan
                .iter()
                .filter(|output| output.protocol == MsrProtocol::Rtmp)
                .count(),
            0
        );
        assert_eq!(
            srt_plan
                .iter()
                .filter(|output| output.protocol == MsrProtocol::Srt)
                .count(),
            MSR_TOTAL_OUTPUTS
        );
    }

    #[test]
    fn protocol_mix_parser_accepts_calibration_shapes() {
        assert_eq!(
            MsrProtocolMix::parse("canonical").unwrap(),
            MsrProtocolMix::Canonical
        );
        assert_eq!(
            MsrProtocolMix::parse("rtmp-only").unwrap(),
            MsrProtocolMix::RtmpOnly
        );
        assert_eq!(
            MsrProtocolMix::parse("srt-only").unwrap(),
            MsrProtocolMix::SrtOnly
        );
        assert_eq!(
            MsrProtocolMix::parse("srt-every:10").unwrap(),
            MsrProtocolMix::SrtEvery(10)
        );
        assert!(MsrProtocolMix::parse("srt-every:0").is_err());
        assert!(MsrProtocolMix::parse("banana").is_err());
    }

    #[test]
    fn every_output_selects_its_rank_audio_track() {
        for output in msr_output_plan() {
            assert_eq!(output.language_code, MSR_LANGUAGE_CODES[output.rank - 1]);
            assert_eq!(output.language_name, MSR_LANGUAGE_NAMES[output.rank - 1]);
            assert_eq!(
                output.encoding,
                format!("source+atrack:{}", output.rank - 1)
            );
        }
    }

    #[test]
    fn signal_calibration_keeps_full_shape_but_uses_two_track_oracle_fixture() {
        let plan = msr_output_plan_for_mix_and_profile(
            MsrProtocolMix::Canonical,
            MsrRunProfile::SignalCalibration,
        );

        assert_eq!(plan.len(), MSR_TOTAL_OUTPUTS);
        assert_eq!(
            plan.iter()
                .filter(|output| output.protocol == MsrProtocol::Rtmp)
                .count(),
            MSR_RTMP_OUTPUTS
        );
        assert_eq!(
            plan.iter()
                .filter(|output| output.protocol == MsrProtocol::Srt)
                .count(),
            MSR_SRT_OUTPUTS
        );
        assert!(
            plan.iter()
                .all(|output| output.name.starts_with("msr-signal-rank"))
        );
        assert!(plan.iter().all(|output| {
            output.encoding == "source+atrack:0" || output.encoding == "source+atrack:1"
        }));
        assert_eq!(MsrRunProfile::SignalCalibration.audio_tracks(), 2);
    }

    #[test]
    fn signal_sample_selection_is_deterministic_and_includes_srt_when_present() {
        let plan = msr_output_plan();
        let samples = msr_signal_sample_outputs(&plan[..30], 4);

        assert_eq!(samples.len(), 4);
        assert_eq!(samples[0].ordinal, 1);
        assert!(
            samples
                .iter()
                .any(|output| output.protocol == MsrProtocol::Srt),
            "checkpoint sample should include the SRT output when one exists"
        );
    }

    #[test]
    fn srt_outputs_use_mediamtx_standard_stream_id() {
        let env = ResourceSweepEnv {
            work_dir: PathBuf::from("."),
            summary_json: PathBuf::from("summary.json"),
            summary_csv: PathBuf::from("summary.csv"),
            samples_jsonl: PathBuf::from("samples.jsonl"),
            restream_log: PathBuf::from("restream.log"),
            mediamtx_log: PathBuf::from("mediamtx.log"),
            mediamtx_config: PathBuf::from("mediamtx.yml"),
            restream_bin: PathBuf::from("restream"),
            restream_db_path: PathBuf::from("restream.db"),
            restream_http: 3030,
            restream_rtmp: 1935,
            restream_srt: 10080,
            mtx_rtmp: 1936,
            mtx_srt: 8891,
            mtx_api: 9997,
            sample_secs: 1,
            sample_interval_ms: 1000,
            settle_secs: 1,
            ingest_counts: Vec::new(),
            egress_counts: Vec::new(),
            scenario_filter: None,
            lifecycle: ResourceSweepLifecycle::Continuous,
            no_cleanup: false,
            srt_crypto: HarnessSrtCrypto::plaintext(),
            backend_policy_env: Vec::new(),
        };
        let output = MsrOutputSpec {
            ordinal: 20,
            rank: 1,
            language_code: "eng",
            language_name: "English",
            protocol: MsrProtocol::Srt,
            encoding: "source+atrack:0".to_string(),
            name: "msr-rank01-srt-0001".to_string(),
        };

        assert_eq!(
            msr_output_url(&env, &output),
            "srt://127.0.0.1:8891?streamid=#!::m=publish,r=msr-rank01-srt-0001"
        );
        assert_eq!(msr_mediamtx_path(&output), "msr-rank01-srt-0001");
    }

    #[test]
    fn requested_mahashivratri_language_codes_are_present() {
        let required = [
            "eng", "tam", "hin", "tel", "kan", "mar", "nep", "ben", "mal", "guj", "ori", "ita",
            "spa", "fra", "deu", "rus", "por", "ara", "ind",
        ];
        for language_code in required {
            assert!(
                MSR_LANGUAGE_CODES.contains(&language_code),
                "missing required MSR language code {language_code}"
            );
        }
        assert_eq!(
            MSR_LANGUAGE_CODES
                .iter()
                .filter(|code| **code == "zho")
                .count(),
            2,
            "Simplified and Traditional Chinese both require zho entries"
        );
    }

    #[test]
    fn report_includes_mediamtx_path_health_columns() {
        let aggregate = MsrCheckpointAggregate {
            resource: ResourceAggregate {
                scenario: MSR_MODE.to_string(),
                label: "30-outputs".to_string(),
                lifecycle: "continuous".to_string(),
                pipelines: 1,
                outputs: 30,
                ingest_types: "h264-srt-30a".to_string(),
                egress_mix: "rtmp:29,srt:1".to_string(),
                transcode: "no".to_string(),
                sample_count: 6,
                restream_cpu_avg_pct: 30.0,
                restream_cpu_peak_pct: 40.0,
                ffmpeg_cpu_avg_pct: 0.0,
                ffmpeg_cpu_peak_pct: 0.0,
                total_cpu_avg_pct: 32.1,
                total_cpu_peak_pct: 42.4,
                rss_avg_kb: 90.0 * 1024.0,
                rss_peak_kb: 90 * 1024,
                ffmpeg_rss_peak_kb: 0,
                retained_peak_kb: 0,
                source_ring_peak_kb: 0,
                transcoder_ring_peak_kb: 0,
                tsmux_ring_peak_kb: 0,
                avio_len_peak_kb: 0,
                avio_hwm_peak_kb: 92,
                anonymous_peak_kb: 0,
                private_dirty_peak_kb: 0,
                shared_clean_peak_kb: 0,
                pss_peak_kb: 0,
                unattributed_peak_kb: 0,
                active_transcoder_buffers_peak: 0,
                ingests_peak: 1,
                egresses_peak: 30,
                stages_peak: 1,
                pipeline_count_peak: 1,
            },
            path_health: MediaMtxPathHealth {
                expected_paths: 30,
                ready_paths: 30,
                reader_count: 0,
                paths_with_tracks: 30,
                inbound_frame_errors: 0,
                bytes_received_before: 1_000,
                bytes_received_after: 5_000_000,
                bytes_received_delta: 4_999_000,
                sample_secs: 3,
            },
            post_sample_path_health: MediaMtxPathHealth {
                expected_paths: 30,
                ready_paths: 30,
                reader_count: 0,
                paths_with_tracks: 30,
                inbound_frame_errors: 0,
                bytes_received_before: 5_000_000,
                bytes_received_after: 6_000_000,
                bytes_received_delta: 1_000_000,
                sample_secs: 2,
            },
            ffprobe_checks: Vec::new(),
        };

        let report = format_msr_report(30, 30, 29, 1, &[aggregate]);

        assert!(report.contains("MediaMTX ready"));
        assert!(report.contains("MediaMTX bytes delta"));
        assert!(report.contains("| 30 | rtmp:29,srt:1 | 30/30 |"));
    }

    #[test]
    fn ffprobe_sample_selection_is_seeded_and_includes_srt_when_present() {
        let plan = msr_output_plan();
        let samples = msr_ffprobe_sample_outputs(&plan[..30], 4, 1234);

        assert_eq!(samples.len(), 4);
        assert!(
            samples
                .iter()
                .any(|output| output.protocol == MsrProtocol::Srt),
            "sampled correctness gate should include an SRT output when the checkpoint has one"
        );
        assert_eq!(samples, msr_ffprobe_sample_outputs(&plan[..30], 4, 1234));
    }

    #[test]
    fn ffprobe_confidence_uses_without_replacement_detection_math() {
        let confidence = msr_ffprobe_detection_confidence(60, 1200, 0.05);

        assert!(
            confidence > 0.95,
            "60 samples should give >95% chance to catch at least one defect when >=5% are bad"
        );
    }
}
