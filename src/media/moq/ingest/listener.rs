//! The MoQ listener: one OS thread "moq-ingest" with its own Compio runtime
//! owns the UDP endpoint (native QUIC and HTTP/3 WebTransport on one port,
//! moq-lite or IETF MOQT), every connection and its media. Tokio only admits,
//! probes and records ([`super::TokioLifecycle`]).
//!
//! Each accepted connection runs as a local task: transport handshake, MoQ
//! SETUP (`Server::accept_request`, any version), then routing. A connection
//! that presents an origin token or declares itself a subscriber (moq-lite
//! only has the role) is served by the origin
//! ([`crate::media::moq::origin`]); any other is a publisher
//! ([`super::session::publish`]). At most `max_connections` handshakes and
//! publishers run at once; further handshakes are refused. An admitted
//! viewer leaves that count (the origin's viewer cap bounds viewers), so an
//! audience never locks publishers out. Shutdown closes the endpoint, gives the
//! sessions a moment to close cleanly and ends every remaining one with its
//! input released.

use std::cell::Cell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use compio::quic::{Endpoint, Incoming, VarInt};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use super::session::{SessionLimits, publish};
use super::{MoqIngestServer, TokioLifecycle};
use crate::config::{MoqIngestConfig, MoqIngestSetting};
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::moq::origin::{Origin, request_token};
use crate::media::moq::transport;
use crate::media::security::IngestSecurityService;
use restream_moq::tls;

/// How long shutdown waits for sessions to close before dropping them.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(1);

struct Listener {
    lifecycle: TokioLifecycle,
    origin: Rc<Origin>,
    limits: SessionLimits,
    shutdown: CancellationToken,
    /// Every connection task, for the shutdown drain.
    active: Cell<usize>,
    /// Handshakes in progress and publishers, under `max_connections`.
    capped: Cell<usize>,
}

/// Counts one connection task for the shutdown drain.
struct Active(Rc<Listener>);

impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.set(self.0.active.get().saturating_sub(1));
    }
}

/// Holds one `max_connections` slot: a handshake, then a publisher. An
/// admitted viewer drops it.
struct CapSlot(Rc<Listener>);

impl Drop for CapSlot {
    fn drop(&mut self) {
        self.0.capped.set(self.0.capped.get().saturating_sub(1));
    }
}

/// Start the listener the setting asks for, logging the outcome. Nothing
/// runs when it is off; an invalid setting or a failed start is logged and
/// leaves MoQ off without affecting the other listeners.
pub(crate) async fn launch(
    setting: &MoqIngestSetting,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    engine: Arc<MediaEngine>,
    security: Arc<IngestSecurityService>,
) {
    let config = match setting {
        MoqIngestSetting::Off => return,
        MoqIngestSetting::Invalid(reason) => {
            error!(
                event_class = "lifecycle",
                event_type = "restream.moq.ingest.config_invalid",
                reason = %reason,
                "MoQ listener not started: invalid configuration",
            );
            return;
        }
        MoqIngestSetting::On(config) => config,
    };
    let server = Arc::new(MoqIngestServer::new(pipeline_access, engine, security));
    match start(config, server).await {
        Ok(local) => info!(
            event_class = "lifecycle",
            event_type = "restream.moq.ingest.ready",
            addr = %local,
            max_connections = config.max_connections,
            "MoQ listener ready (QUIC and WebTransport)",
        ),
        Err(reason) => error!(
            event_class = "lifecycle",
            event_type = "restream.moq.ingest.start_failed",
            addr = %config.listen,
            reason = %reason,
            "MoQ listener failed to start",
        ),
    }
}

/// Start the listener thread and wait until its endpoint is bound. The
/// thread is joined and stopped with the engine's other listeners. An
/// error leaves nothing running.
async fn start(
    config: &MoqIngestConfig,
    server: Arc<MoqIngestServer>,
) -> Result<SocketAddr, String> {
    let engine = server.engine().clone();
    let tokio = tokio::runtime::Handle::current();
    let shutdown = CancellationToken::new();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("moq-ingest".to_string())
        .spawn({
            let config = config.clone();
            let shutdown = shutdown.clone();
            move || run_thread(&config, server, tokio, shutdown, ready_tx)
        })
        .map_err(|error| format!("spawn the MoQ listener thread: {error}"))?;
    let local = ready_rx
        .await
        .unwrap_or_else(|_| Err("the MoQ listener thread exited before binding".to_string()))?;
    engine.register_os_thread(thread);
    engine.register_listener_shutdown(move || shutdown.cancel());
    Ok(local)
}

