//! RTMP TCP admission and the fixed Compio connection owner.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use compio::driver::{DriverType, ProactorBuilder};
use compio::runtime::RuntimeBuilder;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::PipelineAccessAuthenticator;
use crate::media::security::IngestSecurityService;

use super::ingest::{RtmpControlCommand, handle_rtmp_client, run_rtmp_control_session};

const CONTROL_SESSION_CAPACITY: usize = 16;
const MAX_IO_URING_ENTRIES: u32 = 32_768;
// Bounds bytes handed across domains and held by permit-backed queued or
// processing commands. It excludes decoded media awaiting permits: a
// connection may retain one completed 24-bit message in the result Vec while
// the parser assembles the next 24-bit message, plus a 4 KiB socket read and
// a 4 KiB parser staging buffer and small event metadata. Aggregate residual
// memory scales with connection limit.
const MEDIA_HANDOFF_BYTES: usize = 64 * 1024 * 1024;

struct ControlSession {
    commands: mpsc::Receiver<RtmpControlCommand>,
    shutdown: CancellationToken,
}

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
    mut started: Option<oneshot::Sender<Result<SocketAddr, String>>>,
) {
    let shutdown_for_engine = shutdown.clone();
    engine.register_listener_shutdown(move || shutdown_for_engine.cancel());

    let addr = format!("0.0.0.0:{port}");
    let listener = match bind_rtmp_listener_with_backlog(port, engine.config.rtmp_backlog) {
        Ok(listener) => listener,
        Err(error) => {
            report_listener_error(
                &engine,
                &error,
                "bind_failed",
                "failed to bind RTMP TCP listener",
            );
            if let Some(started) = started.take() {
                let _ = started.send(Err(format!("failed to bind RTMP TCP listener: {error}")));
            }
            return;
        }
    };
    let bound_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            error!(%error, "failed to inspect RTMP TCP listener address");
            if let Some(started) = started.take() {
                let _ = started.send(Err(format!(
                    "failed to inspect RTMP listener address: {error}"
                )));
            }
            return;
        }
    };

    let connection_limit = engine.config.rtmp_max_connections.clamp(1, 16_384);
    let (control_tx, mut control_rx) = mpsc::channel(connection_limit);
    let media_handoff = Arc::new(tokio::sync::Semaphore::new(MEDIA_HANDOFF_BYTES));
    let (ready_tx, ready_rx) = oneshot::channel();
    let acceptor = match spawn_compio_owner(
        listener,
        control_tx,
        shutdown.clone(),
        engine.clone(),
        ready_tx,
        media_handoff,
        connection_limit,
    ) {
        Ok(acceptor) => acceptor,
        Err(error) => {
            error!(%error, "failed to start RTMP Compio owner thread");
            if let Some(started) = started.take() {
                let _ = started.send(Err(format!("failed to start RTMP owner thread: {error}")));
            }
            return;
        }
    };
    engine.register_os_thread(acceptor);

    match ready_rx.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            error!(%error, "RTMP Compio io_uring owner failed to start");
            if let Some(started) = started.take() {
                let _ = started.send(Err(error));
            }
            return;
        }
        Err(error) => {
            error!("RTMP Compio owner exited before reporting startup");
            if let Some(started) = started.take() {
                let _ = started.send(Err(format!(
                    "RTMP Compio owner exited before startup readiness: {error}"
                )));
            }
            return;
        }
    }

    info!("Server listening on {}", addr);
    if let Some(started) = started {
        let _ = started.send(Ok(bound_addr));
    }
    let mut control_sessions = JoinSet::new();
    loop {
        tokio::select! {
            session = control_rx.recv() => {
                let Some(session) = session else {
                    break;
                };
                let pipeline_access = pipeline_access.clone();
                let security = security.clone();
                let engine = engine.clone();
                control_sessions.spawn(run_rtmp_control_session(
                    session.commands,
                    session.shutdown,
                    pipeline_access,
                    security,
                    engine,
                ));
            }
            Some(result) = control_sessions.join_next(), if !control_sessions.is_empty() => {
                if let Err(error) = result {
                    warn!(%error, "RTMP control session task failed");
                }
            }
        }
    }
    while let Some(result) = control_sessions.join_next().await {
        if let Err(error) = result {
            warn!(%error, "RTMP control session task failed");
        }
    }

    if shutdown.is_cancelled() {
        info!("RTMP Compio owner stopped during shutdown");
    } else {
        warn!("RTMP Compio owner stopped unexpectedly; no new connections will be accepted");
    }
}

