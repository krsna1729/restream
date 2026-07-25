use super::*;

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
                let policy = restream::config::backend_policy_from_env();
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

/// A/B resource comparison for the RTMP fabric: the same RTMP-source
/// workload run twice, once with the legacy per-connection sender (the
/// default, `RESTREAM_EGRESS_FABRIC` unset) and once with the fabric routed
/// (`RESTREAM_EGRESS_FABRIC=rtmp`), each in its own isolated
/// mediamtx+restream stack. Mirrors `srt_crypto_matrix`'s shape, but varies
/// `backend_policy_env` (already a generic env-var-override seam used by
/// the backend-policy matrix) instead of SRT crypto, and calls
/// `run_resource_egress_growth` directly for a single named scenario rather
/// than `run_branch_matrix_variant`'s full branch-tagged scenario set,
/// since RTMP-fabric parity is a two-way A/B, not a scenario sweep.
///
/// `RTMP_FABRIC_MATRIX_EGRESS_COUNT` (default 10) and
/// `RTMP_FABRIC_MATRIX_SCENARIO` (default `egress-growth-source-same`)
/// bound the run to a modest, bounded scale — a smoke-scale correctness +
/// resource comparison proving the fabric path works and roughly where it
/// stands under real live-process conditions, not the exhaustive
/// 1,000+-output parity proof `docs/egress-implementation.md` Phase 5's
/// exit gate ultimately requires before a default-mode flip.
pub(crate) async fn rtmp_fabric_matrix() -> Result<Value, String> {
    let mut env =
        BranchMatrixEnv::from_env_with_default_dir(".local/artifacts/rtmp-fabric-matrix")?;
    let egress_count = env_usize("RTMP_FABRIC_MATRIX_EGRESS_COUNT", 10).max(1);
    env.resource.egress_counts = vec![egress_count];
    env.resource.ingest_counts = vec![1];
    let scenario_name = std::env::var("RTMP_FABRIC_MATRIX_SCENARIO")
        .unwrap_or_else(|_| "egress-growth-source-same".to_string());
    let scenario = resource_egress_scenario(&scenario_name)
        .ok_or_else(|| format!("unknown rtmp fabric matrix scenario: {scenario_name}"))?;

    let parent_work_dir = env.resource.work_dir.clone();
    let variants: [(&str, Vec<(&'static str, String)>); 2] = [
        ("legacy", Vec::new()),
        (
            "fabric",
            vec![("RESTREAM_EGRESS_FABRIC", "rtmp".to_string())],
        ),
    ];

    let mut runs = Vec::new();
    for (label, backend_policy_env) in variants {
        let mut variant_resource = env.resource.clone();
        variant_resource.backend_policy_env = backend_policy_env;
        variant_resource.work_dir = parent_work_dir.join(label);
        variant_resource.summary_csv = variant_resource.work_dir.join("results.csv");
        variant_resource.samples_jsonl = variant_resource.work_dir.join("samples.jsonl");
        variant_resource.restream_log = variant_resource.work_dir.join("restream.log");
        variant_resource.mediamtx_log = variant_resource.work_dir.join("mediamtx.log");
        variant_resource.mediamtx_config = variant_resource.work_dir.join("mediamtx.yml");
        variant_resource.restream_db_path = variant_resource
            .work_dir
            .join(format!("rtmp-fabric-matrix-{label}.db"));
        std::fs::create_dir_all(&variant_resource.work_dir).map_err(|e| e.to_string())?;

        let mut stack = None;
        let mut retained_publishers = Vec::new();
        let aggregates = run_resource_egress_growth(
            &variant_resource,
            &mut stack,
            &mut retained_publishers,
            &scenario.name,
            sweep_configs()[scenario.config_index],
            &scenario.output_kinds,
        )
        .await?;
        write_resource_sweep_csv(&variant_resource.summary_csv, &aggregates)?;
        runs.push(json!({
            "variant": label,
            "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
            "artifacts": {
                "summaryCsv": variant_resource.summary_csv,
                "samplesJsonl": variant_resource.samples_jsonl,
                "restreamLog": variant_resource.restream_log,
                "mediamtxLog": variant_resource.mediamtx_log,
            },
        }));
    }

    let summary_json = parent_work_dir.join("rtmp-fabric-matrix-results.json");
    let result = json!({
        "mode": "rtmp-fabric-matrix",
        "scenario": scenario.name,
        "egressCount": egress_count,
        "variants": runs,
    });
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
