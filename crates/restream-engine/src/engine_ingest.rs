//! Ingest session selection and lifecycle operations owned by `MediaEngine`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::engine::MediaEngine;
use crate::ingest_lease::IngestLease;
use crate::ingest_publisher::PublishTarget;
use crate::ingest_registry::{
    ActiveIngest, IngestMetadata, IngestRegistration, RecentIngestOutcome,
};
use crate::input_gate::InputForwardState;
use crate::stage_metrics::StageMetrics;
use restream_sync::RingBuffer;

impl MediaEngine {
    /// Registers the pipeline's only publisher, unless one is active.
    pub async fn try_register_ingest(
        &self,
        pipeline_id: &str,
        stream_key: &str,
        protocol: &str,
    ) -> Option<IngestLease> {
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

    /// Registers one attempt on `input_id`, unless a live attempt holds it.
    /// The returned lease ends the attempt: by `finish`, or on drop through
    /// the ingest registry's release task.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order `selection` (this pipeline's) -> `cancel_tokens` -> `sessions` -> `selected_inputs` -> `active` (module docs of `ingest_registry`); no engine-wide guard is held across the gate wait, and every write guard is taken before the first insert (below)"
    )]
    pub async fn try_register_pipeline_input_attempt(
        &self,
        pipeline_id: &str,
        input_id: &str,
        stream_key: &str,
        protocol: &str,
        selected: bool,
    ) -> Option<IngestLease> {
        let _selection = self.ingests.selection.lock(pipeline_id).await;
        if live_attempt(self.ingests.cancel_tokens.read().await.get(input_id)) {
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
        // A previous input demoted here is restored if this future is dropped
        // before the new ingest is inserted: otherwise it stays in `active`
        // but forwards nothing, and nothing re-promotes it.
        let mut demoted = DemotedGate(None);
        if selected {
            let previous = self.ingests.active.read().await.get(pipeline_id).cloned();
            if let Some(previous) = previous
                && previous.input_id != input_id
            {
                demoted = DemotedGate(Some((previous.gate.clone(), previous.gate.state())));
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
                crate::input_gate::InputPacketGate::active()
            } else if selected {
                let gate = crate::input_gate::InputPacketGate::standby();
                gate.arm_for_promotion();
                gate
            } else {
                crate::input_gate::InputPacketGate::standby()
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
        // Take every write guard before the first insert: no await may follow
        // it. Lock order: `cancel_tokens` -> `sessions` -> `selected_inputs` ->
        // `active`; `unregister_ingest_if_current` takes the same order, and
        // every other path takes these locks one at a time.
        // Tokio's `RwLock` write waits while a reader holds it (the input
        // status JSON reads `sessions`), so a caller whose future is dropped
        // at one of those awaits — a pull's cancel lands in this window —
        // would otherwise leave a token nothing cancels and a session with no
        // owner: the input shows as connected until the next registration
        // overwrites it.
        let mut tokens = self.ingests.cancel_tokens.write().await;
        // Checked again under the write guard: the gate wait above holds only
        // this pipeline's selection, and another pipeline may name this input
        // id. A refusal here restores the demoted gate (`demoted`'s drop).
        if live_attempt(tokens.get(input_id)) {
            return None;
        }
        let mut sessions = self.ingests.sessions.write().await;
        let mut selected_inputs = if selected {
            Some(self.ingests.selected_inputs.write().await)
        } else {
            None
        };
        let mut active = if selected {
            Some(self.ingests.active.write().await)
        } else {
            None
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
            first_media_ms: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(StageMetrics::new()),
            last_progress_ms: Arc::new(AtomicU64::new(0)),
            metadata: std::sync::RwLock::new(IngestMetadata::default()),
            audio_tracks: std::sync::Mutex::new(Arc::new(Vec::new())),
            keyframe_times: Arc::new(std::sync::Mutex::new(Vec::new())),
            video_sequence_header: Arc::new(std::sync::Mutex::new(None)),
            audio_sequence_header: Arc::new(std::sync::Mutex::new(None)),
            prev_bytes_received: AtomicU64::new(0),
            prev_sample_time: std::sync::Mutex::new(now),
            bitrate_kbps: std::sync::Mutex::new(None),
        });
        sessions.insert(input_id.to_string(), ingest.clone());
        if let Some(selected_inputs) = selected_inputs.as_mut() {
            selected_inputs.insert(pipeline_id.to_string(), input_id.to_string());
        }
        if let Some(active) = active.as_mut() {
            active.insert(pipeline_id.to_string(), ingest);
        }
        drop((sessions, selected_inputs, active, tokens));
        demoted.disarm();

        self.runtime
            .event_log
            .emit(crate::events::EventKind::IngestConnected {
                pipeline_id: pipeline_id.to_string(),
                protocol: protocol.to_string(),
                stream_key: stream_key.to_string(),
            });
        self.ingests.start_release_drain();
        Some(IngestLease::new(&self.ingests, pipeline_id, registration))
    }

    /// Starts the task that ends dropped ingest leases' attempts
    /// (`IngestRegistry::start_release_drain`).
    pub fn start_ingest_release_drain(&self) {
        self.ingests.start_release_drain();
    }

    /// Shutdown, once no session remains: stops the release task, then ends
    /// the backlog it left.
    pub async fn stop_ingest_release_drain(&self) {
        self.ingests.stop_release_drain().await;
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "this pipeline's `selection` serializes the selection; under it each ingest map is taken one at a time (module docs of `ingest_registry`)"
    )]
    pub async fn select_pipeline_input(&self, pipeline_id: &str, input_id: &str) -> bool {
        let _selection = self.ingests.selection.lock(pipeline_id).await;
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
                let audio_tracks = restream_sync::lock::lock(&replacement.audio_tracks).clone();
                if let Some(ring) = self.ingests.pipelines.read().await.get(pipeline_id) {
                    if let Some(video) = metadata.video {
                        ring.set_codec_hint(&video.codec);
                    } else if metadata.tracks_authoritative {
                        ring.remove_video_track();
                    }
                    if !audio_tracks.is_empty() || metadata.tracks_authoritative {
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

    /// Whether `input_id` has a live publisher session on `pipeline_id`.
    pub async fn input_has_session(&self, pipeline_id: &str, input_id: &str) -> bool {
        self.ingests
            .sessions
            .read()
            .await
            .get(input_id)
            .is_some_and(|session| session.pipeline_id == pipeline_id)
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

    /// Opens the publish target of one registered ingest attempt (see
    /// `PublishTarget::open`); fails once the attempt is no longer its
    /// input's current session.
    pub async fn open_publish_target(
        &self,
        pipeline_id: String,
        registration: IngestRegistration,
        ring: Arc<RingBuffer>,
    ) -> Result<PublishTarget, String> {
        PublishTarget::open(self.ingests.clone(), pipeline_id, registration, ring).await
    }

    /// Replace the wanted `srt_call` inputs the SRT server dials; returns
    /// the inputs whose call is new or changed (`SrtCallInputs::converge`).
    pub fn converge_srt_calls(&self, specs: Vec<restream_srt::calls::SrtCallSpec>) -> Vec<String> {
        self.ingests.srt_calls.converge(specs)
    }

    /// The wanted `srt_call` inputs and their generation, when `seen` is out
    /// of date (`SrtCallInputs::snapshot_since`).
    pub fn srt_calls_since(
        &self,
        seen: u64,
    ) -> Option<(
        u64,
        std::collections::HashMap<String, restream_srt::calls::SrtCallSpec>,
    )> {
        self.ingests.srt_calls.snapshot_since(seen)
    }

    pub async fn record_ingest_disconnect(
        &self,
        pipeline_id: &str,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) {
        self.ingests
            .record_ingest_disconnect(pipeline_id, phase, reason, had_error)
            .await;
    }

    pub(crate) async fn record_ingest_disconnect_if_current(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) -> bool {
        self.ingests
            .record_ingest_disconnect_if_current(
                pipeline_id,
                registration,
                phase,
                reason,
                had_error,
            )
            .await
    }

    /// Releases every ingest session of the pipeline and forgets its input
    /// selection.
    pub async fn unregister_ingest(&self, pipeline_id: &str) {
        self.release_ingest_sessions(pipeline_id, |_| true, true)
            .await;
    }

    /// Releases the pipeline's file-source sessions only: a live publisher on
    /// another input keeps its session, and the selection stays.
    pub async fn unregister_file_ingest(&self, pipeline_id: &str) {
        self.release_ingest_sessions(pipeline_id, |ingest| ingest.protocol == "file", false)
            .await;
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order `selection` (this pipeline's) -> `cancel_tokens` -> `sessions` (module docs of `ingest_registry`); the later maps are taken one at a time"
    )]
    async fn release_ingest_sessions(
        &self,
        pipeline_id: &str,
        release: impl Fn(&ActiveIngest) -> bool,
        forget_selection: bool,
    ) {
        let _selection = self.ingests.selection.lock(pipeline_id).await;
        // A session and its input's token leave together: between two
        // separate sections another pipeline naming the same input id could
        // register, and this release would then cancel that attempt's token.
        {
            let mut tokens = self.ingests.cancel_tokens.write().await;
            let mut sessions = self.ingests.sessions.write().await;
            let input_ids = sessions
                .values()
                .filter(|ingest| ingest.pipeline_id == pipeline_id && release(ingest))
                .map(|ingest| ingest.input_id.clone())
                .collect::<Vec<_>>();
            for input_id in input_ids {
                sessions.remove(&input_id);
                if let Some(token) = tokens.remove(&input_id) {
                    token.cancel();
                }
            }
        }

        let removed_selected = {
            let mut active = self.ingests.active.write().await;
            if active
                .get(pipeline_id)
                .is_some_and(|ingest| release(ingest))
            {
                active.remove(pipeline_id)
            } else {
                None
            }
        };
        if forget_selection {
            self.ingests
                .selected_inputs
                .write()
                .await
                .remove(pipeline_id);
        }
        let protocol = removed_selected
            .as_ref()
            .map(|ingest| ingest.protocol.clone())
            .unwrap_or_default();
        if let Some(ingest) = removed_selected {
            let remote_addr = restream_sync::lock::read(&ingest.metadata)
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

    pub async fn recent_ingest_outcome(&self, pipeline_id: &str) -> Option<RecentIngestOutcome> {
        self.ingests.recent.read().await.get(pipeline_id).cloned()
    }
}

/// Whether an input's token belongs to an attempt that still holds it.
fn live_attempt(token: Option<&CancellationToken>) -> bool {
    token.is_some_and(|token| !token.is_cancelled())
}

/// A gate this registration demoted, with the state it had. Dropped while
/// armed — the registration's future was dropped before the new ingest was
/// inserted — it restores that state, so the input it was replacing is left
/// exactly as it was found.
///
/// Restoring `Active` resumes forwarding at once, so a video packet dropped
/// during the window (milliseconds, so zero or one frame) leaves a gap until
/// the next keyframe. Re-arming for promotion instead would stall the input
/// for up to a GOP on every dropped registration, dropped packet or not; the
/// rarer cost was chosen.
struct DemotedGate(Option<(Arc<crate::input_gate::InputPacketGate>, InputForwardState)>);

impl DemotedGate {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for DemotedGate {
    fn drop(&mut self) {
        if let Some((gate, state)) = self.0.take() {
            match state {
                InputForwardState::Active => gate.activate(),
                InputForwardState::AwaitingKeyframe => gate.arm_for_promotion(),
                InputForwardState::Standby => gate.demote(),
            }
        }
    }
}
