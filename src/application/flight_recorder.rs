//! A flight recorder per pipeline: a bounded, fixed-clock history of what
//! the stream-health monitor and the Stability Lab replay. Every
//! [`FLIGHT_SAMPLE_INTERVAL`] it reads the selected ingest (bitrate over the
//! interval, frame rate, keyframe interval, transport RTT and loss), the
//! source ring (fill, worst reader lag, overflows) and the pipeline's
//! outputs (wanted, delivering, degraded, retrying, stalled, offered and
//! delivered rate, resyncs, worst feed lag), and keeps
//! [`FLIGHT_HISTORY_SPAN`] of them (`GET /api/v1/pipelines/:id/flight-recorder`).
//!
//! It reads counters the engine already keeps, on its own task, once per
//! interval: nothing is added to the media path. The stream inspector
//! (`media::stream_inspector`), started here, reads the source rings' packet
//! headers on its own thread for each track's timestamps.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::application::fleet_history::is_delivering;
use crate::application::runtime_state::hub::StateHub;
use crate::application::runtime_state::{Convergence, Flow, OutputState};
use crate::media::engine::MediaEngine;
use restream_domain::domain::state::DesiredOutputState;

/// How often the recorder samples: the dashboard's own sampling clock.
pub const FLIGHT_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// How far back each pipeline's trace reaches.
pub const FLIGHT_HISTORY_SPAN: Duration = Duration::from_secs(10 * 60);

/// Samples kept per pipeline: the span at the interval.
pub const FLIGHT_HISTORY_CAP: usize =
    (FLIGHT_HISTORY_SPAN.as_secs() / FLIGHT_SAMPLE_INTERVAL.as_secs()) as usize;

/// Every this many samples, the newest of each pipeline is also persisted.
pub const FLIGHT_PERSIST_EVERY: u32 = 5;

/// The persisted history's spacing: [`FLIGHT_PERSIST_EVERY`] samples.
pub const FLIGHT_PERSIST_INTERVAL: Duration =
    Duration::from_secs(FLIGHT_SAMPLE_INTERVAL.as_secs() * FLIGHT_PERSIST_EVERY as u64);

/// The most samples one trace request returns; longer windows are thinned.
pub const FLIGHT_MAX_POINTS: usize = 2_000;

/// The trace format's name and version, for files exported from the page.
pub const TRACE_SCHEMA: &str = "restream.trace.v1";

/// The selected ingest at one instant. A value the publisher's protocol
/// does not report is absent, never zero.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IngestHealth {
    pub input_id: String,
    pub protocol: String,
    /// Bits per second over the last interval; absent on the session's
    /// first sample (no earlier counter to difference).
    pub bitrate_bps: Option<f64>,
    pub packets_per_sec: f64,
    pub video_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Mean and longest gap between the last keyframes the ingest saw.
    pub gop_sec: Option<f64>,
    pub max_gop_sec: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub loss_per_sec: Option<f64>,
    pub retrans_per_sec: Option<f64>,
    /// SRT receive buffer fill, 0..1.
    pub receive_buffer: Option<f64>,
    /// The picture and sound checks' findings, when they run for the
    /// pipeline (absent in samples taken before they existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qa: Option<crate::media::stream_qa::StreamQa>,
    /// Each track's timestamps and audio/video timing over the last minute
    /// (`media::stream_inspector`), read from the source ring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<crate::media::stream_inspector::StreamInspection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RingHealth {
    pub fill: u64,
    pub capacity: u64,
    pub worst_lag_slots: u64,
    /// Cumulative reader overflows (a reader fell off the ring).
    pub overflows: u64,
}

/// The pipeline's outputs at one instant.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OutputsHealth {
    pub wanted: u32,
    pub delivering: u32,
    pub degraded: u32,
    pub retrying: u32,
    pub stalled: u32,
    /// Sum of the retrying outputs' attempt counters: how hard the
    /// pipeline's destinations are being hammered.
    pub retry_attempts: u32,
    pub offered_bps: f64,
    pub delivered_bps: f64,
    /// Cumulative feed resyncs over the running egresses.
    pub resyncs: u64,
    pub worst_feed_lag: u64,
}

