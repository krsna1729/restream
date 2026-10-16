//! `hls-push-duty` execution: build the two arms, drive a rated window, and
//! emit the isolation result JSON.

use super::*;

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
        sink_ports: sink_ports(options.total_publishers())?,
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
            "segmentType": options.segment_type.as_str(),
            "durationSecs": options.duration_secs,
            "segmentSecs": options.segment_secs,
            "ports": {"http": ports.http, "rtmp": ports.rtmp, "srt": ports.srt},
            "ffmpeg": plan.ffmpeg.display().to_string(),
            "fixture": plan.fixture.display().to_string(),
        })
    );

    let (restream, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;
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
        for task in &self.tasks {
            wait_for_api_input_live(&self.api, &task.pipeline_id, Duration::from_secs(60)).await?;
        }
        for task in &self.tasks {
            wait_for_api_input_media_ready(&self.api, &task.pipeline_id, Duration::from_secs(60))
                .await?;
        }
        println!(
            "[hls-push-duty] {}",
            json!({"phase": "ready", "pipelines": self.tasks.len()})
        );

        tokio::time::sleep(options.window()).await;
        let report = self.sample(options, plan, started, args).await?;
        Ok(report)
    }

    async fn create_pipelines_and_sinks(
        &mut self,
        options: &HlsPushOptions,
        plan: &Plan,
    ) -> Result<(), String> {
        for index in 0..options.total_publishers() {
            let kind = if index < options.base_publishers {
                "rtmp"
            } else {
                "hls"
            };
            let name = format!("hls-push-{kind}-{index:02}");
            let stream_key = format!("sk-hls-push-{kind}-{index:02}");
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
        }
        println!(
            "[hls-push-duty] {}",
            json!({"phase": "pipelines-live", "pipelines": self.tasks.len()})
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
    ) -> Result<Value, String> {
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
        for task in &self.tasks {
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
            let input = input_published_summary(&probe);
            let keyframe_count = gop_keyframe_count(&probe);
            let published = published_ok(&probe, keyframe_count);
            let delivered = delivered_ok(&sink_summary);
            if !published {
                failures.push(format!("{}: ingest published no media", task.name));
            }
            if !delivered {
                failures.push(format!("{}: output delivered no media", task.name));
            }
            rows.push(json!({
                "name": task.name,
                "kind": task.kind,
                "pipelineId": task.pipeline_id,
                "streamKey": task.stream_key,
                "segmentType": (task.kind == "hls").then(|| options.segment_type.as_str()),
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
                "egress": pipeline_egress_summary(egress, shard),
                "checks": {
                    "inputPublished": published,
                    "outputDelivered": delivered,
                },
            }));
        }

        let passed = failures.is_empty();
        let cpu_count = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(0);
        let report = json!({
            "passed": passed,
            "mode": "hls-push-duty",
            "config": {
                "basePublishers": options.base_publishers,
                "hlsPublishers": options.hls_publishers,
                "segmentType": options.segment_type.as_str(),
                "durationSecs": options.duration_secs,
                "segmentSecs": options.segment_secs,
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
        "PUT".to_string(),
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
        && sink_summary.get("dtsMonotone").and_then(Value::as_bool) == Some(true)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
