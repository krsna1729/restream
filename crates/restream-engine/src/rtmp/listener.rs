//! RTMP TCP admission and fixed Compio connection owners.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;

use compio::driver::{DriverType, ProactorBuilder};
use compio::runtime::RuntimeBuilder;
use futures_util::FutureExt;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::engine::MediaEngine;
use crate::ingest_auth::PipelineAccessAuthenticator;
use crate::security::IngestSecurityService;

use super::connection::RtmpControlCommand;
use super::ingest::{handle_rtmp_client, run_rtmp_control_session};

const CONTROL_SESSION_CAPACITY: usize = 16;
const MAX_IO_URING_ENTRIES: u32 = 32_768;
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

pub async fn start_rtmp_server_on_with_shutdown(
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
    let limits = engine.rtmp_ingress_limits();
    let connection_limit = limits.max_connections.clamp(1, 16_384);
    let client_slots = restream_sync::client_slots::ClientSlots::new(limits.max_connections_per_ip);
    let parser_budget = limits.parser_budget_bytes;
    let owners = limits
        .ingress_owners
        .clamp(1, 64)
        .min(connection_limit)
        .min((parser_budget / limits.max_message_bytes.max(1)).max(1));
    log_owner_limits(&limits, owners, connection_limit, parser_budget);
    let Some((listeners, bound_addr)) = bind_for_startup(&engine, port, owners, &mut started)
    else {
        return;
    };

    // Partition exact totals, including remainders. Owner-local accounting
    // keeps parser reads free of cross-thread atomics; hash skew may reject
    // on one owner before another owner's share is used.
    let owners_shutdown = shutdown.child_token();
    let _owners_guard = owners_shutdown.clone().drop_guard();
    let accepting = CancellationToken::new();
    let (control_tx, control_rx) = mpsc::channel(connection_limit);
    let mut readiness = Vec::with_capacity(owners);
    for (index, listener) in listeners.into_iter().enumerate() {
        let (ready_tx, ready_rx) = oneshot::channel();
        match spawn_compio_owner(
            index,
            listener,
            control_tx.clone(),
            owners_shutdown.clone(),
            accepting.clone(),
            engine.clone(),
            ready_tx,
            OwnerLimits {
                connections: owner_share(connection_limit, index, owners),
                parser_budget_bytes: owner_share(parser_budget, index, owners),
                client_slots: Arc::clone(&client_slots),
            },
        ) {
            Ok(owner) => {
                engine.register_os_thread(owner);
                readiness.push(ready_rx);
            }
            Err(error) => {
                report_owner_launch_failure(&mut started, index, &error);
                return;
            }
        }
    }
    drop(control_tx);
    if let Err(failure) = await_owner_readiness(readiness, &owners_shutdown).await {
        if let Some(started) = started.take() {
            let _ = started.send(Err(failure));
        }
        return;
    }
    // No owner accepts until every runtime/socket has initialized.
    accepting.cancel();
    log_listener_started(owners, &addr);
    let _running = engine.publish_rtmp_owners(owners);
    if let Some(started) = started {
        let _ = started.send(Ok(bound_addr));
    }
    serve_control_sessions(control_rx, &pipeline_access, &security, &engine).await;

    log_listener_stopped(&shutdown);
}

fn log_owner_limits(
    limits: &crate::engine::RtmpIngressLimits,
    owners: usize,
    connection_limit: usize,
    parser_budget: usize,
) {
    if owners < limits.ingress_owners.clamp(1, 64) {
        warn!(
            requested = limits.ingress_owners,
            owners,
            connection_limit,
            parser_budget,
            "RTMP ingress owners reduced by connection limit or parser budget"
        );
    }
}

fn log_listener_started(owners: usize, addr: &str) {
    info!(owners, "Server listening on {}", addr);
}

fn log_listener_stopped(shutdown: &CancellationToken) {
    if shutdown.is_cancelled() {
        info!("RTMP Compio owner stopped during shutdown");
    } else {
        warn!("RTMP Compio owner stopped unexpectedly; no new connections will be accepted");
    }
}