/// A transcoding stage's input: how far behind the source its FFmpeg is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecoderHealth {
    /// The stage kind, e.g. `transcode:h264_720p`.
    pub stage: String,
    /// Packets the stage's reader is behind the source ring's writer.
    pub lag_slots: u64,
    /// The same in seconds, at the ring's packet rate (absent until known).
    pub lag_sec: Option<f64>,
    /// Share of the interval the engine waited to write FFmpeg's input
    /// (its pipe was full: FFmpeg took input slower than it arrived), 0..1;
    /// absent on the stage's first sample.
    pub input_blocked: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FlightSample {
    pub at_ms: u64,
    pub ingest: Option<IngestHealth>,
    pub ring: Option<RingHealth>,
    pub outputs: OutputsHealth,
    /// The pipeline's FFmpeg stages, by stage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decoders: Vec<DecoderHealth>,
}

/// What a page or an export receives.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FlightTrace {
    pub schema: &'static str,
    pub pipeline_id: String,
    pub interval_ms: u64,
    /// After when the trace is complete: the recorder started then, the
    /// pipeline appeared then, or older samples were dropped.
    pub complete_from_ms: u64,
    /// Where the full-rate samples begin; older ones, from the persisted
    /// history, are [`FLIGHT_PERSIST_INTERVAL`] apart.
    pub detailed_from_ms: u64,
    pub persisted_interval_ms: u64,
    pub samples: Vec<FlightSample>,
}

/// One pipeline's ingest as read from the engine, before differencing.
#[derive(Debug, Clone, Default)]
pub struct IngestProbe {
    pub input_id: String,
    pub protocol: String,
    pub bytes_received: u64,
    pub packets_per_sec: f64,
    pub video_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Keyframe presentation times, milliseconds, oldest first.
    pub keyframe_ms: Vec<i64>,
    pub rtt_ms: Option<f64>,
    pub loss_per_sec: Option<f64>,
    pub retrans_per_sec: Option<f64>,
    pub receive_buffer: Option<f64>,
    pub qa: Option<crate::media::stream_qa::StreamQa>,
    pub timeline: Option<crate::media::stream_inspector::StreamInspection>,
}

/// One running egress's counters.
#[derive(Debug, Clone, Default)]
pub struct EgressProbe {
    pub offered_bps: Option<f64>,
    pub delivered_bps: Option<f64>,
    pub resyncs: u64,
    pub feed_lag: u64,
}

/// One FFmpeg stage's input counters, before differencing.
#[derive(Debug, Clone, Default)]
pub struct DecoderProbe {
    pub stage: String,
    pub lag_slots: u64,
    /// The source ring's packet rate, packets per second (0 unknown).
    pub packets_per_sec: f64,
    /// Cumulative microseconds spent blocked writing FFmpeg's input.
    pub stall_us: u64,
}

/// Everything the recorder reads about one pipeline at one instant.
#[derive(Debug, Clone, Default)]
pub struct PipelineProbe {
    pub pipeline_id: String,
    pub ingest: Option<IngestProbe>,
    pub ring: Option<RingHealth>,
    pub outputs: OutputsHealth,
    pub decoders: Vec<DecoderProbe>,
}

/// Mean and longest gap between successive keyframes, in seconds; `None`
/// with fewer than two. A gap that runs backwards (a timestamp reset) is
/// skipped rather than counted as zero.
pub fn keyframe_intervals(times_ms: &[i64]) -> (Option<f64>, Option<f64>) {
    let gaps: Vec<f64> = times_ms
        .windows(2)
        .filter(|pair| pair[1] > pair[0])
        .map(|pair| (pair[1] - pair[0]) as f64 / 1000.0)
        .collect();
    if gaps.is_empty() {
        return (None, None);
    }
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let max = gaps.iter().copied().fold(0.0, f64::max);
    (Some(mean), Some(max))
}

