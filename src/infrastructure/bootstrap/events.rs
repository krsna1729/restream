use tracing::{error, info, warn};

pub(super) fn persist_runtime_event(event: crate::events::Event) {
    use crate::events::EventKind;

    let seq = event.seq;
    match event.kind {
        EventKind::IngestConnected {
            pipeline_id,
            protocol,
            ..
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "ingest.connected",
            protocol = %protocol,
            seq,
            "publisher connected",
        ),
        EventKind::IngestDisconnected {
            pipeline_id,
            protocol,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "ingest.disconnected",
            protocol = %protocol,
            seq,
            "publisher disconnected",
        ),
        EventKind::StageRegistered {
            pipeline_id,
            encoding,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.registered",
            encoding = %encoding,
            seq,
            "stage registered",
        ),
        EventKind::StageWaitingForCapacity {
            pipeline_id,
            encoding,
            backend,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.waitingForCapacity",
            encoding = %encoding,
            backend = %backend,
            seq,
            "stage waiting for capacity",
        ),
        EventKind::StageBackendSpawned {
            pipeline_id,
            encoding,
            backend,
            pid,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.backendSpawned",
            encoding = %encoding,
            backend = %backend,
            pid = ?pid,
            seq,
            "stage backend spawned",
        ),
        EventKind::StageFirstInput {
            pipeline_id,
            encoding,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.firstInput",
            encoding = %encoding,
            seq,
            "stage first input",
        ),
        EventKind::StageFirstOutput {
            pipeline_id,
            encoding,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.firstOutput",
            encoding = %encoding,
            seq,
            "stage first output",
        ),
        EventKind::StageFailed {
            pipeline_id,
            encoding,
            error,
        } => error!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.failed",
            encoding = %encoding,
            error = %error,
            seq,
            "stage failed",
        ),
        EventKind::StageStopped {
            pipeline_id,
            encoding,
        } => info!(
            pipeline_id = %pipeline_id,
            event_class = "lifecycle",
            event_type = "stage.stopped",
            encoding = %encoding,
            seq,
            "stage stopped",
        ),
        EventKind::EgressStarted {
            pipeline_id,
            output_id,
        } => info!(
            pipeline_id = %pipeline_id,
            output_id = %output_id,
            event_class = "lifecycle",
            event_type = "egress.started",
            seq,
            "output started",
        ),
        EventKind::EgressStopped {
            pipeline_id,
            output_id,
        } => info!(
            pipeline_id = %pipeline_id,
            output_id = %output_id,
            event_class = "lifecycle",
            event_type = "egress.stopped",
            seq,
            "output stopped",
        ),
        EventKind::EgressFailed {
            pipeline_id,
            output_id,
            phase,
            error: error_message,
        } => warn!(
            pipeline_id = %pipeline_id,
            output_id = %output_id,
            event_class = "lifecycle",
            event_type = "egress.failed",
            phase = %phase,
            error = %error_message,
            seq,
            "output failed",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::Layer;
    use tracing_subscriber::prelude::*;

    #[derive(Clone, Default)]
    struct CapturingLayer {
        levels: Arc<Mutex<Vec<Level>>>,
    }

    impl<S> Layer<S> for CapturingLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.levels
                .lock()
                .expect("captured levels lock poisoned")
                .push(*event.metadata().level());
        }
    }

    #[test]
    fn egress_failed_lifecycle_event_logs_at_warn() {
        let layer = CapturingLayer::default();
        let levels = layer.levels.clone();
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            persist_runtime_event(crate::events::Event {
                seq: 1,
                timestamp: chrono::Utc::now(),
                kind: crate::events::EventKind::EgressFailed {
                    pipeline_id: "pipe".to_string(),
                    output_id: "out".to_string(),
                    phase: "send".to_string(),
                    error: "remote closed connection".to_string(),
                },
            });
        });

        assert_eq!(
            levels
                .lock()
                .expect("captured levels lock poisoned")
                .as_slice(),
            &[Level::WARN]
        );
    }
}