fn run_thread(
    config: &MoqIngestConfig,
    server: Arc<MoqIngestServer>,
    tokio: tokio::runtime::Handle,
    shutdown: CancellationToken,
    ready: tokio::sync::oneshot::Sender<Result<SocketAddr, String>>,
) {
    let runtime = match compio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!("MoQ listener runtime: {error}")));
            return;
        }
    };
    runtime.block_on(async move {
        let endpoint = match bind(config).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let local = match endpoint.local_addr() {
            Ok(local) => local,
            Err(error) => {
                let _ = ready.send(Err(format!("MoQ listener address: {error}")));
                return;
            }
        };
        let engine = server.engine().clone();
        let listener = Rc::new(Listener {
            lifecycle: TokioLifecycle::new(tokio.clone(), server),
            origin: Origin::new(engine, config.origin_max_viewers),
            limits: SessionLimits::default(),
            shutdown,
            active: Cell::new(0),
            capped: Cell::new(0),
        });
        if ready.send(Ok(local)).is_err() {
            return;
        }
        accept_loop(&endpoint, &listener, config.max_connections).await;
        endpoint.close(VarInt::from_u32(0), b"Restream is shutting down");
        let deadline = Instant::now() + SHUTDOWN_DRAIN;
        while listener.active.get() > 0 && Instant::now() < deadline {
            compio::time::sleep(Duration::from_millis(10)).await;
        }
        info!(
            event_class = "lifecycle",
            event_type = "restream.moq.ingest.stopped",
            remaining = listener.active.get(),
            "MoQ listener stopped",
        );
    });
    // Dropping the runtime drops any session still running; each releases
    // its input on drop.
}

/// The TLS configuration from the PEM files, and the endpoint. Reading the
/// files blocks this thread only, before it serves anything.
async fn bind(config: &MoqIngestConfig) -> Result<Endpoint, String> {
    let cert = std::fs::read(&config.tls_cert_path)
        .map_err(|error| format!("read {}: {error}", config.tls_cert_path))?;
    let key = std::fs::read(&config.tls_key_path)
        .map_err(|error| format!("read {}: {error}", config.tls_key_path))?;
    let tls = tls::server_config(&transport::SERVER_ALPNS, &cert, &key)?;
    transport::server_endpoint(config.listen, tls)
        .await
        .map_err(|error| format!("bind MoQ listener on {}: {error}", config.listen))
}

async fn accept_loop(endpoint: &Endpoint, listener: &Rc<Listener>, max_connections: usize) {
    loop {
        let incoming = tokio::select! {
            biased;
            () = listener.shutdown.cancelled() => return,
            incoming = endpoint.wait_incoming() => incoming,
        };
        let Some(incoming) = incoming else {
            return;
        };
        if listener.capped.get() >= max_connections {
            debug!(
                peer = %incoming.remote_address(),
                max_connections,
                "MoQ connection refused: at the connection cap",
            );
            incoming.refuse();
            continue;
        }
        listener.active.set(listener.active.get() + 1);
        listener.capped.set(listener.capped.get() + 1);
        let active = Active(listener.clone());
        let slot = CapSlot(listener.clone());
        compio::runtime::spawn(async move {
            serve_connection(incoming, &active.0, slot).await;
            drop(active);
        })
        .detach();
    }
}

/// One connection: handshake, SETUP, then origin or publisher.
async fn serve_connection(incoming: Incoming, listener: &Rc<Listener>, slot: CapSlot) {
    let peer = incoming.remote_address();
    let accepted = match transport::accept(incoming).await {
        Ok(accepted) => accepted,
        Err(error) => {
            debug!(%peer, %error, "MoQ handshake failed");
            return;
        }
    };
    // A peer that never sends its SETUP must not hold a connection slot.
    let setup = compio::time::timeout(
        transport::HANDSHAKE_TIMEOUT,
        moq_net::Server::new().accept_request(Instant::now(), accepted.session.clone()),
    )
    .await;
    let handshake = match setup {
        Ok(Ok(handshake)) => handshake,
        Ok(Err(error)) => {
            debug!(%peer, %error, "MoQ SETUP failed");
            return;
        }
        Err(_) => {
            debug!(%peer, "MoQ SETUP timed out");
            return;
        }
    };
    let token = request_token(&accepted, handshake.path());
    if token.is_some() || handshake.role() == Some(moq_net::Role::Subscriber) {
        match listener.origin.admit(&accepted, token) {
            Ok(grant) => {
                // Viewers are bounded by the origin's viewer cap, not this one.
                drop(slot);
                listener.origin.clone().serve(grant, handshake).await;
            }
            Err(reject) => {
                info!(
                    event_class = "media",
                    event_type = "restream.moq.origin.refused",
                    %peer,
                    reason = %reject,
                    "MoQ subscriber refused",
                );
                handshake.close(reject.error());
            }
        }
        return;
    }
    let path = if accepted.path.is_empty() {
        handshake.path().to_string()
    } else {
        accepted.path.clone()
    };
    let end = publish(
        &listener.lifecycle,
        peer,
        &path,
        handshake,
        listener.limits,
        &listener.shutdown,
    )
    .await;
    info!(
        event_class = "media",
        event_type = "restream.moq.ingest.session_ended",
        %peer,
        reason = %end.reason,
        "MoQ publisher session ended",
    );
}

#[cfg(test)]
#[path = "listener_tests.rs"]
mod tests;
