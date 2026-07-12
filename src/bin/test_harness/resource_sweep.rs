use super::*;

#[path = "resource_sweep/bitrate.rs"]
mod bitrate;
pub(crate) use bitrate::bitrate_sweep;
#[path = "resource_sweep/msr.rs"]
mod msr;
pub(crate) use msr::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceSweepLifecycle {
    Isolated,
    Continuous,
    Cumulative,
}

impl ResourceSweepLifecycle {
    fn from_env() -> Result<Self, String> {
        match std::env::var("RESOURCE_SWEEP_LIFECYCLE")
            .unwrap_or_else(|_| "isolated".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "isolated" => Ok(Self::Isolated),
            "continuous" => Ok(Self::Continuous),
            "cumulative" => Ok(Self::Cumulative),
            other => Err(format!(
                "RESOURCE_SWEEP_LIFECYCLE must be isolated, continuous, or cumulative (got {other})"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Continuous => "continuous",
            Self::Cumulative => "cumulative",
        }
    }
}

/// Environment and output paths for resource-sweep measurement runs.
#[derive(Clone)]
struct ResourceSweepEnv {
    work_dir: PathBuf,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    samples_jsonl: PathBuf,
    restream_log: PathBuf,
    mediamtx_log: PathBuf,
    mediamtx_config: PathBuf,
    restream_bin: PathBuf,
    restream_db_path: PathBuf,
    restream_http: u16,
    restream_rtmp: u16,
    restream_srt: u16,
    mtx_rtmp: u16,
    mtx_srt: u16,
    mtx_api: u16,
    sample_secs: u64,
    sample_interval_ms: u64,
    settle_secs: u64,
    ingest_counts: Vec<usize>,
    egress_counts: Vec<usize>,
    scenario_filter: Option<HashSet<String>>,
    lifecycle: ResourceSweepLifecycle,
    no_cleanup: bool,
    srt_crypto: HarnessSrtCrypto,
    backend_policy_env: Vec<(&'static str, String)>,
}

impl ResourceSweepEnv {
    fn from_env() -> Result<Self, String> {
        Self::from_env_with_default_dir(".local/artifacts/resource-sweep")
    }

    fn from_env_with_default_dir(default_dir: &str) -> Result<Self, String> {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_dir));
        let ports = harness_port_defaults();
        Ok(Self {
            summary_json: work_dir.join("resource-sweep-results.json"),
            summary_csv: work_dir.join("resource-sweep-results.csv"),
            samples_jsonl: work_dir.join("resource-sweep-samples.jsonl"),
            restream_log: work_dir.join("restream.log"),
            mediamtx_log: work_dir.join("mediamtx.log"),
            mediamtx_config: work_dir.join("mediamtx.yml"),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "resource-sweep.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            sample_secs: env_secs("RESOURCE_SWEEP_SAMPLE_SECS", 6),
            sample_interval_ms: env_secs("RESOURCE_SWEEP_SAMPLE_INTERVAL_MS", 1000),
            settle_secs: env_secs("RESOURCE_SWEEP_SETTLE_SECS", 4),
            ingest_counts: parse_usize_list("RESOURCE_SWEEP_INGEST_COUNTS", "1,3,5"),
            egress_counts: parse_usize_list("RESOURCE_SWEEP_EGRESS_COUNTS", "1,5,10"),
            scenario_filter: parse_string_set("RESOURCE_SWEEP_SCENARIOS"),
            lifecycle: ResourceSweepLifecycle::from_env()?,
            no_cleanup: std::env::var("RESOURCE_SWEEP_NO_CLEANUP")
                .ok()
                .is_some_and(|v| v == "1"),
            srt_crypto: harness_srt_crypto_from_env(),
            backend_policy_env: Vec::new(),
            work_dir,
        })
    }

    fn scenario_enabled(&self, scenario: &str) -> bool {
        self.scenario_filter
            .as_ref()
            .is_none_or(|filter| filter.contains(scenario))
    }
}

fn parse_usize_list(name: &str, default: &str) -> Vec<usize> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .filter_map(|part| part.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .collect()
}

fn parse_string_set(name: &str) -> Option<HashSet<String>> {
    let values: HashSet<String> = std::env::var(name)
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    (!values.is_empty()).then_some(values)
}

fn parse_sweep_configs(name: &str) -> Result<Vec<SweepConfig>, String> {
    let raw = std::env::var(name).unwrap_or_else(|_| {
        sweep_configs()
            .iter()
            .map(|cfg| cfg.name)
            .collect::<Vec<_>>()
            .join(",")
    });
    let mut out = Vec::new();
    for part in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let config = sweep_configs()
            .iter()
            .copied()
            .find(|cfg| cfg.name == part)
            .ok_or_else(|| format!("unknown sweep config {part:?}"))?;
        out.push(config);
    }
    if out.is_empty() {
        return Err(format!("{name} produced no configs"));
    }
    Ok(out)
}

/// Input fixture shape used by resource and bitrate sweep families.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SweepConfig {
    pub(crate) name: &'static str,
    pub(crate) ingest_proto: &'static str,
    pub(crate) video_codec: &'static str,
    pub(crate) multi_audio: bool,
}

static SWEEP_CONFIGS_FROM_DSL: OnceLock<Vec<SweepConfig>> = OnceLock::new();

fn sweep_configs() -> &'static [SweepConfig] {
    SWEEP_CONFIGS_FROM_DSL.get_or_init(|| {
        serde_json::from_str(include_str!("sweep_configs.json"))
            .expect("embedded sweep_configs.json should define valid sweep rows")
    })
}

/// Output shape used by resource-sweep scenarios.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SweepOutputKind {
    RtmpSource,
    SrtSource,
    RtmpSourceDownmix,
    SrtSourceDownmix,
    Rtmp720p,
    Srt720p,
    Rtmp1080p,
    Srt1080p,
}

impl SweepOutputKind {
    fn label(self) -> &'static str {
        match self {
            Self::RtmpSource => "rtmp-source",
            Self::SrtSource => "srt-source",
            Self::RtmpSourceDownmix => "rtmp-source-downmix",
            Self::SrtSourceDownmix => "srt-source-downmix",
            Self::Rtmp720p => "rtmp.720p.a0",
            Self::Srt720p => "srt.720p.a0",
            Self::Rtmp1080p => "rtmp.1080p.a0",
            Self::Srt1080p => "srt.1080p.a0",
        }
    }

    pub(crate) fn publish_url(self, rtmp_port: u16, srt_port: u16, name: &str) -> String {
        match self {
            Self::RtmpSource | Self::RtmpSourceDownmix | Self::Rtmp720p | Self::Rtmp1080p => {
                format!("rtmp://127.0.0.1:{rtmp_port}/live/{name}")
            }
            Self::SrtSource | Self::SrtSourceDownmix | Self::Srt720p | Self::Srt1080p => {
                harness_srt_output_url(srt_port, name, HarnessSrtMode::Publish)
            }
        }
    }

    pub(crate) fn read_url(self, rtmp_port: u16, srt_port: u16, name: &str) -> String {
        match self {
            Self::RtmpSource | Self::RtmpSourceDownmix | Self::Rtmp720p | Self::Rtmp1080p => {
                format!("rtmp://127.0.0.1:{rtmp_port}/live/{name}")
            }
            Self::SrtSource | Self::SrtSourceDownmix | Self::Srt720p | Self::Srt1080p => {
                harness_srt_output_url(srt_port, name, HarnessSrtMode::Read)
            }
        }
    }

    pub(crate) const fn encoding(self, multi_audio: bool) -> &'static str {
        match (self, multi_audio) {
            (Self::RtmpSource, true) => "source+atrack:0",
            (Self::SrtSource, true) => "source+atrack:0,1",
            (Self::RtmpSource | Self::SrtSource, false) => "source",
            (Self::RtmpSourceDownmix | Self::SrtSourceDownmix, _) => "source+downmix:0",
            (Self::Rtmp720p, true) => "720p+atrack:0",
            (Self::Srt720p, true) => "720p+atrack:0,1",
            (Self::Rtmp720p | Self::Srt720p, false) => "720p",
            (Self::Rtmp1080p, true) => "1080p+atrack:0",
            (Self::Srt1080p, true) => "1080p+atrack:0,1",
            (Self::Rtmp1080p | Self::Srt1080p, false) => "1080p",
        }
    }
}