fn report_owner_launch_failure(
    started: &mut Option<oneshot::Sender<Result<SocketAddr, String>>>,
    index: usize,
    error: &io::Error,
) {
    error!(%error, owner = index, "failed to start RTMP Compio owner thread");
    if let Some(started) = started.take() {
        let _ = started.send(Err(format!("failed to start RTMP owner thread: {error}")));
    }
}

fn bind_for_startup(
    engine: &MediaEngine,
    port: u16,
    owners: usize,
    started: &mut Option<oneshot::Sender<Result<SocketAddr, String>>>,
) -> Option<(Vec<std::net::TcpListener>, SocketAddr)> {
    let listeners = match bind_rtmp_listeners(port, engine.config.rtmp_backlog, owners) {
        Ok(listeners) => listeners,
        Err(error) => {
            report_listener_error(
                engine,
                &error,
                "bind_failed",
                "failed to bind RTMP TCP listener",
            );
            if let Some(started) = started.take() {
                let _ = started.send(Err(format!("failed to bind RTMP TCP listener: {error}")));
            }
            return None;
        }
    };
    let bound_addr = match listeners[0].local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            error!(%error, "failed to inspect RTMP TCP listener address");
            if let Some(started) = started.take() {
                let _ = started.send(Err(format!(
                    "failed to inspect RTMP listener address: {error}"
                )));
            }
            return None;
        }
    };
    Some((listeners, bound_addr))
}

async fn serve_control_sessions(
    mut control_rx: mpsc::Receiver<ControlSession>,
    pipeline_access: &Arc<dyn PipelineAccessAuthenticator>,
    security: &Arc<IngestSecurityService>,
    engine: &Arc<MediaEngine>,
) {
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
}

async fn await_owner_readiness(
    readiness: Vec<oneshot::Receiver<Result<(), String>>>,
    owners_shutdown: &CancellationToken,
) -> Result<(), String> {
    for (index, ready_rx) in readiness.into_iter().enumerate() {
        let failure = match ready_rx.await {
            Ok(Ok(())) => continue,
            Ok(Err(error)) => error,
            Err(error) => format!("RTMP Compio owner exited before startup readiness: {error}"),
        };
        error!(%failure, owner = index, "RTMP Compio io_uring owner failed to start");
        return Err(failure);
    }
    if owners_shutdown.is_cancelled() {
        return Err("RTMP ingress cancelled before startup readiness".to_string());
    }
    Ok(())
}

/// What one ingress owner may admit: its share of the connection cap and the
/// parser budget, and the per-client slots every owner shares.
struct OwnerLimits {
    connections: usize,
    parser_budget_bytes: usize,
    client_slots: Arc<restream_sync::client_slots::ClientSlots>,
}

fn owner_share(total: usize, index: usize, owners: usize) -> usize {
    total / owners + usize::from(index < total % owners)
}

#[allow(clippy::too_many_arguments)]
fn spawn_compio_owner(
    index: usize,
    listener: std::net::TcpListener,
    control_tx: mpsc::Sender<ControlSession>,
    shutdown: CancellationToken,
    accepting: CancellationToken,
    engine: Arc<MediaEngine>,
    ready: oneshot::Sender<Result<(), String>>,
    limits: OwnerLimits,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("restream-rtmp-compio-owner-{index}"))
        .spawn(move || {
            // A failed/panicked owner cancels its siblings, not just its own
            // connections; the listener task then returns and its supervisor
            // restarts it. The server's guard also covers control-task abort.
            let _owner_guard = shutdown.clone().drop_guard();
            // Thread-entry boundary: a panic outside the per-connection one
            // (accept loop, close, compio's driver-error panic) is counted
            // and reported instead of unwinding the thread unseen. compio's
            // `block_on` clears its executor before re-raising, so no task
            // is polled during this unwind.
            let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                serve_compio_owner(
                    listener, control_tx, shutdown, accepting, &engine, ready, limits,
                );
            }));
            if let Err(payload) = served {
                let panic = crate::panic_boundary::record_contained(payload.as_ref());
                error!(
                    event_class = "lifecycle",
                    event_type = "rtmp.listener.owner_panicked",
                    owner = index,
                    panic,
                    "RTMP Compio owner panicked; the RTMP listener stops and restarts"
                );
            }
        })
}

