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

pub(super) struct RuntimeLaunch {
    pub config: Arc<AppConfig>,
    pub state: Arc<AppState>,
    pub engine: Arc<MediaEngine>,
    pub security: Arc<IngestSecurityService>,
    pub pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    pub srt_ingest_policy_store: Arc<SrtIngestPolicyStore>,
}

pub(super) struct RuntimeTasks {
    http: JoinHandle<()>,
    rtmp: JoinHandle<()>,
    srt: JoinHandle<()>,
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
        } = launch;

        let http_addr = format!("{}:{}", config.http_bind_addr, config.ports.http);
        let app = crate::api::create_router(state);
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
        let http = tokio::spawn(async move {
            if let Err(error) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            {
                error!(err = ?error, "axum server error");
            }
        });

        let rtmp_port = config.ports.rtmp;
        let rtmp_engine = engine.clone();
        let rtmp_security = security.clone();
        let rtmp_pipeline_access = pipeline_access.clone();
        let rtmp = tokio::spawn(async move {
            crate::media::rtmp::start_rtmp_server_on(
                rtmp_pipeline_access,
                rtmp_security,
                rtmp_engine,
                rtmp_port,
            )
            .await;
            error!("RTMP server task exited unexpectedly");
        });

        let srt_server = Arc::new(crate::media::srt::SrtServer::new(
            pipeline_access,
            engine,
            security,
            srt_ingest_policy_store,
        ));
        let srt_port = config.ports.srt;
        let srt = tokio::spawn(async move {
            srt_server.run(srt_port).await;
            error!("SRT server task exited unexpectedly");
        });

        Self { http, rtmp, srt }
    }

    pub async fn wait_for_reconcile_tick(
        &mut self,
        shutdown: &CancellationToken,
        interval: Duration,
    ) -> bool {
        tokio::select! {
            _ = shutdown.cancelled() => false,
            result = &mut self.http => {
                error!(result = ?result, "critical HTTP listener task exited");
                shutdown.cancel();
                false
            }
            result = &mut self.rtmp => {
                error!(result = ?result, "critical RTMP listener task exited");
                shutdown.cancel();
                false
            }
            result = &mut self.srt => {
                error!(result = ?result, "critical SRT listener task exited");
                shutdown.cancel();
                false
            }
            _ = tokio::time::sleep(interval) => true,
        }
    }

    pub fn into_handles(self) -> (JoinHandle<()>, JoinHandle<()>, JoinHandle<()>) {
        (self.http, self.rtmp, self.srt)
    }
}
