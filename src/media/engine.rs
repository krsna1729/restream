//! Central media engine state and public runtime contracts.

use ffmpeg_next as ffmpeg;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::domain::stage::StageKey;
use crate::domain::state::{EgressPhase, EgressRuntimeStatus, EgressStatus};
pub(crate) use crate::media::engine_hls::hls_preview_registry_key;
use crate::media::engine_registries::{
    EgressRegistry, FabricRegistry, FileIngestRegistry, HlsRegistry, IngestRegistry,
    RecordingRegistry, RuntimeInfra, StageRegistry,
};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::ring_buffer::RingBuffer;
use crate::media::snapshots::PublisherQuality;
use crate::media::stage_metrics::StageMetrics;
use crate::media::stage_metrics::StageMetricsSnapshot;

pub(crate) const EGRESS_PROGRESS_STALE_MS: u64 = 10_000;
pub(crate) const INGEST_FLAP_WINDOW_MS: u64 = 30_000;
pub(crate) const EGRESS_FLAP_WINDOW_MS: u64 = 30_000;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Publisher {
    pub protocol: String,
    pub remote_addr: Option<String>,
    pub quality: PublisherQuality,
}

#[derive(Clone, Default)]
pub struct IngestMetadata {
    pub remote_addr: Option<String>,
    pub video: Option<VideoMeta>,
    pub selected_video_track_index: Option<u32>,
    pub video_track_count: usize,
    pub audio: Option<AudioMeta>,
    pub quality: PublisherQuality,
}

pub struct ActiveIngest {
    pub attempt_id: u64,
    pub pipeline_id: String,
    pub input_id: String,
    pub stream_key: String,
    pub gate: Arc<crate::media::input_gate::InputPacketGate>,
    pub start_time: Instant,
    pub protocol: String,
    pub bytes_received: Arc<AtomicU64>,
    pub metrics: Arc<StageMetrics>,
    pub last_progress_ms: Arc<AtomicU64>,
    pub metadata: std::sync::RwLock<IngestMetadata>,
    pub audio_tracks: std::sync::Mutex<Arc<Vec<AudioMeta>>>,
    pub keyframe_times: Arc<std::sync::Mutex<Vec<i64>>>,
    pub video_sequence_header: std::sync::Mutex<Option<bytes::Bytes>>,
    pub audio_sequence_header: std::sync::Mutex<Option<bytes::Bytes>>,
    pub prev_bytes_received: AtomicU64,
    pub prev_sample_time: std::sync::Mutex<Instant>,
    pub bitrate_kbps: std::sync::Mutex<Option<f64>>,
}

impl ActiveIngest {
    pub fn metadata(&self) -> IngestMetadata {
        self.metadata
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Clone)]
pub struct IngestRegistration {
    pub cancel_token: CancellationToken,
    pub attempt_id: u64,
    pub input_id: String,
    pub gate: Arc<crate::media::input_gate::InputPacketGate>,
    pub last_forwarded_dts: Arc<AtomicI64>,
    pub preview_ring: Arc<arc_swap::ArcSwapOption<RingBuffer>>,
}

pub struct ActiveEgress {
    pub attempt_id: u64,
    pub output_id: String,
    pub pipeline_id: String,
    pub protocol: String,
    pub target_url: String,
    pub target_addr: Arc<std::sync::Mutex<Option<String>>>,
    pub status: EgressStatus,
    pub phase: Arc<std::sync::Mutex<EgressPhase>>,
    pub started_at: String,
    pub start_instant: Instant,
    pub bytes_sent: Arc<AtomicU64>,
    pub metrics: Arc<StageMetrics>,
    pub last_progress_ms: Arc<AtomicU64>,
    pub last_error: Arc<std::sync::Mutex<Option<String>>>,
    pub last_error_ms: Arc<AtomicU64>,
    pub failure_phase: Arc<std::sync::Mutex<Option<String>>>,
    pub quality: Arc<std::sync::Mutex<PublisherQuality>>,
    pub prev_bytes_sent: AtomicU64,
    pub prev_sample_time: std::sync::Mutex<Instant>,
    pub bitrate_kbps: std::sync::Mutex<Option<f64>>,
    pub terminal_stage_key: Option<StageKey>,
    pub output_name: String,
    pub encoding: String,
    /// Whether this output is owned by the egress fabric's shard runtime
    /// (RTMP, RTMPS, SRT, sink discard, and pipeline recirculation — every
    /// network-egress output type). `false` for output types the fabric
    /// does not cover (HLS PUT upload, recording), which have their own
    /// task shape and no shard/leaf concept. Set once, right after
    /// registration, from the same routing decision the bootstrap egress
    /// reconciler already made — never inferred from `protocol`.
    pub is_fabric: bool,
    /// Fabric shard index this output is assigned to, when `is_fabric` is
    /// true. `None` for non-fabric output types.
    pub shard_id: Option<u32>,
    pub resync_count: Arc<AtomicU64>,
    /// Feed units the leaf's cursor is currently behind the feed head.
    /// Fabric outputs only; stays 0 for non-fabric output types, which have
    /// no shared feed cursor to report against.
    pub feed_lag_units: Arc<AtomicU64>,
    /// Reason for the leaf's current send-path health (`None` when
    /// idle/healthy). Fabric outputs only.
    pub backpressure_reason: Arc<std::sync::Mutex<Option<&'static str>>>,
}