fn serve_compio_owner(
    listener: std::net::TcpListener,
    control_tx: mpsc::Sender<ControlSession>,
    shutdown: CancellationToken,
    accepting: CancellationToken,
    engine: &Arc<MediaEngine>,
    ready: oneshot::Sender<Result<(), String>>,
    limits: OwnerLimits,
) {
    let entries = rtmp_io_uring_entries(limits.connections);
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
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return listener.close().await,
            _ = accepting.cancelled() => {}
        }
        run_compio_owner(listener, control_tx, shutdown, owner_engine, limits).await
    });
    if let Err(error) = owner_result {
        report_listener_error(engine, &error, "accept_failed", "RTMP Compio accept failed");
    }
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
    limits: OwnerLimits,
) -> io::Result<()> {
    let OwnerLimits {
        connections: connection_limit,
        parser_budget_bytes,
        client_slots,
    } = limits;
    let connection_shutdown = CancellationToken::new();
    let parser_budget = restream_rtmp::parser_budget::ParserBudget::new(parser_budget_bytes);
    let mut connections = FuturesUnordered::new();
    // One accept stays in flight across iterations and is replaced only when
    // it completes. Dropping a pending Compio accept (as a fresh
    // `listener.accept()` per `select!` iteration did whenever another branch
    // won) discards a completion that may already hold an accepted socket:
    // the kernel accepted the client, Restream closed it unseen.
    let mut accept = Box::pin(listener.accept());
    let accept_result = loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break Ok(()),
            Some(()) = connections.next(), if !connections.is_empty() => {}
            accepted = &mut accept => {
                accept.set(listener.accept());
                let (stream, peer_addr) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                let Some(admitted) = admit_client(
                    &stream,
                    peer_addr,
                    connection_limit,
                    connections.len(),
                    &client_slots,
                    &control_tx,
                    &connection_shutdown,
                ) else {
                    continue;
                };
                let AdmittedClient {
                    commands,
                    shutdown: session_shutdown,
                    slot: client_slot,
                } = admitted;
                let connection_engine = engine.clone();
                let connection_parser_budget = parser_budget.clone();
                // Fault domain: a panic while serving one connection ends that
                // connection (its socket, parser charge and gate lease drop
                // with the future; the control session sees its command
                // channel close), not the owner thread and every other
                // connection on it.
                let connection = std::panic::AssertUnwindSafe(handle_rtmp_client(
                    stream,
                    peer_addr,
                    commands,
                    session_shutdown,
                    connection_engine,
                    connection_parser_budget,
                ));
                connections.push(Box::pin(connection.catch_unwind().map(move |result| {
                    drop(client_slot);
                    // Runs on the owner, outside the per-connection boundary.
                    #[cfg(test)]
                    super::ingest::injected_panics::owner_unwind_if_marked(peer_addr);
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => warn!(%error, %peer_addr, "error handling RTMP client"),
                        Err(payload) => {
                            let panic = crate::panic_boundary::record_contained(payload.as_ref());
                            error!(
                                %peer_addr,
                                panic,
                                "RTMP connection panicked; only that connection was closed"
                            );
                        }
                    }
                })));
            }
        }
    };

    connection_shutdown.cancel();
    drop(accept);
    let close_result = listener.close().await;
    while connections.next().await.is_some() {}
    accept_result.and(close_result)
}

struct AdmittedClient {
    commands: mpsc::Sender<RtmpControlCommand>,
    shutdown: CancellationToken,
    slot: restream_sync::client_slots::ClientSlot,
}

