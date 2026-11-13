//! Launches the dashboard and ingest listeners, and supervises them in a
//! task of their own: a faulted RTMP or SRT listener restarts after its
//! backoff, and a dashboard listener that exits ends the process, whatever
//! the reconciler or housekeeping is doing.

use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::api::AppState;
use crate::config::AppConfig;
use restream_engine::engine::MediaEngine;
use restream_engine::ingest_auth::PipelineAccessAuthenticator;
use restream_engine::security::IngestSecurityService;
use restream_engine::srt::SrtIngestPolicyStore;

use super::listener_supervisor::SupervisedListener;

pub(super) struct RuntimeLaunch {
    pub config: Arc<AppConfig>,
    pub state: Arc<AppState>,
    pub engine: Arc<MediaEngine>,
    pub security: Arc<IngestSecurityService>,
    pub pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    pub srt_ingest_policy_store: Arc<SrtIngestPolicyStore>,
    pub hls_ingest: Arc<restream_engine::hls_ingest::HlsIngest>,
    pub hls_catalog: Arc<crate::application::catalog_auth::CatalogAuth>,
}

pub(super) struct RuntimeTasks {
    pub http_stop: HttpStop,
    pub listeners: Listeners,
}

/// The listener tasks: the dashboard's, and the supervised ingest ones.
pub(super) struct Listeners {
    /// `None` once the dashboard listener exited and its handle was awaited.
    http: Option<JoinHandle<()>>,
    rtmp: SupervisedListener,
    srt: SupervisedListener,
}

/// The listener handles shutdown cleanup ends.
pub(super) struct ListenerHandles {
    /// `None` when the dashboard listener already exited.
    pub http: Option<JoinHandle<()>>,
    pub rtmp: JoinHandle<()>,
    pub srt: JoinHandle<()>,
}

/// The running supervision task and the listeners it owns.
pub(super) struct ListenerSupervision {
    listeners: Arc<tokio::sync::Mutex<Listeners>>,
    task: JoinHandle<()>,
}

/// Ends the dashboard listener first at shutdown: live state streams close,
/// then the listener stops accepting and drains in-flight responses.
pub(super) struct HttpStop {
    listener: CancellationToken,
    streams: Arc<crate::application::runtime_state::hub::StateHub>,
}

impl HttpStop {
    pub fn begin(&self) {
        self.streams.close_streams();
        self.listener.cancel();
    }
}

impl RuntimeTasks {
    pub async fn launch(launch: RuntimeLaunch) -> Result<Self, String> {
        let RuntimeLaunch {
            config,
            state,
            engine,
            security,
            pipeline_access,
            srt_ingest_policy_store,
            hls_ingest,
            hls_catalog,
        } = launch;

        let http_addr = format!("{}:{}", config.http_bind_addr, config.ports.http);
        let http_stop = HttpStop {
            listener: CancellationToken::new(),
            streams: state.runtime_state.clone(),
        };
        let app = crate::api::create_router(state).merge(crate::api::create_hls_ingest_router(
            hls_ingest,
            hls_catalog,
        ));
        let listener = tokio::net::TcpListener::bind(&http_addr)
            .await
            .map_err(|error| {
                format!("failed to bind the dashboard listener on {http_addr}: {error}")
            })?;
        info!(
            event_class = "lifecycle",
            event_type = "restream.http.ready",
            addr = %http_addr,
            "dashboard API server listening",
        );
        let http = tokio::spawn(crate::api::http_server::serve(
            listener,
            app,
            crate::api::http_server::ServeOptions::new(
                config.tuning.http_max_connections,
                http_stop.listener.clone(),
            ),
        ));

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
                    restream_engine::rtmp::start_rtmp_server_on_with_shutdown(
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

        restream_engine::moq::ingest::listener::launch(
            &config.engine.moq_ingest,
            pipeline_access.clone(),
            engine.clone(),
            security.clone(),
        )
        .await;

        let srt_restarts = Arc::clone(&engine.runtime.listener_stats.restarts);
        let srt_server = Arc::new(restream_engine::srt::SrtServer::new(
            pipeline_access,
            engine,
            security,
            srt_ingest_policy_store,
        ));
        let srt_port = config.ports.srt;
        let spawn_srt = move || tokio::spawn(Arc::clone(&srt_server).run(srt_port));
        let srt = SupervisedListener::new("srt", spawn_srt(), Box::new(spawn_srt), srt_restarts);

        Ok(Self {
            http_stop,
            listeners: Listeners::new(http, rtmp, srt),
        })
    }
}

impl Listeners {
    pub(super) fn new(
        http: JoinHandle<()>,
        rtmp: SupervisedListener,
        srt: SupervisedListener,
    ) -> Self {
        Self {
            http: Some(http),
            rtmp,
            srt,
        }
    }

    /// Supervises the listeners in a task of their own until `shutdown`;
    /// cancels `shutdown` itself when the dashboard listener exits.
    pub(super) fn supervise(self, shutdown: CancellationToken) -> ListenerSupervision {
        // Shared so a restart after a panic resumes the same listeners; a
        // tokio Mutex is not poisoned by the panicking holder.
        let listeners = Arc::new(tokio::sync::Mutex::new(self));
        let task = super::supervised_task::spawn_supervised("listener_supervisor", {
            let listeners = listeners.clone();
            move || supervise(listeners.clone(), shutdown.clone())
        });
        ListenerSupervision { listeners, task }
    }
}

impl ListenerSupervision {
    /// Waits for the supervision task to end at shutdown and hands back the
    /// listener handles.
    pub(super) async fn stop(self) -> ListenerHandles {
        let _ = self.task.await;
        let mut listeners = self.listeners.lock().await;
        ListenerHandles {
            http: listeners.http.take(),
            rtmp: listeners.rtmp.take_handle(),
            srt: listeners.srt.take_handle(),
        }
    }
}

enum ListenerEvent {
    Shutdown,
    HttpExited(Result<(), tokio::task::JoinError>),
    Changed,
}

#[expect(
    clippy::await_holding_invalid_type,
    reason = "the one supervision task holds the listeners for its lifetime; the mutex exists only so a restart after a panic resumes them"
)]
async fn supervise(listeners: Arc<tokio::sync::Mutex<Listeners>>, shutdown: CancellationToken) {
    let mut listeners = listeners.lock().await;
    let Listeners { http, rtmp, srt } = &mut *listeners;
    loop {
        let now = Instant::now();
        rtmp.restart_if_due(now);
        srt.restart_if_due(now);
        let restart_due = [rtmp.restart_due(), srt.restart_due()]
            .into_iter()
            .flatten()
            .min();
        let event = tokio::select! {
            () = shutdown.cancelled() => ListenerEvent::Shutdown,
            result = async {
                match http.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => ListenerEvent::HttpExited(result),
            () = rtmp.exited() => ListenerEvent::Changed,
            () = srt.exited() => ListenerEvent::Changed,
            () = async {
                match restart_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => ListenerEvent::Changed,
        };
        match event {
            ListenerEvent::Shutdown => return,
            ListenerEvent::HttpExited(result) => {
                *http = None;
                error!(result = ?result, "critical HTTP listener task exited");
                shutdown.cancel();
                return;
            }
            ListenerEvent::Changed => {}
        }
    }
}
