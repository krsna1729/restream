//! `hls-push-duty` execution: build the two arms, drive a rated window, and
//! emit the isolation result JSON.

use super::host_probe::{HostProbe, HostProbeReport};
use super::*;
use crate::hls_pull_duty::metrics::{pull_delivered_ok, pull_published_ok};
use crate::hls_pull_duty::run::{create_pull_input, promote_input, wait_for_preview_master};

/// The pull source's preview must serve a master playlist before the pulls
/// can start.
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(90);

/// Empirical mean-cost regression threshold for the measured TS workload
/// on this host class (60-170 us observed). This is not a hard work budget;
/// hardware, host stalls, and pending polls all affect the mean.
const MAX_MEAN_POLL_CPU_US: f64 = 250.0;

/// One pipeline under test: an ingest input plus one RTMP sink output.
struct PipelineTask {
    kind: &'static str,
    name: String,
    stream_key: String,
    pipeline_id: String,
    output_id: String,
    sink_port: u16,
    metrics: Arc<GeneralizedSinkMetrics>,
    sink: Option<GeneralizedSinkServer>,
}

/// Static paths and ports plan for one run.
struct Plan {
    ports: TestPorts,
    ffmpeg: PathBuf,
    fixture: PathBuf,
    work_dir: PathBuf,
    sink_ports: Vec<u16>,
}

impl Plan {
    fn hls_url(&self, stream_key: &str) -> String {
        format!(
            "http://127.0.0.1:{}/ingest/hls/{stream_key}/index.m3u8",
            self.ports.http
        )
    }

    fn rtmp_publish_url(&self, stream_key: &str) -> String {
        format!("rtmp://127.0.0.1:{}/live/{stream_key}", self.ports.rtmp)
    }
}

/// Live state owned across the rated window so every early return still tears
/// the publishers, sinks and Restream process down.
struct Session {
    restream: Child,
    api: RampApi,
    tasks: Vec<PipelineTask>,
    publishers: Vec<Child>,
}

/// What the rated window started from: the executor snapshot and the host's
/// memory counters, subtracted from the end of the window in the report.
struct Window {
    secs: f64,
    executor_start: Value,
    memory_start: host_memory::HostMemorySample,
    /// The host-stall probe's report, when `--host-probe` ran it.
    host_probe: Option<HostProbeReport>,
}

pub(crate) async fn run_hls_push_duty(
    options: HlsPushOptions,
    args: &[String],
) -> Result<Value, String> {
    let started = Instant::now();
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/hls-push-duty"));
    std::fs::create_dir_all(&work_dir).map_err(|error| error.to_string())?;

    let ports = TestPorts::from_env();
    let restream_bin = default_restream_bin();
    let plan = Plan {
        ffmpeg: restream::ffmpeg_binary::ffmpeg_bin_path().to_path_buf(),
        fixture: restream::test_fixtures::canonical_h264_ts_fixture()?,
        sink_ports: sink_ports(options.total_pipelines())?,
        ports: TestPorts {
            http: ports.http,
            rtmp: ports.rtmp,
            srt: ports.srt,
        },
        work_dir: work_dir.clone(),
    };
    let db_path = default_work_db_path(&work_dir, "hls-push-duty.sqlite");
    let log_path = work_dir.join("restream.log");

    println!(
        "[hls-push-duty] {}",
        json!({
            "phase": "config",
            "basePublishers": options.base_publishers,
            "hlsPublishers": options.hls_publishers,
            "pullSessions": options.pull_sessions,
            "segmentType": options.segment_type.as_str(),
            "durationSecs": options.duration_secs,
            "segmentSecs": options.segment_secs,
            "pollUsage": options.poll_usage,
            "ports": {"http": ports.http, "rtmp": ports.rtmp, "srt": ports.srt},
            "ffmpeg": plan.ffmpeg.display().to_string(),
            "fixture": plan.fixture.display().to_string(),
        })
    );

    let env_overrides: Vec<(&str, String)> = if options.poll_usage {
        vec![("RESTREAM_MEDIA_POLL_USAGE", "1".to_string())]
    } else {
        Vec::new()
    };
    let restream =
        start_restream_child_with_env(&restream_bin, &ports, &db_path, &log_path, &env_overrides)
            .await?;
    let api = login_api(&ports).await?;
    let mut session = Session {
        restream,
        api,
        tasks: Vec::new(),
        publishers: Vec::new(),
    };
    let outcome = session.execute(&options, &plan, started, args).await;
    session.shutdown().await;

    match outcome {
        Ok(report) => {
            let report_path = work_dir.join("report.json");
            if let Ok(bytes) = serde_json::to_vec_pretty(&report) {
                let _ = std::fs::write(&report_path, bytes);
            }
            Ok(report)
        }
        Err(error) => Err(error),
    }
}