/// Admit one accepted socket: TCP options, owner and per-client limits, then
/// the control session. `None` drops the socket.
fn admit_client(
    stream: &compio::net::TcpStream,
    peer_addr: SocketAddr,
    connection_limit: usize,
    active: usize,
    client_slots: &Arc<restream_sync::client_slots::ClientSlots>,
    control_tx: &mpsc::Sender<ControlSession>,
    connection_shutdown: &CancellationToken,
) -> Option<AdmittedClient> {
    apply_socket_options(stream, peer_addr)?;
    check_connection_limit(peer_addr, connection_limit, active)?;
    let slot = acquire_client_slot(client_slots, peer_addr)?;
    let (commands, shutdown) = open_control_session(control_tx, connection_shutdown, peer_addr)?;
    Some(AdmittedClient {
        commands,
        shutdown,
        slot,
    })
}

fn apply_socket_options(stream: &compio::net::TcpStream, peer_addr: SocketAddr) -> Option<()> {
    if let Err(error) = stream.set_nodelay(true) {
        warn!(%peer_addr, %error, "failed to enable TCP_NODELAY for RTMP client");
        return None;
    }
    Some(())
}

fn check_connection_limit(
    peer_addr: SocketAddr,
    connection_limit: usize,
    active: usize,
) -> Option<()> {
    if active >= connection_limit {
        warn!(%peer_addr, "RTMP connection rejected: max connection limit reached");
        return None;
    }
    Some(())
}

fn acquire_client_slot(
    client_slots: &Arc<restream_sync::client_slots::ClientSlots>,
    peer_addr: SocketAddr,
) -> Option<restream_sync::client_slots::ClientSlot> {
    let Some(slot) = client_slots.try_acquire(peer_addr.ip()) else {
        warn!(%peer_addr, "RTMP connection rejected: this client holds its maximum connections");
        return None;
    };
    Some(slot)
}

fn open_control_session(
    control_tx: &mpsc::Sender<ControlSession>,
    connection_shutdown: &CancellationToken,
    peer_addr: SocketAddr,
) -> Option<(mpsc::Sender<RtmpControlCommand>, CancellationToken)> {
    let (commands, command_rx) = mpsc::channel(CONTROL_SESSION_CAPACITY);
    let session_shutdown = connection_shutdown.child_token();
    if control_tx
        .try_send(ControlSession {
            commands: command_rx,
            shutdown: session_shutdown.clone(),
        })
        .is_err()
    {
        warn!(%peer_addr, "RTMP connection rejected: control sessions are saturated");
        return None;
    }
    Some((commands, session_shutdown))
}

fn report_listener_error(
    engine: &MediaEngine,
    error: &io::Error,
    kind: &'static str,
    message: &'static str,
) {
    let fd_exhaustion = is_fd_exhaustion_error(error);
    engine.record_rtmp_listener_error(kind == "accept_failed", fd_exhaustion);
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

/// Bind one socket per owner to the same port; Linux hashes connections to
/// `SO_REUSEPORT` listeners. Port zero is resolved by the first bind.
fn bind_rtmp_listeners(
    port: u16,
    backlog: u32,
    owners: usize,
) -> io::Result<Vec<std::net::TcpListener>> {
    let first = bind_rtmp_listener(port, backlog, owners > 1)?;
    let port = first.local_addr()?.port();
    let mut listeners = Vec::with_capacity(owners);
    listeners.push(first);
    for _ in 1..owners {
        listeners.push(bind_rtmp_listener(port, backlog, true)?);
    }
    Ok(listeners)
}

#[cfg(test)]
fn bind_rtmp_listener_with_backlog(port: u16, backlog: u32) -> io::Result<std::net::TcpListener> {
    bind_rtmp_listener(port, backlog, false)
}

/// Nonblocking, close-on-exec IPv4 listener adopted directly by Compio.
fn bind_rtmp_listener(
    port: u16,
    backlog: u32,
    reuse_port: bool,
) -> Result<std::net::TcpListener, io::Error> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, sockopt};

    let socket = rustix::net::socket_with(
        AddressFamily::INET,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    sockopt::set_socket_reuseaddr(&socket, true)?;
    if reuse_port {
        sockopt::set_socket_reuseport(&socket, true)?;
    }
    rustix::net::bind(
        &socket,
        &std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, port),
    )?;
    rustix::net::listen(&socket, i32::try_from(backlog).unwrap_or(i32::MAX))?;
    Ok(std::net::TcpListener::from(socket))
}