fn spawn_compio_owner(
    listener: std::net::TcpListener,
    control_tx: mpsc::Sender<ControlSession>,
    shutdown: CancellationToken,
    engine: Arc<MediaEngine>,
    ready: oneshot::Sender<Result<(), String>>,
    media_handoff: Arc<tokio::sync::Semaphore>,
    connection_limit: usize,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("restream-rtmp-compio-owner".to_string())
        .spawn(move || {
            let entries = rtmp_io_uring_entries(connection_limit);
            let mut proactor = ProactorBuilder::new();
            proactor
                .driver_type(DriverType::IoUring)
                .capacity(entries)
                .cqsize(entries * 2);
            let mut runtime_builder = RuntimeBuilder::new();
            runtime_builder.with_proactor(proactor);
            let runtime = match runtime_builder.build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready.send(Err(format!(
                        "RTMP ingress requires Compio io_uring (entries={entries}): {error}"
                    )));
                    return;
                }
            };
            if runtime.driver_type() != DriverType::IoUring {
                let _ = ready.send(Err(format!(
                    "RTMP ingress requires Compio io_uring, got {:?}",
                    runtime.driver_type()
                )));
                return;
            }
            let owner_engine = engine.clone();
            // SQ capacity batches operations; Compio submits/drains when full.
            // Size it beyond the admitted socket count and keep CQ space for
            // the corresponding completion burst; startup fails if unsupported.
            let owner_result = runtime.block_on(async move {
                let listener = match compio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = ready.send(Err(format!(
                            "failed to initialize RTMP Compio listener: {error}"
                        )));
                        return Err(error);
                    }
                };
                let _ = ready.send(Ok(()));
                run_compio_owner(
                    listener,
                    control_tx,
                    shutdown,
                    owner_engine,
                    media_handoff,
                    connection_limit,
                )
                .await
            });
            if let Err(error) = owner_result {
                report_listener_error(
                    &engine,
                    &error,
                    "accept_failed",
                    "RTMP Compio accept failed",
                );
            }
        })
}
fn rtmp_io_uring_entries(connection_limit: usize) -> u32 {
    connection_limit
        .saturating_add(16)
        .next_power_of_two()
        .clamp(64, MAX_IO_URING_ENTRIES as usize) as u32
}

async fn run_compio_owner(
    listener: compio::net::TcpListener,
    control_tx: mpsc::Sender<ControlSession>,
    shutdown: CancellationToken,
    engine: Arc<MediaEngine>,
    media_handoff: Arc<tokio::sync::Semaphore>,
    connection_limit: usize,
) -> io::Result<()> {
    let connection_shutdown = CancellationToken::new();
    let mut connections = FuturesUnordered::new();
    let accept_result = loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break Ok(()),
            Some(()) = connections.next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, peer_addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                if let Err(error) = stream.set_nodelay(true) {
                    warn!(%peer_addr, %error, "failed to enable TCP_NODELAY for RTMP client");
                    drop(stream);
                    continue;
                }
                if connections.len() >= connection_limit {
                    warn!(%peer_addr, "RTMP connection rejected: max connection limit reached");
                    drop(stream);
                    continue;
                }

                let (command_tx, command_rx) = mpsc::channel(CONTROL_SESSION_CAPACITY);
                let session_shutdown = connection_shutdown.child_token();
                if control_tx.try_send(ControlSession {
                    commands: command_rx,
                    shutdown: session_shutdown.clone(),
                }).is_err() {
                    warn!(%peer_addr, "RTMP connection rejected: control sessions are saturated");
                    drop(stream);
                    continue;
                }
                let connection_media_handoff = media_handoff.clone();
                let connection_engine = engine.clone();
                connections.push(Box::pin(async move {
                    if let Err(error) = handle_rtmp_client(
                        stream,
                        peer_addr,
                        command_tx,
                        session_shutdown,
                        connection_engine,
                        connection_media_handoff,
                    ).await {
                        warn!(%error, %peer_addr, "error handling RTMP client");
                    }
                }));
            }
        }
    };

    connection_shutdown.cancel();
    let close_result = listener.close().await;
    while connections.next().await.is_some() {}
    accept_result.and(close_result)
}

