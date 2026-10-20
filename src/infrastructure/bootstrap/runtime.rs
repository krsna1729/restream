use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::api::AppState;
use crate::config::AppConfig;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::security::IngestSecurityService;
use crate::media::srt::SrtIngestPolicyStore;

use super::listener_supervisor::SupervisedListener;

pub(super) struct RuntimeLaunch {
    pub config: Arc<AppConfig>,
    pub state: Arc<AppState>,
    pub engine: Arc<MediaEngine>,
    pub security: Arc<IngestSecurityService>,
    pub pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    pub srt_ingest_policy_store: Arc<SrtIngestPolicyStore>,
    pub hls_ingest: Arc<crate::media::hls_ingest::HlsIngest>,
    pub hls_inputs: Arc<dyn crate::application::ingest::PipelineInputLookup>,
}

pub(super) struct RuntimeTasks {
    http: JoinHandle<()>,
    rtmp: SupervisedListener,
    srt: SupervisedListener,
}

impl RuntimeTasks {
    pub async fn launch(launch: RuntimeLaunch) -> Self {
        let RuntimeLaunch {
            config,
            state,
            engine,
            security,
            pipeline_access,
            srt_ingest_policy_store,
            hls_ingest,
            hls_inputs,
        } = launch;

        let http_addr = format!("{}:{}", config.http_bind_addr, config.ports.http);
        let app = crate::api::create_router(state)
            .merge(crate::api::create_hls_ingest_router(hls_ingest, hls_inputs));
        let listener = tokio::net::TcpListener::bind(&http_addr)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "Failed to bind TCP listener on port {}: {}",
                    config.ports.http, error
                )
            });
        info!(
            event_class = "lifecycle",
            event_type = "restream.http.ready",
            addr = %http_addr,
            "dashboard API server listening",
        );
        let http = tokio::spawn(crate::api::http_server::serve(listener, app));

        let rtmp_port = config.ports.rtmp;
        let rtmp_restarts = Arc::clone(&engine.runtime.rtmp_listener_stats.restarts);
        let spawn_rtmp = {
            let engine = engine.clone();
            let security = security.clone();
            let pipeline_access = pipeline_access.clone();
            move |started: Option<tokio::sync::oneshot::Sender<_>>| {
                let engine = engine.clone();
                let security = security.clone();
                let pipeline_access = pipeline_access.clone();
                tokio::spawn(async move {
                    crate::media::rtmp::start_rtmp_server_on_with_shutdown(
                        pipeline_access,
                        security,
                        engine,
                        rtmp_port,
                        CancellationToken::new(),
                        started,
                    )
                    .await;
                })
            }
        };
        let (rtmp_started_tx, rtmp_started_rx) = tokio::sync::oneshot::channel();
        let rtmp_task = spawn_rtmp(Some(rtmp_started_tx));
        let _ = rtmp_started_rx.await;
        let rtmp = SupervisedListener::new(
            "rtmp",
            rtmp_task,
            Box::new(move || spawn_rtmp(None)),
            rtmp_restarts,
        );

        crate::media::moq::ingest::listener::launch(
            &config.moq_ingest,
            pipeline_access.clone(),
            engine.clone(),
            security.clone(),
        )
        .await;

        let srt_restarts = Arc::clone(&engine.runtime.listener_stats.restarts);
        let srt_server = Arc::new(crate::media::srt::SrtServer::new(
            pipeline_access,
            engine,
            security,
            srt_ingest_policy_store,
        ));
        let srt_port = config.ports.srt;
        let spawn_srt = move || tokio::spawn(Arc::clone(&srt_server).run(srt_port));
        let srt = SupervisedListener::new("srt", spawn_srt(), Box::new(spawn_srt), srt_restarts);

        Self { http, rtmp, srt }
    }

    pub async fn wait_for_reconcile_tick(
        &mut self,
        shutdown: &CancellationToken,
        wait: Duration,
        requested: impl std::future::Future<Output = ()>,
    ) -> bool {
        let now = tokio::time::Instant::now();
        self.rtmp.restart_if_due(now);
        self.srt.restart_if_due(now);
        // Wake for a due restart even when the requested wait is longer.
        let wake = [self.rtmp.restart_due(), self.srt.restart_due()]
            .into_iter()
            .flatten()
            .fold(now + wait, std::cmp::min);
        tokio::select! {
            _ = shutdown.cancelled() => false,
            result = &mut self.http => {
                error!(result = ?result, "critical HTTP listener task exited");
                shutdown.cancel();
                false
            }
            () = self.rtmp.exited() => true,
            () = self.srt.exited() => true,
            _ = tokio::time::sleep_until(wake) => true,
            () = requested => true,
        }
    }

    pub fn into_handles(self) -> (JoinHandle<()>, JoinHandle<()>, JoinHandle<()>) {
        (self.http, self.rtmp.into_handle(), self.srt.into_handle())
    }
}
