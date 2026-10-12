use super::*;

#[derive(Clone)]
struct BranchMatrixEnv {
    resource: ResourceSweepEnv,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    summary_md: PathBuf,
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

pub(crate) async fn branch_matrix() -> Result<Value, String> {
    let env = BranchMatrixEnv::from_env()?;
    run_branch_matrix_variant(&env).await
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
        Some(start_resource_sweep_stack(resource, LocalPeerNeeds::ALL).await?)
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
        &resource.srt_crypto.transport_label(),
        &aggregates,
    )?;
    let result = json!({
        "mode": "branch-matrix",
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
            stop_children(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

fn write_branch_matrix_markdown(
    path: &Path,
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
