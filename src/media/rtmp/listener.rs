//! RTMP TCP listener admission and connection limits.

use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::io::DuplexStream;
use tokio::net::TcpSocket;
use tokio::sync::Semaphore;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::security::IngestSecurityService;

use super::ingest::{RtmpClientSocket, duplicate_socket_fd, handle_rtmp_client};

const MAX_COMPIO_RTMP_WORKERS: usize = 8;

struct AcceptedRtmpConnection {
    stream: DuplexStream,
    peer_addr: SocketAddr,
    socket_fd: Option<OwnedFd>,
    closed: CancellationToken,
}

type RtmpWorkerItem = (AcceptedRtmpConnection, tokio::sync::OwnedSemaphorePermit);
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
    start_rtmp_server_on_with_shutdown(
        pipeline_access,
        security,
        engine,
        port,
        CancellationToken::new(),
        None,
    )
    .await;
}

pub(crate) async fn start_rtmp_server_on_with_shutdown(
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
    port: u16,
    shutdown: CancellationToken,
    started: Option<oneshot::Sender<SocketAddr>>,
) {
    let shutdown_for_engine = shutdown.clone();
    engine.register_listener_shutdown(move || shutdown_for_engine.cancel());

    let addr = format!("0.0.0.0:{port}");
    let backlog = engine.config.rtmp_backlog;
    let listener = match bind_rtmp_listener_with_backlog(port, backlog) {
        Ok(listener) => listener,
        Err(error) => {
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
                    "rtmp.listener.bind_failed"
                },
                addr = %addr,
                error = %error,
                error_kind = ?error.kind(),
                raw_os_error = ?error.raw_os_error(),
                fd_exhaustion,
                "failed to bind RTMP TCP listener",
            );
            return;
        }
    };
    let bound_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            error!(%error, "failed to inspect RTMP TCP listener address");
            return;
        }
    };
    info!("Server listening on {}", addr);
    let (accepted_tx, mut accepted_rx) =
        mpsc::channel::<AcceptedRtmpConnection>(engine.config.rtmp_max_connections.clamp(1, 1024));
    let connection_permits = Arc::new(Semaphore::new(engine.config.rtmp_max_connections));
    let worker_count = compio_rtmp_worker_count();
    let worker_capacity = worker_channel_capacity(engine.config.rtmp_max_connections, worker_count);
    let workers = match spawn_compio_rtmp_workers(
        worker_count,
        worker_capacity,
        pipeline_access,
        security,
        engine.clone(),
    ) {
        Ok(workers) => workers,
        Err(error) => {
            error!(%error, "failed to start RTMP session workers");
            return;
        }
    };
    let acceptor =
        match spawn_compio_acceptor(listener, accepted_tx, shutdown.clone(), engine.clone()) {
            Ok(acceptor) => acceptor,
            Err(error) => {
                error!(%error, "failed to start Compio RTMP acceptor thread");
                return;
            }
        };
    engine.register_os_thread(acceptor);
    if let Some(started) = started {
        let _ = started.send(bound_addr);
    }

    let mut next_worker = 0;
    while let Some(accepted) = accepted_rx.recv().await {
        let permit = match connection_permits.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                accepted.closed.cancel();
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
        if let Some((accepted, _permit)) = item {
            accepted.closed.cancel();
            warn!("RTMP connection rejected: session workers are saturated");
        }
    }

    if shutdown.is_cancelled() {
        info!("RTMP Compio acceptor stopped during shutdown");
    } else {
        warn!("RTMP Compio acceptor stopped unexpectedly; no new connections will be accepted");
    }
}

fn compio_rtmp_worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |parallelism| parallelism.get())
        .clamp(1, MAX_COMPIO_RTMP_WORKERS)
}

