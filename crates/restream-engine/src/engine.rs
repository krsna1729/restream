//! Central media engine state and public runtime contracts.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::engine_registries::{
    EgressRegistry, FabricRegistry, FileIngestRegistry, HlsRegistry, RecordingRegistry,
    RuntimeInfra, StreamQaRegistry,
};
pub use crate::engine_runtime::IngestListener;
use crate::ingest_registry::{
    ActiveIngest, INGEST_FLAP_WINDOW_MS, IngestRegistration, IngestRegistry, RecentIngestOutcome,
};
use crate::snapshots::PublisherQuality;
use crate::stage_registry::StageRegistry;
use restream_sync::RingBuffer;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Publisher {
    pub protocol: String,
    pub remote_addr: Option<String>,
    pub quality: PublisherQuality,
}

/// RTMP ingress limits derived from the engine configuration.
#[derive(Clone, Copy, Debug)]
pub struct RtmpIngressLimits {
    pub max_connections: usize,
    pub max_connections_per_ip: usize,
    pub parser_budget_bytes: usize,
    pub ingress_owners: usize,
    pub max_message_bytes: usize,
}

pub struct MediaEngine {
    pub ingests: Arc<IngestRegistry>,
    pub egresses: EgressRegistry,
    pub fabric: FabricRegistry,
    pub recordings: RecordingRegistry,
    pub stream_qa: StreamQaRegistry,
    pub hls: HlsRegistry,
    pub file_ingests: FileIngestRegistry,
    pub stages: Arc<StageRegistry>,
    pub(crate) runtime: RuntimeInfra,
    pub config: Arc<crate::config::EngineConfig>,
}

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaEngine {
    pub fn new() -> Self {
        Self::new_with_config(Arc::new(crate::config::EngineConfig::from_env()))
    }

    pub fn new_with_config(config: Arc<crate::config::EngineConfig>) -> Self {
        let runtime = RuntimeInfra::new(&config);
        Self {
            ingests: Arc::new(IngestRegistry::new(
                runtime.event_log.clone(),
                config.ring_capacity,
                config.ring_headroom_secs,
            )),
            egresses: EgressRegistry::new(),
            fabric: FabricRegistry::new(),
            recordings: RecordingRegistry::new(),
            stream_qa: StreamQaRegistry::default(),
            hls: HlsRegistry::new(),
            file_ingests: FileIngestRegistry::new(),
            stages: Arc::new(StageRegistry::new()),
            runtime,
            config,
        }
    }

    pub fn now_epoch_ms() -> u64 {
        crate::timing::epoch_ms()
    }

    /// The RTMP ingress limits this engine was configured with.
    pub fn rtmp_ingress_limits(&self) -> RtmpIngressLimits {
        RtmpIngressLimits {
            max_connections: self.config.rtmp_max_connections,
            max_connections_per_ip: self.config.rtmp_max_connections_per_ip,
            parser_budget_bytes: self.config.rtmp_ingest_parser_budget_bytes,
            ingress_owners: self.config.rtmp_ingress_owners,
            max_message_bytes: self.config.rtmp_max_message_bytes,
        }
    }

    /// The configured egress fabric shard count, at least one.
    pub fn egress_shard_count(&self) -> u32 {
        self.config.egress_fabric.shards.max(1)
    }

    /// Extra PEM roots the HLS uploader trusts, when configured.
    pub fn hls_upload_trust_roots(&self) -> Option<&str> {
        self.config.rtmps_extra_trust_roots_pem_path.as_deref()
    }

    /// `ms` as [`restream_domain::timestamp::rfc3339`]; `None` for 0 (never).
    pub fn epoch_ms_to_rfc3339(ms: u64) -> Option<String> {
        if ms == 0 {
            return None;
        }
        std::time::SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_millis(ms))
            .map(restream_domain::timestamp::rfc3339)
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
        self.ingests.with_active_ingest(pipeline_id, f).await
    }

    /// Runs `f` over every selected ingest, keyed by pipeline id; `f` runs
    /// under the ingest map's read guard and must not wait.
    pub async fn with_active_ingests<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<ActiveIngest>>) -> R,
    ) -> R {
        self.ingests.with_active_ingests(f).await
    }

    /// Runs `f` over every live input session, keyed by input id; `f` runs
    /// under the session map's read guard and must not wait.
    pub async fn with_ingest_sessions<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<ActiveIngest>>) -> R,
    ) -> R {
        self.ingests.with_ingest_sessions(f).await
    }

    /// Runs `f` over every source ring, keyed by pipeline (or input preview
    /// resource) id; `f` runs under the ring map's read guard and must not
    /// wait.
    pub async fn with_source_rings<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<RingBuffer>>) -> R,
    ) -> R {
        self.ingests.with_source_rings(f).await
    }

    /// [`Self::with_source_rings`] without waiting: `None` while the ring map
    /// is being written.
    pub(crate) fn try_with_source_rings<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<RingBuffer>>) -> R,
    ) -> Option<R> {
        self.ingests.try_with_source_rings(f)
    }

    /// The pipeline's source ring, if one exists.
    pub async fn source_ring(&self, pipeline_id: &str) -> Option<Arc<RingBuffer>> {
        self.ingests.source_ring(pipeline_id).await
    }

    /// Every pipeline's last ingest disconnect outcome, copied.
    pub async fn recent_ingest_outcomes(&self) -> HashMap<String, RecentIngestOutcome> {
        self.ingests.recent_ingest_outcomes().await
    }

    /// Runs `f` on the pipeline's source ring with the pipeline's ingest
    /// selection held (`IngestRegistry::with_selected_source_ring`). `f` must
    /// not take this pipeline's `selection`, directly or through an ingest
    /// operation (the lock is not reentrant), and must not hold an ingest map
    /// across its awaits.
    pub(crate) async fn with_selected_source_ring<T>(
        &self,
        pipeline_id: &str,
        f: impl AsyncFnOnce(Arc<RingBuffer>) -> T,
    ) -> Option<T> {
        self.ingests.with_selected_source_ring(pipeline_id, f).await
    }

    /// The selected active ingest for a pipeline.
    pub async fn active_ingest(&self, pipeline_id: &str) -> Option<Arc<ActiveIngest>> {
        self.ingests.active_ingest(pipeline_id).await
    }

    /// The live session registered for one input.
    pub async fn ingest_session(&self, input_id: &str) -> Option<Arc<ActiveIngest>> {
        self.ingests.ingest_session(input_id).await
    }

    pub async fn with_ingest_session<R>(
        &self,
        registration: &IngestRegistration,
        f: impl FnOnce(&ActiveIngest) -> R,
    ) -> Option<R> {
        self.ingests.with_ingest_session(registration, f).await
    }

    pub(super) async fn current_ingest_session(
        &self,
        registration: &IngestRegistration,
    ) -> Option<Arc<ActiveIngest>> {
        self.ingests.current_ingest_session(registration).await
    }

    pub async fn is_ingest_session_selected(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> bool {
        self.ingests
            .is_ingest_session_selected(pipeline_id, registration)
            .await
    }

    pub fn egress_protocol_from_url(url: &str) -> &'static str {
        restream_domain::domain::output_spec::EgressProtocol::from_url(url).as_str()
    }

    pub fn graph_protocol_label(protocol: &str) -> String {
        if protocol.is_empty() || protocol == "unknown" {
            "Unknown".to_string()
        } else {
            protocol.to_uppercase()
        }
    }

    pub fn graph_slug(value: &str) -> String {
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

    pub fn source_buffer_format(protocol: Option<&str>) -> &'static str {
        match protocol {
            Some("rtmp") => "FLV media packets",
            Some("srt") => "Demuxed MPEG-TS media packets",
            Some("file") => "Demuxed file media packets",
            Some("hls") => "Demuxed HLS segment media packets",
            _ => "Media packets",
        }
    }

    pub fn source_to_egress_label(protocol: &str) -> &'static str {
        match protocol {
            "rtmp" => "RTMP publish packets",
            "srt" => "MPEG-TS packetization",
            "hls" => "HLS segment input",
            _ => "media packets",
        }
    }

    pub fn sample_ingest_bitrate_kbps(ingest: &ActiveIngest) -> Option<f64> {
        let bytes_received = ingest.bytes_received.load(Ordering::Relaxed);
        let previous_bytes = ingest.prev_bytes_received.load(Ordering::Relaxed);
        let mut previous_time = restream_sync::lock::lock(&ingest.prev_sample_time);
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
            *restream_sync::lock::lock(&ingest.bitrate_kbps) = Some(rate);
            Some(rate)
        } else if elapsed > 1.0 && bytes_received == previous_bytes {
            *restream_sync::lock::lock(&ingest.bitrate_kbps) = Some(0.0);
            Some(0.0)
        } else {
            *restream_sync::lock::lock(&ingest.bitrate_kbps)
        }
    }

    pub async fn has_active_ingest(&self, pipeline_id: &str) -> bool {
        self.ingests.has_active_ingest(pipeline_id).await
    }

    pub async fn has_recent_ingest_disconnect(&self, pipeline_id: &str, grace_ms: u64) -> bool {
        self.ingests
            .has_recent_ingest_disconnect(pipeline_id, grace_ms)
            .await
    }

    pub fn recent_ingest_flap_state(recent: Option<&RecentIngestOutcome>) -> (u32, bool) {
        let Some(recent) = recent else {
            return (0, false);
        };
        if Self::now_epoch_ms().saturating_sub(recent.disconnected_at_ms) > INGEST_FLAP_WINDOW_MS {
            return (0, false);
        }
        (recent.disconnect_count, recent.disconnect_count >= 2)
    }
}

#[cfg(test)]
pub(crate) mod tests;
