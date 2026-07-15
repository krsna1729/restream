//! Central media engine state — owns all active ingests, egresses, ring buffers,
//! and recordings. Byte counters use `AtomicU64` for lock-free updates from the
//! hot ingest/egress paths; higher layers read that state through
//! `crate::api_runtime_views` when they need API-facing health JSON.

use ffmpeg_next as ffmpeg;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::domain::stage::{StageKey, StageKind};
use crate::domain::state::{EgressPhase, EgressRuntimeStatus, EgressStatus};
use crate::media::avio::MemoryQueue;
pub use crate::media::engine_hls::HlsConsumers;
pub(crate) use crate::media::engine_hls::hls_preview_registry_key;
use crate::media::engine_registries::{
    EgressRegistry, FileIngestRegistry, HlsRegistry, IngestRegistry, RecordingRegistry,
    RuntimeInfra, StageRegistry,
};
pub use crate::media::pipe_metrics::PipeMetrics;
use crate::media::ring_buffer::{MediaType, Reader, RingBuffer};
pub use crate::media::snapshots::{
    AudioMeta, EgressDiagSnapshot, FileIngestDependencySnapshot, HlsDependencySnapshot,
    IngestDiagSnapshot, ListenerSocketStats, PublisherQuality, RingBufferDiagSnapshot,
    RtmpListenerStats, SrtListenerDiagSnapshot, VideoMeta,
};
pub use crate::media::stage_metrics::StageMetrics;

pub(crate) const EGRESS_PROGRESS_STALE_MS: u64 = 10_000;
pub(crate) const INGEST_FLAP_WINDOW_MS: u64 = 30_000;
pub(crate) const EGRESS_FLAP_WINDOW_MS: u64 = 30_000;

/// Publisher connection info.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Publisher {
    pub protocol: String,
    pub remote_addr: Option<String>,
    pub quality: PublisherQuality,
}

/// Runtime state for one active ingest connection.
pub struct ActiveIngest {
    pub attempt_id: u64,
    pub stream_key: String,
    pub start_time: Instant,
    pub protocol: String, // "rtmp" | "srt" | "file"
    pub bytes_received: Arc<AtomicU64>,
    pub metrics: Arc<StageMetrics>,
    pub last_progress_ms: Arc<AtomicU64>,
    pub remote_addr: Option<String>,
    pub video: Option<VideoMeta>,
    pub selected_video_track_index: Option<u32>,
    pub video_track_count: usize,
    pub audio: Option<AudioMeta>,
    pub audio_tracks: std::sync::Mutex<std::sync::Arc<Vec<AudioMeta>>>,
    pub quality: PublisherQuality,
    pub keyframe_times: Arc<std::sync::Mutex<Vec<i64>>>,
    /// Cached FLV sequence headers for RTMP play subscribers (video config + audio config)
    pub video_sequence_header: std::sync::Mutex<Option<bytes::Bytes>>,
    pub audio_sequence_header: std::sync::Mutex<Option<bytes::Bytes>>,
    pub prev_bytes_received: AtomicU64,
    pub prev_sample_time: std::sync::Mutex<Instant>,
    pub bitrate_kbps: std::sync::Mutex<Option<f64>>,
}

#[derive(Clone, Debug)]
pub struct IngestRegistration {
    pub cancel_token: CancellationToken,
    pub attempt_id: u64,
}