fn report_listener_error(
    engine: &MediaEngine,
    error: &io::Error,
    kind: &'static str,
    message: &'static str,
) {
    let fd_exhaustion = is_fd_exhaustion_error(error);
    if kind == "accept_failed" {
        engine
            .runtime
            .rtmp_listener_stats
            .rtmp_accept_errors
            .fetch_add(1, Ordering::Relaxed);
    }
    if fd_exhaustion {
        engine
            .runtime
            .rtmp_listener_stats
            .rtmp_fd_exhaustion_errors
            .fetch_add(1, Ordering::Relaxed);
    }
    let event_type = if fd_exhaustion {
        "rtmp.listener.fd_exhausted"
    } else if kind == "bind_failed" {
        "rtmp.listener.bind_failed"
    } else {
        "rtmp.listener.accept_failed"
    };
    error!(
        event_class = "resource",
        event_type,
        error = %error,
        error_kind = ?error.kind(),
        raw_os_error = ?error.raw_os_error(),
        fd_exhaustion,
        message,
        "RTMP listener failure",
    );
}

fn is_fd_exhaustion_error(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

/// Create the listening socket the Compio owner adopts: IPv4 any-address,
/// `SO_REUSEADDR`, nonblocking and close-on-exec, with an explicit backlog
/// (std's `TcpListener::bind` cannot set one). Plain syscalls, so no Tokio
/// network type ever touches the RTMP transport socket.
fn bind_rtmp_listener_with_backlog(
    port: u16,
    backlog: u32,
) -> Result<std::net::TcpListener, io::Error> {
    use std::os::fd::{FromRawFd, OwnedFd};

    let fd = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh socket owned by nothing else; `OwnedFd` closes it
    // on every early return below.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let check = |result: libc::c_int| {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    let enable: libc::c_int = 1;
    check(unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&enable as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    })?;
    let address = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from(std::net::Ipv4Addr::UNSPECIFIED).to_be(),
        },
        sin_zero: [0; 8],
    };
    check(unsafe {
        libc::bind(
            fd,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    })?;
    let backlog = libc::c_int::try_from(backlog).unwrap_or(libc::c_int::MAX);
    check(unsafe { libc::listen(fd, backlog) })?;
    Ok(std::net::TcpListener::from(socket))
}

#[cfg(test)]
mod tests {
    use super::{bind_rtmp_listener_with_backlog, start_rtmp_server_on_with_shutdown};
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

    #[tokio::test]
    async fn owner_shutdown_releases_listener_for_rebind() {
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
        let startup = tokio::time::timeout(Duration::from_secs(5), started_rx).await;
        let address = match startup {
            Ok(Ok(Ok(address))) => address,
            Ok(Ok(Err(error))) => {
                server
                    .await
                    .expect("failed RTMP startup task should not panic");
                let handles = engine.drain_os_thread_handles();
                tokio::task::spawn_blocking(move || {
                    for handle in handles {
                        handle.join().expect("RTMP owner thread should join");
                    }
                })
                .await
                .expect("RTMP owner joins should complete");
                panic!("RTMP owner startup failed before readiness: {error}");
            }
            Ok(Err(error)) => {
                server
                    .await
                    .expect("failed RTMP startup task should not panic");
                let handles = engine.drain_os_thread_handles();
                tokio::task::spawn_blocking(move || {
                    for handle in handles {
                        handle.join().expect("RTMP owner thread should join");
                    }
                })
                .await
                .expect("RTMP owner joins should complete");
                panic!("RTMP owner exited before startup readiness: {error}");
            }
            Err(error) => {
                engine.shutdown_listeners();
                server
                    .await
                    .expect("failed RTMP startup task should not panic");
                let handles = engine.drain_os_thread_handles();
                tokio::task::spawn_blocking(move || {
                    for handle in handles {
                        handle.join().expect("RTMP owner thread should join");
                    }
                })
                .await
                .expect("RTMP owner joins should complete");
                panic!("RTMP owner startup timed out: {error}");
            }
        };

        let client = TcpStream::connect(address)
            .await
            .expect("client should connect while owner is accepting");
        engine.shutdown_listeners();
        drop(client);
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("RTMP listener task should stop")
            .expect("RTMP listener task should not panic");
        let rebound = bind_rtmp_listener_with_backlog(address.port(), 512)
            .expect("owner shutdown should release the listening port");
        drop(rebound);

        let handles = engine.drain_os_thread_handles();
        assert_eq!(
            handles.len(),
            1,
            "RTMP ingress owns one fixed Compio thread"
        );
        tokio::task::spawn_blocking(move || {
            for handle in handles {
                handle.join().expect("RTMP owner thread should join");
            }
        })
        .await
        .expect("thread joins should complete");
    }
}
