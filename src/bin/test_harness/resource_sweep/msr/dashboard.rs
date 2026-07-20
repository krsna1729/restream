use super::*;

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
        let output_id = create_output_with_rtmp_mode(
            stack,
            pipeline_id,
            &output.name,
            &url,
            &output.encoding,
            output.rtmp_mode,
        )
        .await?;
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
        false,
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