/// Runtime state for one active egress target.
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
    pub last_error: Option<String>,
    pub last_error_ms: u64,
    pub failure_phase: Option<String>,
    pub first_failure_at_ms: u64,
    pub failure_count: u32,
    pub quality: PublisherQuality,
    pub metrics: serde_json::Value,
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
    pub recordings: RecordingRegistry,
    pub hls: HlsRegistry,
    pub file_ingests: FileIngestRegistry,
    pub stages: StageRegistry,
    pub runtime: RuntimeInfra,
    pub config: Arc<crate::AppConfig>,
    backend_policy: RwLock<crate::planner::backend_policy::BackendPolicy>,
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
        // Initialize FFmpeg once. On failure, emit a human-readable message
        // and exit — a panic here produces an unreadable backtrace with no
        // context about what went wrong or which library is missing.
        if let Err(e) = ffmpeg::init() {
            error!(err = %e, "fatal: FFmpeg initialization failed; check library paths");
            std::process::exit(1);
        }
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Warning);

        Self {
            ingests: IngestRegistry::new(),
            egresses: EgressRegistry::new(),
            recordings: RecordingRegistry::new(),
            hls: HlsRegistry::new(),
            file_ingests: FileIngestRegistry::new(),
            stages: StageRegistry::new(),
            runtime: RuntimeInfra::new(&config),
            backend_policy: RwLock::new(config.backend_policy),
            config,
        }
    }

    pub fn backend_policy(&self) -> crate::planner::backend_policy::BackendPolicy {
        *self
            .backend_policy
            .read()
            .expect("backend policy lock poisoned")
    }

    pub fn set_backend_policy(&self, policy: crate::planner::backend_policy::BackendPolicy) {
        *self
            .backend_policy
            .write()
            .expect("backend policy lock poisoned") = policy;
    }

    pub(crate) fn now_epoch_ms() -> u64 {
        chrono::Utc::now().timestamp_millis().max(0) as u64
    }

    pub(crate) fn epoch_ms_to_rfc3339(ms: u64) -> Option<String> {
        if ms == 0 {
            return None;
        }
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64).map(|dt| dt.to_rfc3339())
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
        ingests.get(pipeline_id).map(f)
    }

    pub async fn with_active_egress<R>(
        &self,
        output_id: &str,
        f: impl FnOnce(&ActiveEgress) -> R,
    ) -> Option<R> {
        let egresses = self.egresses.active.read().await;
        egresses.get(output_id).map(f)
    }

    async fn with_current_egress<R>(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        f: impl FnOnce(&ActiveEgress) -> R,
    ) -> Option<R> {
        let egresses = self.egresses.active.read().await;
        let egress = egresses.get(output_id)?;
        (egress.attempt_id == registration.attempt_id).then(|| f(egress))
    }

    pub fn listener_stats_handle(&self) -> Arc<ListenerSocketStats> {
        self.runtime.listener_stats.clone()
    }

    pub fn sender_semaphore_handle(&self) -> Arc<tokio::sync::Semaphore> {
        self.runtime.sender_semaphore.clone()
    }

    pub fn srt_egress_muxer_port_handle(&self) -> Arc<std::sync::Mutex<Option<u16>>> {
        self.runtime.srt_egress_muxer_port.clone()
    }

    pub async fn stop_file_ingest_child(&self, ingest_id: &str) -> bool {
        let mut children = self.file_ingests.children.write().await;
        let Some(mut child) = children.remove(ingest_id) else {
            return false;
        };
        drop(children);
        let _ = child.kill().await;
        let _ = child.wait().await;
        true
    }

    pub async fn take_file_ingest_child(&self, ingest_id: &str) -> Option<tokio::process::Child> {
        self.file_ingests.children.write().await.remove(ingest_id)
    }

    pub fn bonding_available(&self) -> bool {
        self.runtime
            .listener_stats
            .bonding_available
            .load(Ordering::Relaxed)
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
            .map(|ch| {
                if ch.is_ascii_alphanumeric() {
                    ch.to_ascii_lowercase()
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

    pub(crate) fn egress_effective_status(egress: &ActiveEgress, has_ingest: bool) -> String {
        if !has_ingest {
            return "stopped".to_string();
        }

        let phase = *egress.phase.lock().unwrap_or_else(|e| e.into_inner());
        if phase == EgressPhase::Failed {
            return "failed".to_string();
        }
        if egress.status != EgressStatus::Running {
            return egress.status.to_string();
        }
        if egress.target_url.starts_with("hls://") && phase == EgressPhase::Segmenting {
            return "running".to_string();
        }

        let last_progress_ms = egress.last_progress_ms.load(Ordering::Relaxed);
        let now_ms = Self::now_epoch_ms();
        let no_progress_too_long = last_progress_ms == 0
            && egress.start_instant.elapsed().as_millis() as u64 >= EGRESS_PROGRESS_STALE_MS;
        let stale_progress = last_progress_ms > 0
            && now_ms.saturating_sub(last_progress_ms) >= EGRESS_PROGRESS_STALE_MS;
        if no_progress_too_long || stale_progress {
            return "stalled".to_string();
        }

        "running".to_string()
    }

    pub(crate) fn sample_egress_bitrate_kbps(egress: &ActiveEgress) -> Option<f64> {
        let bytes_sent = egress.bytes_sent.load(Ordering::Relaxed);
        let prev = egress.prev_bytes_sent.load(Ordering::Relaxed);
        let mut prev_time = egress
            .prev_sample_time
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let elapsed = prev_time.elapsed().as_secs_f64();

        if elapsed > 0.5 && bytes_sent > prev {
            let delta = bytes_sent - prev;
            let rate = (delta as f64 * 8.0) / (elapsed * 1000.0);
            egress.prev_bytes_sent.store(bytes_sent, Ordering::Relaxed);
            *prev_time = Instant::now();
            *egress
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(rate);
            Some(rate)
        } else {
            *egress
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        }
    }

    pub(crate) fn sample_ingest_bitrate_kbps(ingest: &ActiveIngest) -> Option<f64> {
        let bytes_received = ingest.bytes_received.load(Ordering::Relaxed);
        let prev = ingest.prev_bytes_received.load(Ordering::Relaxed);
        let mut prev_time = ingest
            .prev_sample_time
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let elapsed = prev_time.elapsed().as_secs_f64();

        if elapsed > 0.5 && bytes_received > prev {
            let delta = bytes_received - prev;
            let rate = (delta as f64 * 8.0) / (elapsed * 1000.0);
            ingest
                .prev_bytes_received
                .store(bytes_received, Ordering::Relaxed);
            ingest
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
            *prev_time = Instant::now();
            *ingest
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(rate);
            Some(rate)
        } else if elapsed > 1.0 && bytes_received == prev {
            *ingest
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(0.0);
            Some(0.0)
        } else {
            *ingest
                .bitrate_kbps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        }
    }

    fn recent_egress_status(egress: &ActiveEgress, has_ingest: bool) -> EgressRuntimeStatus {
        let phase = *egress.phase.lock().unwrap_or_else(|e| e.into_inner());
        if phase == EgressPhase::Failed
            || egress
                .last_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        {
            return EgressRuntimeStatus::Failed;
        }
        if !has_ingest {
            return EgressRuntimeStatus::Stopped;
        }
        EgressRuntimeStatus::from(Self::egress_effective_status(egress, has_ingest))
    }

    /// Register an OS thread JoinHandle so it can be joined at shutdown.
    /// Already-finished handles are pruned opportunistically to prevent unbounded accumulation
    /// in long-running servers with many short-lived per-connection threads.
    pub fn register_os_thread(&self, handle: std::thread::JoinHandle<()>) {
        let mut guards = self
            .runtime
            .os_threads
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guards.retain(|h| !h.is_finished());
        guards.push(handle);
    }

    pub fn register_listener_shutdown(&self, shutdown: impl Fn() + Send + Sync + 'static) {
        self.runtime
            .listener_shutdowns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(shutdown));
    }

    pub fn shutdown_listeners(&self) {
        let shutdowns: Vec<_> = self
            .runtime
            .listener_shutdowns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for shutdown in shutdowns {
            shutdown();
        }
    }

    /// Drain all registered OS thread handles for joining at shutdown.
    pub fn drain_os_thread_handles(&self) -> Vec<std::thread::JoinHandle<()>> {
        self.runtime
            .os_threads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
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

    pub(crate) fn recent_egress_flap_state(recent: Option<&RecentEgressOutcome>) -> (u32, bool) {
        let Some(recent) = recent else {
            return (0, false);
        };
        if recent.failure_count == 0 {
            return (0, false);
        }
        if Self::now_epoch_ms().saturating_sub(recent.ended_at_ms) > EGRESS_FLAP_WINDOW_MS {
            return (0, false);
        }
        (recent.failure_count, recent.failure_count >= 2)
    }

    fn build_recent_ingest_outcome(
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

    fn build_recent_egress_outcome(
        previous: Option<&RecentEgressOutcome>,
        egress: &ActiveEgress,
        has_ingest: bool,
    ) -> RecentEgressOutcome {
        let phase = *egress.phase.lock().unwrap_or_else(|e| e.into_inner());
        let last_error = egress
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let failure_phase = egress
            .failure_phase
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let ended_at_ms = Self::now_epoch_ms();
        let had_error =
            phase == EgressPhase::Failed || last_error.is_some() || failure_phase.is_some();
        let (first_failure_at_ms, failure_count) = if had_error {
            previous
                .filter(|previous| {
                    previous.failure_count > 0
                        && ended_at_ms.saturating_sub(previous.ended_at_ms) <= EGRESS_FLAP_WINDOW_MS
                })
                .map(|previous| {
                    (
                        if previous.first_failure_at_ms > 0 {
                            previous.first_failure_at_ms
                        } else {
                            previous.ended_at_ms
                        },
                        previous.failure_count.saturating_add(1),
                    )
                })
                .unwrap_or((ended_at_ms, 1))
        } else {
            (0, 0)
        };

        RecentEgressOutcome {
            output_id: egress.output_id.clone(),
            pipeline_id: egress.pipeline_id.clone(),
            protocol: egress.protocol.clone(),
            target_url: egress.target_url.clone(),
            target_addr: egress
                .target_addr
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            status: Self::recent_egress_status(egress, has_ingest),
            raw_status: egress.status,
            phase,
            started_at: egress.started_at.clone(),
            uptime_secs: egress.start_instant.elapsed().as_secs_f64(),
            bytes_sent: egress.bytes_sent.load(Ordering::Relaxed),
            last_progress_ms: egress.last_progress_ms.load(Ordering::Relaxed),
            last_error,
            last_error_ms: egress.last_error_ms.load(Ordering::Relaxed),
            failure_phase,
            first_failure_at_ms,
            failure_count,
            quality: egress
                .quality
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            metrics: egress.metrics.snapshot(),
            ended_at_ms,
        }
    }

    pub async fn has_active_egress(&self, output_id: &str) -> bool {
        self.egresses
            .cancel_tokens
            .read()
            .await
            .contains_key(output_id)
    }

    pub async fn get_or_create_diag_semaphore(
        &self,
        pipeline_id: &str,
    ) -> Arc<tokio::sync::Semaphore> {
        let mut map = self.runtime.diag_semaphores.write().await;
        map.entry(pipeline_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    }

    /// Wait for the upstream media ring to have data and parameter sets ready before connecting.
    /// This prevents zero-byte startup stalls where egress connects before transcoder is producing.
    pub async fn wait_for_upstream_warmup(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        ring_buffer: Arc<RingBuffer>,
        cancel_token: CancellationToken,
    ) {
        if ring_buffer.codec_hint_str().is_empty() {
            return;
        }

        self.update_egress_phase_if_current(output_id, registration, EgressPhase::WaitingUpstream)
            .await;

        let mut warmup = Reader::new(format!("egress_warmup:{}", output_id), ring_buffer.clone());

        let mut warmup_packets = Vec::with_capacity(crate::media::MEDIA_PULL_BURST_PACKETS);
        tokio::select! {
            _ = cancel_token.cancelled() => {}
            _ = async {
                loop {
                    warmup.wait_for_data().await;
                    warmup_packets.clear();
                    let _ = warmup.pull_burst(&mut warmup_packets, crate::media::MEDIA_PULL_BURST_PACKETS);

                    if ring_buffer.video_parameter_sets().is_some() {
                        break;
                    }

                    if warmup_packets.iter().any(|p| p.media_type == MediaType::Video) {
                        break;
                    }

                    if !warmup_packets.is_empty() && ring_buffer.video_parameter_sets().is_none() {
                        break;
                    }
                }
            } => {}
        }
    }

    pub async fn register_egress_queue(&self, output_id: &str, queue: Arc<MemoryQueue>) {
        self.egresses
            .queues
            .write()
            .await
            .insert(output_id.to_string(), queue);
    }

    pub async fn register_egress_queue_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        queue: Arc<MemoryQueue>,
    ) -> bool {
        if self
            .with_current_egress(output_id, registration, |_| ())
            .await
            .is_none()
        {
            return false;
        }
        self.egresses
            .queues
            .write()
            .await
            .insert(output_id.to_string(), queue);
        true
    }

    pub async fn remove_egress_queue(&self, output_id: &str) {
        self.egresses.queues.write().await.remove(output_id);
    }

    pub async fn remove_egress_queue_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
    ) -> bool {
        if self
            .with_current_egress(output_id, registration, |_| ())
            .await
            .is_none()
        {
            return false;
        }
        self.egresses.queues.write().await.remove(output_id);
        true
    }

    pub async fn is_file_ingest_running(&self, id: &str) -> bool {
        let mut children = self.file_ingests.children.write().await;
        if let Some(child) = children.get_mut(id) {
            match child.try_wait() {
                Ok(None) => {
                    self.file_ingests
                        .active
                        .write()
                        .await
                        .insert(id.to_string());
                    true
                }
                _ => {
                    children.remove(id);
                    self.file_ingests.active.write().await.remove(id);
                    false
                }
            }
        } else {
            self.file_ingests.active.read().await.contains(id)
        }
    }

    pub async fn reap_file_ingests(&self) {
        let mut children = self.file_ingests.children.write().await;
        let mut stopped = Vec::new();
        children.retain(|id, child| match child.try_wait() {
            Ok(None) => true,
            _ => {
                info!("File ingest child process {} has exited/stopped", id);
                stopped.push(id.clone());
                false
            }
        });
        drop(children);

        if !stopped.is_empty() {
            let mut active = self.file_ingests.active.write().await;
            for id in stopped {
                active.remove(&id);
            }
        }
    }

    pub async fn mark_file_ingest_running(&self, id: &str) {
        self.file_ingests
            .active
            .write()
            .await
            .insert(id.to_string());
    }

    pub async fn clear_file_ingest_running(&self, id: &str) {
        self.file_ingests.active.write().await.remove(id);
    }

    pub async fn get_or_create_pipeline(&self, pipeline_id: &str) -> Arc<RingBuffer> {
        let mut pipelines = self.ingests.pipelines.write().await;
        if let Some(rb) = pipelines.get(pipeline_id) {
            return rb.clone();
        }
        let rb = Arc::new(RingBuffer::new(self.config.ring_capacity));
        pipelines.insert(pipeline_id.to_string(), rb.clone());
        rb
    }

    /// Called after stream probe: sizes the source ring for 5 s jitter headroom.
    ///
    /// Formula: `needed = ceil(pkt_rate × HEADROOM_SECS)`, clamped to
    /// `[configured ring capacity, MAX_RING_CAPACITY]`.  If the ring is already
    /// large enough no action is taken.  Otherwise the ring is always swapped in,
    /// even if egress readers are already attached — those readers are cancelled so
    /// the reconciler restarts them (within ~1 s) onto the new correctly-sized ring.
    /// Cancelling early readers is safe: the probe fires at ~2–3 s, before any
    /// viewer has meaningfully started watching, and the reconnect is invisible.
    ///
    /// Returns `Some(new_ring)` when resized so the SRT ingest loop can update its
    /// local `ring_buffer` Arc (the old one is stale and receives no further data).
    pub async fn adapt_pipeline_ring(
        &self,
        pipeline_id: &str,
        video_fps: f64,
        audio_track_count: usize,
    ) -> Option<Arc<RingBuffer>> {
        const AUDIO_PKT_RATE: f64 = 50.0; // AAC 48 kHz, 960 samples/frame
        const HEADROOM_SECS: f64 = 6.0; // 20 % margin above the 5 s requirement
        const MAX_RING_CAPACITY: usize = 16_384;

        let pkt_rate = video_fps.max(0.0) + audio_track_count as f64 * AUDIO_PKT_RATE;
        let needed = ((pkt_rate * HEADROOM_SECS).ceil() as usize)
            .max(self.config.ring_capacity)
            .min(MAX_RING_CAPACITY);

        let mut pipelines = self.ingests.pipelines.write().await;
        let old_rb = pipelines.get(pipeline_id).cloned()?;

        // Always record the packet rate for buffer-depth telemetry.
        old_rb.set_estimated_pkt_rate(pkt_rate);

        if needed <= old_rb.capacity() {
            return None; // already large enough
        }

        // Create a new ring that continues the write-index sequence of the old
        // one so migrating readers pick up exactly where they left off.
        let old_write_idx = old_rb.get_write_idx();
        let new_rb = Arc::new(RingBuffer::new_continuing(needed, old_write_idx));
        let seeded_packets = new_rb.seed_readable_tail_from(&old_rb);
        new_rb.set_estimated_pkt_rate(pkt_rate);
        if let Some(hint) = old_rb.codec_hint.get() {
            new_rb.set_codec_hint(hint);
        }
        if let Some(parameter_sets) = old_rb.video_parameter_sets() {
            new_rb.set_video_parameter_sets(parameter_sets);
        }
        if let Some(audio_tracks) = old_rb.audio_tracks() {
            new_rb.set_audio_tracks(audio_tracks.to_vec());
        }
        let new_rb_clone = new_rb.clone();

        // Install the new ring in the engine map so that the producer (SRT
        // ingest) switches to it after we return Some(new_rb_clone).
        pipelines.insert(pipeline_id.to_string(), new_rb.clone());
        drop(pipelines);

        // Seal the old ring and forward its readers to the new one.
        // Readers blocked in wait_for_data() are woken here; they drain any
        // remaining unread slots in the old ring, then migrate autonomously.
        // External egress connections (RTMP/SRT to mediamtx) are never
        // cancelled — they see only a sub-millisecond pause in data flow.
        old_rb.seal_and_forward(new_rb);

        info!(
            pipeline_id,
            pkt_rate = format!("{pkt_rate:.0}"),
            video_fps = format!("{video_fps:.0}"),
            audio_track_count,
            new_capacity = needed,
            seeded_packets,
            headroom_secs = format!("{:.1}", needed as f64 / pkt_rate),
            "adaptive ring resize: readers migrate in-place, no egress reconnect"
        );

        Some(new_rb_clone)
    }

    /// Get or create a shared transcoder stage for a pipeline + encoding combo.
    /// Keyed by the full encoding string — callers are responsible for splitting
    /// video and audio into separate stages when sharing is needed.
    ///
    /// Used for both video transcoding (keyed on video preset) and audio-only
    /// filtering (keyed on full compound encoding). Multiple egresses wanting
    /// the same encoding share the same output RingBuffer.
    pub async fn get_or_create_transcoder(
        self: &Arc<Self>,
        pipeline_id: &str,
        stage_kind: StageKind,
        source_buffer: Arc<RingBuffer>,
        // When the source_buffer is a transcoded ring whose codec differs from the
        // original ingest (e.g. hevc_to_h264 → video:720p), pass the actual codec
        // of the packets in source_buffer so the TsMuxer gets the right PMT.
        input_codec_override: Option<&str>,
    ) -> Arc<RingBuffer> {
        let key = StageKey::new(pipeline_id, stage_kind.clone());
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(self.clone());
        let (handle, created) = manager
            .ensure_stage(key.clone(), source_buffer.clone(), input_codec_override)
            .await;

        if created {
            manager.spawn_stage(handle.clone(), source_buffer.clone(), input_codec_override);
        }

        handle.ring
    }

    /// Get or create a shared H.265→H.264 transcoder stage for a pipeline.
    ///
    /// Keyed by `"<pipeline_id>:hevc_to_h264:from:<upstream_stage_key>"` so that
    /// RTMP-passthrough (`from:source`) and RTMP-720p (`from:720p`) stages are
    /// independent and all RTMP egresses on the same preset share one converter.
    pub async fn get_or_create_h264_transcoder(
        self: &Arc<Self>,
        pipeline_id: &str,
        upstream: StageKind,
        source_buffer: Arc<RingBuffer>,
    ) -> Arc<RingBuffer> {
        let key = StageKey::new(pipeline_id, StageKind::codec_edge("hevc_to_h264", upstream));
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(self.clone());
        let (handle, created) = manager
            .ensure_stage(key.clone(), source_buffer.clone(), None)
            .await;

        if created {
            manager.spawn_codec_edge_stage(handle.clone(), source_buffer.clone());
        }

        handle.ring
    }

    /// Return the active processing stages for a pipeline as (kind, is_alive) pairs.
    pub async fn active_transcoder_stages(&self, pipeline_id: &str) -> Vec<(StageKind, bool)> {
        let runtimes = self.stages.runtimes.read().await;
        runtimes
            .iter()
            .filter(|(key, runtime)| key.pipeline.as_str() == pipeline_id && runtime.ring.is_some())
            .map(|(key, runtime)| (key.kind.clone(), !runtime.cancel.is_cancelled()))
            .collect()
    }

    pub async fn remove_pipeline(&self, pipeline_id: &str) {
        let mut pipelines = self.ingests.pipelines.write().await;
        pipelines.remove(pipeline_id);
    }

    /// Remove all transcoder stage entries for a pipeline from the runtime registry.
    ///
    /// Stages whose cancel tokens have already fired are cleaned up lazily by
    /// `get_or_create_transcoder`. This function does the eager sweep on pipeline
    /// deletion so the `Arc<RingBuffer>` for every stage is freed immediately
    /// instead of surviving until the next reconciler creates a replacement stage.
    pub async fn cleanup_pipeline_stages(&self, pipeline_id: &str) {
        let mut runtimes = self.stages.runtimes.write().await;
        let mut removed = Vec::new();
        // Cancel all still-running stages then remove every entry for this pipeline.
        runtimes.retain(|key, runtime| {
            if key.pipeline.as_str() == pipeline_id {
                runtime.cancel.cancel();
                removed.push(key.clone());
                false
            } else {
                true
            }
        });
        drop(runtimes);
        self.remove_stage_artifacts(&removed).await;
    }

    pub async fn sweep_unused_transcoder_stages(
        &self,
        active_keys: &std::collections::HashSet<StageKey>,
    ) {
        let mut runtimes = self.stages.runtimes.write().await;
        let mut removed = Vec::new();
        runtimes.retain(|key, runtime| {
            if runtime.ring.is_some() && !active_keys.contains(key) {
                debug!("Sweeping unused transcoder stage: {}", key);
                runtime.cancel.cancel();
                removed.push(key.clone());
                false
            } else {
                true
            }
        });
        drop(runtimes);
        self.remove_stage_artifacts(&removed).await;
    }

    async fn remove_stage_artifacts(&self, keys: &[StageKey]) {
        if keys.is_empty() {
            return;
        }
        let mut metrics = self.stages.metrics.write().await;
        let mut lifecycles = self.stages.lifecycles.write().await;
        for key in keys {
            metrics.remove(key);
            lifecycles.remove(key);
        }
    }

    pub async fn sweep_unused_stages(&self) {
        let mut stages = self.stages.ts_muxers.write().await;
        stages.retain(|key, stage| {
            let has_readers = if let Ok(mut r) = stage.ring.readers.lock() {
                r.retain(|w| w.upgrade().is_some());
                !r.is_empty()
            } else {
                false
            };

            let in_use = has_readers;

            if !in_use {
                debug!("Sweeping unused TS muxer stage: {}", key);
                stage.cancel.cancel();
                false
            } else {
                true
            }
        });
    }

    ///
    /// A pipeline has one application-level producer. A bonded SRT publisher is
    /// still one producer because libsrt presents the accepted bond as one group
    /// socket. A second independent RTMP/SRT connection must be rejected instead
    /// of overwriting the token and creating concurrent RingBuffer writers.
    pub async fn try_register_ingest_attempt(
        &self,
        pipeline_id: &str,
        stream_key: &str,
        protocol: &str,
    ) -> Option<IngestRegistration> {
        let mut tokens = self.ingests.cancel_tokens.write().await;
        if let Some(existing) = tokens.get(pipeline_id)
            && !existing.is_cancelled()
        {
            return None;
        }

        let attempt_id = self.ingests.next_attempt_id.fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        let registration = IngestRegistration {
            cancel_token: token.clone(),
            attempt_id,
        };
        tokens.insert(pipeline_id.to_string(), token.clone());

        let mut ingests = self.ingests.active.write().await;
        let now = Instant::now();
        ingests.insert(
            pipeline_id.to_string(),
            ActiveIngest {
                attempt_id,
                stream_key: stream_key.to_string(),
                start_time: now,
                protocol: protocol.to_string(),
                bytes_received: Arc::new(AtomicU64::new(0)),
                metrics: Arc::new(StageMetrics::new()),
                last_progress_ms: Arc::new(AtomicU64::new(0)),
                remote_addr: None,
                video: None,
                selected_video_track_index: None,
                video_track_count: 0,
                audio: None,
                audio_tracks: std::sync::Mutex::new(std::sync::Arc::new(Vec::new())),
                quality: PublisherQuality::default(),
                keyframe_times: Arc::new(std::sync::Mutex::new(Vec::new())),
                video_sequence_header: std::sync::Mutex::new(None),
                audio_sequence_header: std::sync::Mutex::new(None),
                prev_bytes_received: AtomicU64::new(0),
                prev_sample_time: std::sync::Mutex::new(now),
                bitrate_kbps: std::sync::Mutex::new(None),
            },
        );

        self.runtime
            .event_log
            .emit(crate::events::EventKind::IngestConnected {
                pipeline_id: pipeline_id.to_string(),
                protocol: protocol.to_string(),
                stream_key: stream_key.to_string(),
            });
        Some(registration)
    }

    pub async fn try_register_ingest(
        &self,
        pipeline_id: &str,
        stream_key: &str,
        protocol: &str,
    ) -> Option<CancellationToken> {
        self.try_register_ingest_attempt(pipeline_id, stream_key, protocol)
            .await
            .map(|registration| registration.cancel_token)
    }

    pub async fn record_ingest_disconnect(
        &self,
        pipeline_id: &str,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) {
        let ingests = self.ingests.active.read().await;
        let Some(ingest) = ingests.get(pipeline_id) else {
            return;
        };

        let previous = self.ingests.recent.read().await.get(pipeline_id).cloned();
        let snapshot = Self::build_recent_ingest_outcome(
            previous.as_ref(),
            ingest.protocol.clone(),
            phase,
            reason,
            had_error,
            ingest.remote_addr.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        );
        drop(ingests);

        self.ingests
            .recent
            .write()
            .await
            .insert(pipeline_id.to_string(), snapshot);
    }

    pub async fn record_ingest_disconnect_if_current(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) -> bool {
        let ingests = self.ingests.active.read().await;
        let Some(ingest) = ingests.get(pipeline_id) else {
            return false;
        };
        if ingest.attempt_id != registration.attempt_id {
            return false;
        }

        let previous = self.ingests.recent.read().await.get(pipeline_id).cloned();
        let snapshot = Self::build_recent_ingest_outcome(
            previous.as_ref(),
            ingest.protocol.clone(),
            phase,
            reason,
            had_error,
            ingest.remote_addr.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        );
        drop(ingests);

        self.ingests
            .recent
            .write()
            .await
            .insert(pipeline_id.to_string(), snapshot);
        true
    }

    pub async fn unregister_ingest(&self, pipeline_id: &str) {
        let mut tokens = self.ingests.cancel_tokens.write().await;
        if let Some(token) = tokens.remove(pipeline_id) {
            token.cancel();
        }

        let mut ingests = self.ingests.active.write().await;
        let removed = ingests.remove(pipeline_id);
        drop(ingests);

        let protocol = removed
            .as_ref()
            .map(|ingest| ingest.protocol.clone())
            .unwrap_or_default();
        if let Some(ingest) = removed {
            let mut recent = self.ingests.recent.write().await;
            recent
                .entry(pipeline_id.to_string())
                .or_insert_with(|| RecentIngestOutcome {
                    protocol: ingest.protocol,
                    disconnected_at_ms: Self::now_epoch_ms(),
                    first_disconnect_at_ms: Self::now_epoch_ms(),
                    disconnect_count: 1,
                    reason: None,
                    failure_phase: None,
                    had_error: false,
                    remote_addr: ingest.remote_addr,
                    bytes_received: ingest.bytes_received.load(Ordering::Relaxed),
                });
        }

        if !protocol.is_empty() {
            self.runtime
                .event_log
                .emit(crate::events::EventKind::IngestDisconnected {
                    pipeline_id: pipeline_id.to_string(),
                    protocol,
                });
        }
    }

    pub async fn unregister_ingest_if_current(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> bool {
        let mut tokens = self.ingests.cancel_tokens.write().await;
        let mut ingests = self.ingests.active.write().await;
        let Some(active) = ingests.get(pipeline_id) else {
            return false;
        };
        if active.attempt_id != registration.attempt_id {
            return false;
        }

        if let Some(token) = tokens.remove(pipeline_id) {
            token.cancel();
        }

        let removed = ingests.remove(pipeline_id);
        drop(ingests);

        let protocol = removed
            .as_ref()
            .map(|ingest| ingest.protocol.clone())
            .unwrap_or_default();
        if let Some(ingest) = removed {
            let mut recent = self.ingests.recent.write().await;
            recent
                .entry(pipeline_id.to_string())
                .or_insert_with(|| RecentIngestOutcome {
                    protocol: ingest.protocol,
                    disconnected_at_ms: Self::now_epoch_ms(),
                    first_disconnect_at_ms: Self::now_epoch_ms(),
                    disconnect_count: 1,
                    reason: None,
                    failure_phase: None,
                    had_error: false,
                    remote_addr: ingest.remote_addr,
                    bytes_received: ingest.bytes_received.load(Ordering::Relaxed),
                });
        }

        if !protocol.is_empty() {
            self.runtime
                .event_log
                .emit(crate::events::EventKind::IngestDisconnected {
                    pipeline_id: pipeline_id.to_string(),
                    protocol,
                });
        }
        true
    }

    pub async fn register_egress_attempt(
        &self,
        output_id: &str,
        pipeline_id: &str,
        url: &str,
        terminal_stage_key: Option<StageKey>,
    ) -> EgressRegistration {
        self.register_egress_attempt_with_meta(
            output_id,
            pipeline_id,
            url,
            None,
            None,
            terminal_stage_key,
        )
        .await
    }

    pub async fn register_egress_attempt_with_meta(
        &self,
        output_id: &str,
        pipeline_id: &str,
        url: &str,
        output_name: Option<&str>,
        encoding: Option<&str>,
        terminal_stage_key: Option<StageKey>,
    ) -> EgressRegistration {
        self.egresses.retry.write().await.remove(output_id);

        let mut tokens = self.egresses.cancel_tokens.write().await;
        let attempt_id = self
            .egresses
            .next_attempt_id
            .fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        let registration = EgressRegistration {
            cancel_token: token.clone(),
            attempt_id,
        };
        tokens.insert(output_id.to_string(), token.clone());

        let mut egresses = self.egresses.active.write().await;
        let now = Instant::now();
        egresses.insert(
            output_id.to_string(),
            ActiveEgress {
                attempt_id,
                output_id: output_id.to_string(),
                pipeline_id: pipeline_id.to_string(),
                protocol: Self::egress_protocol_from_url(url).to_string(),
                target_url: url.to_string(),
                target_addr: Arc::new(std::sync::Mutex::new(None)),
                status: EgressStatus::Running,
                phase: Arc::new(std::sync::Mutex::new(EgressPhase::Starting)),
                started_at: chrono::Utc::now().to_rfc3339(),
                start_instant: now,
                bytes_sent: Arc::new(AtomicU64::new(0)),
                metrics: Arc::new(StageMetrics::new()),
                last_progress_ms: Arc::new(AtomicU64::new(0)),
                last_error: Arc::new(std::sync::Mutex::new(None)),
                last_error_ms: Arc::new(AtomicU64::new(0)),
                failure_phase: Arc::new(std::sync::Mutex::new(None)),
                quality: Arc::new(std::sync::Mutex::new(PublisherQuality::default())),
                prev_bytes_sent: AtomicU64::new(0),
                prev_sample_time: std::sync::Mutex::new(now),
                bitrate_kbps: std::sync::Mutex::new(None),
                terminal_stage_key,
                output_name: output_name.unwrap_or("").to_string(),
                encoding: encoding.unwrap_or("").to_string(),
            },
        );

        self.runtime
            .event_log
            .emit(crate::events::EventKind::EgressStarted {
                pipeline_id: pipeline_id.to_string(),
                output_id: output_id.to_string(),
            });
        registration
    }

    pub async fn register_egress(
        &self,
        output_id: &str,
        pipeline_id: &str,
        url: &str,
    ) -> CancellationToken {
        self.register_egress_attempt(output_id, pipeline_id, url, None)
            .await
            .cancel_token
    }

    pub async fn unregister_egress(&self, output_id: &str) {
        let previous_recent = self.egresses.recent.read().await.get(output_id).cloned();
        let mut tokens = self.egresses.cancel_tokens.write().await;
        if let Some(token) = tokens.remove(output_id) {
            token.cancel();
        }

        let mut egresses = self.egresses.active.write().await;
        let release_srt_muxer = egresses.get(output_id).and_then(|egress| {
            (egress.protocol == "srt").then(|| {
                (
                    egress.pipeline_id.clone(),
                    egress.encoding.clone(),
                    egress.attempt_id,
                )
            })
        });
        let pipeline_id = egresses
            .get(output_id)
            .map(|e| e.pipeline_id.clone())
            .unwrap_or_default();
        let recent_outcome = egresses.get(output_id).map(|egress| {
            let has_ingest = self
                .ingests
                .active
                .try_read()
                .map(|ingests| ingests.contains_key(egress.pipeline_id.as_str()))
                .unwrap_or(false);
            Self::build_recent_egress_outcome(previous_recent.as_ref(), egress, has_ingest)
        });
        egresses.remove(output_id);
        drop(egresses);

        if let Some((srt_pipeline_id, encoding, attempt_id)) = release_srt_muxer {
            self.release_srt_egress_muxer_stage(&srt_pipeline_id, &encoding, output_id, attempt_id)
                .await;
        }

        if let Some(outcome) = recent_outcome {
            self.egresses
                .recent
                .write()
                .await
                .insert(output_id.to_string(), outcome);
        }

        if !pipeline_id.is_empty() {
            self.runtime
                .event_log
                .emit(crate::events::EventKind::EgressStopped {
                    pipeline_id,
                    output_id: output_id.to_string(),
                });
        }
    }

    pub async fn unregister_egress_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
    ) -> bool {
        let previous_recent = self.egresses.recent.read().await.get(output_id).cloned();
        let mut tokens = self.egresses.cancel_tokens.write().await;
        let mut egresses = self.egresses.active.write().await;
        let Some(active) = egresses.get(output_id) else {
            return false;
        };
        if active.attempt_id != registration.attempt_id {
            return false;
        }

        if let Some(token) = tokens.remove(output_id) {
            token.cancel();
        }

        let release_srt_muxer = (active.protocol == "srt").then(|| {
            (
                active.pipeline_id.clone(),
                active.encoding.clone(),
                active.attempt_id,
            )
        });
        let pipeline_id = active.pipeline_id.clone();
        let has_ingest = self
            .ingests
            .active
            .try_read()
            .map(|ingests| ingests.contains_key(active.pipeline_id.as_str()))
            .unwrap_or(false);
        let outcome =
            Self::build_recent_egress_outcome(previous_recent.as_ref(), active, has_ingest);
        egresses.remove(output_id);
        drop(egresses);

        if let Some((srt_pipeline_id, encoding, attempt_id)) = release_srt_muxer {
            self.release_srt_egress_muxer_stage(&srt_pipeline_id, &encoding, output_id, attempt_id)
                .await;
        }

        self.egresses
            .recent
            .write()
            .await
            .insert(output_id.to_string(), outcome);

        if !pipeline_id.is_empty() {
            self.runtime
                .event_log
                .emit(crate::events::EventKind::EgressStopped {
                    pipeline_id,
                    output_id: output_id.to_string(),
                });
        }
        true
    }

    pub async fn update_egress_phase(&self, output_id: &str, phase: EgressPhase) {
        let egresses = self.egresses.active.read().await;
        if let Some(egress) = egresses.get(output_id) {
            *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
        }
    }

    pub async fn update_egress_phase_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        phase: EgressPhase,
    ) -> bool {
        self.with_current_egress(output_id, registration, |egress| {
            *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
        })
        .await
        .is_some()
    }

    pub async fn update_egress_target_addr(&self, output_id: &str, addr: String) {
        let egresses = self.egresses.active.read().await;
        if let Some(egress) = egresses.get(output_id) {
            *egress.target_addr.lock().unwrap_or_else(|e| e.into_inner()) = Some(addr);
        }
    }

    pub async fn update_egress_target_addr_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        addr: String,
    ) -> bool {
        self.with_current_egress(output_id, registration, |egress| {
            *egress.target_addr.lock().unwrap_or_else(|e| e.into_inner()) = Some(addr);
        })
        .await
        .is_some()
    }

    pub async fn update_egress_quality(&self, output_id: &str, quality: PublisherQuality) {
        let egresses = self.egresses.active.read().await;
        if let Some(egress) = egresses.get(output_id) {
            *egress.quality.lock().unwrap_or_else(|e| e.into_inner()) = quality;
        }
    }

    pub async fn update_egress_quality_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        quality: PublisherQuality,
    ) -> bool {
        self.with_current_egress(output_id, registration, |egress| {
            *egress.quality.lock().unwrap_or_else(|e| e.into_inner()) = quality;
        })
        .await
        .is_some()
    }

    pub async fn record_egress_progress(&self, output_id: &str, bytes: u64) {
        let egresses = self.egresses.active.read().await;
        if let Some(egress) = egresses.get(output_id) {
            egress.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
            egress.metrics.record_out(bytes);
            egress
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
            let active_phase = if egress.protocol == "hls" {
                EgressPhase::Uploading
            } else {
                EgressPhase::Sending
            };
            *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = active_phase;
            *egress
                .failure_phase
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *egress.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
            egress.last_error_ms.store(0, Ordering::Relaxed);
        }
    }

    pub async fn record_egress_progress_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        bytes: u64,
    ) -> bool {
        self.with_current_egress(output_id, registration, |egress| {
            egress.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
            egress.metrics.record_out(bytes);
            egress
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
            let active_phase = if egress.protocol == "hls" {
                EgressPhase::Uploading
            } else {
                EgressPhase::Sending
            };
            *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = active_phase;
            *egress
                .failure_phase
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            *egress.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
            egress.last_error_ms.store(0, Ordering::Relaxed);
        })
        .await
        .is_some()
    }

    pub async fn egress_has_recorded_progress(&self, output_id: &str) -> bool {
        let egresses = self.egresses.active.read().await;
        egresses
            .get(output_id)
            .is_some_and(|egress| egress.last_progress_ms.load(Ordering::Relaxed) > 0)
    }

    pub async fn egress_has_recorded_progress_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
    ) -> bool {
        self.with_current_egress(output_id, registration, |egress| {
            egress.last_progress_ms.load(Ordering::Relaxed) > 0
        })
        .await
        .unwrap_or(false)
    }

    pub async fn recent_egress_outcome(&self, output_id: &str) -> Option<RecentEgressOutcome> {
        self.egresses.recent.read().await.get(output_id).cloned()
    }

    pub async fn update_egress_retry_state(
        &self,
        output_id: &str,
        attempts: u32,
        backoff_ms: u64,
        remaining_ms: u64,
    ) {
        if self.has_active_egress(output_id).await {
            self.egresses.retry.write().await.remove(output_id);
            return;
        }
        let next_retry_at_ms = Self::now_epoch_ms().saturating_add(remaining_ms);
        self.egresses.retry.write().await.insert(
            output_id.to_string(),
            EgressRetryState {
                attempts,
                backoff_ms,
                next_retry_at_ms,
            },
        );
    }

    pub async fn update_egress_retry_state_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        attempts: u32,
        backoff_ms: u64,
        remaining_ms: u64,
    ) -> bool {
        if self
            .with_current_egress(output_id, registration, |_| {})
            .await
            .is_none()
        {
            return false;
        }
        let next_retry_at_ms = Self::now_epoch_ms().saturating_add(remaining_ms);
        self.egresses.retry.write().await.insert(
            output_id.to_string(),
            EgressRetryState {
                attempts,
                backoff_ms,
                next_retry_at_ms,
            },
        );
        true
    }

    pub async fn clear_egress_retry_state(&self, output_id: &str) {
        self.egresses.retry.write().await.remove(output_id);
    }

    pub async fn egress_retry_state(&self, output_id: &str) -> Option<EgressRetryState> {
        self.egresses.retry.read().await.get(output_id).cloned()
    }

    pub async fn record_egress_error(
        &self,
        output_id: &str,
        phase: &str,
        message: impl Into<String>,
    ) {
        let message = message.into();
        let event = {
            let egresses = self.egresses.active.read().await;
            if let Some(egress) = egresses.get(output_id) {
                let pipeline_id = egress.pipeline_id.clone();
                *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = EgressPhase::Failed;
                *egress
                    .failure_phase
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(phase.to_string());
                *egress.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(message.clone());
                egress
                    .last_error_ms
                    .store(Self::now_epoch_ms(), Ordering::Relaxed);
                Some(crate::events::EventKind::EgressFailed {
                    pipeline_id,
                    output_id: output_id.to_string(),
                    phase: phase.to_string(),
                    error: message,
                })
            } else {
                None
            }
        };
        if let Some(event) = event {
            self.runtime.event_log.emit(event);
        }
    }

    pub async fn record_egress_error_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        phase: &str,
        message: impl Into<String>,
    ) -> bool {
        let message = message.into();
        let event = self
            .with_current_egress(output_id, registration, |egress| {
                let pipeline_id = egress.pipeline_id.clone();
                *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = EgressPhase::Failed;
                *egress
                    .failure_phase
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(phase.to_string());
                *egress.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(message.clone());
                egress
                    .last_error_ms
                    .store(Self::now_epoch_ms(), Ordering::Relaxed);
                crate::events::EventKind::EgressFailed {
                    pipeline_id,
                    output_id: output_id.to_string(),
                    phase: phase.to_string(),
                    error: message.clone(),
                }
            })
            .await;
        if let Some(event) = event {
            self.runtime.event_log.emit(event);
            true
        } else {
            false
        }
    }

    /// Update bytes received counter for an active ingest (lock-free atomic).
    pub async fn update_ingest_bytes(&self, pipeline_id: &str, bytes: u64) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            ingest.bytes_received.fetch_add(bytes, Ordering::Relaxed);
            ingest
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
        }
    }

    pub async fn record_keyframe(&self, pipeline_id: &str, pts: i64) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let mut times = ingest
                .keyframe_times
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            times.push(pts);
            if times.len() > 30 {
                times.remove(0);
            }
        }
    }

    /// Update egress bytes sent counter (lock-free atomic).
    pub async fn update_egress_bytes(&self, output_id: &str, bytes: u64) {
        let egresses = self.egresses.active.read().await;
        if let Some(egress) = egresses.get(output_id) {
            egress.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
            egress
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
        }
    }

    pub async fn egress_bytes(&self, output_id: &str) -> u64 {
        let egresses = self.egresses.active.read().await;
        egresses
            .get(output_id)
            .map(|e| e.bytes_sent.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Update stream metadata discovered during demux/decode for an active ingest.
    pub async fn update_ingest_meta(
        &self,
        pipeline_id: &str,
        video: Option<VideoMeta>,
        audio: Option<AudioMeta>,
        remote_addr: Option<String>,
    ) {
        if let Some(video_meta) = video.as_ref() {
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_codec_hint(&video_meta.codec);
            }
        }
        let mut ingests = self.ingests.active.write().await;
        if let Some(ingest) = ingests.get_mut(pipeline_id) {
            if video.is_some() {
                ingest.video = video;
                if ingest.video_track_count == 0 {
                    ingest.video_track_count = 1;
                }
                if ingest.selected_video_track_index.is_none() {
                    ingest.selected_video_track_index = Some(0);
                }
            }
            if audio.is_some() {
                ingest.audio = audio;
            }
            if remote_addr.is_some() {
                ingest.remote_addr = remote_addr;
            }
        }
    }

    pub async fn update_ingest_video_track_selection(
        &self,
        pipeline_id: &str,
        video_track_count: usize,
        selected_video_track_index: Option<u32>,
    ) {
        let mut ingests = self.ingests.active.write().await;
        if let Some(ingest) = ingests.get_mut(pipeline_id) {
            ingest.video_track_count = video_track_count;
            ingest.selected_video_track_index = selected_video_track_index;
        }
    }

    pub async fn cache_sequence_header(
        &self,
        pipeline_id: &str,
        is_video: bool,
        data: bytes::Bytes,
    ) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            if is_video {
                *ingest
                    .video_sequence_header
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(data);
            } else {
                *ingest
                    .audio_sequence_header
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(data);
            }
        }
    }

    pub async fn get_sequence_headers(
        &self,
        pipeline_id: &str,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let video = ingest
                .video_sequence_header
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let audio = ingest
                .audio_sequence_header
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            (video, audio)
        } else {
            (None, None)
        }
    }

    /// Update audio track metadata for an active ingest (multi-track support).
    pub async fn update_ingest_audio_tracks(&self, pipeline_id: &str, tracks: Vec<AudioMeta>) {
        // Update the ingest metadata registry (used for API views and stage metadata lookups).
        {
            let ingests = self.ingests.active.read().await;
            if let Some(ingest) = ingests.get(pipeline_id) {
                *ingest
                    .audio_tracks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = std::sync::Arc::new(tracks.clone());
            }
        }
        // Also propagate audio_tracks to the source ring so downstream stages
        // (audio_router, RTMP/SRT egress) can read them directly from the ring
        // without going through the ingest registry.  This makes the source ring
        // authoritative for audio metadata, matching how codec_hint and
        // video_parameter_sets are handled.
        if !tracks.is_empty() {
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_audio_tracks(tracks);
            }
        }
    }

    /// Update publisher transport quality metrics.
    pub async fn update_publisher_quality(&self, pipeline_id: &str, quality: PublisherQuality) {
        let mut ingests = self.ingests.active.write().await;
        if let Some(ingest) = ingests.get_mut(pipeline_id) {
            ingest.quality = quality;
        }
    }

    pub async fn recent_ingest_outcome(&self, pipeline_id: &str) -> Option<RecentIngestOutcome> {
        self.ingests.recent.read().await.get(pipeline_id).cloned()
    }

    /// Register an active recording for a pipeline. Returns a cancellation token.
    pub async fn register_recording(&self, pipeline_id: &str) -> CancellationToken {
        let mut tokens = self.recordings.cancel_tokens.write().await;
        let token = CancellationToken::new();
        tokens.insert(pipeline_id.to_string(), token.clone());
        token
    }

    /// Unregister (and cancel) an active recording for a pipeline.
    pub async fn unregister_recording(&self, pipeline_id: &str) {
        let mut tokens = self.recordings.cancel_tokens.write().await;
        if let Some(token) = tokens.remove(pipeline_id) {
            token.cancel();
        }
    }

    /// Check if a recording is actively running for a pipeline.
    pub async fn is_recording_active(&self, pipeline_id: &str) -> bool {
        let tokens = self.recordings.cancel_tokens.read().await;
        tokens
            .get(pipeline_id)
            .is_some_and(|token| !token.is_cancelled())
    }

    pub async fn cancel_all_active_tasks(&self) {
        {
            let egress = self.egresses.cancel_tokens.read().await;
            for token in egress.values() {
                token.cancel();
            }
        }
        {
            let ingests = self.ingests.cancel_tokens.read().await;
            for token in ingests.values() {
                token.cancel();
            }
        }
        {
            let recordings = self.recordings.cancel_tokens.read().await;
            for token in recordings.values() {
                token.cancel();
            }
        }
    }
}

#[cfg(test)]
#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
