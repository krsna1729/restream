//! Ingest session selection and lifecycle operations owned by `MediaEngine`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::media::engine::{
    ActiveIngest, IngestMetadata, IngestRegistration, MediaEngine, RecentIngestOutcome,
};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_metrics::StageMetrics;

impl MediaEngine {
    pub async fn try_register_ingest_attempt(
        &self,
        pipeline_id: &str,
        stream_key: &str,
        protocol: &str,
    ) -> Option<IngestRegistration> {
        if self.ingests.active.read().await.contains_key(pipeline_id) {
            return None;
        }
        self.try_register_pipeline_input_attempt(
            pipeline_id,
            stream_key,
            stream_key,
            protocol,
            true,
        )
        .await
    }

    pub async fn try_register_pipeline_input_attempt(
        &self,
        pipeline_id: &str,
        input_id: &str,
        stream_key: &str,
        protocol: &str,
        selected: bool,
    ) -> Option<IngestRegistration> {
        let _selection = self.ingests.selection_lock.lock().await;
        let mut tokens = self.ingests.cancel_tokens.write().await;
        if let Some(existing) = tokens.get(input_id)
            && !existing.is_cancelled()
        {
            return None;
        }

        let selected = self
            .ingests
            .selected_inputs
            .read()
            .await
            .get(pipeline_id)
            .map(|selected_input| selected_input == input_id)
            .unwrap_or(selected);
        if selected {
            let previous = self.ingests.active.read().await.get(pipeline_id).cloned();
            if let Some(previous) = previous
                && previous.input_id != input_id
            {
                previous.gate.demote();
                previous.gate.wait_until_idle().await;
            }
        }

        let attempt_id = self.ingests.next_attempt_id.fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        let last_forwarded_dts = {
            let mut timelines = self.ingests.timelines.write().await;
            timelines
                .entry(pipeline_id.to_string())
                .or_insert_with(|| Arc::new(AtomicI64::new(i64::MIN)))
                .clone()
        };
        let preview_ring = {
            let mut preview_slots = self.ingests.preview_slots.write().await;
            preview_slots
                .entry(input_id.to_string())
                .or_insert_with(|| Arc::new(arc_swap::ArcSwapOption::empty()))
                .clone()
        };
        let gate = Arc::new(
            if selected && last_forwarded_dts.load(Ordering::Acquire) == i64::MIN {
                crate::media::input_gate::InputPacketGate::active()
            } else if selected {
                let gate = crate::media::input_gate::InputPacketGate::standby();
                gate.arm_for_promotion();
                gate
            } else {
                crate::media::input_gate::InputPacketGate::standby()
            },
        );
        let registration = IngestRegistration {
            cancel_token: token.clone(),
            attempt_id,
            input_id: input_id.to_string(),
            gate: gate.clone(),
            last_forwarded_dts,
            preview_ring,
        };
        tokens.insert(input_id.to_string(), token.clone());

        let now = Instant::now();
        let ingest = Arc::new(ActiveIngest {
            attempt_id,
            pipeline_id: pipeline_id.to_string(),
            input_id: input_id.to_string(),
            stream_key: stream_key.to_string(),
            gate,
            start_time: now,
            protocol: protocol.to_string(),
            bytes_received: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(StageMetrics::new()),
            last_progress_ms: Arc::new(AtomicU64::new(0)),
            metadata: std::sync::RwLock::new(IngestMetadata::default()),
            audio_tracks: std::sync::Mutex::new(Arc::new(Vec::new())),
            keyframe_times: Arc::new(std::sync::Mutex::new(Vec::new())),
            video_sequence_header: std::sync::Mutex::new(None),
            audio_sequence_header: std::sync::Mutex::new(None),
            prev_bytes_received: AtomicU64::new(0),
            prev_sample_time: std::sync::Mutex::new(now),
            bitrate_kbps: std::sync::Mutex::new(None),
        });
        self.ingests
            .sessions
            .write()
            .await
            .insert(input_id.to_string(), ingest.clone());
        if selected {
            self.ingests
                .selected_inputs
                .write()
                .await
                .insert(pipeline_id.to_string(), input_id.to_string());
            self.ingests
                .active
                .write()
                .await
                .insert(pipeline_id.to_string(), ingest);
        }

        self.runtime
            .event_log
            .emit(crate::events::EventKind::IngestConnected {
                pipeline_id: pipeline_id.to_string(),
                protocol: protocol.to_string(),
                stream_key: stream_key.to_string(),
            });
        Some(registration)
    }

