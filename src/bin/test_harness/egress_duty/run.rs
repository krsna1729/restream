//! Child lifecycle, artifact accumulation, and the rated-window run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::super::*;

use super::*;
pub(super) const ARTIFACT_FILE: &str = "egress-duty.json";
/// Wait for the product's effective-config startup event to reach the
/// redirected log. A read immediately after HTTP readiness can race the
/// logger's file write, which would turn a real bound into an unproven
/// `null` artifact value.
async fn wait_for_visit_max_bytes(path: &PathBuf, timeout: Duration) -> Option<u64> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Some(value) = parse_visit_max_bytes(&text)
        {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Children this mode spawns, stopped in a defined order on every exit path.
#[derive(Default)]
pub(super) struct DutyChildren {
    pub(super) receiver: Option<super::Child>,
    pub(super) receiver_pid: Option<u32>,
    pub(super) receiver_sudo: bool,
    pub(super) receiver_exit: Value,
    pub(super) restream: Option<super::Child>,
    pub(super) publisher: Option<super::Child>,
}

impl DutyChildren {
    pub(super) async fn stop_publisher(&mut self) {
        if let Some(mut child) = self.publisher.take() {
            stop_child(&mut child).await;
        }
    }

    pub(super) async fn stop_restream(&mut self) {
        if let Some(mut child) = self.restream.take() {
            stop_child(&mut child).await;
        }
    }

    /// Stop the receiver with `SIGTERM` (its clean-stop path: drain, write the
    /// result row, exit). The receiver is root-owned inside the namespace when
    /// it was launched through `sudo`, so the signal goes through `sudo -n`.
    pub(super) async fn stop_receiver(&mut self) -> Value {
        let Some(mut child) = self.receiver.take() else {
            return json!({"stopped": false, "reason": "not running"});
        };
        let mut signaled = false;
        let mut error = Value::Null;
        if let Some(pid) = self.receiver_pid {
            let mut kill = if self.receiver_sudo {
                let mut kill = Command::new("sudo");
                kill.args(["-n", "kill"]);
                kill
            } else {
                // Same UID, so a plain `libc::kill` needs no helper binary.
                let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                if rc == 0 {
                    signaled = true;
                } else {
                    error = json!(format!(
                        "kill({pid}, SIGTERM): {}",
                        std::io::Error::last_os_error()
                    ));
                }
                Command::new("true")
            };
            if self.receiver_sudo {
                match kill
                    .args(["-TERM", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                {
                    Ok(status) if status.success() => signaled = true,
                    Ok(status) => error = json!(format!("sudo kill exit status {status}")),
                    Err(err) => error = json!(err.to_string()),
                }
            }
        }
        let wait = tokio::time::timeout(Duration::from_secs(20), child.wait()).await;
        let (exited_before_timeout, exit) = match wait {
            Ok(Ok(status)) => (true, json!(status.to_string())),
            Ok(Err(error)) => (true, json!(format!("wait failed: {error}"))),
            Err(_) => (false, json!("killed after SIGTERM timeout")),
        };
        if !exited_before_timeout {
            stop_child(&mut child).await;
        }
        self.receiver_exit = exit.clone();
        json!({
            "stopped": true,
            "pid": self.receiver_pid,
            "signaled": signaled,
            "exitedBeforeTimeout": exited_before_timeout,
            "exit": exit,
            "signalError": error,
        })
    }

    /// Ordered teardown for a failed attempt: publisher (the drain's source) →
    /// Restream → receiver (whose clean stop writes its result row).
    pub(super) async fn stop_all(&mut self) -> Value {
        self.stop_publisher().await;
        self.stop_restream().await;
        let receiver = self.stop_receiver().await;
        json!({"publisher": "stopped", "restream": "stopped", "receiver": receiver})
    }
}

/// Accumulates the artifact, and preserves it when the run is rejected.
pub(super) struct Artifact {
    pub(super) value: Value,
    pub(super) path: PathBuf,
}

impl Artifact {
    pub(super) fn new(cfg: &EgressDutyConfig) -> Self {
        let path = artifact_path(ARTIFACT_FILE);
        Self {
            value: json!({
                "mode": "egress-duty",
                "stage": "D",
                "config": cfg.json(),
                "artifactPath": path.display().to_string(),
            }),
            path,
        }
    }

    pub(super) fn set(&mut self, key: &str, value: Value) {
        self.value[key] = value;
    }

    /// Fill one field of the `observed` block created at receiver startup.
    pub(super) fn set_observed(&mut self, key: &str, value: Value) {
        if let Some(observed) = self.value["observed"].as_object_mut() {
            observed.insert(key.to_string(), value);
        }
    }

    pub(super) fn write(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(
            &self.path,
            serde_json::to_vec_pretty(&self.value).unwrap_or_default(),
        )
        .map_err(|error| error.to_string())
    }

    /// Preserve a rejected attempt before the error is returned.
    pub(super) fn reject(&mut self, error: &str) {
        self.value["verdict"] = json!("rejected");
        self.value["error"] = json!(error);
        let reasons = self.value["verdictReasons"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut reasons: Vec<Value> = reasons;
        reasons.push(json!(error));
        self.value["verdictReasons"] = Value::Array(reasons);
        if let Err(write_error) = self.write() {
            eprintln!("[egress-duty] failed to preserve rejected artifact: {write_error}");
        } else {
            println!(
                "[egress-duty] preserved rejected attempt at {}",
                self.path.display()
            );
        }
    }
}

pub(super) async fn run_duty(
    cfg: &EgressDutyConfig,
    artifact: &mut Artifact,
) -> Result<(), String> {
    if !cfg.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {} (set EGRESS_DUTY_RESTREAM_BIN)",
            cfg.restream_bin.display()
        ));
    }
    if !cfg.receiver_bin.exists() {
        return Err(format!(
            "receiver binary not found at {} (set EGRESS_DUTY_RECEIVER_BIN)",
            cfg.receiver_bin.display()
        ));
    }

    let netns = match &cfg.netns {
        Some(netns) => {
            let resolved = probe_netns(netns)?;
            artifact.set("netns", resolved.json());
            Some(resolved)
        }
        None => {
            artifact.set("netns", json!({"prefix": []}));
            None
        }
    };

    let mut children = DutyChildren::default();
    // The harness keeps itself (and therefore every child it spawns without an
    // explicit mask — the ffmpeg publisher, API polling) off the receiver's
    // CPUs: children inherit this mask, and a debug-profile harness polling the
    // API from the receiver's cores was measurably starving the receiver's own
    // datapath queue. Restream and the receiver override it (taskset / --cpus).
    let harness_mask = parse_cpu_mask(&cfg.harness_cpus)?;
    let observed_harness = set_affinity_mask(0, &harness_mask)?;
    artifact.set(
        "harnessAffinity",
        json!({
            "requested": harness_mask.iter().collect::<Vec<_>>(),
            "observed": observed_harness,
        }),
    );
    let outcome = run_duty_inner(cfg, artifact, &mut children, netns.as_ref()).await;
    if outcome.is_err() {
        // Errors tear down in the same defined order; the success path has
        // already emptied every child.
        let teardown = children.stop_all().await;
        artifact.set("teardown", teardown);
    }
    outcome
}

/// Wait until every SRT shard index the engine reports has a matching
/// `egress-shard-<n>` thread in `/proc/<pid>/task/*/comm`, then return those
/// threads with the engine's shard indices. Bounded: a partial match is
pub(super) async fn discover_srt_shard_threads(
    api: &RampApi,
    pid: u32,
    timeout: Duration,
) -> Result<(Vec<EgressShardThread>, Vec<u64>), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let indices = match api.get_json("/metrics/system").await {
            Ok(system) => engine_srt_shard_indices(&system),
            Err(_) => Vec::new(),
        };
        let all = select_egress_shard_threads(&read_thread_comms(pid)?);
        let matched: Vec<EgressShardThread> = all
            .iter()
            .filter(|thread| indices.contains(&u64::from(thread.index)))
            .cloned()
            .collect();
        let wanted: BTreeSet<u32> = indices.iter().map(|index| *index as u32).collect();
        let found: BTreeSet<u32> = matched.iter().map(|thread| thread.index).collect();
        if !indices.is_empty() && found == wanted {
            return Ok((matched, indices));
        }
        if Instant::now() >= deadline {
            println!(
                "[egress-duty] {}",
                json!({
                    "phase": "shard-discovery-timeout",
                    "srtShardIndices": indices,
                    "egressShardThreads": all.iter().map(EgressShardThread::json).collect::<Vec<_>>(),
                })
            );
            return Ok((matched, indices));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub(super) async fn run_duty_inner(
    cfg: &EgressDutyConfig,
    artifact: &mut Artifact,
    children: &mut DutyChildren,
    netns: Option<&NetnsExec>,
) -> Result<(), String> {
    let ports = TestPorts::from_env();
    let tsv_path = cfg.work_dir.join("receiver-result.tsv");
    let receiver_stdout = cfg.work_dir.join("receiver.stdout.log");
    let receiver_stderr = cfg.work_dir.join("receiver.stderr.log");
    let restream_log = cfg.work_dir.join("restream.log");
    let publisher_log = cfg.work_dir.join("publisher.log");
    let restream_db = default_work_db_path(&cfg.work_dir, "egress-duty.sqlite");
    let _ = std::fs::remove_file(&tsv_path);

    let spawned =
        receiver::spawn_receiver(cfg, netns, &tsv_path, &receiver_stdout, &receiver_stderr).await?;
    children.receiver = Some(spawned.child);
    children.receiver_pid = spawned.pid;
    children.receiver_sudo = spawned.sudo;
    artifact.set(
        "observed",
        json!({
            "receiverArgv": spawned.argv.clone(),
            "receiverPid": children.receiver_pid,
            "receiverSudo": children.receiver_sudo,
            "receiverTsv": tsv_path.display().to_string(),
        }),
    );
    println!(
        "[egress-duty] {}",
        json!({"phase": "receiver-started", "pid": children.receiver_pid, "argv": spawned.argv})
    );
    receiver::wait_for_listening(&receiver_stdout, &receiver_stderr, Duration::from_secs(15))
        .await?;
    let receiver_net_before = receiver::read_proc_net_counters(children.receiver_pid);

    // ── Restream ────────────────────────────────────────────────────────
    cleanup_ramp_db(&restream_db);
    let restream_mask = cfg.restream_mask();
    let log_dir = cfg.work_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|error| error.to_string())?;
    let log = std::fs::File::create(&restream_log).map_err(|error| error.to_string())?;
    let log_err = log.try_clone().map_err(|error| error.to_string())?;
    let mut restream_cmd = Command::new("taskset");
    restream_cmd
        .args(["-c", &restream_mask])
        .arg(&cfg.restream_bin)
        .env("RESTREAM_HTTP_PORT", ports.http.to_string())
        .env("RESTREAM_RTMP_PORT", ports.rtmp.to_string())
        .env("RESTREAM_SRT_PORT", ports.srt.to_string())
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
        .env("RESTREAM_LOG_DIR", &log_dir)
        .env(
            "RESTREAM_DB_PATH",
            restream_db.to_string_lossy().to_string(),
        )
        .env(
            "RESTREAM_EGRESS_SHARDS",
            cfg.requested_shards
                .unwrap_or(cfg.egress_shards)
                .to_string(),
        );
    if let Some(requested) = cfg.requested_shards {
        restream_cmd.env("RESTREAM_WI37_SRT_SHARDS", requested.to_string());
    }
    restream_cmd
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true);
    let restream = restream_cmd
        .spawn()
        .map_err(|error| format!("failed to spawn restream: {error}"))?;
    children.restream = Some(restream);
    let restream_pid = children
        .restream
        .as_ref()
        .and_then(super::Child::id)
        .ok_or("restream pid missing")?;
    wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", ports.http),
        Duration::from_secs(30),
    )
    .await
    .map_err(|error| format!("restream did not become ready: {error}"))?;
    let api = login_api(&ports).await?;
    let observed_mask = read_cpus_allowed_list(restream_pid);
    artifact.set_observed("restreamPid", json!(restream_pid));
    artifact.set_observed("restreamCpusAllowedList", json!(observed_mask));
    artifact.set_observed("restreamRequestedMask", json!(restream_mask));
    artifact.set_observed(
        "ports",
        json!({
            "http": ports.http,
            "rtmp": ports.rtmp,
            "srt": ports.srt,
        }),
    );
    println!(
        "[egress-duty] {}",
        json!({"phase": "restream-ready", "pid": restream_pid, "cpusAllowedList": observed_mask})
    );

    // ── Pipeline, publisher, outputs ────────────────────────────────────
    // The canonical SRT-ingest shape is the resource sweep's `h264-srt` config
    // row (`sweep_configs.json` index 1) published as mpegts over SRT; the
    // output shape is the catalog's `SrtSource` kind with the destination
    // replaced by the receiver's `DEST_BASE:PORT_BASE+i`.
    let sweep_config = SweepConfig {
        name: "h264-srt",
        ingest_proto: "srt",
        video_codec: "h264",
        multi_audio: false,
    };
    let output_kind = SweepOutputKind::SrtSource;
    let pipeline_id = create_resource_pipeline(&api, "egress-duty", "egress-duty-input").await?;
    let fixture = sweep_fixture(sweep_config, &cfg.bitrate)?;
    let publisher_url = harness_srt_ffmpeg_url(
        ports.srt,
        "egress-duty-input",
        HarnessSrtMode::Publish,
        None,
    );
    let publisher = spawn_publisher_with_selection(
        &fixture,
        &publisher_url,
        "mpegts",
        PublishTrackSelection::PrimaryAv,
        Some(&publisher_log),
    )?;
    children.publisher = Some(publisher);
    wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(45)).await?;

    let mut output_ids: Vec<String> = Vec::new();
    let mut urls: BTreeMap<String, String> = BTreeMap::new();
    for index in 0..cfg.outputs {
        let name = format!("egress-duty-{index:03}");
        let url = cfg.output_url(index);
        let output_id = create_output_with_rtmp_mode(
            &api,
            &pipeline_id,
            &name,
            &url,
            output_kind.encoding(false),
            output_kind.rtmp_mode(),
        )
        .await?;
        start_output(&api, &pipeline_id, &output_id).await?;
        urls.insert(output_id.clone(), url);
        output_ids.push(output_id);
    }
    let visit_max_env_value = std::env::var("RESTREAM_EGRESS_VISIT_MAX_BYTES").ok();
    let visit_max_bytes = wait_for_visit_max_bytes(&restream_log, Duration::from_secs(10)).await;
    artifact.set(
        "workload",
        json!({
            "pipelineId": pipeline_id,
            "fanout": cfg.outputs,
            "ingest": {
                "type": "srt",
                "sweepConfig": sweep_config.name,
                "fixtureBitrate": cfg.bitrate,
                "publisherUrl": publisher_url,
                "fixture": fixture.display().to_string(),
            },
            "output": {
                "kind": "SrtSource",
                "encoding": output_kind.encoding(false),
                "crypto": "none",
                "urls": output_ids.iter().map(|id| urls[id].clone()).collect::<Vec<_>>(),
                // The product-side bound on one shard visit's media hand-off,
                // read back from the product's own effective-config event. The
                // receiver's per-connection datapath queue has to be larger than
                // this or a single visit's burst is dropped at the receiver.
                "visitBurstBound": {
                    "knob": "RESTREAM_EGRESS_VISIT_MAX_BYTES",
                    "envValue": visit_max_env_value.clone(),
                    "observedVisitMaxBytes": visit_max_bytes,
                    "observedSource": "restream.config.effective (product startup log)",
                    "payloadBytes": HARNESS_SRT_PACKET_SIZE,
                    "datagramsPerVisit": visit_max_bytes.map(|bytes| bytes / u64::from(HARNESS_SRT_PACKET_SIZE)),
                },
            },
        }),
    );
    wait_for_outputs_progress(
        &api,
        &pipeline_id,
        &output_ids,
        Duration::from_secs(cfg.progress_timeout_secs),
    )
    .await?;
    println!(
        "[egress-duty] {}",
        json!({"phase": "outputs-live", "outputs": output_ids.len()})
    );

    // ── Discover and pin the SRT egress shard threads ───────────────────
    let ticks_per_sec = clock_ticks_per_sec();
    let (shard_threads, srt_shard_indices) =
        discover_srt_shard_threads(&api, restream_pid, Duration::from_secs(10)).await?;

    let mut affinity_rows = Vec::new();
    let mut pinned: Vec<EgressShardThread> = Vec::new();
    for thread in &shard_threads {
        let requested_cpu = cfg.shard_cpu_for_index(thread.index);
        let (observed, method, error) = match requested_cpu {
            Some(cpu) => match set_thread_affinity(thread.tid, cpu) {
                Ok((observed, method)) => (Some(observed), method, Value::Null),
                Err(error) => (None, "failed", json!(error)),
            },
            None => (
                None,
                "not-configured",
                json!("no configured CPU for this observed shard index"),
            ),
        };
        if error.is_null() {
            pinned.push(thread.clone());
        }
        affinity_rows.push(json!({
            "tid": thread.tid,
            "index": thread.index,
            "commTruncated": thread.comm_truncated,
            "requestedCpu": requested_cpu,
            "observedAffinity": observed,
            "method": method,
            "error": error,
        }));
    }
    let all_egress_threads = select_egress_shard_threads(&read_thread_comms(restream_pid)?);
    let mut per_index_counts: BTreeMap<u32, usize> = BTreeMap::new();
    for thread in &shard_threads {
        *per_index_counts.entry(thread.index).or_default() += 1;
    }
    let ambiguous: Vec<u32> = per_index_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(index, _)| *index)
        .collect();
    artifact.set_observed(
        "shardThreads",
        json!({
            "requestedShardCount": cfg.requested_shards,
            "configuredShardCpus": cfg.shard_cpus.iter().collect::<Vec<_>>(),
            "srtShardIndices": srt_shard_indices,
            "allEgressShardThreads": all_egress_threads.iter().map(EgressShardThread::json).collect::<Vec<_>>(),
            "srtShardThreads": shard_threads.iter().map(EgressShardThread::json).collect::<Vec<_>>(),
            "threadsPerIndex": per_index_counts,
            "ambiguousIndices": ambiguous,
            "affinity": affinity_rows,
            "processCpusAllowedList": read_cpus_allowed_list(restream_pid),
            "note": "a shard contributes more than one thread with the same comm: an unnamed thread created from a named one inherits its comm on Linux, so the shard's own compio runtime/driver thread also reads `egress-shard-<n>`. comm+index cannot separate them, so every match is pinned and its own CPU is reported per TID; the idle member of a pair is visible as ~0 cpuSecs",
        }),
    );
    println!(
        "[egress-duty] {}",
        json!({
            "phase": "shards-pinned",
            "srtShardIndices": srt_shard_indices,
            "tids": pinned.iter().map(|thread| thread.tid).collect::<Vec<_>>(),
        })
    );
    let system_before = api.get_json("/metrics/system").await?;
    let engine_before = engine_srt_counters(&system_before);
    let engine_shards_before = engine_srt_shard_counters(&system_before);
    let fault_before = engine_owner_faulted(&system_before);
    let process_before = read_proc_stat_cpu(&PathBuf::from(format!("/proc/{restream_pid}/stat")))?;
    let mut shard_cpu_before: BTreeMap<u32, CpuTicks> = BTreeMap::new();
    for thread in &pinned {
        let path = PathBuf::from(format!("/proc/{restream_pid}/task/{}/stat", thread.tid));
        shard_cpu_before.insert(thread.tid, read_proc_stat_cpu(&path)?);
    }
    let telemetry_before = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
        .await?;
    let outputs_before = output_samples(&telemetry_before, &urls);
    let window_open = Instant::now();

    let mut window_samples = vec![
        json!({"atSecs": 0.0, "outputs": outputs_before.iter().map(OutputSample::json).collect::<Vec<_>>()}),
    ];
    // Two mid-window samples: stability is a whole-window claim, and two
    // endpoints cannot see a stall that recovered.
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_secs(cfg.window_secs / 3)).await;
        let telemetry = api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
            .await?;
        let samples = output_samples(&telemetry, &urls);
        window_samples.push(json!({
            "atSecs": round6(window_open.elapsed().as_secs_f64()),
            "outputs": samples.iter().map(OutputSample::json).collect::<Vec<_>>(),
        }));
    }
    let remaining = cfg
        .window_secs
        .saturating_sub(window_open.elapsed().as_secs());
    tokio::time::sleep(Duration::from_secs(remaining)).await;

    let system_after = api.get_json("/metrics/system").await?;
    let engine_after = engine_srt_counters(&system_after);
    let engine_shards_after = engine_srt_shard_counters(&system_after);
    let engine_shard_deltas = counter_deltas(&engine_shards_before, &engine_shards_after);
    let fault_after = engine_owner_faulted(&system_after);
    let process_after = read_proc_stat_cpu(&PathBuf::from(format!("/proc/{restream_pid}/stat")))?;
    let mut shard_cpu_after: BTreeMap<u32, CpuTicks> = BTreeMap::new();
    for thread in &pinned {
        let path = PathBuf::from(format!("/proc/{restream_pid}/task/{}/stat", thread.tid));
        shard_cpu_after.insert(thread.tid, read_proc_stat_cpu(&path)?);
    }
    let telemetry_after = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
        .await?;
    let outputs_after = output_samples(&telemetry_after, &urls);
    let window_secs_observed = window_open.elapsed().as_secs_f64();
    window_samples.push(json!({
        "atSecs": round6(window_secs_observed),
        "outputs": outputs_after.iter().map(OutputSample::json).collect::<Vec<_>>(),
    }));

    let engine_deltas = counter_deltas(&engine_before, &engine_after);
    let data_first_delta = counter_at(&engine_deltas, &["txClass", "dataFirst"]);
    let wire_delta = counter_at(&engine_deltas, &["txPackets"]);
    let mut per_shard_cpu = Vec::new();
    let mut per_shard_cpu_by_index: BTreeMap<u32, (f64, usize)> = BTreeMap::new();
    let mut egress_thread_cpu_secs = 0.0;
    for thread in &pinned {
        let before = shard_cpu_before
            .get(&thread.tid)
            .copied()
            .unwrap_or_default();
        let after = shard_cpu_after
            .get(&thread.tid)
            .copied()
            .unwrap_or_default();
        let cpu_secs = cpu_secs_between(before, after, ticks_per_sec);
        if let Some(cpu_secs) = cpu_secs {
            egress_thread_cpu_secs += cpu_secs;
            let entry = per_shard_cpu_by_index.entry(thread.index).or_default();
            entry.0 += cpu_secs;
            entry.1 += 1;
        }
        per_shard_cpu.push(json!({
            "tid": thread.tid,
            "index": thread.index,
            "beforeTicks": before.total(),
            "afterTicks": after.total(),
            "cpuSecs": opt_round6(cpu_secs),
        }));
    }
    let mut per_shard_cpu_summary = Vec::new();
    let mut hottest_shard_cpu_utilization: f64 = 0.0;
    let mut coolest_shard_cpu_utilization = f64::INFINITY;
    for (index, (cpu_secs, thread_count)) in &per_shard_cpu_by_index {
        let data_first = engine_shard_deltas
            .get(index.to_string())
            .and_then(|counters| counter_at(counters, &["txClass", "dataFirst"]));
        let utilization = cpu_secs / window_secs_observed.max(0.001);
        hottest_shard_cpu_utilization = hottest_shard_cpu_utilization.max(utilization);
        coolest_shard_cpu_utilization = coolest_shard_cpu_utilization.min(utilization);
        per_shard_cpu_summary.push(json!({
            "index": index,
            "threadCount": thread_count,
            "cpuSecs": round6(*cpu_secs),
            "cpuUtilization": round6(utilization),
            "dataFirst": data_first,
            "usPerDataFirst": opt_round6(data_first.and_then(|units| micros_per_unit(*cpu_secs, units))),
        }));
    }
    if !coolest_shard_cpu_utilization.is_finite() {
        coolest_shard_cpu_utilization = 0.0;
    }
    let process_cpu_secs = cpu_secs_between(process_before, process_after, ticks_per_sec);
    let non_egress_cpu_secs = process_cpu_secs
        .zip(Some(egress_thread_cpu_secs))
        .map(|(process, egress)| (process - egress).max(0.0));
    artifact.set(
        "cpu",
        json!({
            "clockTicksPerSec": ticks_per_sec,
            "scope": {
                "egressThread": "sum over every matched SRT egress shard thread pinned to its configured shard CPU",
                "process": "/proc/<pid>/stat utime+stime (every Restream thread)",
                "nonEgress": "processCpuSecs - matched egress shard thread CPU; includes control/media and unclassified Restream threads",
            },
            "egressThreadCpuSecs": round6(egress_thread_cpu_secs),
            "egressThreadCpuSecsPerShard": per_shard_cpu_summary,
            "egressThreadCpuSecsPerThread": per_shard_cpu,
            "hottestShardCpuUtilization": round6(hottest_shard_cpu_utilization),
            "shardCpuImbalance": round6(
                hottest_shard_cpu_utilization - coolest_shard_cpu_utilization
            ),
            "egressThreadUsPerDataFirst": opt_round6(
                data_first_delta.and_then(|units| micros_per_unit(egress_thread_cpu_secs, units))
            ),
            "egressThreadUsPerWireDatagram": opt_round6(
                wire_delta.and_then(|units| micros_per_unit(egress_thread_cpu_secs, units))
            ),
            "processCpuSecs": opt_round6(process_cpu_secs),
            "nonEgressCpuSecs": opt_round6(non_egress_cpu_secs),
            "processUsPerDataFirst": opt_round6(
                data_first_delta.and_then(|units| process_cpu_secs.and_then(|secs| micros_per_unit(secs, units)))
            ),
            "processUsPerWireDatagram": opt_round6(
                wire_delta.and_then(|units| process_cpu_secs.and_then(|secs| micros_per_unit(secs, units)))
            ),
            "processCpuSecsLifecycle": opt_round6(cpu_secs_between(
                CpuTicks::default(),
                process_after,
                ticks_per_sec,
            )),
        }),
    );

    let mut unavailable = Vec::new();
    collect_null_paths(&engine_after, "engine.closing", &mut unavailable);
    artifact.set(
        "window",
        json!({
            "requestedSecs": cfg.window_secs,
            "observedSecs": round6(window_secs_observed),
            "samples": window_samples,
            "receiverFence": "whole-run: the pinned receiver has no per-interval output and one aggregate TSV row per process, so it cannot delimit or reset a rated window",
        }),
    );
    artifact.set(
        "engine",
        json!({
            "source": "GET /metrics/system egressShards[protocol=srt].srtOwners[] + shard counters",
            "baseline": engine_before,
            "closing": engine_after,
            "delta": engine_deltas,
            "perShardBaseline": engine_shards_before,
            "perShardClosing": engine_shards_after,
            "perShardDelta": engine_shard_deltas,
            "ownerFaultedBaseline": fault_before,
            "ownerFaultedClosing": fault_after,
        }),
    );
    artifact.set("unavailable", json!(unavailable));

    // ── Drain, then stop in the contract's order ────────────────────────
    children.stop_publisher().await;
    let drain = drain_to_quiescence(cfg, &api).await?;
    let system_final = api.get_json("/metrics/system").await?;
    let engine_final = engine_srt_counters(&system_final);
    let fault_final = engine_owner_faulted(&system_final);
    let process_final = read_proc_stat_cpu(&PathBuf::from(format!("/proc/{restream_pid}/stat")))?;
    artifact.set(
        "drain",
        json!({
            "settled": drain.0,
            "elapsedSecs": round6(drain.1),
            "engineAfterDrain": engine_final,
            "ownerFaultedAfterDrain": fault_final,
            "processCpuSecsLifecycle": opt_round6(cpu_secs_between(CpuTicks::default(), process_final, ticks_per_sec)),
        }),
    );
    children.stop_restream().await;
    let receiver_net_after = receiver::read_proc_net_counters(children.receiver_pid);
    let receiver_net_delta = counter_deltas(&receiver_net_before, &receiver_net_after);
    artifact.set(
        "receiverNetwork",
        json!({
            "before": receiver_net_before,
            "after": receiver_net_after,
            "delta": receiver_net_delta,
        }),
    );

    let receiver_teardown = children.stop_receiver().await;
    let receiver_stdout_text = std::fs::read_to_string(&receiver_stdout).unwrap_or_default();
    let receiver_stderr_text = std::fs::read_to_string(&receiver_stderr).unwrap_or_default();
    let receiver_row = match std::fs::read_to_string(&tsv_path) {
        Ok(text) if !text.trim().is_empty() => Some(
            parse_receiver_tsv(&text)
                .map_err(|error| format!("receiver TSV {}: {error}", tsv_path.display()))?,
        ),
        _ => None,
    };

    verdict::emit(VerdictInput {
        cfg,
        artifact,
        receiver_row,
        receiver_teardown,
        receiver_exit: children.receiver_exit.clone(),
        receiver_stdout_text: &receiver_stdout_text,
        receiver_stderr_text: &receiver_stderr_text,
        window_samples: &window_samples,
        window_secs_observed,
        outputs_before: &outputs_before,
        outputs_after: &outputs_after,
        receiver_net_delta,
        engine_after,
        engine_deltas,
        engine_final,
        fault_before,
        fault_after,
        fault_final,
        data_first_delta,
        wire_delta,
        shard_threads: &shard_threads,
        all_egress_threads: &all_egress_threads,
        srt_shard_indices: &srt_shard_indices,
        affinity_rows: &affinity_rows,
        per_index_counts: &per_index_counts,
        visit_max_bytes,
        visit_max_env_value,
        tsv_path: &tsv_path,
        receiver_stdout: &receiver_stdout,
        receiver_stderr: &receiver_stderr,
        restream_log: &restream_log,
        publisher_log: &publisher_log,
    })?;
    Ok(())
}