/// The output half of a sample, from the runtime state and the running
/// egresses' counters.
pub fn outputs_health<'a>(
    outputs: impl IntoIterator<Item = &'a OutputState>,
    egresses: &HashMap<String, EgressProbe>,
) -> OutputsHealth {
    let mut health = OutputsHealth::default();
    for output in outputs {
        if let Some(egress) = egresses.get(&output.id) {
            health.offered_bps += egress.offered_bps.unwrap_or(0.0);
            health.delivered_bps += egress.delivered_bps.unwrap_or(0.0);
            health.resyncs += egress.resyncs;
            health.worst_feed_lag = health.worst_feed_lag.max(egress.feed_lag);
        }
        if output.desired != DesiredOutputState::Running {
            continue;
        }
        health.wanted += 1;
        if is_delivering(output) {
            health.delivering += 1;
        }
        if output.diagnosis.is_some() {
            health.degraded += 1;
        }
        if matches!(
            output.convergence,
            Convergence::Retrying | Convergence::RetriesExhausted
        ) {
            health.retrying += 1;
            health.retry_attempts += output.retry.as_ref().map_or(0, |retry| retry.attempts);
        }
        if output.flow == Flow::Stalled {
            health.stalled += 1;
        }
    }
    health
}

struct Trace {
    samples: VecDeque<FlightSample>,
    complete_from_ms: u64,
    /// The selected session's byte counter at the last sample, for the
    /// next bitrate: (input, bytes, at).
    last_bytes: Option<(String, u64, u64)>,
    /// Each FFmpeg stage's blocked-write counter at the last sample: (µs, at).
    last_stalls: HashMap<String, (u64, u64)>,
}

/// Every pipeline's trace. Pipelines that disappear from a sample are
/// dropped with their trace.
pub struct FlightRecorder {
    traces: Mutex<HashMap<String, Trace>>,
    /// Each running destination's usual, fed on the same clock
    /// (`destination_baselines`); the alert rules read its anomalies.
    pub baselines: crate::application::destination_baselines::DestinationBaselines,
    cap: usize,
    started_ms: u64,
}

impl Default for FlightRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl FlightRecorder {
    pub fn new() -> Self {
        Self::started_at(MediaEngine::now_epoch_ms(), FLIGHT_HISTORY_CAP)
    }

    pub(crate) fn started_at(started_ms: u64, cap: usize) -> Self {
        Self {
            traces: Mutex::new(HashMap::new()),
            baselines: Default::default(),
            cap: cap.max(1),
            started_ms,
        }
    }