impl Session {
    async fn execute(
        &mut self,
        options: &HlsPushOptions,
        plan: &Plan,
        started: Instant,
        args: &[String],
    ) -> Result<Value, String> {
        self.create_pipelines_and_sinks(options, plan).await?;
        self.spawn_publishers(options, plan)?;
        self.wait_for_media(0).await?;
        if options.pull_sessions > 0 {
            let published = self.tasks.len();
            self.create_pulls(options, plan).await?;
            self.wait_for_media(published).await?;
        }
        println!(
            "[hls-push-duty] {}",
            json!({"phase": "ready", "pipelines": self.tasks.len()})
        );

        // Each task's probe and sink counters before the window, so the pass
        // checks below require progress inside the rated window rather than
        // totals since the mode started; the executor and host memory
        // counters likewise, so the report carries the window's deltas.
        let baselines = self.snapshot_baselines().await;
        let executor_start = self.media_executor().await?;
        let memory_start = host_memory::capture(self.restream.id());
        let probe = options
            .host_probe
            .then(|| HostProbe::start(Duration::from_millis(1), Duration::from_millis(4)))
            .transpose()?;
        let window_started = Instant::now();
        tokio::time::sleep(options.window()).await;
        let window = Window {
            secs: window_started.elapsed().as_secs_f64(),
            executor_start,
            memory_start,
            host_probe: probe.map(HostProbe::stop).transpose()?,
        };
        let report = self
            .sample(options, plan, started, args, &baselines, &window)
            .await?;
        Ok(report)
    }

    /// Wait until every task from `first` on has a live ingest with media.
    async fn wait_for_media(&self, first: usize) -> Result<(), String> {
        for task in &self.tasks[first..] {
            wait_for_api_input_live(&self.api, &task.pipeline_id, Duration::from_secs(60)).await?;
        }
        for task in &self.tasks[first..] {
            wait_for_api_input_media_ready(&self.api, &task.pipeline_id, Duration::from_secs(60))
                .await?;
        }
        Ok(())
    }

