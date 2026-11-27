//! Protocol-specific fabric preparation for one registered output attempt.

use std::sync::{Arc, atomic::AtomicBool};

use restream_domain::domain::output_spec::{OutputUrlScheme, RecirculationTarget};
use restream_domain::models::Output;
use restream_engine::egress::leaf::EgressProgressSink;
use restream_engine::engine_registries::EgressRegistration;
use restream_engine::stage_registry::StageLease;
use tracing::warn;

use super::EgressStarter;
use super::egress_task::{
    HlsPutFabricTask, PipelineFabricTask, PipelineStart, RtmpFabricTask, SinkFabricTask,
    SrtFabricTask,
};
use crate::application::egress::PreparedOutput;

impl EgressStarter {
    pub(super) async fn prepare_rtmp_fabric(
        &self,
        scheme: OutputUrlScheme,
        output: &Output,
        prepared: &PreparedOutput,
        registration: &EgressRegistration,
    ) -> Option<RtmpFabricTask> {
        if !matches!(scheme, OutputUrlScheme::Rtmp | OutputUrlScheme::Rtmps) {
            return None;
        }
        match crate::application::egress_rtmp_fabric::prepare_rtmp_fabric_startup(
            &self.engine,
            output,
            prepared,
        )
        .await
        {
            Ok(startup) => {
                let feed =
                    crate::application::egress_rtmp_fabric::prepare_rtmp_fabric_feed(prepared);
                let mut spec = crate::application::egress_rtmp_fabric::rtmp_fabric_output_spec(
                    output,
                    registration.attempt_id,
                    feed.feed_id.clone(),
                );
                let terminated = Arc::new(AtomicBool::new(false));
                if let Some(sink) = self
                    .engine
                    .with_active_egress(&output.id, |egress| EgressProgressSink {
                        bytes_sent: Some(egress.bytes_sent.clone()),
                        metrics: Some(egress.metrics.clone()),
                        last_progress_ms: Some(egress.last_progress_ms.clone()),
                        resync_count: Some(egress.resync_count.clone()),
                        feed_lag_units: Some(egress.feed_lag_units.clone()),
                        backpressure_reason: Some(egress.backpressure_reason.clone()),
                        quality: Some(egress.quality.clone()),
                        terminated_unexpectedly: Some(terminated.clone()),
                        upload_status: None,
                    })
                    .await
                {
                    spec.progress = sink;
                }
                Some(RtmpFabricTask {
                    spec,
                    feed_id: feed.feed_id,
                    feed: feed.feed,
                    startup: startup.into(),
                    terminated,
                })
            }
            Err(error) => {
                self.engine
                    .record_egress_error_if_current(
                        &output.id,
                        registration,
                        "rtmp_fabric_startup",
                        error,
                    )
                    .await;
                None
            }
        }
    }

    pub(super) async fn prepare_srt_fabric(
        &self,
        output: &Output,
        prepared: &PreparedOutput,
        registration: &EgressRegistration,
        stages: &mut Vec<StageLease>,
    ) -> SrtFabricTask {
        let feed = crate::application::egress::prepare_srt_fabric_feed(
            &self.engine,
            output,
            prepared,
            registration.attempt_id,
        )
        .await;
        stages.push(feed.muxer_lease);
        let mut spec = crate::application::egress::srt_fabric_output_spec(
            output,
            registration.attempt_id,
            feed.feed_id.clone(),
            std::time::Duration::from_millis(self.engine.config.srt_connect_timeout_ms),
        );
        let terminated = Arc::new(AtomicBool::new(false));
        if let Some(sink) = self
            .engine
            .with_active_egress(&output.id, |egress| EgressProgressSink {
                bytes_sent: Some(egress.bytes_sent.clone()),
                metrics: Some(egress.metrics.clone()),
                last_progress_ms: Some(egress.last_progress_ms.clone()),
                resync_count: Some(egress.resync_count.clone()),
                feed_lag_units: Some(egress.feed_lag_units.clone()),
                backpressure_reason: Some(egress.backpressure_reason.clone()),
                quality: Some(egress.quality.clone()),
                terminated_unexpectedly: Some(terminated.clone()),
                upload_status: None,
            })
            .await
        {
            spec.progress = sink;
        }
        SrtFabricTask {
            spec,
            feed_id: feed.feed_id,
            feed: feed.feed,
            source: feed.source,
            terminated,
        }
    }

    pub(super) async fn prepare_sink_fabric(
        &self,
        output: &Output,
        prepared: &PreparedOutput,
        registration: &EgressRegistration,
    ) -> SinkFabricTask {
        let feed = crate::application::egress::prepare_sink_fabric_feed(output, prepared);
        let mut spec = crate::application::egress::sink_fabric_output_spec(
            output,
            registration.attempt_id,
            feed.feed_id.clone(),
        );
        let terminated = Arc::new(AtomicBool::new(false));
        if let Some(sink) = self
            .engine
            .with_active_egress(&output.id, |egress| EgressProgressSink {
                bytes_sent: Some(egress.bytes_sent.clone()),
                metrics: Some(egress.metrics.clone()),
                last_progress_ms: Some(egress.last_progress_ms.clone()),
                resync_count: Some(egress.resync_count.clone()),
                feed_lag_units: Some(egress.feed_lag_units.clone()),
                backpressure_reason: Some(egress.backpressure_reason.clone()),
                terminated_unexpectedly: Some(terminated.clone()),
                ..Default::default()
            })
            .await
        {
            spec.progress = sink;
        }
        SinkFabricTask {
            spec,
            feed_id: feed.feed_id,
            feed: feed.feed,
            terminated,
        }
    }