#[derive(Debug, Clone)]
pub struct RecentIngestOutcome {
    pub protocol: String,
    pub disconnected_at_ms: u64,
    pub first_disconnect_at_ms: u64,
    pub disconnect_count: u32,
    pub reason: Option<String>,
    pub failure_phase: Option<String>,
    pub had_error: bool,
    pub remote_addr: Option<String>,
    pub bytes_received: u64,
}

#[derive(Debug, Clone)]
pub struct RecentEgressOutcome {
    pub output_id: String,
    pub pipeline_id: String,
    pub protocol: String,
    pub target_url: String,
    pub target_addr: Option<String>,
    pub status: EgressRuntimeStatus,
    pub raw_status: EgressStatus,
    pub phase: EgressPhase,
    pub started_at: String,
    pub uptime_secs: f64,
    pub bytes_sent: u64,
    pub last_progress_ms: u64,
    pub resync_count: u64,
    pub feed_lag_units: u64,
    pub backpressure_reason: Option<&'static str>,
    pub last_error: Option<String>,
    pub last_error_ms: u64,
    pub failure_phase: Option<String>,
    pub first_failure_at_ms: u64,
    pub failure_count: u32,
    pub quality: PublisherQuality,
    pub metrics: StageMetricsSnapshot,
    pub ended_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct EgressRetryState {
    pub attempts: u32,
    pub backoff_ms: u64,
    pub next_retry_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct EgressRegistration {
    pub cancel_token: CancellationToken,
    pub attempt_id: u64,
}

pub struct MediaEngine {
    pub ingests: IngestRegistry,
    pub egresses: EgressRegistry,
    pub fabric: FabricRegistry,
    pub recordings: RecordingRegistry,
    pub hls: HlsRegistry,
    pub file_ingests: FileIngestRegistry,
    pub stages: StageRegistry,
    pub runtime: RuntimeInfra,
    pub config: Arc<crate::AppConfig>,
    backend_policy: RwLock<crate::planner::BackendPolicy>,
}

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaEngine {
    pub fn new() -> Self {
        Self::new_with_config(Arc::new(crate::AppConfig::from_env()))
    }

    pub fn new_with_config(config: Arc<crate::AppConfig>) -> Self {
        if let Err(error) = ffmpeg::init() {
            error!(err = %error, "fatal: FFmpeg initialization failed; check library paths");
            std::process::exit(1);
        }
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Warning);

        Self {
            ingests: IngestRegistry::new(),
            egresses: EgressRegistry::new(),
            fabric: FabricRegistry::new(),
            recordings: RecordingRegistry::new(),
            hls: HlsRegistry::new(),
            file_ingests: FileIngestRegistry::new(),
            stages: StageRegistry::new(),
            runtime: RuntimeInfra::new(&config),
            backend_policy: RwLock::new(config.backend_policy),
            config,
        }
    }

    pub fn backend_policy(&self) -> crate::planner::BackendPolicy {
        *self
            .backend_policy
            .read()
            // SAFE-EXPECT: a poisoned backend-policy lock is a process-wide invariant failure.
            .expect("backend policy lock poisoned")
    }

    pub fn set_backend_policy(&self, policy: crate::planner::BackendPolicy) {
        *self
            .backend_policy
            .write()
            // SAFE-EXPECT: a poisoned backend-policy lock is a process-wide invariant failure.
            .expect("backend policy lock poisoned") = policy;
    }

    pub(crate) fn now_epoch_ms() -> u64 {
        chrono::Utc::now().timestamp_millis().max(0) as u64
    }

    pub(crate) fn epoch_ms_to_rfc3339(ms: u64) -> Option<String> {
        if ms == 0 {
            return None;
        }
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
            .map(|timestamp| timestamp.to_rfc3339())
    }

    pub fn set_event_sink(&self, sink: tokio::sync::mpsc::UnboundedSender<crate::events::Event>) {
        self.runtime.event_log.set_sink(sink);
    }

    pub fn recent_events(
        &self,
        limit: usize,
        pipeline_id: Option<&str>,
    ) -> Vec<crate::events::Event> {
        self.runtime.event_log.recent(limit, pipeline_id)
    }

    pub async fn with_active_ingest<R>(
        &self,
        pipeline_id: &str,
        f: impl FnOnce(&ActiveIngest) -> R,
    ) -> Option<R> {
        let ingests = self.ingests.active.read().await;
        ingests.get(pipeline_id).map(|ingest| f(ingest.as_ref()))
    }

    pub async fn with_ingest_session<R>(
        &self,
        registration: &IngestRegistration,
        f: impl FnOnce(&ActiveIngest) -> R,
    ) -> Option<R> {
        self.current_ingest_session(registration)
            .await
            .map(|ingest| f(ingest.as_ref()))
    }

    pub(super) async fn current_ingest_session(
        &self,
        registration: &IngestRegistration,
    ) -> Option<Arc<ActiveIngest>> {
        let sessions = self.ingests.sessions.read().await;
        let ingest = sessions.get(&registration.input_id)?;
        (ingest.attempt_id == registration.attempt_id).then(|| ingest.clone())
    }

    pub async fn is_ingest_session_selected(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> bool {
        self.ingests
            .active
            .read()
            .await
            .get(pipeline_id)
            .is_some_and(|ingest| ingest.attempt_id == registration.attempt_id)
    }

    pub(crate) fn egress_protocol_from_url(url: &str) -> &'static str {
        crate::domain::output_spec::EgressProtocol::from_url(url).as_str()
    }

    pub(crate) fn graph_protocol_label(protocol: &str) -> String {
        if protocol.is_empty() || protocol == "unknown" {
            "Unknown".to_string()
        } else {
            protocol.to_uppercase()
        }
    }

    pub(crate) fn graph_slug(value: &str) -> String {
        let slug: String = value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        slug.trim_matches('_').to_string()
    }

    pub(crate) fn source_buffer_format(protocol: Option<&str>) -> &'static str {
        match protocol {
            Some("rtmp") => "FLV media packets",
            Some("srt") => "Demuxed MPEG-TS media packets",
            Some("file") => "Demuxed file media packets",
            _ => "Media packets",
        }
    }