    pub async fn select_pipeline_input(&self, pipeline_id: &str, input_id: &str) -> bool {
        let _selection = self.ingests.selection_lock.lock().await;
        let previous = self.ingests.active.read().await.get(pipeline_id).cloned();
        if previous
            .as_ref()
            .is_some_and(|ingest| ingest.input_id == input_id)
        {
            self.ingests
                .selected_inputs
                .write()
                .await
                .insert(pipeline_id.to_string(), input_id.to_string());
            return true;
        }
        if let Some(previous) = previous
            && previous.input_id != input_id
        {
            previous.gate.demote();
            previous.gate.wait_until_idle().await;
        }

        self.ingests
            .selected_inputs
            .write()
            .await
            .insert(pipeline_id.to_string(), input_id.to_string());
        let replacement = self.ingests.sessions.read().await.get(input_id).cloned();
        match replacement {
            Some(replacement) if replacement.pipeline_id == pipeline_id => {
                replacement.gate.arm_for_promotion();
                let metadata = replacement.metadata();
                let audio_tracks = replacement
                    .audio_tracks
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if let Some(ring) = self.ingests.pipelines.read().await.get(pipeline_id) {
                    if let Some(video) = metadata.video {
                        ring.set_codec_hint(&video.codec);
                    }
                    if !audio_tracks.is_empty() {
                        ring.set_audio_tracks(audio_tracks.as_ref().clone());
                    }
                }
                self.ingests
                    .active
                    .write()
                    .await
                    .insert(pipeline_id.to_string(), replacement);
                true
            }
            Some(_) | None => {
                self.ingests.active.write().await.remove(pipeline_id);
                false
            }
        }
    }

    pub async fn connected_input_count(&self, pipeline_id: &str) -> usize {
        self.ingests
            .sessions
            .read()
            .await
            .values()
            .filter(|ingest| ingest.pipeline_id == pipeline_id)
            .count()
    }

    pub async fn cancel_pipeline_input(&self, input_id: &str) -> bool {
        let token = self
            .ingests
            .cancel_tokens
            .read()
            .await
            .get(input_id)
            .cloned();
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub async fn ensure_input_preview_ring(&self, input_id: &str) -> Option<Arc<RingBuffer>> {
        let _preview = self.ingests.preview_lock.lock().await;
        let ingest = self.ingests.sessions.read().await.get(input_id).cloned()?;
        let slot = self
            .ingests
            .preview_slots
            .read()
            .await
            .get(input_id)
            .cloned()?;
        if let Some(ring) = slot.load_full() {
            return Some(ring);
        }

        let ring = Arc::new(RingBuffer::new(self.config.ring_capacity));
        let metadata = ingest.metadata();
        if let Some(video) = metadata.video {
            ring.set_codec_hint(&video.codec);
        }
        let audio_tracks = ingest
            .audio_tracks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if !audio_tracks.is_empty() {
            ring.set_audio_tracks(audio_tracks.as_ref().clone());
        }
        let video_header = ingest
            .video_sequence_header
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let audio_header = ingest
            .audio_sequence_header
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(payload) = video_header {
            ring.push(MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts: 0,
                dts: 0,
                is_keyframe: false,
                format: PayloadFormat::Flv,
                payload,
            });
        }
        if let Some(payload) = audio_header {
            ring.push(MediaPacket {
                media_type: MediaType::Audio,
                track_index: 0,
                pts: 0,
                dts: 0,
                is_keyframe: false,
                format: PayloadFormat::Flv,
                payload,
            });
        }
        slot.store(Some(ring.clone()));
        Some(ring)
    }