/// Declarative resource-sweep egress scenario row.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceEgressScenario {
    pub(crate) name: String,
    pub(crate) config_index: usize,
    pub(crate) output_kinds: Vec<SweepOutputKind>,
    pub(crate) branch_order: Option<usize>,
    branch_label: Option<&'static str>,
}

impl ResourceEgressScenario {
    pub(crate) fn branch_label(&self) -> &'static str {
        self.branch_label.unwrap_or("other")
    }
}

static RESOURCE_EGRESS_SCENARIOS_FROM_DSL: OnceLock<Vec<ResourceEgressScenario>> = OnceLock::new();

pub(crate) fn resource_egress_scenarios() -> &'static [ResourceEgressScenario] {
    RESOURCE_EGRESS_SCENARIOS_FROM_DSL.get_or_init(|| {
        serde_json::from_str(include_str!("resource_egress_scenarios.json"))
            .expect("embedded resource_egress_scenarios.json should define valid resource rows")
    })
}

pub(crate) fn resource_egress_scenario(name: &str) -> Option<&'static ResourceEgressScenario> {
    resource_egress_scenarios()
        .iter()
        .find(|scenario| scenario.name == name)
}

/// Live process stack shared by a resource-sweep sample.
struct ResourceSweepStack {
    mediamtx: Child,
    restream: Child,
    api: RampApi,
    restream_pid: u32,
}

/// Environment and output paths for branch-matrix runs.
#[derive(Clone)]
struct BranchMatrixEnv {
    resource: ResourceSweepEnv,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    summary_md: PathBuf,
    backend: String,
    srt_variants: Vec<HarnessSrtCrypto>,
    scenario_filter: Option<HashSet<String>>,
}

impl BranchMatrixEnv {
    fn from_env() -> Result<Self, String> {
        Self::from_env_with_default_dir(".local/artifacts/branch-matrix")
    }

    fn from_env_with_default_dir(default_dir: &str) -> Result<Self, String> {
        let mut resource = ResourceSweepEnv::from_env_with_default_dir(default_dir)?;
        let work_dir = resource.work_dir.clone();
        let egress_count = env_usize("BRANCH_MATRIX_EGRESS_COUNT", 10).max(1);
        resource.egress_counts = vec![egress_count];
        resource.ingest_counts = vec![1];
        resource.summary_json = work_dir.join("branch-matrix-results.json");
        resource.summary_csv = work_dir.join("branch-matrix-results.csv");
        resource.samples_jsonl = work_dir.join("branch-matrix-samples.jsonl");
        if std::env::var_os("RESTREAM_DB_PATH").is_none() {
            resource.restream_db_path = work_dir.join("branch-matrix.db");
        }
        Ok(Self {
            summary_json: work_dir.join("branch-matrix-results.json"),
            summary_csv: work_dir.join("branch-matrix-results.csv"),
            summary_md: work_dir.join("branch-matrix-summary.md"),
            backend: {
                let policy = restream::planner::backend_policy::BackendPolicy::from_env();
                if policy.internal_video_presets
                    || policy.internal_hevc_to_h264
                    || policy.internal_hls_preview
                    || policy.internal_complex_audio
                {
                    "internal".to_string()
                } else {
                    "external".to_string()
                }
            },
            srt_variants: vec![harness_srt_crypto_from_env()],
            scenario_filter: parse_string_set("BRANCH_MATRIX_SCENARIOS"),
            resource,
        })
    }

    fn scenario_enabled(&self, scenario: &str) -> bool {
        self.scenario_filter
            .as_ref()
            .is_none_or(|filter| filter.contains(scenario))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct BackendPolicyVariant {
    name: &'static str,
    internal_video_presets: bool,
    internal_hevc_to_h264: bool,
    internal_hls_preview: bool,
    internal_complex_audio: bool,
}

impl BackendPolicyVariant {
    const fn new(
        name: &'static str,
        internal_video_presets: bool,
        internal_hevc_to_h264: bool,
        internal_hls_preview: bool,
        internal_complex_audio: bool,
    ) -> Self {
        Self {
            name,
            internal_video_presets,
            internal_hevc_to_h264,
            internal_hls_preview,
            internal_complex_audio,
        }
    }

    fn env_overrides(self) -> Vec<(&'static str, String)> {
        vec![
            (
                "RESTREAM_INTERNAL_VIDEO_PRESETS",
                bool_env(self.internal_video_presets),
            ),
            (
                "RESTREAM_INTERNAL_HEVC_TO_H264",
                bool_env(self.internal_hevc_to_h264),
            ),
            (
                "RESTREAM_INTERNAL_HLS_PREVIEW",
                bool_env(self.internal_hls_preview),
            ),
            (
                "RESTREAM_INTERNAL_AUDIO_COMPLEX",
                bool_env(self.internal_complex_audio),
            ),
        ]
    }

    pub(crate) const fn name(self) -> &'static str {
        self.name
    }

    fn policy_json(self) -> Value {
        json!({
            "internalVideoPresets": self.internal_video_presets,
            "internalHevcToH264": self.internal_hevc_to_h264,
            "internalHlsPreview": self.internal_hls_preview,
            "internalComplexAudio": self.internal_complex_audio,
        })
    }

    fn branch_filter(self) -> Option<HashSet<String>> {
        let mut scenarios = HashSet::new();
        if self.internal_video_presets {
            scenarios.insert("egress-growth-transcode-mixed".to_string());
            scenarios.insert("egress-growth-source-plus-transcode-mixed".to_string());
            scenarios.insert("egress-growth-transcode-dual-mixed".to_string());
            scenarios.insert("egress-growth-source-plus-transcode-dual-mixed".to_string());
        }
        if self.internal_hevc_to_h264 {
            scenarios.insert("egress-growth-hevc-bridge".to_string());
        }
        (!scenarios.is_empty()).then_some(scenarios)
    }

    fn needs_branch_probe(self) -> bool {
        self.name == "external-all" || self.internal_video_presets || self.internal_hevc_to_h264
    }

    fn needs_hls_probe(self) -> bool {
        self.internal_hls_preview
    }

    fn needs_complex_audio_probe(self) -> bool {
        self.internal_complex_audio
    }
}

fn bool_env(value: bool) -> String {
    if value { "1" } else { "0" }.to_string()
}

const BACKEND_POLICY_VARIANTS: &[BackendPolicyVariant] = &[
    BackendPolicyVariant::new("external-all", false, false, false, false),
    BackendPolicyVariant::new("internal-video-presets", true, false, false, false),
    BackendPolicyVariant::new("internal-hevc-to-h264", false, true, false, false),
    BackendPolicyVariant::new("internal-hls-preview", false, false, true, false),
    BackendPolicyVariant::new("internal-complex-audio", false, false, false, true),
    BackendPolicyVariant::new("internal-all", true, true, true, true),
];

fn backend_policy_variant_by_name(name: &str) -> Option<BackendPolicyVariant> {
    BACKEND_POLICY_VARIANTS
        .iter()
        .copied()
        .find(|variant| variant.name() == name)
}

pub(crate) fn selected_backend_policy_variants() -> Result<Vec<BackendPolicyVariant>, String> {
    let raw = std::env::var("BACKEND_POLICY_MATRIX_VARIANTS").unwrap_or_else(|_| {
        BACKEND_POLICY_VARIANTS
            .iter()
            .map(|variant| variant.name())
            .collect::<Vec<_>>()
            .join(",")
    });
    let mut variants = Vec::new();
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if name == "all" || name == "default" {
            variants.extend(BACKEND_POLICY_VARIANTS.iter().copied());
            continue;
        }
        variants.push(
            backend_policy_variant_by_name(name)
                .ok_or_else(|| format!("unknown backend policy variant {name:?}"))?,
        );
    }
    if variants.is_empty() {
        return Err("BACKEND_POLICY_MATRIX_VARIANTS selected no variants".to_string());
    }
    Ok(variants)
}