#[cfg(test)]
mod tests {
    use super::{bind_rtmp_listener_with_backlog, owner_share, start_rtmp_server_on_with_shutdown};
    use crate::engine::MediaEngine;
    use crate::ingest_auth::{
        AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessFuture,
        PipelineAccessMode,
    };
    use crate::security::IngestSecurityService;
    use restream_domain::domain::ingest_security::IngestSecurityConfig;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        exercise_owner_shutdown(1, false).await;
    }

    #[tokio::test]
    async fn sharded_owner_shutdown_releases_all_listeners_for_rebind() {
        exercise_owner_shutdown(3, false).await;
    }

    #[tokio::test]
    async fn aborting_control_task_stops_all_ingress_owners() {
        exercise_owner_shutdown(3, true).await;
    }

    proptest::proptest! {
        #[test]
        fn owner_shares_preserve_global_limits(total in 1usize..16_384, owners in 1usize..65) {
            let owners = owners.min(total);
            let shares: Vec<_> = (0..owners).map(|index| owner_share(total, index, owners)).collect();
            proptest::prop_assert_eq!(shares.iter().sum::<usize>(), total);
            proptest::prop_assert!(shares.iter().all(|share| *share > 0));
            proptest::prop_assert!(shares.iter().max().unwrap() - shares.iter().min().unwrap() <= 1);
        }
    }

    async fn exercise_owner_shutdown(owners: usize, abort_control: bool) {
        let engine = Arc::new(MediaEngine::new_with_config(Arc::new(
            crate::config::EngineConfig {
                rtmp_ingress_owners: owners,
                ..crate::config::EngineConfig::default()
            },
        )));
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

        let running = || engine.rtmp_listener_health().ingress_owners;
        assert_eq!(running(), owners as u64, "every bound owner is reported");
        let mut clients = Vec::new();
        for _ in 0..16 {
            let mut client = TcpStream::connect(address).await.unwrap();
            let mut c0c1 = [0u8; 1537];
            c0c1[0] = 3;
            client.write_all(&c0c1).await.unwrap();
            let mut response = [0u8; 3073];
            tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut response))
                .await
                .expect("every admitted client completes the RTMP handshake")
                .unwrap();
            assert_eq!(response[0], 3);
            client.write_all(&response[1..1537]).await.unwrap();
            clients.push(client);
        }
        if abort_control {
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        } else {
            engine.shutdown_listeners();
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .expect("RTMP listener task should stop")
                .expect("RTMP listener task should not panic");
        }
        let handles = engine.drain_os_thread_handles();
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                for handle in handles {
                    handle.join().expect("RTMP owner thread should join");
                }
            }),
        )
        .await
        .expect("all owner threads stop")
        .unwrap();
        assert_eq!(
            running(),
            0,
            "a stopped or aborted listener reports no owners"
        );
        for mut client in clients {
            let mut remainder = Vec::new();
            let closed =
                tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut remainder))
                    .await
                    .expect("shutdown closes every admitted socket");
            // Cancelling while C2 is still unread legitimately resets TCP.
            assert!(
                closed.is_ok() || closed.unwrap_err().kind() == std::io::ErrorKind::ConnectionReset
            );
        }
        let rebound = bind_rtmp_listener_with_backlog(address.port(), 512)
            .expect("owner shutdown should release the listening port");
        drop(rebound);
    }
}