fn worker_channel_capacity(max_connections: usize, workers: usize) -> usize {
    max_connections.max(1).div_ceil(workers.max(1))
}
fn spawn_compio_rtmp_workers(
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
        let worker_engine = engine.clone();
        let handle = thread::Builder::new()
            .name(format!("restream-rtmp-session-{worker_index}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        error!(%error, worker_index, "failed to build RTMP session runtime");
                        return;
                    }
                };
                runtime.block_on(run_rtmp_session_worker(
                    rx,
                    pipeline_access,
                    security,
                    worker_engine,
                ));
            })
            .map_err(|error| std::io::Error::other(format!("spawn RTMP worker: {error}")))?;
        engine.register_os_thread(handle);
        workers.push(tx);
    }
    Ok(workers)
}

async fn run_rtmp_session_worker(
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
                    let addr = accepted.peer_addr;
                    let socket = RtmpClientSocket::from_duplex(
                        accepted.stream,
                        accepted.socket_fd,
                        accepted.closed,
                    );
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

fn spawn_compio_acceptor(
    listener: std::net::TcpListener,
    accepted_tx: mpsc::Sender<AcceptedRtmpConnection>,
    shutdown: CancellationToken,
    engine: Arc<MediaEngine>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("restream-rtmp-compio-acceptor".to_string())
        .spawn(move || {
            let runtime = match compio::runtime::Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    error!(%error, "failed to start Compio RTMP acceptor runtime");
                    return;
                }
            };
            let result = runtime.block_on(async move {
                let listener = compio::net::TcpListener::from_std(listener)?;
                let mut bridges = FuturesUnordered::new();
                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => break,
                        accepted = listener.accept() => {
                            let (stream, peer_addr) = accepted?;
                            stream.set_nodelay(true)?;
                            let socket_fd = duplicate_socket_fd(stream.as_raw_fd());
                            let (application_stream, bridge_stream) =
                                tokio::io::duplex(64 * 1024);
                            let closed = CancellationToken::new();
                            let accepted = AcceptedRtmpConnection {
                                stream: application_stream,
                                peer_addr,
                                socket_fd,
                                closed: closed.clone(),
                            };
                            match accepted_tx.try_send(accepted) {
                                Ok(()) => bridges.push(bridge_compio_tcp(
                                    stream,
                                    bridge_stream,
                                    closed,
                                )),
                                Err(mpsc::error::TrySendError::Full(accepted)) => {
                                    accepted.closed.cancel();
                                    warn!(%peer_addr, "RTMP connection rejected: accept queue is full");
                                }
                                Err(mpsc::error::TrySendError::Closed(accepted)) => {
                                    accepted.closed.cancel();
                                    break;
                                }
                            }
                        }
                        Some(result) = bridges.next(), if !bridges.is_empty() => {
                            if let Err(error) = result {
                                warn!(%error, "Compio RTMP connection bridge failed");
                            }
                        }
                    }
                }
                Ok::<(), io::Error>(())
            });
            if let Err(error) = result {
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
                    "Compio RTMP accept failed",
                );
            }
        })
}