    pub(crate) fn source_to_egress_label(protocol: &str) -> &'static str {
        match protocol {
            "rtmp" => "RTMP publish packets",
            "srt" => "MPEG-TS packetization",
            "hls" => "HLS segment input",
            _ => "media packets",
        }
    }

    pub(crate) fn sample_ingest_bitrate_kbps(ingest: &ActiveIngest) -> Option<f64> {
        let bytes_received = ingest.bytes_received.load(Ordering::Relaxed);
        let previous_bytes = ingest.prev_bytes_received.load(Ordering::Relaxed);
        let mut previous_time = ingest
            .prev_sample_time
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let elapsed = previous_time.elapsed().as_secs_f64();

        if elapsed > 0.5 && bytes_received > previous_bytes {
            let delta = bytes_received - previous_bytes;
            let rate = (delta as f64 * 8.0) / (elapsed * 1000.0);
            ingest
                .prev_bytes_received
                .store(bytes_received, Ordering::Relaxed);
            ingest
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
            *previous_time = Instant::now();
            *ingest
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(rate);
            Some(rate)
        } else if elapsed > 1.0 && bytes_received == previous_bytes {
            *ingest
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(0.0);
            Some(0.0)
        } else {
            *ingest
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        }
    }

    pub async fn has_active_ingest(&self, pipeline_id: &str) -> bool {
        self.ingests.active.read().await.contains_key(pipeline_id)
    }

    pub async fn has_recent_ingest_disconnect(&self, pipeline_id: &str, grace_ms: u64) -> bool {
        if grace_ms == 0 {
            return false;
        }
        let recent = self.ingests.recent.read().await;
        recent.get(pipeline_id).is_some_and(|outcome| {
            Self::now_epoch_ms().saturating_sub(outcome.disconnected_at_ms) < grace_ms
        })
    }

    pub(crate) fn recent_ingest_flap_state(recent: Option<&RecentIngestOutcome>) -> (u32, bool) {
        let Some(recent) = recent else {
            return (0, false);
        };
        if Self::now_epoch_ms().saturating_sub(recent.disconnected_at_ms) > INGEST_FLAP_WINDOW_MS {
            return (0, false);
        }
        (recent.disconnect_count, recent.disconnect_count >= 2)
    }

    pub(super) fn build_recent_ingest_outcome(
        previous: Option<&RecentIngestOutcome>,
        protocol: String,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
        remote_addr: Option<String>,
        bytes_received: u64,
    ) -> RecentIngestOutcome {
        let disconnected_at_ms = Self::now_epoch_ms();
        let (first_disconnect_at_ms, disconnect_count) = previous
            .filter(|previous| {
                disconnected_at_ms.saturating_sub(previous.disconnected_at_ms)
                    <= INGEST_FLAP_WINDOW_MS
            })
            .map(|previous| {
                (
                    previous.first_disconnect_at_ms,
                    previous.disconnect_count.saturating_add(1),
                )
            })
            .unwrap_or((disconnected_at_ms, 1));

        RecentIngestOutcome {
            protocol,
            disconnected_at_ms,
            first_disconnect_at_ms,
            disconnect_count,
            reason,
            failure_phase: phase.map(ToOwned::to_owned),
            had_error,
            remote_addr,
            bytes_received,
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
