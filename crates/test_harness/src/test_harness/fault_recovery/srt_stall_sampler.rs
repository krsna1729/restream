//! Process and engine sampling for the frozen-destination proof in
//! `fault.srt-output-stall`.
//!
//! The test owns one sampling task from live progress until capture
//! completion. The task stops when the test sends the stop signal or drops
//! the sender, and it finishes any sample already in flight first; dropping
//! the [`Sampler`] without [`Sampler::finish`] aborts the task. The test
//! awaits the task before it stops the restream child, so every sample runs
//! against a live engine and any sample error is a capture failure.

use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use super::*;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Two sampling intervals.
const MAX_SAMPLE_GAP_SECS: f64 = 2.0;

/// A slower sample is a sample error. Sample starts are at most
/// `max(SAMPLE_INTERVAL, SAMPLE_TIMEOUT)` apart, under
/// [`MAX_SAMPLE_GAP_SECS`], and a stuck request cannot hide a gap at the end
/// of the capture.
const SAMPLE_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long the output must keep retrying against the frozen receiver
/// before the endpoint RSS reading.
pub(super) const RETRY_WINDOW: Duration = Duration::from_secs(120);

/// Phase times in seconds since sampling started (live progress).
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CaptureMarkers {
    pub(super) sigstop: f64,
    pub(super) retry_observed: f64,
    pub(super) output_stopped: f64,
    pub(super) pipeline_deleted: f64,
    pub(super) capture_complete: f64,
}

/// One line of `samples.jsonl`.
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProcessSample {
    /// Seconds since sampling started.
    pub(super) t: f64,
    pub(super) rss_kb: u64,
    pub(super) pss_kb: Option<u64>,
    pub(super) threads: u64,
    pub(super) fds: u64,
    pub(super) srt_shards: Vec<Value>,
    pub(super) outputs: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) memory_accounting: Option<Value>,
}

#[derive(Debug, Default)]
pub(super) struct SamplerOutcome {
    pub(super) samples: Vec<ProcessSample>,
    /// Failed samples, as `t=<secs>s: <error>`.
    pub(super) errors: Vec<String>,
}

/// Aborts the sampling task on drop unless [`Sampler::finish`] took it.
pub(super) struct Sampler {
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<SamplerOutcome, String>>>,
}

impl Sampler {
    /// Start sampling `pid` now (the first sample is immediate) and write
    /// each sample to `path`.
    pub(super) fn spawn(
        api: &RampApi,
        pid: u32,
        origin: Instant,
        path: PathBuf,
    ) -> Result<Self, String> {
        let file = std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let api = RampApi {
            client: api.client.clone(),
            base_url: api.base_url.clone(),
            cookie: api.cookie.clone(),
        };
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(sample_until_stopped(api, pid, origin, file, stopped));
        Ok(Self {
            stop: Some(stop),
            task: Some(task),
        })
    }

