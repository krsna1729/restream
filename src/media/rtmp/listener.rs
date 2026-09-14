//! RTMP TCP listener admission and connection limits.

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use restream_dataplane::tcp::{AcceptedTcp, UringTcpAcceptor};
use tokio::net::TcpSocket;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::security::IngestSecurityService;

use super::ingest::handle_rtmp_client;

const MAX_NATIVE_RTMP_WORKERS: usize = 8;

type RtmpWorkerItem = (AcceptedTcp, tokio::sync::OwnedSemaphorePermit);

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
    let worker_count = native_rtmp_worker_count();
    let worker_capacity = worker_channel_capacity(engine.config.rtmp_max_connections, worker_count);
    let workers = match spawn_native_rtmp_workers(
        worker_count,
        worker_capacity,
        pipeline_access.clone(),
        security.clone(),
        engine.clone(),
    ) {
        Ok(workers) => workers,
        Err(error) => {
            error!(%error, "failed to start native RTMP ingress workers");
            return;
        }
    };
    let mut next_worker = 0;

    while let Some(accepted) = accepted_rx.recv().await {
        let permit = match connection_permits.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!("RTMP connection rejected: max connection limit reached");
                continue;
            }
        };

        let mut item = Some((accepted, permit));
        for _ in 0..workers.len() {
            let worker = &workers[next_worker];
            next_worker = (next_worker + 1) % workers.len();
            let Some(candidate) = item.take() else {
                break;
            };
            match worker.try_send(candidate) {
                Ok(()) => break,
                Err(mpsc::error::TrySendError::Full(candidate))
                | Err(mpsc::error::TrySendError::Closed(candidate)) => {
                    item = Some(candidate);
                }
            }
        }
        if item.is_some() {
            warn!("RTMP connection rejected: native ingress workers are saturated");
        }
    }

    warn!("native RTMP acceptor stopped; no new RTMP connections will be accepted");
}

fn native_rtmp_worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |parallelism| parallelism.get())
        .clamp(1, MAX_NATIVE_RTMP_WORKERS)
}

fn worker_channel_capacity(max_connections: usize, workers: usize) -> usize {
    max_connections.max(1).div_ceil(workers.max(1))
}

fn spawn_native_rtmp_workers(
    worker_count: usize,
    worker_capacity: usize,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) -> std::io::Result<Vec<mpsc::Sender<RtmpWorkerItem>>> {
    let mut workers = Vec::with_capacity(worker_count);
    for worker_index in 0..worker_count {
        let (tx, rx) = mpsc::channel(worker_capacity);
        let pipeline_access = pipeline_access.clone();
        let security = security.clone();
        let engine = engine.clone();
        thread::Builder::new()
            .name(format!("restream-rtmp-ingress-{worker_index}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        error!(%error, worker_index, "failed to build native RTMP worker runtime");
                        return;
                    }
                };
                runtime.block_on(run_native_rtmp_worker(
                    rx,
                    pipeline_access,
                    security,
                    engine,
                ));
            })
            .map_err(|error| std::io::Error::other(format!("spawn RTMP worker: {error}")))?;
        workers.push(tx);
    }
    Ok(workers)
}

async fn run_native_rtmp_worker(
    mut accepted_rx: mpsc::Receiver<RtmpWorkerItem>,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) {
    let mut connections = FuturesUnordered::new();
    loop {
        tokio::select! {
            biased;
            Some(()) = connections.next(), if !connections.is_empty() => {}
            item = accepted_rx.recv() => {
                let Some((accepted, permit)) = item else { break };
                let pipeline_access = pipeline_access.clone();
                let security = security.clone();
                let engine = engine.clone();
                connections.push(Box::pin(async move {
                    let _permit = permit;
                    let socket = match tokio::net::TcpStream::from_std(accepted.into_std()) {
                        Ok(socket) => socket,
                        Err(error) => {
                            warn!(%error, "failed to adopt native RTMP connection");
                            return;
                        }
                    };
                    let addr = match socket.peer_addr() {
                        Ok(addr) => addr,
                        Err(error) => {
                            warn!(%error, "failed to read native RTMP peer address");
                            return;
                        }
                    };
                    if let Err(error) = handle_rtmp_client(
                        socket,
                        addr,
                        pipeline_access,
                        security,
                        engine,
                    ).await {
                        warn!(%error, %addr, "error handling RTMP client");
                    }
                }));
            }
        }
    }
    while connections.next().await.is_some() {}
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

#[cfg(test)]
mod tests {
    use super::{MAX_NATIVE_RTMP_WORKERS, native_rtmp_worker_count, worker_channel_capacity};

    #[test]
    fn native_worker_count_is_small_and_nonzero() {
        let count = native_rtmp_worker_count();
        assert!((1..=MAX_NATIVE_RTMP_WORKERS).contains(&count));
    }

    #[test]
    fn worker_channels_cover_the_connection_budget() {
        for max_connections in [0, 1, 7, 512, 16_384] {
            for workers in [0, 1, 4, 8] {
                let capacity = worker_channel_capacity(max_connections, workers);
                assert!(capacity >= 1);
                assert!(capacity.saturating_mul(workers.max(1)) >= max_connections.max(1));
            }
        }
    }
}
