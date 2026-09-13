//! RTMP TCP listener admission and connection limits.

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use restream_dataplane::tcp::{AcceptedTcp, UringTcpAcceptor};
use tokio::net::TcpSocket;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::security::IngestSecurityService;

use super::ingest::handle_rtmp_client;

/// RTMP Ingest Server
pub async fn start_rtmp_server(
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) {
    start_rtmp_server_on(pipeline_access, security, engine, 1935).await;
}

pub async fn start_rtmp_server_on(
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
    port: u16,
) {
    let addr = format!("0.0.0.0:{port}");
    let backlog = engine.config.rtmp_backlog;
    let listener = match bind_rtmp_listener_with_backlog(port, backlog) {
        Ok(l) => l,
        Err(e) => {
            let fd_exhaustion = is_fd_exhaustion_error(&e);
            if fd_exhaustion {
                engine
                    .runtime
                    .rtmp_listener_stats
                    .rtmp_fd_exhaustion_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
            error!(
                event_class = "resource",
                event_type = if fd_exhaustion {
                    "rtmp.listener.fd_exhausted"
                } else {
                    "rtmp.listener.bind_failed"
                },
                addr = %addr,
                error = %e,
                error_kind = ?e.kind(),
                raw_os_error = ?e.raw_os_error(),
                fd_exhaustion,
                "failed to bind RTMP TCP listener",
            );
            return;
        }
    };
    info!("Server listening on {}", addr);
    let (accepted_tx, mut accepted_rx) =
        mpsc::channel::<AcceptedTcp>(engine.config.rtmp_max_connections.clamp(1, 1024));
    spawn_native_acceptor(listener, accepted_tx, engine.clone());
    let connection_permits = Arc::new(Semaphore::new(engine.config.rtmp_max_connections));

    while let Some(accepted) = accepted_rx.recv().await {
        let socket = match tokio::net::TcpStream::from_std(accepted.into_std()) {
            Ok(socket) => socket,
            Err(error) => {
                warn!(%error, "failed to adopt native RTMP connection");
                continue;
            }
        };
        let addr = match socket.peer_addr() {
            Ok(addr) => addr,
            Err(error) => {
                warn!(%error, "failed to read native RTMP peer address");
                continue;
            }
        };
        let permit = match connection_permits.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!("RTMP connection rejected: max connection limit reached");
                continue;
            }
        };
        let pipeline_access_clone = pipeline_access.clone();
        let security_clone = security.clone();
        let engine_clone = engine.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_rtmp_client(
                socket,
                addr,
                pipeline_access_clone,
                security_clone,
                engine_clone,
            )
            .await
            {
                warn!("error handling client {}: {:?}", addr, e);
            }
        });
    }

    warn!("native RTMP acceptor stopped; no new RTMP connections will be accepted");
}

fn spawn_native_acceptor(
    listener: std::net::TcpListener,
    accepted_tx: mpsc::Sender<AcceptedTcp>,
    engine: Arc<MediaEngine>,
) {
    let _ = thread::Builder::new()
        .name("restream-rtmp-ingress".to_string())
        .spawn(move || {
            let mut acceptor = match UringTcpAcceptor::new(listener.as_raw_fd(), 256) {
                Ok(acceptor) => acceptor,
                Err(error) => {
                    error!(%error, "failed to start native RTMP acceptor");
                    return;
                }
            };
            let mut accepted = [None];
            while !accepted_tx.is_closed() {
                match acceptor.accept(&mut accepted) {
                    Ok(1) => {
                        let Some(socket) = accepted[0].take() else {
                            continue;
                        };
                        if accepted_tx.blocking_send(socket).is_err() {
                            break;
                        }
                    }
                    Ok(0) => {}
                    Ok(_) => unreachable!("single-slot accept buffer returned multiple sockets"),
                    Err(error) => {
                        engine
                            .runtime
                            .rtmp_listener_stats
                            .rtmp_accept_errors
                            .fetch_add(1, Ordering::Relaxed);
                        let fd_exhaustion = is_fd_exhaustion_error(&error);
                        if fd_exhaustion {
                            engine
                                .runtime
                                .rtmp_listener_stats
                                .rtmp_fd_exhaustion_errors
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        error!(
                            event_class = "resource",
                            event_type = if fd_exhaustion {
                                "rtmp.listener.fd_exhausted"
                            } else {
                                "rtmp.listener.accept_failed"
                            },
                            error = %error,
                            error_kind = ?error.kind(),
                            raw_os_error = ?error.raw_os_error(),
                            fd_exhaustion,
                            "native RTMP accept failed",
                        );
                        if fd_exhaustion {
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            }
        });
}

fn is_fd_exhaustion_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

fn bind_rtmp_listener_with_backlog(
    port: u16,
    backlog: u32,
) -> Result<std::net::TcpListener, std::io::Error> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    socket.bind(addr)?;
    socket.listen(backlog)?.into_std()
}