/// One periodic process/memory sample in resource-oriented sweeps.
#[derive(Clone)]
struct ResourceSample {
    scenario: String,
    label: String,
    lifecycle: String,
    pipelines: usize,
    outputs: usize,
    ingest_types: String,
    egress_mix: String,
    transcode: String,
    restream_cpu_pct: f64,
    ffmpeg_cpu_pct: f64,
    total_cpu_pct: f64,
    rss_kb: u64,
    ffmpeg_count: u64,
    ffmpeg_rss_kb: u64,
    anonymous_kb: u64,
    private_dirty_kb: u64,
    private_clean_kb: u64,
    shared_clean_kb: u64,
    shared_dirty_kb: u64,
    pss_kb: u64,
    swap_kb: u64,
    retained_kb: u64,
    source_ring_kb: u64,
    transcoder_ring_kb: u64,
    tsmux_ring_kb: u64,
    avio_len_kb: u64,
    avio_hwm_kb: u64,
    active_transcoder_buffers: u64,
    ingests: usize,
    egresses: usize,
    stages: usize,
    pipeline_count: usize,
    unattributed_kb: u64,
}

/// Rollup statistics for a resource-sweep scenario.
#[derive(Clone)]
struct ResourceAggregate {
    scenario: String,
    label: String,
    lifecycle: String,
    pipelines: usize,
    outputs: usize,
    ingest_types: String,
    egress_mix: String,
    transcode: String,
    sample_count: usize,
    restream_cpu_avg_pct: f64,
    restream_cpu_peak_pct: f64,
    ffmpeg_cpu_avg_pct: f64,
    ffmpeg_cpu_peak_pct: f64,
    total_cpu_avg_pct: f64,
    total_cpu_peak_pct: f64,
    rss_avg_kb: f64,
    rss_peak_kb: u64,
    ffmpeg_rss_peak_kb: u64,
    retained_peak_kb: u64,
    source_ring_peak_kb: u64,
    transcoder_ring_peak_kb: u64,
    tsmux_ring_peak_kb: u64,
    avio_len_peak_kb: u64,
    avio_hwm_peak_kb: u64,
    anonymous_peak_kb: u64,
    private_dirty_peak_kb: u64,
    shared_clean_peak_kb: u64,
    pss_peak_kb: u64,
    unattributed_peak_kb: u64,
    active_transcoder_buffers_peak: u64,
    ingests_peak: usize,
    egresses_peak: usize,
    stages_peak: usize,
    pipeline_count_peak: usize,
}

/// Static labels and dimensions for one resource-sweep scenario.
struct ResourceScenarioMeta<'a> {
    scenario: &'a str,
    label: String,
    pipelines: usize,
    outputs: usize,
    ingest_types: String,
    egress_mix: String,
    transcode: &'a str,
}

/// Parsed `/proc/<pid>/smaps_rollup` memory counters used for attribution.
struct ProcMemRollup {
    anonymous_kb: u64,
    private_dirty_kb: u64,
    private_clean_kb: u64,
    shared_clean_kb: u64,
    shared_dirty_kb: u64,
    pss_kb: u64,
    swap_kb: u64,
}