    /// Stop sampling and wait for the task, including a sample in flight.
    /// The handle stays in `self` while awaited, so cancelling this future
    /// drops `self` and aborts the task instead of detaching it.
    pub(super) async fn finish(mut self) -> Result<SamplerOutcome, String> {
        // A send error means the task already ended; its result says why.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let task = self.task.as_mut().ok_or("sampler task already taken")?;
        let result = task.await;
        self.task = None;
        result.map_err(|e| format!("sampler task failed: {e}"))?
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn sample_until_stopped(
    api: RampApi,
    pid: u32,
    origin: Instant,
    mut file: std::fs::File,
    mut stop: oneshot::Receiver<()>,
) -> Result<SamplerOutcome, String> {
    let mut ticks = tokio::time::interval(SAMPLE_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut outcome = SamplerOutcome::default();
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => return Ok(outcome),
            _ = ticks.tick() => {}
        }
        let t = origin.elapsed().as_secs_f64();
        let sample = tokio::time::timeout(SAMPLE_TIMEOUT, take_sample(&api, pid, t))
            .await
            .unwrap_or_else(|_| {
                Err(format!(
                    "sample timed out after {:.1}s",
                    SAMPLE_TIMEOUT.as_secs_f64()
                ))
            });
        let line = match sample {
            Ok(sample) => {
                let line = serde_json::to_string(&sample).map_err(|e| e.to_string())?;
                outcome.samples.push(sample);
                line
            }
            Err(error) => {
                let line = json!({ "t": t, "error": error }).to_string();
                outcome.errors.push(format!("t={t:.1}s: {error}"));
                line
            }
        };
        writeln!(file, "{line}").map_err(|e| format!("samples.jsonl: {e}"))?;
    }
}

async fn take_sample(api: &RampApi, pid: u32, t: f64) -> Result<ProcessSample, String> {
    let proc_dir = format!("/proc/{pid}");
    let status = std::fs::read_to_string(format!("{proc_dir}/status"))
        .map_err(|e| format!("{proc_dir}/status: {e}"))?;
    let status_field = |key: &str| {
        proc_field_u64(&status, key).ok_or_else(|| format!("{key} missing in {proc_dir}/status"))
    };
    let rss_kb = status_field("VmRSS")?;
    let threads = status_field("Threads")?;
    let pss_kb = std::fs::read_to_string(format!("{proc_dir}/smaps_rollup"))
        .ok()
        .and_then(|rollup| proc_field_u64(&rollup, "Pss"));
    let fds = std::fs::read_dir(format!("{proc_dir}/fd"))
        .map_err(|e| format!("{proc_dir}/fd: {e}"))?
        .count() as u64;

    let metrics = api
        .get_json("/metrics/system")
        .await
        .map_err(|e| format!("/metrics/system: {e}"))?;
    let srt_shards = metrics["egressShards"]
        .as_array()
        .ok_or("/metrics/system: no egressShards")?
        .iter()
        .filter(|shard| shard["protocol"] == "srt")
        .cloned()
        .collect();
    let health = api
        .get_json("/api/v1/engine/health")
        .await
        .map_err(|e| format!("/api/v1/engine/health: {e}"))?;
    let telemetry = api
        .get_json("/api/v1/engine/telemetry")
        .await
        .map_err(|e| format!("/api/v1/engine/telemetry: {e}"))?;
    let memory_accounting = telemetry
        .get("memoryAccounting")
        .filter(|value| !value.is_null())
        .cloned();

    Ok(ProcessSample {
        t,
        rss_kb,
        pss_kb,
        threads,
        fds,
        srt_shards,
        outputs: output_rows(&health),
        memory_accounting,
    })
}

/// First number of a `/proc` `Key:   value [kB]` line.
fn proc_field_u64(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// Every pipeline's output rows from `/api/v1/engine/health`, by output id.
fn output_rows(health: &Value) -> Value {
    let rows = health["pipelines"]
        .as_object()
        .into_iter()
        .flat_map(|pipelines| pipelines.values())
        .filter_map(|pipeline| pipeline["outputs"].as_object())
        .flat_map(|outputs| outputs.iter().map(|(id, row)| (id.clone(), row.clone())))
        .collect();
    Value::Object(rows)
}

/// Why the capture is not evidence: sample errors, a sample without
/// positive RSS, a gap over two intervals, a phase marker (sampling start
/// included) with no sample start within two intervals, samples that do not
/// span from before SIGSTOP to after pipeline deletion, or a retry window
/// under [`RETRY_WINDOW`]. Empty means the capture is complete.
pub(super) fn capture_errors(outcome: &SamplerOutcome, markers: &CaptureMarkers) -> Vec<String> {
    let mut errors: Vec<String> = outcome
        .errors
        .iter()
        .map(|error| format!("sample error at {error}"))
        .collect();
    let samples = &outcome.samples;
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        errors.push("no process samples captured".to_string());
        return errors;
    };
    errors.extend(
        samples
            .iter()
            .filter(|sample| sample.rss_kb == 0)
            .map(|sample| format!("no positive RSS in the sample at {:.1}s", sample.t)),
    );
    errors.extend(samples.windows(2).filter_map(|pair| {
        let gap = pair[1].t - pair[0].t;
        (gap > MAX_SAMPLE_GAP_SECS)
            .then(|| format!("sampling gap of {gap:.1}s ends at {:.1}s", pair[1].t))
    }));
    let phases = [
        ("sampling start", 0.0),
        ("SIGSTOP", markers.sigstop),
        ("retry observation", markers.retry_observed),
        ("output stop", markers.output_stopped),
        ("pipeline deletion", markers.pipeline_deleted),
        ("capture completion", markers.capture_complete),
    ];
    errors.extend(phases.into_iter().filter_map(|(phase, at)| {
        let nearest = samples
            .iter()
            .map(|sample| (sample.t - at).abs())
            .fold(f64::INFINITY, f64::min);
        (nearest > MAX_SAMPLE_GAP_SECS)
            .then(|| format!("no sample within {MAX_SAMPLE_GAP_SECS:.1}s of {phase} at {at:.1}s"))
    }));
    if first.t >= markers.sigstop {
        errors.push(format!(
            "first sample at {:.1}s is not before SIGSTOP at {:.1}s",
            first.t, markers.sigstop
        ));
    }
    if last.t <= markers.pipeline_deleted {
        errors.push(format!(
            "last sample at {:.1}s is not after pipeline deletion at {:.1}s",
            last.t, markers.pipeline_deleted
        ));
    }
    let retry_window = markers.output_stopped - markers.retry_observed;
    if retry_window < RETRY_WINDOW.as_secs_f64() {
        errors.push(format!(
            "retry window sampled for {retry_window:.1}s, under {}s",
            RETRY_WINDOW.as_secs()
        ));
    }
    errors
}

/// Reported, not gated: peak RSS growth over the last sample before
/// SIGSTOP, and the sampled maxima.
pub(super) fn sampled_summary(samples: &[ProcessSample], sigstop: f64) -> Value {
    let baseline = samples
        .iter()
        .rev()
        .find(|sample| sample.t < sigstop)
        .map(|sample| sample.rss_kb);
    let peak = samples.iter().map(|sample| sample.rss_kb).max();
    json!({
        "sampleCount": samples.len(),
        "baselineRssKb": baseline,
        "peakRssGrowthKb": baseline.zip(peak).map(|(baseline, peak)| peak.saturating_sub(baseline)),
        "rssMbMax": peak.map(|kb| kb / 1024),
        "threadsMax": samples.iter().map(|sample| sample.threads).max(),
        "fdsMax": samples.iter().map(|sample| sample.fds).max(),
    })
}

/// Retry-window RSS growth limit. Current runs grow ~3.5 MiB; the historical
/// per-retry shard-churn staircase grew 58.5 MiB over the same window.
const MAX_RETRY_RSS_GROWTH_KB: u64 = 16 * 1024;

/// The pass gate: peak RSS between retry observation and output stop over
/// the first sample at or after retry observation. Ring warm-up before the
/// first retry stays out of the baseline. No sample in the window is not
/// evidence of bounded memory.
pub(super) fn retry_window_rss(samples: &[ProcessSample], markers: &CaptureMarkers) -> Value {
    let mut window = samples
        .iter()
        .filter(|sample| (markers.retry_observed..=markers.output_stopped).contains(&sample.t))
        .map(|sample| sample.rss_kb);
    let baseline = window.next().filter(|kb| *kb > 0);
    let peak = baseline.map(|baseline| window.fold(baseline, u64::max));
    let growth = baseline.zip(peak).map(|(baseline, peak)| peak - baseline);
    json!({
        "available": growth.is_some(),
        "bounded": growth.is_some_and(|growth| growth <= MAX_RETRY_RSS_GROWTH_KB),
        "baselineKb": baseline,
        "peakKb": peak,
        "growthKb": growth,
        "limitKb": MAX_RETRY_RSS_GROWTH_KB,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t: f64, rss_kb: u64) -> ProcessSample {
        ProcessSample {
            t,
            rss_kb,
            threads: 40,
            ..ProcessSample::default()
        }
    }

    /// Cancelling `finish` while it waits must abort the sampling task, not
    /// detach it: the task's state is dropped once the runtime runs again.
    #[tokio::test]
    async fn cancelling_finish_aborts_the_sampling_task() {
        let alive = std::sync::Arc::new(());
        let held = alive.clone();
        let sampler = Sampler {
            stop: None,
            task: Some(tokio::spawn(async move {
                let _held = held;
                std::future::pending::<Result<SamplerOutcome, String>>().await
            })),
        };
        let cancelled = tokio::time::timeout(Duration::from_millis(20), sampler.finish()).await;
        assert!(cancelled.is_err(), "finish must still be waiting");
        for _ in 0..100 {
            if std::sync::Arc::strong_count(&alive) == 1 {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("sampling task survived a cancelled finish");
    }

    /// One sample per second from live progress to capture completion.
    fn complete_capture() -> (SamplerOutcome, CaptureMarkers) {
        let samples = (0..=141).map(|t| sample(f64::from(t), 100_000)).collect();
        let markers = CaptureMarkers {
            sigstop: 4.5,
            retry_observed: 9.5,
            output_stopped: 135.0,
            pipeline_deleted: 138.0,
            capture_complete: 141.5,
        };
        (
            SamplerOutcome {
                samples,
                errors: Vec::new(),
            },
            markers,
        )
    }

    #[test]
    fn frozen_capture_accepts_continuous_positive_coverage() {
        let (outcome, markers) = complete_capture();
        assert_eq!(capture_errors(&outcome, &markers), Vec::<String>::new());
    }

    #[test]
    fn frozen_capture_needs_a_sample_near_capture_completion() {
        // The last sample starts after deletion, then blocks past completion.
        let (mut outcome, markers) = complete_capture();
        outcome.samples.truncate(139);
        outcome.samples.push(sample(138.5, 100_000));
        let errors = capture_errors(&outcome, &markers);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("of capture completion"), "{errors:?}");
    }

    #[test]
    fn frozen_capture_rejects_gaps_over_two_intervals() {
        let (mut outcome, markers) = complete_capture();
        outcome.samples.remove(50);
        assert!(
            capture_errors(&outcome, &markers).is_empty(),
            "a 2 s gap is allowed"
        );

        outcome.samples[50].t = 51.01;
        let errors = capture_errors(&outcome, &markers);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("sampling gap"), "{errors:?}");
    }

    #[test]
    fn frozen_capture_rejects_missing_rss_and_sample_errors() {
        let (mut outcome, markers) = complete_capture();
        outcome.samples[20].rss_kb = 0;
        outcome
            .errors
            .push("t=30.0s: /metrics/system: timed out".to_string());
        let errors = capture_errors(&outcome, &markers);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("no positive RSS")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("/metrics/system")),
            "{errors:?}"
        );

        outcome.samples.clear();
        outcome.errors.clear();
        assert_eq!(
            capture_errors(&outcome, &markers),
            ["no process samples captured"]
        );
    }