    /// Records one sample of every pipeline in `probes`, at `at_ms`.
    pub fn record(&self, at_ms: u64, probes: Vec<PipelineProbe>) {
        let mut traces = restream_sync::lock::lock(&self.traces);
        let present: std::collections::HashSet<&str> = probes
            .iter()
            .map(|probe| probe.pipeline_id.as_str())
            .collect();
        traces.retain(|pipeline_id, _| present.contains(pipeline_id.as_str()));
        for probe in probes {
            let trace = traces
                .entry(probe.pipeline_id.clone())
                .or_insert_with(|| Trace {
                    samples: VecDeque::new(),
                    complete_from_ms: at_ms.max(self.started_ms),
                    last_bytes: None,
                    last_stalls: HashMap::new(),
                });
            // Strictly after the newest sample, even if the wall clock
            // stepped back, so the trace stays in time order.
            let at_ms = trace
                .samples
                .back()
                .map_or(at_ms, |newest| at_ms.max(newest.at_ms.saturating_add(1)));
            let ingest = probe.ingest.map(|ingest| {
                let bitrate_bps = match &trace.last_bytes {
                    Some((input, bytes, then))
                        if *input == ingest.input_id
                            && ingest.bytes_received >= *bytes
                            && at_ms > *then =>
                    {
                        Some(
                            (ingest.bytes_received - bytes) as f64 * 8000.0 / (at_ms - then) as f64,
                        )
                    }
                    _ => None,
                };
                trace.last_bytes = Some((ingest.input_id.clone(), ingest.bytes_received, at_ms));
                let (gop_sec, max_gop_sec) = keyframe_intervals(&ingest.keyframe_ms);
                IngestHealth {
                    input_id: ingest.input_id,
                    protocol: ingest.protocol,
                    bitrate_bps,
                    packets_per_sec: ingest.packets_per_sec,
                    video_codec: ingest.video_codec,
                    width: ingest.width,
                    height: ingest.height,
                    fps: ingest.fps,
                    gop_sec,
                    max_gop_sec,
                    rtt_ms: ingest.rtt_ms,
                    loss_per_sec: ingest.loss_per_sec,
                    retrans_per_sec: ingest.retrans_per_sec,
                    receive_buffer: ingest.receive_buffer,
                    qa: ingest.qa,
                    timeline: ingest.timeline,
                }
            });
            if ingest.is_none() {
                trace.last_bytes = None;
            }
            if trace.samples.len() >= self.cap
                && let Some(evicted) = trace.samples.pop_front()
            {
                trace.complete_from_ms = evicted.at_ms;
            }
            let stalls_now: HashMap<String, (u64, u64)> = probe
                .decoders
                .iter()
                .map(|decoder| (decoder.stage.clone(), (decoder.stall_us, at_ms)))
                .collect();
            let decoders = probe
                .decoders
                .into_iter()
                .map(|decoder| {
                    let input_blocked = match trace.last_stalls.get(&decoder.stage) {
                        Some((stall_us, then))
                            if decoder.stall_us >= *stall_us && at_ms > *then =>
                        {
                            let waited_ms = (decoder.stall_us - stall_us) as f64 / 1_000.0;
                            Some((waited_ms / (at_ms - then) as f64).min(1.0))
                        }
                        _ => None,
                    };
                    DecoderHealth {
                        lag_sec: (decoder.packets_per_sec > 0.0)
                            .then(|| decoder.lag_slots as f64 / decoder.packets_per_sec),
                        lag_slots: decoder.lag_slots,
                        input_blocked,
                        stage: decoder.stage,
                    }
                })
                .collect::<Vec<_>>();
            trace.last_stalls = stalls_now;
            trace.samples.push_back(FlightSample {
                at_ms,
                ingest,
                ring: probe.ring,
                outputs: probe.outputs,
                decoders,
            });
        }
    }

    /// The pipeline's samples from `since_ms` (inclusive); `None` for a
    /// pipeline the recorder does not know.
    pub fn trace(&self, pipeline_id: &str, since_ms: u64) -> Option<FlightTrace> {
        let traces = restream_sync::lock::lock(&self.traces);
        let trace = traces.get(pipeline_id)?;
        let first = trace
            .samples
            .partition_point(|sample| sample.at_ms < since_ms);
        Some(FlightTrace {
            schema: TRACE_SCHEMA,
            pipeline_id: pipeline_id.to_string(),
            interval_ms: FLIGHT_SAMPLE_INTERVAL.as_millis() as u64,
            complete_from_ms: trace.complete_from_ms,
            detailed_from_ms: trace.complete_from_ms,
            persisted_interval_ms: FLIGHT_PERSIST_INTERVAL.as_millis() as u64,
            samples: trace.samples.range(first..).cloned().collect(),
        })
    }

    /// The newest sample of every pipeline, to persist.
    pub fn newest(&self) -> Vec<(String, FlightSample)> {
        let traces = restream_sync::lock::lock(&self.traces);
        traces
            .iter()
            .filter_map(|(pipeline_id, trace)| {
                Some((pipeline_id.clone(), trace.samples.back()?.clone()))
            })
            .collect()
    }

    /// The kept samples in `[since_ms, until_ms)` and where the kept run
    /// begins; `None` for a pipeline the recorder does not know.
    fn memory_window(
        &self,
        pipeline_id: &str,
        since_ms: u64,
        until_ms: u64,
    ) -> Option<(u64, Vec<FlightSample>)> {
        let traces = restream_sync::lock::lock(&self.traces);
        let trace = traces.get(pipeline_id)?;
        let first = trace
            .samples
            .partition_point(|sample| sample.at_ms < since_ms);
        let end = trace
            .samples
            .partition_point(|sample| sample.at_ms < until_ms);
        Some((
            trace.complete_from_ms,
            trace
                .samples
                .range(first..end.max(first))
                .cloned()
                .collect(),
        ))
    }
}

