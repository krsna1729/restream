use super::*;

#[derive(Clone)]
pub(super) struct ResourceSample {
    pub(super) scenario: String,
    pub(super) label: String,
    pub(super) lifecycle: String,
    pub(super) pipelines: usize,
    pub(super) outputs: usize,
    pub(super) ingest_types: String,
    pub(super) egress_mix: String,
    pub(super) transcode: String,
    pub(super) restream_cpu_pct: f64,
    pub(super) ffmpeg_cpu_pct: f64,
    pub(super) total_cpu_pct: f64,
    pub(super) rss_kb: u64,
    pub(super) ffmpeg_count: u64,
    pub(super) ffmpeg_rss_kb: u64,
    pub(super) anonymous_kb: u64,
    pub(super) private_dirty_kb: u64,
    pub(super) private_clean_kb: u64,
    pub(super) shared_clean_kb: u64,
    pub(super) shared_dirty_kb: u64,
    pub(super) pss_kb: u64,
    pub(super) swap_kb: u64,
    pub(super) retained_kb: u64,
    pub(super) source_ring_kb: u64,
    pub(super) transcoder_ring_kb: u64,
    pub(super) tsmux_ring_kb: u64,
    pub(super) avio_len_kb: u64,
    pub(super) avio_hwm_kb: u64,
    pub(super) active_transcoder_buffers: u64,
    pub(super) ingests: usize,
    pub(super) egresses: usize,
    pub(super) stages: usize,
    pub(super) pipeline_count: usize,
    pub(super) unattributed_kb: u64,
}

/// Rollup statistics for a resource-sweep scenario.
#[derive(Clone)]
pub(super) struct ResourceAggregate {
    pub(super) scenario: String,
    pub(super) label: String,
    pub(super) lifecycle: String,
    pub(super) pipelines: usize,
    pub(super) outputs: usize,
    pub(super) ingest_types: String,
    pub(super) egress_mix: String,
    pub(super) transcode: String,
    pub(super) sample_count: usize,
    pub(super) restream_cpu_avg_pct: f64,
    pub(super) restream_cpu_peak_pct: f64,
    pub(super) ffmpeg_cpu_avg_pct: f64,
    pub(super) ffmpeg_cpu_peak_pct: f64,
    pub(super) total_cpu_avg_pct: f64,
    pub(super) total_cpu_peak_pct: f64,
    pub(super) rss_avg_kb: f64,
    pub(super) rss_peak_kb: u64,
    pub(super) ffmpeg_rss_peak_kb: u64,
    pub(super) retained_peak_kb: u64,
    pub(super) source_ring_peak_kb: u64,
    pub(super) transcoder_ring_peak_kb: u64,
    pub(super) tsmux_ring_peak_kb: u64,
    pub(super) avio_len_peak_kb: u64,
    pub(super) avio_hwm_peak_kb: u64,
    pub(super) anonymous_peak_kb: u64,
    pub(super) private_dirty_peak_kb: u64,
    pub(super) shared_clean_peak_kb: u64,
    pub(super) pss_peak_kb: u64,
    pub(super) unattributed_peak_kb: u64,
    pub(super) active_transcoder_buffers_peak: u64,
    pub(super) ingests_peak: usize,
    pub(super) egresses_peak: usize,
    pub(super) stages_peak: usize,
    pub(super) pipeline_count_peak: usize,
}

/// Static labels and dimensions for one resource-sweep scenario.
pub(super) struct ResourceScenarioMeta<'a> {
    pub(super) scenario: &'a str,
    pub(super) label: String,
    pub(super) pipelines: usize,
    pub(super) outputs: usize,
    pub(super) ingest_types: String,
    pub(super) egress_mix: String,
    pub(super) transcode: &'a str,
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

pub(super) async fn sample_resource_window(
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

pub(super) fn summarize_resource_samples(
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

pub(super) fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(super) fn read_proc_stat_ticks(pid: u32) -> Result<u64, String> {
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

pub(super) fn read_proc_status_kb_checked(
    pid: u32,
    key: &str,
    log_path: &Path,
) -> Result<u64, String> {
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

pub(super) fn resource_aggregate_json(aggregate: &ResourceAggregate) -> Value {
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

pub(super) fn write_resource_sweep_csv(
    path: &Path,
    rows: &[ResourceAggregate],
) -> Result<(), String> {
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

pub(super) fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