    pub async fn release_input_preview_ring(&self, input_id: &str) {
        if let Some(slot) = self.ingests.preview_slots.read().await.get(input_id) {
            slot.store(None);
        }
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
        let ingest = self.ingests.active.read().await.get(pipeline_id).cloned();
        let Some(ingest) = ingest else {
            return;
        };

        let previous = self.ingests.recent.read().await.get(pipeline_id).cloned();
        let metadata = ingest.metadata();
        let snapshot = Self::build_recent_ingest_outcome(
            previous.as_ref(),
            ingest.protocol.clone(),
            phase,
            reason,
            had_error,
            metadata.remote_addr.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        );

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
        let _selection = self.ingests.selection_lock.lock().await;
        let ingest = self
            .ingests
            .sessions
            .read()
            .await
            .get(&registration.input_id)
            .cloned();
        let Some(ingest) = ingest else {
            return false;
        };
        if ingest.pipeline_id != pipeline_id || ingest.attempt_id != registration.attempt_id {
            return false;
        }

        let is_selected = self
            .ingests
            .active
            .read()
            .await
            .get(pipeline_id)
            .is_some_and(|active| active.attempt_id == registration.attempt_id);
        if !is_selected {
            return true;
        }

        let previous = self.ingests.recent.read().await.get(pipeline_id).cloned();
        let metadata = ingest.metadata();
        let snapshot = Self::build_recent_ingest_outcome(
            previous.as_ref(),
            ingest.protocol.clone(),
            phase,
            reason,
            had_error,
            metadata.remote_addr.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        );

        self.ingests
            .recent
            .write()
            .await
            .insert(pipeline_id.to_string(), snapshot);
        true
    }

    pub async fn unregister_ingest(&self, pipeline_id: &str) {
        let _selection = self.ingests.selection_lock.lock().await;
        let removed_sessions = {
            let mut sessions = self.ingests.sessions.write().await;
            let input_ids = sessions
                .values()
                .filter(|ingest| ingest.pipeline_id == pipeline_id)
                .map(|ingest| ingest.input_id.clone())
                .collect::<Vec<_>>();
            input_ids
                .iter()
                .filter_map(|input_id| sessions.remove(input_id))
                .collect::<Vec<_>>()
        };
        {
            let mut tokens = self.ingests.cancel_tokens.write().await;
            for ingest in &removed_sessions {
                if let Some(token) = tokens.remove(&ingest.input_id) {
                    token.cancel();
                }
            }
        }

        let removed_selected = self.ingests.active.write().await.remove(pipeline_id);
        self.ingests
            .selected_inputs
            .write()
            .await
            .remove(pipeline_id);

        let protocol = removed_selected
            .as_ref()
            .map(|ingest| ingest.protocol.clone())
            .unwrap_or_default();
        if let Some(ingest) = removed_selected {
            let remote_addr = ingest
                .metadata
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .remote_addr
                .clone();
            let mut recent = self.ingests.recent.write().await;
            recent
                .entry(pipeline_id.to_string())
                .or_insert_with(|| RecentIngestOutcome {
                    protocol: ingest.protocol.clone(),
                    disconnected_at_ms: Self::now_epoch_ms(),
                    first_disconnect_at_ms: Self::now_epoch_ms(),
                    disconnect_count: 1,
                    reason: None,
                    failure_phase: None,
                    had_error: false,
                    remote_addr,
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
        let _selection = self.ingests.selection_lock.lock().await;
        {
            let mut sessions = self.ingests.sessions.write().await;
            let is_current = sessions.get(&registration.input_id).is_some_and(|ingest| {
                ingest.pipeline_id == pipeline_id && ingest.attempt_id == registration.attempt_id
            });
            if !is_current {
                return false;
            }
            sessions.remove(&registration.input_id);
        }
        if let Some(token) = self
            .ingests
            .cancel_tokens
            .write()
            .await
            .remove(&registration.input_id)
        {
            token.cancel();
        }

        let removed_selected = {
            let mut active = self.ingests.active.write().await;
            let is_selected = active
                .get(pipeline_id)
                .is_some_and(|ingest| ingest.attempt_id == registration.attempt_id);
            is_selected.then(|| active.remove(pipeline_id)).flatten()
        };
        let protocol = removed_selected
            .as_ref()
            .map(|ingest| ingest.protocol.clone())
            .unwrap_or_default();
        if let Some(ingest) = removed_selected {
            let remote_addr = ingest
                .metadata
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .remote_addr
                .clone();
            let mut recent = self.ingests.recent.write().await;
            recent
                .entry(pipeline_id.to_string())
                .or_insert_with(|| RecentIngestOutcome {
                    protocol: ingest.protocol.clone(),
                    disconnected_at_ms: Self::now_epoch_ms(),
                    first_disconnect_at_ms: Self::now_epoch_ms(),
                    disconnect_count: 1,
                    reason: None,
                    failure_phase: None,
                    had_error: false,
                    remote_addr,
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

    pub async fn recent_ingest_outcome(&self, pipeline_id: &str) -> Option<RecentIngestOutcome> {
        self.ingests.recent.read().await.get(pipeline_id).cloned()
    }
}