/// At most `max` samples, evenly spread, the first and last always kept.
pub fn thin(samples: Vec<FlightSample>, max: usize) -> Vec<FlightSample> {
    let len = samples.len();
    if len <= max || max < 2 {
        return samples;
    }
    let mut picked = Vec::with_capacity(max);
    let mut next = 0usize;
    for (index, sample) in samples.into_iter().enumerate() {
        // The i-th pick is at round(i * (len - 1) / (max - 1)).
        let want = (next * (len - 1) + (max - 1) / 2) / (max - 1);
        if index == want {
            picked.push(sample);
            next += 1;
            if next == max {
                break;
            }
        }
    }
    picked
}

/// A pipeline's trace over `[since_ms, until_ms)`: the persisted history
/// for what the memory no longer holds, then the full-rate samples, thinned
/// to `max_points`. `None` when neither knows the pipeline.
pub async fn trace_window(
    recorder: &FlightRecorder,
    pool: &SqlitePool,
    pipeline_id: &str,
    since_ms: u64,
    until_ms: u64,
    max_points: usize,
) -> Option<FlightTrace> {
    let memory = recorder.memory_window(pipeline_id, since_ms, until_ms);
    let detailed_from_ms = memory.as_ref().map_or(until_ms, |(from, _)| *from);
    let mut samples = Vec::new();
    let mut oldest = memory.as_ref().map(|(from, _)| *from);
    if since_ms < detailed_from_ms {
        let to_i64 = |ms: u64| i64::try_from(ms).unwrap_or(i64::MAX);
        if let Ok(rows) = crate::db::flight_repo::window(
            pool,
            pipeline_id,
            to_i64(since_ms),
            to_i64(detailed_from_ms.min(until_ms)),
        )
        .await
        {
            samples.extend(
                rows.into_iter()
                    .filter_map(|(_, json)| serde_json::from_str::<FlightSample>(&json).ok()),
            );
        }
        if let Ok(Some(first)) = crate::db::flight_repo::oldest(pool, pipeline_id).await {
            let first = u64::try_from(first).unwrap_or(0);
            oldest = Some(oldest.map_or(first, |kept| kept.min(first)));
        }
    }
    if memory.is_none() && samples.is_empty() {
        return None;
    }
    if let Some((_, recent)) = memory {
        samples.extend(recent);
    }
    Some(FlightTrace {
        schema: TRACE_SCHEMA,
        pipeline_id: pipeline_id.to_string(),
        interval_ms: FLIGHT_SAMPLE_INTERVAL.as_millis() as u64,
        complete_from_ms: oldest.unwrap_or(since_ms),
        detailed_from_ms,
        persisted_interval_ms: FLIGHT_PERSIST_INTERVAL.as_millis() as u64,
        samples: thin(samples, max_points.clamp(2, FLIGHT_MAX_POINTS)),
    })
}

/// Persists the newest samples and drops what is older than `retention`.
async fn persist(
    recorder: &FlightRecorder,
    pool: &SqlitePool,
    now_ms: u64,
    retention: Duration,
    prune: bool,
) {
    let rows: Vec<(String, i64, String)> = recorder
        .newest()
        .into_iter()
        .filter_map(|(pipeline_id, sample)| {
            let json = serde_json::to_string(&sample).ok()?;
            Some((pipeline_id, i64::try_from(sample.at_ms).ok()?, json))
        })
        .collect();
    if let Err(err) = crate::db::flight_repo::insert_batch(pool, &rows).await {
        tracing::warn!(error = %err, "flight recorder: persisting samples failed");
    }
    if prune {
        let before = now_ms.saturating_sub(retention.as_millis() as u64);
        if let Err(err) =
            crate::db::flight_repo::prune(pool, i64::try_from(before).unwrap_or(0)).await
        {
            tracing::warn!(error = %err, "flight recorder: pruning history failed");
        }
    }
}