pub(crate) async fn resource_sweep() -> Result<Value, String> {
    let env = ResourceSweepEnv::from_env()?;
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);

    let mut stack = if env.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(&env).await?)
    };
    let mut retained_publishers: Vec<Child> = Vec::new();
    let mut aggregates = Vec::new();

    if env.scenario_enabled("baseline-empty") {
        aggregates.push(run_resource_baseline(&env, &mut stack, &mut retained_publishers).await?);
    }
    if env.scenario_enabled("ingest-only") {
        for config in sweep_configs() {
            aggregates.push(
                run_resource_ingest_only(&env, &mut stack, &mut retained_publishers, *config)
                    .await?,
            );
        }
    }
    if env.scenario_enabled("ingest-growth-same") {
        aggregates.extend(
            run_resource_ingest_growth(&env, &mut stack, &mut retained_publishers, false).await?,
        );
    }
    if env.scenario_enabled("ingest-growth-mixed") {
        aggregates.extend(
            run_resource_ingest_growth(&env, &mut stack, &mut retained_publishers, true).await?,
        );
    }
    for scenario in resource_egress_scenarios() {
        if !env.scenario_enabled(&scenario.name) {
            continue;
        }
        aggregates.extend(
            run_resource_egress_growth(
                &env,
                &mut stack,
                &mut retained_publishers,
                &scenario.name,
                sweep_configs()[scenario.config_index],
                &scenario.output_kinds,
            )
            .await?,
        );
    }

    write_resource_sweep_csv(&env.summary_csv, &aggregates)?;
    let result = json!({
        "mode": "resource-sweep",
        "lifecycle": env.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    if env.no_cleanup {
        println!("resource-sweep no-cleanup: leaving final stack running");
        // kill_on_drop(true) is set at spawn time for these children, so simply
        // skipping stop_child() isn't enough — dropping the Child handles below
        // (at function return) would still SIGKILL them. mem::forget leaks the
        // handles instead, which is fine since the process is about to _exit.
        for child in retained_publishers.drain(..) {
            std::mem::forget(child);
        }
        if let Some(stack) = stack.take() {
            std::mem::forget(stack);
        }
    } else {
        for child in &mut retained_publishers {
            stop_child(child).await;
        }
        if let Some(stack) = stack.as_mut() {
            stop_child(&mut stack.restream).await;
            stop_child(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

pub(crate) async fn branch_matrix() -> Result<Value, String> {
    let env = BranchMatrixEnv::from_env()?;
    run_branch_matrix_variant(&env).await
}

pub(crate) async fn backend_policy_matrix() -> Result<Value, String> {
    let base =
        BranchMatrixEnv::from_env_with_default_dir(".local/artifacts/backend-policy-matrix")?;
    let parent_work_dir = base.resource.work_dir.clone();
    let variants = selected_backend_policy_variants()?;
    let mut runs = Vec::new();

    for variant in variants {
        let variant_work_dir = parent_work_dir.join(variant.name());
        let mut probes = serde_json::Map::new();

        if variant.needs_branch_probe() {
            let mut branch_env = base.clone();
            apply_branch_matrix_work_dir(
                &mut branch_env,
                variant_work_dir.join("branch"),
                "branch-matrix",
            );
            branch_env.backend = variant.name().to_string();
            branch_env.resource.backend_policy_env = variant.env_overrides();
            if let Some(filter) = variant.branch_filter() {
                branch_env.scenario_filter = Some(filter);
            }
            probes.insert(
                "branch".to_string(),
                run_branch_matrix_variant(&branch_env).await?,
            );
        }

        if variant.needs_hls_probe() {
            probes.insert(
                "hlsPreview".to_string(),
                run_backend_hls_preview_probe(variant, variant_work_dir.join("hls-preview"))
                    .await?,
            );
        }

        if variant.needs_complex_audio_probe() {
            probes.insert(
                "complexAudio".to_string(),
                run_backend_complex_audio_probe(
                    &base,
                    variant,
                    variant_work_dir.join("complex-audio"),
                )
                .await?,
            );
        }

        runs.push(json!({
            "variant": variant.name(),
            "policy": variant.policy_json(),
            "probes": probes,
        }));
    }

    let summary_json = parent_work_dir.join("backend-policy-matrix-results.json");
    let result = json!({
        "mode": "backend-policy-matrix",
        "variantSelection": std::env::var("BACKEND_POLICY_MATRIX_VARIANTS").unwrap_or_else(|_| "default".to_string()),
        "artifacts": {
            "summaryJson": summary_json,
            "workDir": parent_work_dir,
        },
        "variants": runs,
    });
    if let Some(parent) = summary_json.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&summary_json, serde_json::to_vec_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(result)
}

pub(crate) async fn srt_crypto_matrix() -> Result<Value, String> {
    let mut env = BranchMatrixEnv::from_env()?;
    env.srt_variants =
        parse_srt_crypto_variants("SRT_CRYPTO_MATRIX_VARIANTS", "plaintext,enc16,enc24,enc32")?;

    let parent_work_dir = env.resource.work_dir.clone();
    let mut runs = Vec::new();
    for crypto in env.srt_variants.clone() {
        let mut variant_env = env.clone();
        variant_env.resource.srt_crypto = crypto.clone();
        apply_branch_matrix_work_dir(
            &mut variant_env,
            parent_work_dir.join(&crypto.label),
            "branch-matrix",
        );
        runs.push(run_branch_matrix_variant(&variant_env).await?);
    }

    Ok(json!({
        "mode": "srt-crypto-matrix",
        "variants": runs,
    }))
}

fn apply_branch_matrix_work_dir(env: &mut BranchMatrixEnv, work_dir: PathBuf, db_stem: &str) {
    env.resource.work_dir = work_dir;
    env.resource.summary_json = env.resource.work_dir.join("branch-matrix-results.json");
    env.resource.summary_csv = env.resource.work_dir.join("branch-matrix-results.csv");
    env.resource.samples_jsonl = env.resource.work_dir.join("branch-matrix-samples.jsonl");
    env.resource.restream_log = env.resource.work_dir.join("restream.log");
    env.resource.mediamtx_log = env.resource.work_dir.join("mediamtx.log");
    env.resource.mediamtx_config = env.resource.work_dir.join("mediamtx.yml");
    env.resource.restream_db_path = env.resource.work_dir.join(format!("{db_stem}.db"));
    env.summary_json = env.resource.work_dir.join("branch-matrix-results.json");
    env.summary_csv = env.resource.work_dir.join("branch-matrix-results.csv");
    env.summary_md = env.resource.work_dir.join("branch-matrix-summary.md");
}

async fn run_backend_hls_preview_probe(
    variant: BackendPolicyVariant,
    work_dir: PathBuf,
) -> Result<Value, String> {
    let case = mixed_input_case_for_command("mixed.live.srt.h265.a1.bf2")
        .ok_or("backend policy HLS probe scenario missing")?;
    let mut env =
        MixedEnv::from_env_with_default_work_dir("backend-policy-hls-preview", work_dir.clone());
    apply_mixed_work_dir(&mut env, "backend-policy-hls-preview", work_dir);
    env.only_checks = Some(vec!["ffprobe".to_string(), "hls".to_string()]);
    env.n_per_group = 1;
    env.collect_failures = true;
    env.restream_env_overrides = variant.env_overrides();
    let mut result = run_mixed_input_case_with_env(case, env).await?;
    result["backendPolicyVariant"] = json!(variant.name());
    result["backendPolicy"] = variant.policy_json();
    Ok(result)
}

async fn run_backend_complex_audio_probe(
    base: &BranchMatrixEnv,
    variant: BackendPolicyVariant,
    work_dir: PathBuf,
) -> Result<Value, String> {
    let mut env = base.resource.clone();
    env.work_dir = work_dir;
    env.summary_json = env.work_dir.join("complex-audio-results.json");
    env.summary_csv = env.work_dir.join("complex-audio-results.csv");
    env.samples_jsonl = env.work_dir.join("complex-audio-samples.jsonl");
    env.restream_log = env.work_dir.join("restream.log");
    env.mediamtx_log = env.work_dir.join("mediamtx.log");
    env.mediamtx_config = env.work_dir.join("mediamtx.yml");
    env.restream_db_path = env.work_dir.join("complex-audio.db");
    env.backend_policy_env = variant.env_overrides();
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);

    let mut stack = if env.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(&env).await?)
    };
    let mut retained_publishers = Vec::new();
    let aggregates = run_resource_egress_growth(
        &env,
        &mut stack,
        &mut retained_publishers,
        "backend-policy-complex-audio",
        sweep_configs()[3],
        &[
            SweepOutputKind::RtmpSourceDownmix,
            SweepOutputKind::SrtSourceDownmix,
        ],
    )
    .await?;
    write_resource_sweep_csv(&env.summary_csv, &aggregates)?;
    let result = json!({
        "mode": "backend-policy-complex-audio",
        "backendPolicyVariant": variant.name(),
        "backendPolicy": variant.policy_json(),
        "lifecycle": env.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    if env.no_cleanup {
        for child in retained_publishers.drain(..) {
            std::mem::forget(child);
        }
        if let Some(stack) = stack.take() {
            std::mem::forget(stack);
        }
    } else {
        for child in &mut retained_publishers {
            stop_child(child).await;
        }
        if let Some(stack) = stack.as_mut() {
            stop_child(&mut stack.restream).await;
            stop_child(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

fn apply_mixed_work_dir(env: &mut MixedEnv, log_stem: &str, work_dir: PathBuf) {
    env.media_dir = work_dir.join("media");
    env.scale_log = work_dir.join("scale.csv");
    env.timing_log = work_dir.join("timing.jsonl");
    env.rss_summary = work_dir.join("rss-summary.csv");
    env.summary_log = work_dir.join("summary.txt");
    env.restream_log = work_dir.join(format!("{log_stem}-restream.log"));
    env.mediamtx_log = work_dir.join(format!("{log_stem}-mediamtx.log"));
    env.mediamtx_config = work_dir.join(format!("{log_stem}-mediamtx.yml"));
    env.restream_db_path = default_work_db_path(&work_dir, &format!("{log_stem}.db"));
    env.assertion_log = Some(work_dir.join("assertions.jsonl"));
    env.work_dir = work_dir;
}

async fn run_branch_matrix_variant(env: &BranchMatrixEnv) -> Result<Value, String> {
    let resource = &env.resource;
    std::fs::create_dir_all(&resource.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.summary_md);
    let _ = std::fs::remove_file(&resource.samples_jsonl);

    let mut stack = if resource.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(resource).await?)
    };
    let mut retained_publishers: Vec<Child> = Vec::new();
    let mut aggregates = Vec::new();

    for scenario in resource_egress_scenarios()
        .iter()
        .filter(|scenario| scenario.branch_order.is_some())
    {
        if !env.scenario_enabled(&scenario.name) {
            continue;
        }
        aggregates.extend(
            run_resource_egress_growth(
                resource,
                &mut stack,
                &mut retained_publishers,
                &scenario.name,
                sweep_configs()[scenario.config_index],
                &scenario.output_kinds,
            )
            .await?,
        );
    }

    write_resource_sweep_csv(&env.summary_csv, &aggregates)?;
    write_branch_matrix_markdown(
        &env.summary_md,
        &env.backend,
        &resource.srt_crypto.transport_label(),
        &aggregates,
    )?;
    let result = json!({
        "mode": "branch-matrix",
        "backend": env.backend,
        "srtIngestTransport": resource.srt_crypto.transport_label(),
        "lifecycle": resource.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "summaryMarkdown": env.summary_md,
            "samplesJsonl": resource.samples_jsonl,
            "restreamLog": resource.restream_log,
            "mediamtxLog": resource.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    if resource.no_cleanup {
        println!("branch-matrix no-cleanup: leaving final stack running");
    } else {
        for child in &mut retained_publishers {
            stop_child(child).await;
        }
        if let Some(stack) = stack.as_mut() {
            stop_child(&mut stack.restream).await;
            stop_child(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

async fn start_resource_sweep_stack(env: &ResourceSweepEnv) -> Result<ResourceSweepStack, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    std::fs::create_dir_all(env.work_dir.join("logs")).map_err(|e| e.to_string())?;
    cleanup_ramp_db(&env.restream_db_path);
    let mediamtx_log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let mediamtx_err = mediamtx_log.try_clone().map_err(|e| e.to_string())?;
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nwriteQueueSize: 512\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let mut mediamtx_command = Command::new("mediamtx");
    let mut mediamtx = remove_mediamtx_config_env(&mut mediamtx_command)
        .arg(&env.mediamtx_config)
        .stdout(Stdio::from(mediamtx_log))
        .stderr(Stdio::from(mediamtx_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut mediamtx).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }

    let restream_log = std::fs::File::create(&env.restream_log).map_err(|e| e.to_string())?;
    let restream_err = restream_log.try_clone().map_err(|e| e.to_string())?;
    let mut restream_cmd = Command::new(&env.restream_bin);
    restream_cmd
        .env("RESTREAM_HTTP_PORT", env.restream_http.to_string())
        .env("RESTREAM_RTMP_PORT", env.restream_rtmp.to_string())
        .env("RESTREAM_SRT_PORT", env.restream_srt.to_string())
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
        .env("RESTREAM_LOG_DIR", env.work_dir.join("logs"))
        .env(
            "RESTREAM_DB_PATH",
            env.restream_db_path.to_string_lossy().to_string(),
        )
        .stdout(Stdio::from(restream_log))
        .stderr(Stdio::from(restream_err))
        .kill_on_drop(true);
    for (key, value) in &env.backend_policy_env {
        restream_cmd.env(key, value);
    }
    apply_srt_listener_env(&mut restream_cmd, &env.srt_crypto);
    let mut restream = restream_cmd.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", env.restream_http),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut restream).await;
        stop_child(&mut mediamtx).await;
        return Err(format!("restream did not become ready: {err}"));
    }
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    Ok(ResourceSweepStack {
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

async fn ensure_resource_stack<'a>(
    env: &ResourceSweepEnv,
    stack: &'a mut Option<ResourceSweepStack>,
) -> Result<&'a mut ResourceSweepStack, String> {
    if stack.is_none() {
        *stack = Some(start_resource_sweep_stack(env).await?);
    }
    stack
        .as_mut()
        .ok_or("resource sweep stack missing".to_string())
}

async fn run_resource_baseline(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
) -> Result<ResourceAggregate, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let meta = ResourceScenarioMeta {
        scenario: "baseline-empty",
        label: "empty".to_string(),
        pipelines: 0,
        outputs: 0,
        ingest_types: "none".to_string(),
        egress_mix: "none".to_string(),
        transcode: "none",
    };
    let aggregate = sample_resource_window(env, active, meta).await?;
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    let _ = retained_publishers;
    Ok(aggregate)
}

async fn run_resource_ingest_only(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    config: SweepConfig,
) -> Result<ResourceAggregate, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let stream_key = format!("resource-{}", config.name);
    let pipeline_id = create_resource_pipeline(&active.api, config.name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let meta = ResourceScenarioMeta {
        scenario: "ingest-only",
        label: config.name.to_string(),
        pipelines: 1,
        outputs: 0,
        ingest_types: config.name.to_string(),
        egress_mix: "none".to_string(),
        transcode: "none",
    };
    let aggregate = sample_resource_window(env, active, meta).await?;
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(aggregate)
}

async fn run_resource_ingest_growth(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    mixed: bool,
) -> Result<Vec<ResourceAggregate>, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };

    let mut publishers = Vec::new();
    let mut pipeline_ids = Vec::new();
    let max_ingests = *env.ingest_counts.iter().max().unwrap_or(&1);
    let mut out = Vec::new();
    for index in 1..=max_ingests {
        let config = if mixed {
            sweep_configs()[index - 1]
        } else {
            sweep_configs()[1]
        };
        let stream_key = format!("resource-growth-{index}-{}", config.name);
        let pipeline_id = create_resource_pipeline(
            &active.api,
            &format!("{}-{index}", config.name),
            &stream_key,
        )
        .await?;
        let publisher = spawn_resource_publisher(env, config, &stream_key)?;
        wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
        publishers.push(publisher);
        pipeline_ids.push(pipeline_id);
        if env.ingest_counts.contains(&index) {
            let ingest_types = if mixed {
                sweep_configs()
                    .iter()
                    .take(index)
                    .map(|cfg| cfg.name)
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                "h264-srt".to_string()
            };
            out.push(
                sample_resource_window(
                    env,
                    active,
                    ResourceScenarioMeta {
                        scenario: if mixed {
                            "ingest-growth-mixed"
                        } else {
                            "ingest-growth-same"
                        },
                        label: format!("{index}-pipelines"),
                        pipelines: index,
                        outputs: 0,
                        ingest_types,
                        egress_mix: "none".to_string(),
                        transcode: "none",
                    },
                )
                .await?,
            );
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.extend(publishers);
    } else {
        for child in &mut publishers {
            stop_child(child).await;
        }
        for pipeline_id in pipeline_ids {
            delete_resource_pipeline(&active.api, &pipeline_id).await;
        }
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(out)
}

async fn run_resource_egress_growth(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    scenario_name: &str,
    config: SweepConfig,
    output_kinds: &[SweepOutputKind],
) -> Result<Vec<ResourceAggregate>, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let stream_key = format!("resource-{scenario_name}");
    let pipeline_id = create_resource_pipeline(&active.api, scenario_name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let mut output_ids = Vec::new();
    let max_outputs = *env.egress_counts.iter().max().unwrap_or(&1);
    let mut out = Vec::new();
    for index in 1..=max_outputs {
        for kind in output_kinds {
            let name = format!("{scenario_name}-{}-{index}", kind.label());
            let (url, encoding) = resource_output_url(env, config, *kind, &name);
            let output_id =
                create_output(&active.api, &pipeline_id, &name, &url, &encoding).await?;
            start_output(&active.api, &pipeline_id, &output_id).await?;
            output_ids.push(output_id);
        }
        if env.egress_counts.contains(&index) {
            let progress_timeout = resource_output_progress_timeout(output_ids.len());
            wait_for_outputs_progress(&active.api, &pipeline_id, &output_ids, progress_timeout)
                .await?;
            out.push(
                sample_resource_window(
                    env,
                    active,
                    ResourceScenarioMeta {
                        scenario: scenario_name,
                        label: format!("{index}-per-group"),
                        pipelines: 1,
                        outputs: output_ids.len(),
                        ingest_types: config.name.to_string(),
                        egress_mix: output_kinds
                            .iter()
                            .map(|kind| kind.label())
                            .collect::<Vec<_>>()
                            .join(","),
                        transcode: if output_kinds.iter().any(|kind| {
                            matches!(
                                kind,
                                SweepOutputKind::Rtmp720p
                                    | SweepOutputKind::Srt720p
                                    | SweepOutputKind::Rtmp1080p
                                    | SweepOutputKind::Srt1080p
                                    | SweepOutputKind::RtmpSourceDownmix
                                    | SweepOutputKind::SrtSourceDownmix
                            )
                        }) {
                            "yes"
                        } else {
                            "no"
                        },
                    },
                )
                .await?,
            );
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(out)
}

pub(crate) async fn create_resource_pipeline(
    api: &RampApi,
    name: &str,
    stream_key: &str,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or("pipeline create response missing pipeline.id".to_string())
}

async fn delete_resource_pipeline(api: &RampApi, pipeline_id: &str) {
    let _ = api
        .delete_json(&format!("/api/v1/pipelines/{pipeline_id}"))
        .await;
}

fn spawn_resource_publisher(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    stream_key: &str,
) -> Result<Child, String> {
    spawn_resource_publisher_with_bitrate(
        env.restream_rtmp,
        env.restream_srt,
        &env.work_dir,
        &env.srt_crypto,
        config,
        stream_key,
        "1.5M",
    )
}

fn spawn_resource_publisher_with_bitrate(
    restream_rtmp: u16,
    restream_srt: u16,
    work_dir: &Path,
    srt_crypto: &HarnessSrtCrypto,
    config: SweepConfig,
    stream_key: &str,
    bitrate: &str,
) -> Result<Child, String> {
    let log_path = work_dir.join(format!("publisher-{stream_key}.log"));
    let fixture = sweep_fixture(config, bitrate)?;
    let (url, format, selection) = if config.ingest_proto == "rtmp" {
        (
            format!("rtmp://127.0.0.1:{restream_rtmp}/live/{stream_key}"),
            "flv",
            PublishTrackSelection::PrimaryAv,
        )
    } else {
        (
            append_srt_crypto(
                harness_srt_ffmpeg_url(restream_srt, stream_key, HarnessSrtMode::Publish, None),
                srt_crypto,
            ),
            "mpegts",
            if config.multi_audio {
                PublishTrackSelection::AllStreams
            } else {
                PublishTrackSelection::PrimaryAv
            },
        )
    };
    spawn_publisher_with_selection(&fixture, &url, format, selection, Some(&log_path))
}

fn resource_output_url(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    kind: SweepOutputKind,
    name: &str,
) -> (String, String) {
    (
        kind.publish_url(env.mtx_rtmp, env.mtx_srt, name),
        kind.encoding(config.multi_audio).to_string(),
    )
}

fn resource_output_progress_timeout(output_count: usize) -> Duration {
    let base_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS", 30);
    let per_output_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 4);
    let cap_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_CAP_SECS", 240);
    scaled_output_progress_timeout(output_count, base_secs, per_output_secs, cap_secs)
}

pub(crate) fn scaled_output_progress_timeout(
    output_count: usize,
    base_secs: u64,
    per_output_secs: u64,
    cap_secs: u64,
) -> Duration {
    let cap_secs = cap_secs.max(base_secs);
    let extra_outputs = output_count.saturating_sub(1) as u64;
    let scaled_secs = base_secs.saturating_add(extra_outputs.saturating_mul(per_output_secs));
    Duration::from_secs(scaled_secs.min(cap_secs))
}

async fn sample_resource_window(
    env: &ResourceSweepEnv,
    stack: &mut ResourceSweepStack,
    meta: ResourceScenarioMeta<'_>,
) -> Result<ResourceAggregate, String> {
    tokio::time::sleep(Duration::from_secs(env.settle_secs)).await;
    let mut samples = Vec::new();
    let mut prev_ticks = read_proc_stat_ticks(stack.restream_pid)?;
    let mut prev_ffmpeg_ticks: HashMap<u32, u64> = HashMap::new();
    let mut prev_instant = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(env.sample_secs);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(env.sample_interval_ms)).await;
        let now = Instant::now();
        let ticks = read_proc_stat_ticks(stack.restream_pid)?;
        let ffmpeg = ffmpeg_children_stats(stack.restream_pid)?;
        let interval_secs = prev_instant.elapsed().as_secs_f64().max(0.001);
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 };
        let restream_cpu_pct =
            100.0 * (ticks.saturating_sub(prev_ticks)) as f64 / clk_tck / interval_secs;
        let mut ffmpeg_delta_ticks = 0u64;
        let mut next_ffmpeg_ticks = HashMap::new();
        for pid in &ffmpeg.pids {
            if let Ok(current_ticks) = read_proc_stat_ticks(*pid) {
                let previous_ticks = prev_ffmpeg_ticks.get(pid).copied().unwrap_or(current_ticks);
                ffmpeg_delta_ticks += current_ticks.saturating_sub(previous_ticks);
                next_ffmpeg_ticks.insert(*pid, current_ticks);
            }
        }
        let ffmpeg_cpu_pct = 100.0 * ffmpeg_delta_ticks as f64 / clk_tck / interval_secs;
        let total_cpu_pct = restream_cpu_pct + ffmpeg_cpu_pct;
        prev_ticks = ticks;
        prev_ffmpeg_ticks = next_ffmpeg_ticks;
        prev_instant = now;
        let rss_kb = read_proc_status_kb_checked(stack.restream_pid, "VmRSS", &env.restream_log)?;
        let rollup = read_smaps_rollup(stack.restream_pid)?;
        let telemetry = stack.api.get_json("/api/v1/engine/telemetry").await?;
        let health = stack.api.get_json("/api/v1/engine/health").await?;
        let accounting = &telemetry["memoryAccounting"];
        let retained_kb = accounting["retainedPayloadBytes"].as_u64().unwrap_or(0) / 1024;
        let source_ring_kb = accounting["sourceRings"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let transcoder_ring_kb = accounting["transcoderRings"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let tsmux_ring_kb = accounting["tsMuxerRings"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let avio_queues = &accounting["avioQueues"];
        let avio_len_kb = avio_queues["totalLenBytes"].as_u64().unwrap_or(0) / 1024;
        let avio_hwm_kb = avio_queues["inputQueues"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(avio_queues["egressQueues"].as_array().into_iter().flatten())
            .map(|queue| queue["highWaterBytes"].as_u64().unwrap_or(0))
            .sum::<u64>()
            / 1024;
        let sample = ResourceSample {
            scenario: meta.scenario.to_string(),
            label: meta.label.clone(),
            lifecycle: env.lifecycle.as_str().to_string(),
            pipelines: meta.pipelines,
            outputs: meta.outputs,
            ingest_types: meta.ingest_types.clone(),
            egress_mix: meta.egress_mix.clone(),
            transcode: meta.transcode.to_string(),
            restream_cpu_pct,
            ffmpeg_cpu_pct,
            total_cpu_pct,
            rss_kb,
            ffmpeg_count: ffmpeg.count,
            ffmpeg_rss_kb: ffmpeg.rss_kb,
            anonymous_kb: rollup.anonymous_kb,
            private_dirty_kb: rollup.private_dirty_kb,
            private_clean_kb: rollup.private_clean_kb,
            shared_clean_kb: rollup.shared_clean_kb,
            shared_dirty_kb: rollup.shared_dirty_kb,
            pss_kb: rollup.pss_kb,
            swap_kb: rollup.swap_kb,
            retained_kb,
            source_ring_kb,
            transcoder_ring_kb,
            tsmux_ring_kb,
            avio_len_kb,
            avio_hwm_kb,
            active_transcoder_buffers: telemetry["activeTranscoderBuffers"].as_u64().unwrap_or(0),
            ingests: telemetry["ingests"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0),
            egresses: telemetry["egresses"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0),
            stages: telemetry["stages"].as_array().map(|v| v.len()).unwrap_or(0),
            pipeline_count: health["pipelines"]
                .as_object()
                .map(|v| v.len())
                .unwrap_or(0),
            unattributed_kb: rss_kb.saturating_sub(retained_kb + avio_len_kb),
        };
        append_line(
            &env.samples_jsonl,
            &format!(
                "{}\n",
                serde_json::to_string(&resource_sample_json(&sample)).unwrap()
            ),
        )?;
        samples.push(sample);
    }
    Ok(summarize_resource_samples(meta, env.lifecycle, &samples))
}

fn summarize_resource_samples(
    meta: ResourceScenarioMeta<'_>,
    lifecycle: ResourceSweepLifecycle,
    samples: &[ResourceSample],
) -> ResourceAggregate {
    let restream_cpu_sum: f64 = samples.iter().map(|s| s.restream_cpu_pct).sum();
    let ffmpeg_cpu_sum: f64 = samples.iter().map(|s| s.ffmpeg_cpu_pct).sum();
    let total_cpu_sum: f64 = samples.iter().map(|s| s.total_cpu_pct).sum();
    let rss_sum: u64 = samples.iter().map(|s| s.rss_kb).sum();
    ResourceAggregate {
        scenario: meta.scenario.to_string(),
        label: meta.label,
        lifecycle: lifecycle.as_str().to_string(),
        pipelines: meta.pipelines,
        outputs: meta.outputs,
        ingest_types: meta.ingest_types,
        egress_mix: meta.egress_mix,
        transcode: meta.transcode.to_string(),
        sample_count: samples.len(),
        restream_cpu_avg_pct: round2(restream_cpu_sum / samples.len().max(1) as f64),
        restream_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|s| s.restream_cpu_pct)
                .fold(0.0, f64::max),
        ),
        ffmpeg_cpu_avg_pct: round2(ffmpeg_cpu_sum / samples.len().max(1) as f64),
        ffmpeg_cpu_peak_pct: round2(samples.iter().map(|s| s.ffmpeg_cpu_pct).fold(0.0, f64::max)),
        total_cpu_avg_pct: round2(total_cpu_sum / samples.len().max(1) as f64),
        total_cpu_peak_pct: round2(samples.iter().map(|s| s.total_cpu_pct).fold(0.0, f64::max)),
        rss_avg_kb: round2(rss_sum as f64 / samples.len().max(1) as f64),
        rss_peak_kb: samples.iter().map(|s| s.rss_kb).max().unwrap_or(0),
        ffmpeg_rss_peak_kb: samples.iter().map(|s| s.ffmpeg_rss_kb).max().unwrap_or(0),
        retained_peak_kb: samples.iter().map(|s| s.retained_kb).max().unwrap_or(0),
        source_ring_peak_kb: samples.iter().map(|s| s.source_ring_kb).max().unwrap_or(0),
        transcoder_ring_peak_kb: samples
            .iter()
            .map(|s| s.transcoder_ring_kb)
            .max()
            .unwrap_or(0),
        tsmux_ring_peak_kb: samples.iter().map(|s| s.tsmux_ring_kb).max().unwrap_or(0),
        avio_len_peak_kb: samples.iter().map(|s| s.avio_len_kb).max().unwrap_or(0),
        avio_hwm_peak_kb: samples.iter().map(|s| s.avio_hwm_kb).max().unwrap_or(0),
        anonymous_peak_kb: samples.iter().map(|s| s.anonymous_kb).max().unwrap_or(0),
        private_dirty_peak_kb: samples
            .iter()
            .map(|s| s.private_dirty_kb)
            .max()
            .unwrap_or(0),
        shared_clean_peak_kb: samples.iter().map(|s| s.shared_clean_kb).max().unwrap_or(0),
        pss_peak_kb: samples.iter().map(|s| s.pss_kb).max().unwrap_or(0),
        unattributed_peak_kb: samples.iter().map(|s| s.unattributed_kb).max().unwrap_or(0),
        active_transcoder_buffers_peak: samples
            .iter()
            .map(|s| s.active_transcoder_buffers)
            .max()
            .unwrap_or(0),
        ingests_peak: samples.iter().map(|s| s.ingests).max().unwrap_or(0),
        egresses_peak: samples.iter().map(|s| s.egresses).max().unwrap_or(0),
        stages_peak: samples.iter().map(|s| s.stages).max().unwrap_or(0),
        pipeline_count_peak: samples.iter().map(|s| s.pipeline_count).max().unwrap_or(0),
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn read_proc_stat_ticks(pid: u32) -> Result<u64, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    let fields: Vec<&str> = stat.split_whitespace().collect();
    let utime = fields
        .get(13)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or("proc stat missing utime")?;
    let stime = fields
        .get(14)
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or("proc stat missing stime")?;
    Ok(utime + stime)
}

fn read_proc_status_kb(pid: u32, key: &str) -> Result<u64, String> {
    let status =
        std::fs::read_to_string(format!("/proc/{pid}/status")).map_err(|e| e.to_string())?;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix(&format!("{key}:")) {
            return value
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| format!("failed to parse {key}"));
        }
    }
    Err(format!("{key} missing in /proc/{pid}/status"))
}

fn read_proc_status_kb_checked(pid: u32, key: &str, log_path: &Path) -> Result<u64, String> {
    read_proc_status_kb(pid, key).map_err(|error| {
        let tail = file_tail_lines(log_path, 20);
        if tail.is_empty() {
            format!("restream pid {pid} unavailable while reading {key}: {error}")
        } else {
            format!(
                "restream pid {pid} unavailable while reading {key}: {error}\nrestream log tail:\n{}",
                tail.join("\n")
            )
        }
    })
}

fn read_smaps_rollup(pid: u32) -> Result<ProcMemRollup, String> {
    let text =
        std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).map_err(|e| e.to_string())?;
    let value_for = |name: &str| -> u64 {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}:")))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
    };
    Ok(ProcMemRollup {
        anonymous_kb: value_for("Anonymous"),
        private_dirty_kb: value_for("Private_Dirty"),
        private_clean_kb: value_for("Private_Clean"),
        shared_clean_kb: value_for("Shared_Clean"),
        shared_dirty_kb: value_for("Shared_Dirty"),
        pss_kb: value_for("Pss"),
        swap_kb: value_for("Swap"),
    })
}

pub(crate) fn ffmpeg_children_stats(parent_pid: u32) -> Result<FfmpegStats, String> {
    let mut count = 0u64;
    let mut rss_kb = 0u64;
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(pid) = name.to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let status_path = format!("/proc/{pid}/status");
        let Ok(status) = std::fs::read_to_string(&status_path) else {
            continue;
        };
        let ppid = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .unwrap_or(0);
        if ppid != parent_pid {
            continue;
        }
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let text = String::from_utf8_lossy(&cmdline);
        if text.contains("ffmpeg") {
            count += 1;
            rss_kb += read_proc_status_kb(pid, "VmRSS").unwrap_or(0);
            pids.push(pid);
        }
    }
    Ok(FfmpegStats {
        count,
        rss_kb,
        pids,
    })
}

fn resource_sample_json(sample: &ResourceSample) -> Value {
    json!({
        "scenario": sample.scenario,
        "label": sample.label,
        "lifecycle": sample.lifecycle,
        "pipelines": sample.pipelines,
        "outputs": sample.outputs,
        "ingestTypes": sample.ingest_types,
        "egressMix": sample.egress_mix,
        "transcode": sample.transcode,
        "restreamCpuPct": sample.restream_cpu_pct,
        "ffmpegCpuPct": sample.ffmpeg_cpu_pct,
        "totalCpuPct": sample.total_cpu_pct,
        "rssKb": sample.rss_kb,
        "ffmpegCount": sample.ffmpeg_count,
        "ffmpegRssKb": sample.ffmpeg_rss_kb,
        "anonymousKb": sample.anonymous_kb,
        "privateDirtyKb": sample.private_dirty_kb,
        "privateCleanKb": sample.private_clean_kb,
        "sharedCleanKb": sample.shared_clean_kb,
        "sharedDirtyKb": sample.shared_dirty_kb,
        "pssKb": sample.pss_kb,
        "swapKb": sample.swap_kb,
        "retainedKb": sample.retained_kb,
        "sourceRingKb": sample.source_ring_kb,
        "transcoderRingKb": sample.transcoder_ring_kb,
        "tsmuxRingKb": sample.tsmux_ring_kb,
        "avioLenKb": sample.avio_len_kb,
        "avioHwmKb": sample.avio_hwm_kb,
        "activeTranscoderBuffers": sample.active_transcoder_buffers,
        "ingests": sample.ingests,
        "egresses": sample.egresses,
        "stages": sample.stages,
        "pipelineCount": sample.pipeline_count,
        "unattributedKb": sample.unattributed_kb,
    })
}

fn resource_aggregate_json(aggregate: &ResourceAggregate) -> Value {
    json!({
        "scenario": aggregate.scenario,
        "label": aggregate.label,
        "lifecycle": aggregate.lifecycle,
        "pipelines": aggregate.pipelines,
        "outputs": aggregate.outputs,
        "ingestTypes": aggregate.ingest_types,
        "egressMix": aggregate.egress_mix,
        "transcode": aggregate.transcode,
        "sampleCount": aggregate.sample_count,
        "restreamCpuAvgPct": aggregate.restream_cpu_avg_pct,
        "restreamCpuPeakPct": aggregate.restream_cpu_peak_pct,
        "ffmpegCpuAvgPct": aggregate.ffmpeg_cpu_avg_pct,
        "ffmpegCpuPeakPct": aggregate.ffmpeg_cpu_peak_pct,
        "totalCpuAvgPct": aggregate.total_cpu_avg_pct,
        "totalCpuPeakPct": aggregate.total_cpu_peak_pct,
        "rssAvgKb": aggregate.rss_avg_kb,
        "rssPeakKb": aggregate.rss_peak_kb,
        "ffmpegRssPeakKb": aggregate.ffmpeg_rss_peak_kb,
        "retainedPeakKb": aggregate.retained_peak_kb,
        "sourceRingPeakKb": aggregate.source_ring_peak_kb,
        "transcoderRingPeakKb": aggregate.transcoder_ring_peak_kb,
        "tsmuxRingPeakKb": aggregate.tsmux_ring_peak_kb,
        "avioLenPeakKb": aggregate.avio_len_peak_kb,
        "avioHwmPeakKb": aggregate.avio_hwm_peak_kb,
        "anonymousPeakKb": aggregate.anonymous_peak_kb,
        "privateDirtyPeakKb": aggregate.private_dirty_peak_kb,
        "sharedCleanPeakKb": aggregate.shared_clean_peak_kb,
        "pssPeakKb": aggregate.pss_peak_kb,
        "unattributedPeakKb": aggregate.unattributed_peak_kb,
        "activeTranscoderBuffersPeak": aggregate.active_transcoder_buffers_peak,
        "ingestsPeak": aggregate.ingests_peak,
        "egressesPeak": aggregate.egresses_peak,
        "stagesPeak": aggregate.stages_peak,
        "pipelineCountPeak": aggregate.pipeline_count_peak,
    })
}

fn write_resource_sweep_csv(path: &Path, rows: &[ResourceAggregate]) -> Result<(), String> {
    let mut text = String::from(
        "scenario,label,lifecycle,pipelines,outputs,ingest_types,egress_mix,transcode,sample_count,restream_cpu_avg_pct,restream_cpu_peak_pct,ffmpeg_cpu_avg_pct,ffmpeg_cpu_peak_pct,total_cpu_avg_pct,total_cpu_peak_pct,rss_avg_kb,rss_peak_kb,ffmpeg_rss_peak_kb,retained_peak_kb,source_ring_peak_kb,transcoder_ring_peak_kb,tsmux_ring_peak_kb,avio_len_peak_kb,avio_hwm_peak_kb,anonymous_peak_kb,private_dirty_peak_kb,shared_clean_peak_kb,pss_peak_kb,unattributed_peak_kb,active_transcoder_buffers_peak,ingests_peak,egresses_peak,stages_peak,pipeline_count_peak\n",
    );
    for row in rows {
        text.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(&row.scenario),
            csv_escape(&row.label),
            csv_escape(&row.lifecycle),
            row.pipelines,
            row.outputs,
            csv_escape(&row.ingest_types),
            csv_escape(&row.egress_mix),
            csv_escape(&row.transcode),
            row.sample_count,
            row.restream_cpu_avg_pct,
            row.restream_cpu_peak_pct,
            row.ffmpeg_cpu_avg_pct,
            row.ffmpeg_cpu_peak_pct,
            row.total_cpu_avg_pct,
            row.total_cpu_peak_pct,
            row.rss_avg_kb,
            row.rss_peak_kb,
            row.ffmpeg_rss_peak_kb,
            row.retained_peak_kb,
            row.source_ring_peak_kb,
            row.transcoder_ring_peak_kb,
            row.tsmux_ring_peak_kb,
            row.avio_len_peak_kb,
            row.avio_hwm_peak_kb,
            row.anonymous_peak_kb,
            row.private_dirty_peak_kb,
            row.shared_clean_peak_kb,
            row.pss_peak_kb,
            row.unattributed_peak_kb,
            row.active_transcoder_buffers_peak,
            row.ingests_peak,
            row.egresses_peak,
            row.stages_peak,
            row.pipeline_count_peak,
        ));
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn write_branch_matrix_markdown(
    path: &Path,
    backend: &str,
    srt_ingest_transport: &str,
    rows: &[ResourceAggregate],
) -> Result<(), String> {
    let mut selected: Vec<&ResourceAggregate> = rows.iter().collect();
    selected.sort_by_key(|row| {
        resource_egress_scenario(&row.scenario)
            .and_then(|scenario| scenario.branch_order)
            .unwrap_or(99)
    });

    let mut text = String::new();
    text.push_str("# Branch Matrix\n\n");
    text.push_str(&format!("- Backend: `{backend}`\n"));
    text.push_str(&format!(
        "- SRT ingest transport: `{srt_ingest_transport}`\n"
    ));
    if let Some(row) = selected.first() {
        text.push_str(&format!("- Lifecycle: `{}`\n", row.lifecycle));
        text.push_str(&format!("- Fanout per group: `{}`\n", row.label));
    }
    text.push('\n');
    text.push_str("| Shape | Outputs | Restream MB | Child FFmpeg MB | Combined MB | Total CPU % | Stages |\n");
    text.push_str("|---|---:|---:|---:|---:|---:|---:|\n");
    for row in &selected {
        let combined_mb = (row.rss_peak_kb + row.ffmpeg_rss_peak_kb) as f64 / 1024.0;
        text.push_str(&format!(
            "| {} | {} | {:.1} | {:.1} | {:.1} | {:.2} | {} |\n",
            branch_shape_label(&row.scenario),
            row.outputs,
            row.rss_peak_kb as f64 / 1024.0,
            row.ffmpeg_rss_peak_kb as f64 / 1024.0,
            combined_mb,
            row.total_cpu_avg_pct,
            row.stages_peak,
        ));
    }

    if let (Some(single), Some(single_plus_source), Some(dual), Some(dual_plus_source)) = (
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-transcode-mixed"),
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-source-plus-transcode-mixed"),
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-transcode-dual-mixed"),
        selected
            .iter()
            .find(|row| row.scenario == "egress-growth-source-plus-transcode-dual-mixed"),
    ) {
        text.push_str("\n## Deltas\n\n");
        text.push_str("| Comparison | Output Delta | Combined MB Delta | Total CPU Delta |\n");
        text.push_str("|---|---:|---:|---:|\n");
        text.push_str(&format!(
            "| Add passthrough on top of one transcode family | {} | {:.1} | {:.2} |\n",
            single_plus_source.outputs.saturating_sub(single.outputs),
            ((single_plus_source.rss_peak_kb + single_plus_source.ffmpeg_rss_peak_kb)
                .saturating_sub(single.rss_peak_kb + single.ffmpeg_rss_peak_kb)) as f64
                / 1024.0,
            single_plus_source.total_cpu_avg_pct - single.total_cpu_avg_pct,
        ));
        text.push_str(&format!(
            "| Add a second transcode family | {} | {:.1} | {:.2} |\n",
            dual.outputs.saturating_sub(single.outputs),
            ((dual.rss_peak_kb + dual.ffmpeg_rss_peak_kb)
                .saturating_sub(single.rss_peak_kb + single.ffmpeg_rss_peak_kb)) as f64
                / 1024.0,
            dual.total_cpu_avg_pct - single.total_cpu_avg_pct,
        ));
        text.push_str(&format!(
            "| Add passthrough on top of two transcode families | {} | {:.1} | {:.2} |\n",
            dual_plus_source.outputs.saturating_sub(dual.outputs),
            ((dual_plus_source.rss_peak_kb + dual_plus_source.ffmpeg_rss_peak_kb)
                .saturating_sub(dual.rss_peak_kb + dual.ffmpeg_rss_peak_kb)) as f64
                / 1024.0,
            dual_plus_source.total_cpu_avg_pct - dual.total_cpu_avg_pct,
        ));
    }

    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn branch_shape_label(scenario: &str) -> &'static str {
    resource_egress_scenario(scenario)
        .map(ResourceEgressScenario::branch_label)
        .unwrap_or("custom")
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
