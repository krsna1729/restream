use super::*;
use crate::mediamtx_probe::{
    MediaMtxPathHealth, mediamtx_path_health_json, verify_mediamtx_path_health,
};

pub(crate) const MSR_MODE: &str = "msr";
const MSR_RANK_COUNTS: [usize; 30] = [
    300, 150, 100, 75, 60, 50, 43, 38, 33, 30, 27, 25, 23, 21, 20, 19, 18, 17, 16, 15, 14, 14, 13,
    13, 12, 12, 11, 11, 10, 10,
];
const MSR_TOTAL_OUTPUTS: usize = 1_200;
const MSR_RTMP_OUTPUTS: usize = 1_140;
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

struct MsrCheckpointAggregate {
    resource: ResourceAggregate,
    path_health: MediaMtxPathHealth,
}

fn msr_output_plan() -> Vec<MsrOutputSpec> {
    let mut plan = Vec::with_capacity(MSR_TOTAL_OUTPUTS);
    for (rank_index, count) in MSR_RANK_COUNTS.iter().copied().enumerate() {
        for within_rank in 0..count {
            let ordinal = plan.len() + 1;
            // Exactly every twentieth output uses SRT. This produces the
            // canonical 95/5 split while distributing SRT across rank groups.
            let protocol = if ordinal.is_multiple_of(20) {
                MsrProtocol::Srt
            } else {
                MsrProtocol::Rtmp
            };
            plan.push(MsrOutputSpec {
                ordinal,
                rank: rank_index + 1,
                language_code: MSR_LANGUAGE_CODES[rank_index],
                language_name: MSR_LANGUAGE_NAMES[rank_index],
                protocol,
                encoding: format!("source+atrack:{rank_index}"),
                name: format!(
                    "msr-rank{:02}-{}-{:04}",
                    rank_index + 1,
                    protocol.label(),
                    within_rank + 1
                ),
            });
        }
    }
    plan
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

fn msr_plan_json(plan: &[MsrOutputSpec], checkpoints: &[usize]) -> Value {
    let rtmp = plan
        .iter()
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
        .count();
    let srt = plan.len().saturating_sub(rtmp);
    debug_assert_eq!(rtmp, MSR_RTMP_OUTPUTS);
    debug_assert_eq!(srt, MSR_SRT_OUTPUTS);
    json!({
        "mode": MSR_MODE,
        "scenario": "mahashivratri",
        "zipf": {
            "exponent": 1.0,
            "hotCount": 300,
            "rankCounts": MSR_RANK_COUNTS,
        },
        "ingest": {
            "protocol": "srt",
            "video": { "codec": "h264", "width": 1920, "height": 1080, "fps": 30 },
            "audioTracks": 30,
            "stereoTracks": 29,
            "surroundTracks": 1,
            "surroundLayout": "5.1",
        },
        "outputs": {
            "total": plan.len(),
            "rtmp": rtmp,
            "srt": srt,
            "rtmpPercent": 95,
            "srtPercent": 5,
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

fn spawn_msr_publisher(env: &ResourceSweepEnv, stream_key: &str) -> Result<Child, String> {
    let fixture = restream::test_fixtures::checked_in_fixture(
        "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
    )?;
    let log_path = env.work_dir.join("publisher-msr.log");
    let url = append_srt_crypto(
        harness_srt_ffmpeg_url(env.restream_srt, stream_key, HarnessSrtMode::Publish, None),
        &env.srt_crypto,
    );
    spawn_publisher_with_selection(
        &fixture,
        &url,
        "mpegts",
        PublishTrackSelection::MsrThirtyAudio,
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

fn format_msr_report(executed_outputs: usize, aggregates: &[MsrCheckpointAggregate]) -> String {
    let mut report = format!(
        "Status: PASS at every checkpoint including {executed_outputs} outputs \
         (1 SRT ingest, 30 audio tracks, Zipf fan-out, 95% RTMP / 5% SRT, \
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

pub(crate) async fn msr() -> Result<Value, String> {
    let plan = msr_output_plan();
    let checkpoints = msr_checkpoints()?;
    let plan_json = msr_plan_json(&plan, &checkpoints);
    if std::env::var("MSR_PLAN_ONLY").ok().as_deref() == Some("1") {
        return Ok(json!({
            "status": "PLAN",
            "plan": plan_json,
        }));
    }

    let mut env = ResourceSweepEnv::from_env_with_default_dir(".local/artifacts/msr")?;
    env.lifecycle = ResourceSweepLifecycle::Continuous;
    env.sample_secs = env_secs("MSR_SAMPLE_SECS", 6);
    env.sample_interval_ms = env_secs("MSR_SAMPLE_INTERVAL_MS", 1000);
    env.settle_secs = env_secs("MSR_SETTLE_SECS", 4);
    env.no_cleanup = std::env::var("MSR_NO_CLEANUP")
        .ok()
        .is_some_and(|value| value == "1");
    env.summary_json = env.work_dir.join("msr-results.json");
    env.summary_csv = env.work_dir.join("msr-results.csv");
    env.samples_jsonl = env.work_dir.join("msr-samples.jsonl");
    env.restream_log = env.work_dir.join("restream.log");
    env.mediamtx_log = env.work_dir.join("mediamtx.log");
    env.mediamtx_config = env.work_dir.join("mediamtx.yml");
    if std::env::var_os("RESTREAM_DB_PATH").is_none() {
        env.restream_db_path = env.work_dir.join("msr.db");
    }

    std::fs::create_dir_all(&env.work_dir).map_err(|error| error.to_string())?;
    let report_md = env.work_dir.join("msr-report.md");
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.samples_jsonl);
    let _ = std::fs::remove_file(&report_md);

    let mut stack = start_resource_sweep_stack(&env).await?;
    let stream_key = "msr-hero";
    let pipeline_id = create_resource_pipeline(&stack.api, "MSR hero scenario", stream_key).await?;
    let mut publisher = spawn_msr_publisher(&env, stream_key)?;
    wait_for_api_input_live(&stack.api, &pipeline_id, Duration::from_secs(60)).await?;

    let max_outputs = *checkpoints
        .last()
        .ok_or("MSR checkpoint list unexpectedly empty".to_string())?;
    let mut output_ids = Vec::with_capacity(max_outputs);
    let mut aggregates = Vec::with_capacity(checkpoints.len());

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
            let resource = sample_resource_window(
                &env,
                &mut stack,
                ResourceScenarioMeta {
                    scenario: MSR_MODE,
                    label,
                    pipelines: 1,
                    outputs: output.ordinal,
                    ingest_types: "h264-srt-30a".to_string(),
                    egress_mix: format!("rtmp:{rtmp_count},srt:{srt_count}"),
                    transcode: "no",
                },
            )
            .await?;
            aggregates.push(MsrCheckpointAggregate {
                resource,
                path_health,
            });
        }
    }

    let resource_aggregates = aggregates
        .iter()
        .map(|aggregate| aggregate.resource.clone())
        .collect::<Vec<_>>();
    write_resource_sweep_csv(&env.summary_csv, &resource_aggregates)?;
    std::fs::write(&report_md, format_msr_report(output_ids.len(), &aggregates))
        .map_err(|error| error.to_string())?;
    let result = json!({
        "mode": MSR_MODE,
        "status": "PASS",
        "plan": plan_json,
        "executedOutputs": output_ids.len(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "reportMd": report_md,
            "samplesJsonl": env.samples_jsonl,
            "publisherLog": env.work_dir.join("publisher-msr.log"),
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(msr_checkpoint_aggregate_json).collect::<Vec<_>>(),
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
                bytes_received_before: 1_000,
                bytes_received_after: 5_000_000,
                bytes_received_delta: 4_999_000,
                sample_secs: 3,
            },
        };

        let report = format_msr_report(30, &[aggregate]);

        assert!(report.contains("MediaMTX ready"));
        assert!(report.contains("MediaMTX bytes delta"));
        assert!(report.contains("| 30 | rtmp:29,srt:1 | 30/30 |"));
    }
}