/// Reads one probe per pipeline from the engine and the runtime state.
async fn collect_probes(
    hub: &StateHub,
    engine: &MediaEngine,
    inspector: Option<&crate::media::stream_inspector::StreamInspector>,
) -> Vec<PipelineProbe> {
    let egresses: HashMap<String, EgressProbe> = engine
        .egresses
        .active
        .read()
        .await
        .values()
        .map(|egress| {
            let quality = restream_sync::lock::lock(&egress.quality);
            (
                egress.output_id.clone(),
                EgressProbe {
                    offered_bps: quality.offered_bps,
                    delivered_bps: quality.delivered_bps,
                    resyncs: egress.resync_count.load(Ordering::Relaxed),
                    feed_lag: egress.feed_lag_units.load(Ordering::Relaxed),
                },
            )
        })
        .collect();
    let mut ingests: HashMap<String, IngestProbe> = HashMap::new();
    // Each AAC track's frame length, for the inspector's clock drift.
    let mut aac_frames: HashMap<String, HashMap<u32, f64>> = HashMap::new();
    for (pipeline_id, ingest) in engine.ingests.active.read().await.iter() {
        let metadata = ingest.metadata();
        // Every track a multi-track source declared, else its one track.
        let tracks = restream_sync::lock::lock(&ingest.audio_tracks).clone();
        let frames: HashMap<u32, f64> = tracks
            .iter()
            .chain(
                tracks
                    .is_empty()
                    .then_some(metadata.audio.as_ref())
                    .flatten(),
            )
            .filter(|track| track.codec.eq_ignore_ascii_case("aac"))
            .filter_map(|track| {
                crate::media::stream_inspector::aac_frame_ms(track.sample_rate)
                    .map(|frame_ms| (track.track_index, frame_ms))
            })
            .collect();
        aac_frames.insert(pipeline_id.clone(), frames);
        let quality = &metadata.quality;
        let receive_buffer = match (
            quality.srt_recv_buf_packets,
            quality.srt_recv_buf_capacity_packets,
        ) {
            (Some(held), Some(capacity)) if capacity > 0 => {
                Some(f64::from(held) / f64::from(capacity))
            }
            _ => None,
        };
        let video = metadata.video.as_ref();
        ingests.insert(
            pipeline_id.clone(),
            IngestProbe {
                input_id: ingest.input_id.clone(),
                protocol: ingest.protocol.clone(),
                bytes_received: ingest.bytes_received.load(Ordering::Relaxed),
                packets_per_sec: ingest.metrics.snapshot().packets_per_sec,
                video_codec: video
                    .map(|video| video.codec.clone())
                    .filter(|codec| !codec.is_empty()),
                width: video.map(|video| video.width).filter(|width| *width > 0),
                height: video.map(|video| video.height).filter(|height| *height > 0),
                fps: video.map(|video| video.fps).filter(|fps| *fps > 0.0),
                keyframe_ms: restream_sync::lock::lock(&ingest.keyframe_times).clone(),
                rtt_ms: quality.ms_rtt.or(quality.tcp_rtt_ms),
                loss_per_sec: quality.packets_received_loss_per_sec,
                retrans_per_sec: quality.packets_received_retrans_per_sec,
                receive_buffer,
                qa: engine.stream_qa(pipeline_id),
                timeline: inspector.and_then(|inspector| inspector.inspection(pipeline_id)),
            },
        );
    }
    let mut rings: HashMap<String, RingHealth> = HashMap::new();
    let mut decoders: HashMap<String, Vec<DecoderProbe>> = HashMap::new();
    let mut watched = Vec::new();
    // Copied, so no engine lock is held while the next one is taken.
    let source_rings: HashMap<String, Arc<restream_sync::RingBuffer>> =
        engine.ingests.pipelines.read().await.clone();
    // An FFmpeg stage's reader on its source ring is named by its key.
    for (key, runtime) in engine.stages.runtimes.read().await.iter() {
        let (Some(pipe), Some(ring)) = (
            runtime.pipe_metrics.as_ref(),
            source_rings.get(key.pipeline.as_str()),
        ) else {
            continue;
        };
        let name = key.to_string();
        let Some(reader) = ring
            .reader_snapshots()
            .into_iter()
            .find(|reader| reader.name == name)
        else {
            continue;
        };
        decoders
            .entry(key.pipeline.as_str().to_string())
            .or_default()
            .push(DecoderProbe {
                stage: key.kind.to_string(),
                lag_slots: reader.lag_slots as u64,
                packets_per_sec: ring.estimated_pkt_rate(),
                stall_us: pipe.snapshot().stall_us,
            });
    }
    for (pipeline_id, ring) in source_rings.iter() {
        watched.push(crate::media::stream_inspector::Watched {
            pipeline_id: pipeline_id.clone(),
            ring: Arc::downgrade(ring),
            aac_frame_ms: aac_frames.remove(pipeline_id).unwrap_or_default(),
        });
        let (fill, capacity) = ring.fill_and_capacity();
        let readers = ring.reader_snapshots();
        rings.insert(
            pipeline_id.clone(),
            RingHealth {
                fill: fill as u64,
                capacity: capacity as u64,
                worst_lag_slots: readers
                    .iter()
                    .map(|reader| reader.lag_slots as u64)
                    .max()
                    .unwrap_or(0),
                overflows: readers
                    .iter()
                    .map(|reader| reader.overflow_count as u64)
                    .sum(),
            },
        );
    }
    if let Some(inspector) = inspector {
        inspector.watch(watched);
    }
    hub.read(|projection| {
        let mut by_pipeline: HashMap<&str, Vec<&OutputState>> = HashMap::new();
        for output in projection.outputs.values() {
            by_pipeline
                .entry(output.pipeline_id.as_str())
                .or_default()
                .push(output);
        }
        projection
            .pipelines
            .keys()
            .map(|pipeline_id| PipelineProbe {
                pipeline_id: pipeline_id.clone(),
                ingest: ingests.remove(pipeline_id),
                ring: rings.remove(pipeline_id),
                decoders: decoders.remove(pipeline_id).unwrap_or_default(),
                outputs: outputs_health(
                    by_pipeline
                        .get(pipeline_id.as_str())
                        .into_iter()
                        .flatten()
                        .copied(),
                    &egresses,
                ),
            })
            .collect()
    })
    .await
}