/// Stop refilling is the caller's job; this waits for the engine's own SRT
/// queues to reach quiescence so the receiver's whole-run total can be
/// reconciled against the engine's. Returns `(settled, elapsed_secs)`.
pub(super) async fn drain_to_quiescence(
    cfg: &EgressDutyConfig,
    api: &RampApi,
) -> Result<(bool, f64), String> {
    let deadline = Instant::now() + Duration::from_secs(cfg.drain_settle_secs);
    let started = Instant::now();
    let mut last_wire: Option<u64> = None;
    let mut stable_since: Option<Instant> = None;
    loop {
        let system = api.get_json("/metrics/system").await?;
        let counters = engine_srt_counters(&system);
        let wire = counter_at(&counters, &["txPackets"]);
        // Quiescence is "the sender stopped handing new datagrams to the wire".
        // `txInFlight` is a pool gauge, not a drain signal: it can sit above
        // zero on an idle pool, so it is recorded (in the artifact) rather than
        // fenced on.
        if wire == last_wire && last_wire.is_some() {
            let since = stable_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(1) {
                return Ok((true, started.elapsed().as_secs_f64()));
            }
        } else {
            stable_since = None;
        }
        last_wire = wire;
        if Instant::now() >= deadline {
            return Ok((false, started.elapsed().as_secs_f64()));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