async fn bridge_compio_tcp(
    stream: compio::net::TcpStream,
    application_stream: DuplexStream,
    closed: CancellationToken,
) -> io::Result<()> {
    use compio::io::{AsyncRead as _, AsyncWrite as _, AsyncWriteExt as CompioWriteExt};
    use tokio::io::{AsyncReadExt as TokioReadExt, AsyncWriteExt as TokioWriteExt};

    let (mut network_read, mut network_write) = stream.into_split();
    let (mut application_read, mut application_write) = tokio::io::split(application_stream);
    let network_to_application = async {
        let mut buffer = Vec::with_capacity(16 * 1024);
        loop {
            let read = network_read.read(buffer).await;
            let count = read.0?;
            let returned = read.1;
            if count == 0 {
                application_write.shutdown().await?;
                return Ok::<(), io::Error>(());
            }
            application_write.write_all(&returned[..count]).await?;
            buffer = returned;
            buffer.clear();
        }
    };
    let application_to_network = async {
        let mut buffer = vec![0; 16 * 1024];
        loop {
            let count = application_read.read(&mut buffer).await?;
            if count == 0 {
                network_write.shutdown().await?;
                return Ok::<(), io::Error>(());
            }
            buffer.truncate(count);
            let write = network_write.write_all(buffer).await;
            write.0?;
            buffer = write.1;
            buffer.resize(16 * 1024, 0);
        }
    };
    let pumps = async {
        futures_util::future::try_join(network_to_application, application_to_network).await?;
        Ok::<(), io::Error>(())
    };
    tokio::select! {
        _ = closed.cancelled() => Ok(()),
        result = pumps => result,
    }
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
    use super::{
        MAX_COMPIO_RTMP_WORKERS, bind_rtmp_listener_with_backlog, compio_rtmp_worker_count,
        start_rtmp_server_on_with_shutdown, worker_channel_capacity,
    };
    use crate::domain::ingest_security::IngestSecurityConfig;
    use crate::media::engine::MediaEngine;
    use crate::media::ingest_auth::{
        AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessFuture,
        PipelineAccessMode,
    };
    use crate::media::security::IngestSecurityService;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpStream;
    use tokio_util::sync::CancellationToken;

    struct TestAuthenticator;

    impl PipelineAccessAuthenticator for TestAuthenticator {
        fn authenticate<'a>(
            &'a self,
            _mode: PipelineAccessMode,
            stream_key: &'a str,
            _client_ip: &'a str,
        ) -> PipelineAccessFuture<'a> {
            Box::pin(async move {
                Ok(AuthenticatedPipeline {
                    id: stream_key.to_string(),
                    input_id: stream_key.to_string(),
                    selected: true,
                })
            })
        }
    }

    #[test]
    fn compio_worker_count_is_small_and_nonzero() {
        let count = compio_rtmp_worker_count();
        assert!((1..=MAX_COMPIO_RTMP_WORKERS).contains(&count));
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

    #[tokio::test]
    async fn compio_rtmp_listener_shutdown_joins_acceptor_and_session_workers() {
        let engine = Arc::new(MediaEngine::new());
        let shutdown = CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(start_rtmp_server_on_with_shutdown(
            Arc::new(TestAuthenticator),
            Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
            engine.clone(),
            0,
            shutdown,
            Some(started_tx),
        ));
        let address = tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("RTMP listener should start within five seconds")
            .expect("RTMP startup signal should arrive");

        let mut client = TcpStream::connect(address)
            .await
            .expect("RTMP client should connect");
        tokio::time::timeout(
            Duration::from_secs(5),
            super::super::perform_client_handshake(&mut client, &CancellationToken::new()),
        )
        .await
        .expect("RTMP session should complete its handshake")
        .expect("RTMP handshake should succeed");

        engine.shutdown_listeners();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("RTMP accept task should stop within five seconds")
            .expect("RTMP accept task should not panic");

        let handles = engine.drain_os_thread_handles();
        assert_eq!(
            handles.len(),
            compio_rtmp_worker_count() + 1,
            "shutdown must retain the acceptor and every session worker handle"
        );
        let join = tokio::task::spawn_blocking(move || {
            for handle in handles {
                handle
                    .join()
                    .expect("RTMP listener thread should not panic");
            }
        });
        tokio::time::timeout(Duration::from_secs(5), join)
            .await
            .expect("RTMP listener and worker threads should join within five seconds")
            .expect("thread join task should not panic");

        drop(client);
        let rebound = bind_rtmp_listener_with_backlog(address.port(), 512).unwrap_or_else(|error| {
            let open_fds = std::fs::read_dir("/proc/self/fd")
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    std::fs::read_link(entry.path()).ok().map(|target| {
                        format!("{} -> {}", entry.file_name().to_string_lossy(), target.display())
                    })
                })
                .collect::<Vec<_>>();
            panic!("RTMP listener port should be released after shutdown: {error}; open_fds={open_fds:?}");
        });
        drop(rebound);
    }
}