/// Every running egress's readings for its baseline: delivery ratio, round
/// trip (SRT's, else the TCP socket's) and SRT retransmits per second.
async fn destination_readings(
    engine: &MediaEngine,
) -> Vec<(
    String,
    String,
    crate::application::destination_baselines::Readings,
)> {
    engine
        .egresses
        .active
        .read()
        .await
        .values()
        .map(|egress| {
            let quality = restream_sync::lock::lock(&egress.quality);
            (
                egress.pipeline_id.clone(),
                egress.output_id.clone(),
                crate::application::destination_baselines::Readings {
                    delivery_ratio: quality.delivery_ratio,
                    rtt_ms: quality.ms_rtt.or(quality.tcp_rtt_ms),
                    retrans_per_sec: quality.packets_sent_retrans_per_sec,
                },
            )
        })
        .collect()
}

/// Samples every pipeline each `interval` until `shutdown`.
pub async fn run_flight_recorder(
    recorder: Arc<FlightRecorder>,
    hub: Arc<StateHub>,
    pool: SqlitePool,
    engine: Arc<MediaEngine>,
    interval: Duration,
    retention: Option<Duration>,
    shutdown: CancellationToken,
) {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tick: u32 = 0;
    // Its own OS thread reads the source rings; dropped (and stopped) with
    // this task.
    let inspector = crate::media::stream_inspector::StreamInspector::start();
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        hub.refresh_on_demand(&pool, &engine).await;
        let probes = collect_probes(&hub, &engine, inspector.as_ref()).await;
        let now_ms = MediaEngine::now_epoch_ms();
        recorder.record(now_ms, probes);
        recorder
            .baselines
            .observe(now_ms, destination_readings(&engine).await);
        tick = tick.wrapping_add(1);
        if let Some(retention) = retention
            && tick.is_multiple_of(FLIGHT_PERSIST_EVERY)
        {
            // Prune about once a minute (every sixth persisted tick).
            persist(
                &recorder,
                &pool,
                now_ms,
                retention,
                tick.is_multiple_of(FLIGHT_PERSIST_EVERY * 6),
            )
            .await;
        }
    }
}

#[cfg(test)]
#[path = "flight_recorder_tests.rs"]
mod tests;