    #[test]
    fn frozen_capture_must_span_the_fault_and_the_retry_window() {
        let (complete, markers) = complete_capture();
        let late_start = SamplerOutcome {
            samples: complete.samples.into_iter().skip(5).collect(),
            errors: Vec::new(),
        };
        let errors = capture_errors(&late_start, &markers);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("before SIGSTOP")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("of sampling start")),
            "{errors:?}"
        );

        let (mut early_end, markers) = complete_capture();
        early_end.samples.truncate(139);
        let errors = capture_errors(&early_end, &markers);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors.iter().any(|e| e.contains("after pipeline deletion")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("of capture completion")),
            "{errors:?}"
        );

        let (outcome, mut markers) = complete_capture();
        markers.output_stopped = markers.retry_observed + 119.9;
        let errors = capture_errors(&outcome, &markers);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("retry window"), "{errors:?}");
    }

    #[test]
    fn sampled_growth_is_measured_from_the_last_sample_before_sigstop() {
        let samples = [
            sample(0.0, 90_000),
            sample(1.0, 100_000),
            sample(2.0, 160_000),
            sample(3.0, 120_000),
        ];
        let summary = sampled_summary(&samples, 1.5);
        assert_eq!(summary["baselineRssKb"], 100_000);
        assert_eq!(summary["peakRssGrowthKb"], 60_000);
        assert_eq!(summary["rssMbMax"], 156);
        assert_eq!(summary["threadsMax"], 40);

        let no_baseline = sampled_summary(&samples, 0.0);
        assert!(no_baseline["peakRssGrowthKb"].is_null(), "{no_baseline}");
    }

    /// [`complete_capture`] markers with SIGSTOP just before a retry window
    /// set to `window`.
    fn window_markers(window: std::ops::RangeInclusive<f64>) -> CaptureMarkers {
        CaptureMarkers {
            sigstop: *window.start() - 0.5,
            retry_observed: *window.start(),
            output_stopped: *window.end(),
            ..complete_capture().1
        }
    }

    fn series(rss: &[u64]) -> Vec<ProcessSample> {
        (0u32..)
            .zip(rss)
            .map(|(t, kb)| sample(f64::from(t), *kb))
            .collect()
    }

    #[test]
    fn retry_window_gate_rejects_the_historical_shard_churn_staircase() {
        // Recorded RSS (KB) from retry observation through each 6-shard
        // spawn step to the end of the old frozen-peer baseline run.
        let samples = series(&[141_788, 160_632, 160_632, 175_120, 187_088, 201_708]);
        let gate = retry_window_rss(&samples, &window_markers(0.0..=5.0));
        assert_eq!(gate["growthKb"], 59_920, "{gate}");
        assert_eq!(gate["bounded"], false, "{gate}");
    }

    #[test]
    fn retry_window_gate_ignores_pre_retry_warm_up() {
        // +12.5 MB of ring warm-up between SIGSTOP and retry, then flat.
        let samples = series(&[129_196, 135_000, 141_696, 141_696, 141_696]);
        let markers = CaptureMarkers {
            sigstop: 0.5,
            ..window_markers(2.0..=4.0)
        };
        let gate = retry_window_rss(&samples, &markers);
        assert_eq!(gate["baselineKb"], 141_696, "{gate}");
        assert_eq!(gate["growthKb"], 0, "{gate}");
        assert_eq!(gate["bounded"], true, "{gate}");
    }

    #[test]
    fn retry_window_gate_requires_a_sample_in_the_window() {
        for samples in [series(&[100, 200]), series(&[100, 200, 0, 300]), Vec::new()] {
            let gate = retry_window_rss(&samples, &window_markers(2.0..=3.5));
            assert_eq!(gate["available"], false, "{gate}");
            assert_eq!(gate["bounded"], false, "{gate}");
            assert!(gate["growthKb"].is_null(), "{gate}");
        }
    }

    #[test]
    fn retry_window_gate_takes_the_peak_and_keeps_the_16_mib_boundary() {
        for (peak, bounded) in [
            (100 + MAX_RETRY_RSS_GROWTH_KB, true),
            (101 + MAX_RETRY_RSS_GROWTH_KB, false),
        ] {
            let samples = series(&[100, peak, 100]);
            let gate = retry_window_rss(&samples, &window_markers(0.0..=2.0));
            assert_eq!(gate["peakKb"], peak, "{gate}");
            assert_eq!(gate["bounded"], bounded, "{gate}");
        }
    }
}
