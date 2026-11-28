//! Engine entry points for ingest sessions and publish targets.

use crate::engine::MediaEngine;
use crate::ingest_lease::{IngestLease, RegisteredIngest};
use crate::ingest_publisher::PublishTarget;
use crate::ingest_registry::{IngestRegistration, RecentIngestOutcome};
use restream_sync::RingBuffer;
use std::sync::Arc;

impl MediaEngine {
    /// Registers the pipeline's only publisher, unless one is active.
    pub async fn try_register_ingest(
        &self,
        pipeline_id: &str,
        stream_key: &str,
        protocol: &str,
    ) -> Option<RegisteredIngest> {
        self.ingests
            .try_register_ingest(pipeline_id, stream_key, protocol)
            .await
            .map(|registration| self.admitted_ingest(pipeline_id, registration))
    }

    /// Register an input attempt and hand off its release owner and media state.
    pub async fn try_register_pipeline_input_attempt(
        &self,
        pipeline_id: &str,
        input_id: &str,
        stream_key: &str,
        protocol: &str,
        selected: bool,
    ) -> Option<RegisteredIngest> {
        self.ingests
            .try_register_pipeline_input_attempt(
                pipeline_id,
                input_id,
                stream_key,
                protocol,
                selected,
            )
            .await
            .map(|registration| self.admitted_ingest(pipeline_id, registration))
    }

    fn admitted_ingest(
        &self,
        pipeline_id: &str,
        registration: IngestRegistration,
    ) -> RegisteredIngest {
        let lease = IngestLease::new(&self.ingests, pipeline_id, &registration);
        RegisteredIngest {
            lease,
            registration,
        }
    }

    pub async fn select_pipeline_input(&self, pipeline_id: &str, input_id: &str) -> bool {
        self.ingests
            .select_pipeline_input(pipeline_id, input_id)
            .await
    }

    /// Whether `input_id` has a live publisher session on `pipeline_id`.
    pub async fn input_has_session(&self, pipeline_id: &str, input_id: &str) -> bool {
        self.ingests.input_has_session(pipeline_id, input_id).await
    }

    pub async fn connected_input_count(&self, pipeline_id: &str) -> usize {
        self.ingests.connected_input_count(pipeline_id).await
    }

    pub async fn cancel_pipeline_input(&self, input_id: &str) -> bool {
        self.ingests.cancel_pipeline_input(input_id).await
    }

    pub async fn recent_ingest_outcome(&self, pipeline_id: &str) -> Option<RecentIngestOutcome> {
        self.ingests.recent_ingest_outcome(pipeline_id).await
    }

    pub(crate) async fn has_ingest_outcome(&self, pipeline_id: &str) -> bool {
        self.ingests.has_ingest_outcome(pipeline_id).await
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
    pub(crate) fn srt_calls_since(
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
                &registration.input_id,
                registration.attempt_id,
                phase,
                reason,
                had_error,
            )
            .await
    }

    /// Releases every ingest session of the pipeline and forgets its input
    /// selection.
    pub async fn unregister_ingest(&self, pipeline_id: &str) {
        self.ingests.unregister_ingest(pipeline_id).await;
    }

    /// Releases the pipeline's file-source sessions only: a live publisher on
    /// another input keeps its session, and the selection stays.
    pub async fn unregister_file_ingest(&self, pipeline_id: &str) {
        self.ingests.unregister_file_ingest(pipeline_id).await;
    }
}
