//! RTMP TCP listener admission and connection limits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::security::IngestSecurityService;

use super::handshake::perform_server_handshake;
use super::ingest::handle_rtmp_client;
use super::sink_session::run_sink_rtmp_session;

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
    let connection_permits = Arc::new(Semaphore::new(engine.config.rtmp_max_connections));

    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                let permit = match connection_permits.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!("RTMP connection rejected: max connection limit reached");
                        drop(socket);
                        continue;
                    }
                };
                if engine.config.sink_mode {
                    tokio::spawn(async move {
                        drop(permit);
                        let mut buf = [0u8; 4096];
                        let mut stream = socket;
                        // A real RTMP client (restream's own egress fabric
                        // included) blocks on the handshake, then blocks
                        // again waiting for the server to accept its
                        // connect/publish requests before it ever sends
                        // media — a listener that only discards bytes
                        // never gets that far, leaving every real RTMP
                        // egress connection to this sink hung forever.
                        // Reuses the exact server handshake real ingest
                        // performs, then drives a minimal ServerSession
                        // (sink_session.rs) that accepts connect/publish
                        // and discards every media message unread.
                        match perform_server_handshake(&mut stream, &mut buf).await {
                            Ok(leading) => {
                                info!("[rtmp] SINK_MODE: discarding data from {}", addr);
                                run_sink_rtmp_session(&mut stream, &mut buf, &leading).await;
                            }
                            Err(error) => {
                                warn!("[rtmp] SINK_MODE: handshake failed for {}: {}", addr, error);
                            }
                        }
                    });
                } else {
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
            }
            Err(e) => {
                engine
                    .runtime
                    .rtmp_listener_stats
                    .rtmp_accept_errors
                    .fetch_add(1, Ordering::Relaxed);
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
                        "rtmp.listener.accept_failed"
                    },
                    error = %e,
                    error_kind = ?e.kind(),
                    raw_os_error = ?e.raw_os_error(),
                    fd_exhaustion,
                    "RTMP listener accept failed",
                );
                if fd_exhaustion {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

fn is_fd_exhaustion_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

fn bind_rtmp_listener_with_backlog(port: u16, backlog: u32) -> Result<TcpListener, std::io::Error> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    socket.bind(addr)?;
    socket.listen(backlog)
}