    async fn media_executor(&self) -> Result<Value, String> {
        let dashboard = self.api.get_json("/api/v1/dashboard/runtime").await?;
        Ok(dashboard
            .pointer("/metrics/mediaExecutor")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Each task's probe and sink summary as the rated window opens.
    async fn snapshot_baselines(&self) -> Vec<(Value, Value)> {
        let mut out = Vec::with_capacity(self.tasks.len());
        for task in &self.tasks {
            let probe = self
                .api
                .get_json(&format!("/api/v1/pipelines/{}/probe", task.pipeline_id))
                .await
                .unwrap_or(Value::Null);
            out.push((probe, task.metrics.summary()));
        }
        out
    }

    /// One pipeline with an RTMP sink output of its own; `kind` labels it.
    async fn create_pipeline_with_sink(
        &mut self,
        plan: &Plan,
        kind: &'static str,
        name: String,
        stream_key: String,
    ) -> Result<(), String> {
        let index = self.tasks.len();
        let pipeline_id = create_resource_pipeline(&self.api, &name, &stream_key).await?;
        let sink_port = plan.sink_ports[index];
        let metrics = Arc::new(GeneralizedSinkMetrics::default());
        let sink = start_generalized_sink_server(sink_port, metrics.clone()).await?;
        let output_url = format!("rtmp://127.0.0.1:{sink_port}/live/{name}");
        let output_id = create_output(
            &self.api,
            &pipeline_id,
            &format!("sink-{index:02}"),
            &output_url,
            "source",
        )
        .await?;
        start_output(&self.api, &pipeline_id, &output_id).await?;
        self.tasks.push(PipelineTask {
            kind,
            name,
            stream_key,
            pipeline_id,
            output_id,
            sink_port,
            metrics,
            sink: Some(sink),
        });
        Ok(())
    }

    async fn create_pipelines_and_sinks(
        &mut self,
        options: &HlsPushOptions,
        plan: &Plan,
    ) -> Result<(), String> {
        for index in 0..options.total_publishers() {
            let kind = if index < options.base_publishers {
                "rtmp"
            } else if index < options.base_publishers + options.hls_publishers {
                "hls"
            } else {
                // The pull arm's origin: an RTMP pipeline of its own, so
                // the base pipelines do not also package the pulled preview.
                "pull-source"
            };
            let name = format!("hls-push-{kind}-{index:02}");
            let stream_key = format!("sk-hls-push-{kind}-{index:02}");
            self.create_pipeline_with_sink(plan, kind, name, stream_key)
                .await?;
        }
        println!(
            "[hls-push-duty] {}",
            json!({"phase": "pipelines-live", "pipelines": self.tasks.len()})
        );
        Ok(())
    }

    /// The pull arm: once the pull source's preview serves a master
    /// playlist, `pull_sessions` pipelines, each with a selected `hlsPull`
    /// input on that preview (presenting the harness's session cookie) and
    /// its own sink output.
    async fn create_pulls(&mut self, options: &HlsPushOptions, plan: &Plan) -> Result<(), String> {
        let source = self
            .tasks
            .iter()
            .find(|task| task.kind == "pull-source")
            .ok_or("the pull arm has no pull-source pipeline")?;
        let preview_path = format!("/hls/{}/master.m3u8", source.pipeline_id);
        let preview_url = format!("http://127.0.0.1:{}{preview_path}", plan.ports.http);
        wait_for_preview_master(&self.api, &preview_path, PREVIEW_TIMEOUT).await?;
        let cookie = self
            .api
            .cookie
            .clone()
            .ok_or("harness logged in without a session cookie to present to the preview")?;
        for pull in 0..options.pull_sessions {
            let index = self.tasks.len();
            self.create_pipeline_with_sink(
                plan,
                "hls-pull",
                format!("hls-push-hls-pull-{index:02}"),
                format!("sk-hls-push-hls-pull-{index:02}"),
            )
            .await?;
            let pipeline_id = self.tasks[index].pipeline_id.clone();
            let input_id = create_pull_input(
                &self.api,
                &pipeline_id,
                &format!("Pull {pull:02}"),
                &preview_url,
                &cookie,
            )
            .await?;
            // The pull only publishes while selected (see hls-pull-duty).
            promote_input(&self.api, &pipeline_id, &input_id).await?;
        }
        println!(
            "[hls-push-duty] {}",
            json!({"phase": "pulls-created", "pulls": options.pull_sessions, "url": preview_url})
        );
        Ok(())
    }

    fn spawn_publishers(&mut self, options: &HlsPushOptions, plan: &Plan) -> Result<(), String> {
        for (index, task) in self.tasks.iter().enumerate() {
            let log_path = plan.work_dir.join(format!("publisher-{index:02}.log"));
            let argv = if task.kind == "hls" {
                hls_publisher_args(
                    &plan.fixture,
                    &plan.hls_url(&task.stream_key),
                    options.segment_secs,
                    options.segment_type,
                    options.method,
                )
            } else {
                rtmp_publisher_args(&plan.fixture, &plan.rtmp_publish_url(&task.stream_key))
            };
            let child = spawn_ffmpeg(
                &plan.ffmpeg,
                &argv,
                &log_path,
                &format!("hls-push-{index:02}"),
            )?;
            self.publishers.push(child);
        }
        println!(
            "[hls-push-duty] {}",
            json!({"phase": "publishers-started", "publishers": self.publishers.len()})
        );
        Ok(())
    }

    async fn sample(
        &self,
        options: &HlsPushOptions,
        plan: &Plan,
        started: Instant,
        args: &[String],
        baselines: &[(Value, Value)],
        window: &Window,
    ) -> Result<Value, String> {
        let memory_end = host_memory::capture(self.restream.id());
        let dashboard = self.api.get_json("/api/v1/dashboard/runtime").await?;
        let metrics = dashboard.get("metrics").cloned().unwrap_or(Value::Null);
        let media_executor = metrics.get("mediaExecutor").cloned().unwrap_or(Value::Null);
        let shards: Vec<Value> = metrics
            .get("egressShards")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let shard_lookup = egress_shard_lookup(&shards);

        let telemetry = self.api.get_json("/api/v1/engine/telemetry").await?;
        let egress_by_output: HashMap<&str, &Value> = telemetry
            .get("egresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|egress| {
                egress
                    .get("outputId")
                    .and_then(Value::as_str)
                    .map(|id| (id, egress))
            })
            .collect();

        let mut rows = Vec::with_capacity(self.tasks.len());
        let mut failures: Vec<String> = Vec::new();
        for (index, task) in self.tasks.iter().enumerate() {
            let probe = self
                .api
                .get_json(&format!("/api/v1/pipelines/{}/probe", task.pipeline_id))
                .await
                .unwrap_or(Value::Null);
            let egress = egress_by_output.get(task.output_id.as_str()).copied();
            let shard = egress
                .and_then(|egress| egress.get("fabricFeed").and_then(Value::as_str))
                .and_then(|feed| {
                    shards
                        .iter()
                        .find(|shard| shard.get("feedId").and_then(Value::as_str) == Some(feed))
                })
                .or_else(|| {
                    let protocol = egress
                        .and_then(|egress| egress.get("fabricFeed").and_then(Value::as_str))
                        .and_then(|feed| feed.split(':').next())
                        .unwrap_or("sink")
                        .to_string();
                    let index =
                        egress.and_then(|egress| egress.get("shardId").and_then(Value::as_u64))?;
                    shard_lookup.get(&(protocol, index))
                });
            let sink_summary = task.metrics.summary();
            let executor =
                pipeline_poll_window(&window.executor_start, &media_executor, &task.pipeline_id);
            let input = input_published_summary(&probe);
            let keyframe_count = gop_keyframe_count(&probe);
            let (baseline_probe, baseline_sink) = baselines
                .get(index)
                .map(|(probe, sink)| (probe.clone(), sink.clone()))
                .unwrap_or((Value::Null, Value::Null));
            // The rated window is what this mode measures, so a check requires
            // progress *inside* it: an output that delivered during readiness
            // and then stalled for the whole window passes a lifetime check.
            // The pulled preview is Restream's own HLS output, whose audio
            // cadence is the preview's, so a pull pipeline is held to the
            // hls-pull-duty gate's checks rather than the push ingests'.
            let (published_now, delivered_now) = if task.kind == "hls-pull" {
                (pull_published_ok(&probe), pull_delivered_ok(&sink_summary))
            } else {
                (
                    published_ok(&probe, keyframe_count),
                    delivered_ok(&sink_summary),
                )
            };
            let published_progress = published_progress_ok(&baseline_probe, &probe);
            let delivered_progress = delivered_progress_ok(&baseline_sink, &sink_summary);
            let published = published_now && published_progress;
            let delivered = delivered_now && delivered_progress;
            if !published_now {
                failures.push(format!("{}: ingest published no media", task.name));
            }
            if !published_progress {
                failures.push(format!(
                    "{}: ingest received no new media during the window",
                    task.name
                ));
            }
            if !delivered_now {
                failures.push(format!("{}: output delivered no media", task.name));
            }
            if !delivered_progress {
                failures.push(format!(
                    "{}: output delivered no new media during the window",
                    task.name
                ));
            }
            if task.kind == "hls"
                && let Err(error) = check_mean_poll_cpu(&executor, MAX_MEAN_POLL_CPU_US)
            {
                failures.push(format!("{}: {error}", task.name));
            }
            rows.push(json!({
                "name": task.name,
                "kind": task.kind,
                "pipelineId": task.pipeline_id,
                "streamKey": task.stream_key,
                "segmentType": (task.kind == "hls").then(|| options.segment_type.as_str()),
                "method": (task.kind == "hls").then(|| options.method.as_str()),
                "outputId": task.output_id,
                "sinkPort": task.sink_port,
                "input": input,
                "publishedPackets": {
                    "video": sink_summary.get("videoCount").cloned().unwrap_or(Value::Null),
                    "audio": sink_summary.get("audioCount").cloned().unwrap_or(Value::Null),
                    "keyframes": sink_summary.get("keyframeCount").cloned().unwrap_or(Value::Null),
                    "bytes": sink_summary.get("bytes").cloned().unwrap_or(Value::Null),
                },
                "sink": sink_summary,
                "windowDelivery": window_delivery(&baseline_sink, &sink_summary, window.secs),
                "executor": executor,
                "egress": pipeline_egress_summary(egress, shard),
                "checks": {
                    "inputPublished": published,
                    "outputDelivered": delivered,
                    "inputPublishedProgress": published_progress,
                    "outputDeliveredProgress": delivered_progress,
                },
            }));
        }

        let passed = failures.is_empty();
        let cpu_count = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(0);
        let poll_cpu = executor_class_windows(&window.executor_start, &media_executor, window.secs);
        let report = json!({
            "passed": passed,
            "mode": "hls-push-duty",
            "config": {
                "basePublishers": options.base_publishers,
                "hlsPublishers": options.hls_publishers,
                "pullSessions": options.pull_sessions,
                "segmentType": options.segment_type.as_str(),
                "method": options.method.as_str(),
                "durationSecs": options.duration_secs,
                "segmentSecs": options.segment_secs,
                "pollUsage": options.poll_usage,
                "hostProbe": options.host_probe,
                "cliArgs": args,
            },
            "environment": {
                "cpuCount": cpu_count,
                "ffmpeg": plan.ffmpeg.display().to_string(),
                "fixture": plan.fixture.display().to_string(),
                "restreamBin": default_restream_bin().display().to_string(),
                "ports": {"http": plan.ports.http, "rtmp": plan.ports.rtmp},
            },
            "wallClockSecs": round2(started.elapsed().as_secs_f64()),
            "windowSecs": round2(window.secs),
            "pollCpu": poll_cpu,
            "hostProbe": window.host_probe,
            "hostMemory": host_memory::host_memory_report(
                &window.memory_start,
                &memory_end,
                host_memory::thp_settings(),
            ),
            "mediaExecutor": media_executor,
            "egressShards": shards,
            "pipelines": rows,
            "failedChecks": failures,
        });
        println!(
            "[hls-push-duty] {}",
            json!({
                "phase": "done",
                "passed": passed,
                "pitfalls": report["failedChecks"],
            })
        );
        if !passed {
            let report_path = plan.work_dir.join("report.json");
            let _ = std::fs::write(
                &report_path,
                serde_json::to_vec_pretty(&report).unwrap_or_default(),
            );
            return Err(format!(
                "hls-push-duty failed: {}; report={}",
                report["failedChecks"],
                report_path.display()
            ));
        }
        Ok(report)
    }

    async fn shutdown(&mut self) {
        for mut publisher in self.publishers.drain(..) {
            stop_child(&mut publisher).await;
        }
        for task in &self.tasks {
            let _ = self
                .api
                .post_null(&format!(
                    "/api/v1/pipelines/{}/outputs/{}/stop",
                    task.pipeline_id, task.output_id
                ))
                .await;
        }
        for task in &mut self.tasks {
            if let Some(sink) = task.sink.take() {
                stop_generalized_sink_server(sink);
            }
        }
        stop_child(&mut self.restream).await;
    }
}

/// The RTMP baseline publisher: real-time copy of the checked-in fixture.
pub(crate) fn rtmp_publisher_args(fixture: &Path, url: &str) -> Vec<String> {
    vec![
        "-re".to_string(),
        "-stream_loop".to_string(),
        "-1".to_string(),
        "-i".to_string(),
        fixture.display().to_string(),
        "-map".to_string(),
        "0:v".to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        "-f".to_string(),
        "flv".to_string(),
        url.to_string(),
    ]
}

/// The HLS treatment publisher: real-time HLS push over HTTP PUT.
pub(crate) fn hls_publisher_args(
    fixture: &Path,
    url: &str,
    segment_secs: u64,
    segment_type: HlsSegmentType,
    method: HlsMethod,
) -> Vec<String> {
    let mut argv = vec![
        "-re".to_string(),
        "-stream_loop".to_string(),
        "-1".to_string(),
        "-i".to_string(),
        fixture.display().to_string(),
        "-map".to_string(),
        "0:v".to_string(),
        "-map".to_string(),
        "0:a:0".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        "-f".to_string(),
        "hls".to_string(),
        "-method".to_string(),
        method.as_str().to_string(),
        "-hls_time".to_string(),
        segment_secs.to_string(),
    ];
    argv.extend(segment_type.ffmpeg_args().iter().map(|arg| arg.to_string()));
    argv.push(url.to_string());
    argv
}

fn spawn_ffmpeg(
    ffmpeg: &Path,
    argv: &[String],
    log_path: &Path,
    scope: &str,
) -> Result<Child, String> {
    let mut command = command_with_optional_cgroup(ffmpeg, scope);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    command.args(argv);
    let log = std::fs::File::create(log_path).map_err(|error| error.to_string())?;
    let stderr = log.try_clone().map_err(|error| error.to_string())?;
    command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", ffmpeg.display()))
}

/// The RTMP sink for each pipeline needs its own port so the sink summary can
/// be attributed to exactly one output.
fn sink_ports(count: usize) -> Result<Vec<u16>, String> {
    let base = harness_port_defaults().sink;
    let mut ports = Vec::with_capacity(count);
    for index in 0..count {
        let port = base
            .checked_add(u16::try_from(index).map_err(|_| "too many sink ports".to_string())?)
            .ok_or_else(|| "hls-push-duty sink port range overflow".to_string())?;
        ports.push(port);
    }
    Ok(ports)
}

/// Every publisher this mode starts maps the fixture's video and ADTS AAC
/// audio, so both checks require audio outright rather than asking the
/// system under test whether its source had any.
pub(crate) fn published_ok(probe: &Value, keyframe_count: Option<u64>) -> bool {
    let bytes = probe
        .pointer("/ingest/bytesReceived")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let video_present = probe.get("video").is_some_and(|video| !video.is_null());
    let audio_present = probe
        .get("audioTracks")
        .and_then(Value::as_array)
        .is_some_and(|tracks| !tracks.is_empty());
    bytes > 0 && video_present && audio_present && keyframe_count.is_some_and(|count| count >= 1)
}

pub(crate) fn delivered_ok(sink_summary: &Value) -> bool {
    let count = |name: &str| sink_summary.get(name).and_then(Value::as_u64).unwrap_or(0);
    count("videoCount") > 0
        && count("keyframeCount") > 0
        && count("bytes") > 0
        // An output that delivered none of the source's audio is the
        // HLS-push failure this mode catches (the output planned its tracks
        // before the ingest's probe was recorded).
        && count("audioCount") > 0
        // One AAC frame per audio message: the fixture's ~47 frames/s against
        // ~58 video frames/s puts audio near 0.8 per video frame. A TS audio
        // PES packs several frames, so an output that forwards one
        // PES per message falls to about 0.1; half a message per video frame
        // separates the two with room for start-up skew.
        && count("audioCount").saturating_mul(2) >= count("videoCount")
        && sink_summary.get("dtsMonotone").and_then(Value::as_bool) == Some(true)
}

/// Whether the ingest received media **during the window**: its byte counter
/// moved since the baseline. The rated window is what this mode measures, so a
/// publisher that delivered during readiness and then stalled for the whole
/// window must fail rather than pass on lifetime totals.
pub(crate) fn published_progress_ok(baseline: &Value, probe: &Value) -> bool {
    let bytes = |value: &Value| {
        value
            .pointer("/ingest/bytesReceived")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    bytes(probe) > bytes(baseline)
}

/// The same for the sink: media arrived at the output during the window.
/// `mediaProgressMs` is the sink's accumulated forward media time, so a stall
/// inside the window shows up here even when the counts were already non-zero,
/// and a re-anchor does not freeze it.
pub(crate) fn delivered_progress_ok(baseline: &Value, sink_summary: &Value) -> bool {
    let count = |value: &Value, name: &str| value.get(name).and_then(Value::as_u64).unwrap_or(0);
    count(sink_summary, "videoCount") > count(baseline, "videoCount")
        && count(sink_summary, "mediaProgressMs") > count(baseline, "mediaProgressMs")
        && count(sink_summary, "audioCount") > count(baseline, "audioCount")
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