    pub(super) async fn prepare_pipeline_fabric(
        &self,
        scheme: OutputUrlScheme,
        output: &Output,
        prepared: &PreparedOutput,
        registration: &EgressRegistration,
    ) -> Option<PipelineStart> {
        if !matches!(scheme, OutputUrlScheme::Pipeline) {
            return None;
        }
        let target = match RecirculationTarget::parse(&output.url) {
            Ok(target) => target,
            Err(error) => return Some(PipelineStart::Invalid(error.message())),
        };
        let feed = crate::application::egress::prepare_recirculation_fabric_feed(output, prepared);
        let mut spec = crate::application::egress::recirculation_fabric_output_spec(
            output,
            registration.attempt_id,
            feed.feed_id.clone(),
            &target,
        );
        let terminated = Arc::new(AtomicBool::new(false));
        if let Some(sink) = self
            .engine
            .with_active_egress(&output.id, |egress| EgressProgressSink {
                bytes_sent: Some(egress.bytes_sent.clone()),
                metrics: Some(egress.metrics.clone()),
                last_progress_ms: Some(egress.last_progress_ms.clone()),
                resync_count: Some(egress.resync_count.clone()),
                feed_lag_units: Some(egress.feed_lag_units.clone()),
                backpressure_reason: Some(egress.backpressure_reason.clone()),
                terminated_unexpectedly: Some(terminated.clone()),
                ..Default::default()
            })
            .await
        {
            spec.progress = sink;
        }
        Some(PipelineStart::Ready(Box::new(PipelineFabricTask {
            spec,
            feed_id: feed.feed_id,
            feed: feed.feed,
            target_pipeline_id: target.pipeline_id().to_string(),
            target_input_id: target.input_id().to_string(),
            terminated,
        })))
    }

    pub(super) async fn prepare_hls_put_fabric(
        &self,
        scheme: OutputUrlScheme,
        output: &Output,
        registration: &EgressRegistration,
    ) -> Option<HlsPutFabricTask> {
        // HTTPS on the fabric needs kernel TLS (no userspace fallback);
        // without it HTTPS outputs stay on the Reqwest uploader.
        let configured = self.engine.config.egress_fabric.hls_put_fabric;
        let ktls_available = if configured && matches!(scheme, OutputUrlScheme::Https) {
            // The first call probes the kernel (sockets, setsockopt): keep
            // it off the Tokio worker. Later calls hit the cache.
            let available =
                tokio::task::spawn_blocking(restream_engine::egress::tls::ktls::egress_available)
                    .await
                    .unwrap_or(false);
            if !available {
                static LOGGED: std::sync::Once = std::sync::Once::new();
                LOGGED.call_once(|| {
                    warn!("kernel TLS unavailable; HTTPS HLS outputs use the Reqwest uploader");
                });
            }
            available
        } else {
            false
        };
        if !hls_put_uses_fabric(configured, scheme, ktls_available) {
            return None;
        }
        let terminated = Arc::new(AtomicBool::new(false));
        let upload_status = Arc::new(std::sync::Mutex::new(
            restream_engine::egress::leaf::UploadStatus::default(),
        ));
        let mut spec = restream_engine::egress::OutputSpec {
            id: restream_engine::egress::OutputId::new(output.id.clone()),
            generation: registration.attempt_id,
            feed: restream_engine::engine_hls_egress_fabric::hls_put_feed_id(&output.pipeline_id),
            protocol: restream_engine::egress::ProtocolSpec::HlsPut {
                url: output.url.clone(),
            },
            policy: restream_engine::egress::LeafPolicy::default(),
            progress: Default::default(),
        };
        if let Some(sink) = self
            .engine
            .with_active_egress(&output.id, |egress| EgressProgressSink {
                bytes_sent: Some(egress.bytes_sent.clone()),
                metrics: Some(egress.metrics.clone()),
                last_progress_ms: Some(egress.last_progress_ms.clone()),
                terminated_unexpectedly: Some(terminated.clone()),
                upload_status: Some(upload_status.clone()),
                ..Default::default()
            })
            .await
        {
            spec.progress = sink;
        }
        Some(HlsPutFabricTask {
            spec,
            terminated,
            upload_status,
        })
    }
}

/// Which HLS PUT transport serves one attempt. HTTP always runs on the
/// fabric; HTTPS needs kernel TLS, and no-kTLS HTTPS stays on Reqwest.
fn hls_put_uses_fabric(configured: bool, scheme: OutputUrlScheme, ktls_available: bool) -> bool {
    configured
        && match scheme {
            OutputUrlScheme::Http => true,
            OutputUrlScheme::Https => ktls_available,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_put_transport_selection_covers_http_https_and_ktls() {
        assert!(hls_put_uses_fabric(true, OutputUrlScheme::Http, false));
        assert!(hls_put_uses_fabric(true, OutputUrlScheme::Https, true));
        assert!(
            !hls_put_uses_fabric(true, OutputUrlScheme::Https, false),
            "HTTPS without kernel TLS must stay on the Reqwest uploader"
        );
        assert!(!hls_put_uses_fabric(false, OutputUrlScheme::Http, true));
        assert!(!hls_put_uses_fabric(false, OutputUrlScheme::Https, true));
        assert!(!hls_put_uses_fabric(true, OutputUrlScheme::Hls, true));
        assert!(!hls_put_uses_fabric(true, OutputUrlScheme::Moqt, true));
    }
}
