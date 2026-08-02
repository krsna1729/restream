//! Egress lifecycle operations owned by `MediaEngine`.

mod status;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKey;
use crate::domain::state::{EgressPhase, EgressStatus};
use crate::media::engine::{
    ActiveEgress, EgressRegistration, EgressRetryState, MediaEngine, RecentEgressOutcome,
};
use crate::media::packet::MediaType;
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
use crate::media::snapshots::PublisherQuality;
use crate::media::stage_metrics::StageMetrics;

impl MediaEngine {
    pub async fn with_active_egress<R>(
        &self,
        output_id: &str,
        f: impl FnOnce(&ActiveEgress) -> R,
    ) -> Option<R> {
        let egresses = self.egresses.active.read().await;
        egresses.get(output_id).map(f)
    }

    pub(super) async fn with_current_egress<R>(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
        f: impl FnOnce(&ActiveEgress) -> R,
    ) -> Option<R> {
        let egresses = self.egresses.active.read().await;
        let egress = egresses.get(output_id)?;
        (egress.attempt_id == registration.attempt_id).then(|| f(egress))
    }

    pub async fn has_active_egress(&self, output_id: &str) -> bool {
        self.egresses
            .cancel_tokens
            .read()
            .await
            .contains_key(output_id)
    }

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
        let mut warmup = Reader::new(format!("egress_warmup:{output_id}"), ring_buffer.clone());
        let mut warmup_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
        tokio::select! {
            _ = cancel_token.cancelled() => {}
            _ = async {
                loop {
                    warmup.wait_for_data().await;
                    warmup_packets.clear();
                    let _ = warmup.pull_burst(
                        &mut warmup_packets,
                        MEDIA_PULL_BURST_PACKETS,
                    );

                    if ring_buffer.video_parameter_sets().is_some()
                        || warmup_packets
                            .iter()
                            .any(|packet| packet.media_type == MediaType::Video)
                        || (!warmup_packets.is_empty()
                            && ring_buffer.video_parameter_sets().is_none())
                    {
                        break;
                    }
                }
            } => {}
        }
    }

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
            .map(|egress| egress.bytes_sent.load(Ordering::Relaxed))
            .unwrap_or(0)
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
                is_fabric: false,
                shard_id: None,
                resync_count: Arc::new(AtomicU64::new(0)),
                feed_lag_units: Arc::new(AtomicU64::new(0)),
                backpressure_reason: Arc::new(std::sync::Mutex::new(None)),
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

    /// Records fabric ownership and shard assignment for an
    /// already-registered output. Kept as a separate call rather than a
    /// `register_egress_attempt_with_meta` parameter because whether a
    /// fabric task actually started (as opposed to failing during startup
    /// preparation, which leaves `is_fabric` `false` and records an error
    /// through `record_egress_error_if_current` instead) isn't known until
    /// after registration in the bootstrap egress reconciler.
    pub async fn set_egress_fabric_attribution(
        &self,
        output_id: &str,
        is_fabric: bool,
        shard_id: Option<u32>,
    ) {
        let mut egresses = self.egresses.active.write().await;
        if let Some(egress) = egresses.get_mut(output_id) {
            egress.is_fabric = is_fabric;
            egress.shard_id = shard_id;
        }
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
            Self::build_recent_egress_outcome(previous_recent.as_ref(), egress, has_ingest, true)
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
        let outcome = Self::build_recent_egress_outcome(
            previous_recent.as_ref(),
            active,
            has_ingest,
            registration.cancel_token.is_cancelled(),
        );
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

    pub async fn record_egress_discard_progress_if_current(
        &self,
        output_id: &str,
        registration: &EgressRegistration,
    ) -> bool {
        self.with_current_egress(output_id, registration, |egress| {
            egress
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
            *egress.phase.lock().unwrap_or_else(|e| e.into_inner()) = EgressPhase::Discarding;
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
        // Hold the retry write lock across the active-egress check instead of
        // re-acquiring it after: register_egress_attempt_with_meta's first
        // action is also a `retry` write (it clears stale retry state before
        // marking the egress active), so serializing on this lock closes the
        // window where a late retry publish could land after a racing
        // registration already cleared it, leaving a stale "retrying" entry
        // next to an active egress.
        let mut retry = self.egresses.retry.write().await;
        if self.has_active_egress(output_id).await {
            retry.remove(output_id);
            return;
        }
        let next_retry_at_ms = Self::now_epoch_ms().saturating_add(remaining_ms);
        retry.insert(
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
        // Same reasoning as update_egress_retry_state: hold the retry write
        // lock across the current-attempt check so a racing registration
        // (which clears `retry` as its own first action) cannot slip in
        // between the check and this insert and leave a stale entry behind.
        let mut retry = self.egresses.retry.write().await;
        if self
            .with_current_egress(output_id, registration, |_| {})
            .await
            .is_none()
        {
            return false;
        }
        let next_retry_at_ms = Self::now_epoch_ms().saturating_add(remaining_ms);
        retry.insert(
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
}
