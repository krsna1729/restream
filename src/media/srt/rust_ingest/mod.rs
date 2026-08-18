use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shiguredo_srt::KeyLength;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{error, info, warn};

use crate::config::rust_srt_ingest_worker_count;
use crate::domain::srt_ingest::{ResolvedSrtCrypto, ResolvedSrtIngestConfig};
use crate::media::ingest_auth::{AuthenticatedPipeline, PipelineAccessMode};

use super::SrtServer;
use super::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};
use session::RustIngestSession;
use types::{ConnectionId, IngestEvent, WorkerCommand};
use worker::WorkerOptions;

mod connected;
mod connected_group;
mod connected_worker;
mod connection;
mod read;
mod routing;
mod session;
mod socket;
mod types;
mod worker;

const EVENT_CHANNEL_CAPACITY: usize = 4096;
const COMMAND_CHANNEL_CAPACITY: usize = 1024;

pub(super) async fn run(server: Arc<SrtServer>, port: u16) {
    let global = match server.ingest_policy_store.global_config().resolve() {
        Ok(config) => config,
        Err(error) => {
            error!(%error, "Rust SRT ingest global policy is invalid");
            return;
        }
    };
    let options = match worker_options(&global) {
        Ok(options) => options,
        Err(error) => {
            error!(%error, "Rust SRT ingest configuration is unsupported");
            return;
        }
    };
    let available = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let workers = routing::worker_count(rust_srt_ingest_worker_count(), available);
    let (event_sender, event_receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let (pool, handles) = match RustIngestPool::start(
        port,
        workers,
        server.engine.config.srt_udp_buffer,
        options,
        event_sender,
    ) {
        Ok(pool) => pool,
        Err(error) => {
            error!(%error, "Rust SRT ingest worker pool failed to start");
            return;
        }
    };
    for handle in handles {
        server.engine.register_os_thread(handle);
    }
    let stop = pool.stop_handle();
    server.engine.register_listener_shutdown(move || {
        stop.store(true, Ordering::Release);
    });
    info!(port, workers, "Rust SRT ingest listener started");
    serve_events(&server, pool, event_receiver, global).await;
}

struct RustIngestPool {
    stop: Arc<AtomicBool>,
    commands: Vec<mpsc::Sender<WorkerCommand>>,
}

impl RustIngestPool {
    fn start(
        port: u16,
        workers: usize,
        udp_buffer: usize,
        options: WorkerOptions,
        events: mpsc::Sender<IngestEvent>,
    ) -> Result<(Self, Vec<std::thread::JoinHandle<()>>), String> {
        let stop = Arc::new(AtomicBool::new(false));
        if crate::config::rust_srt_ingest_connected() {
            let (commands, handles) =
                connected::start(port, workers, udp_buffer, options, events, stop.clone())?;
            return Ok((Self { stop, commands }, handles));
        }

        let mut sockets = Vec::with_capacity(workers);
        for worker_index in 0..workers {
            let socket = socket::bind_reuseport(port, udp_buffer)
                .map_err(|error| format!("bind Rust SRT ingest worker {worker_index}: {error}"))?;
            sockets.push(socket);
        }

        let mut commands: Vec<mpsc::Sender<WorkerCommand>> = Vec::with_capacity(workers);
        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(workers);
        for (worker_index, socket) in sockets.into_iter().enumerate() {
            let (command_sender, command_receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
            let handle = match worker::spawn(
                worker_index,
                socket,
                stop.clone(),
                command_receiver,
                events.clone(),
                options.clone(),
            ) {
                Ok(handle) => handle,
                Err(error) => {
                    stop.store(true, Ordering::Release);
                    for sender in &commands {
                        let _ = sender.try_send(WorkerCommand::Stop);
                    }
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(format!(
                        "spawn Rust SRT ingest worker {worker_index}: {error}"
                    ));
                }
            };
            commands.push(command_sender);
            handles.push(handle);
        }
        Ok((Self { stop, commands }, handles))
    }

    fn stop_handle(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    fn authorize(&self, id: ConnectionId, logical_id: ConnectionId, accepted: bool) -> bool {
        self.commands.get(id.worker).is_some_and(|sender| {
            sender
                .try_send(WorkerCommand::Authorize {
                    id,
                    logical_id,
                    accepted,
                })
                .is_ok()
        })
    }

    fn command_sender(&self, id: ConnectionId) -> Option<Sender<WorkerCommand>> {
        self.commands.get(id.worker).cloned()
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        for sender in &self.commands {
            let _ = sender.try_send(WorkerCommand::Stop);
        }
    }
}

async fn serve_events(
    server: &Arc<SrtServer>,
    pool: RustIngestPool,
    mut events: Receiver<IngestEvent>,
    global: ResolvedSrtIngestConfig,
) {
    let mut sessions = HashMap::<ConnectionId, RustIngestSession>::new();
    let mut plays = HashMap::<ConnectionId, tokio_util::sync::CancellationToken>::new();
    let mut physical_to_logical = HashMap::<ConnectionId, ConnectionId>::new();
    let mut bonded_sessions = HashMap::<BondKey, BondSession>::new();
    while let Some(event) = events.recv().await {
        match event {
            IngestEvent::Connected {
                id,
                peer,
                stream_id,
                group,
                peer_socket_id,
            } => {
                admit_connection(
                    server,
                    &pool,
                    &mut sessions,
                    &mut plays,
                    &mut physical_to_logical,
                    &mut bonded_sessions,
                    ConnectionAdmission {
                        id,
                        peer,
                        stream_id,
                        group,
                        peer_socket_id,
                    },
                    &global,
                )
                .await;
            }
            IngestEvent::Data { id, payload } => {
                let logical_id = physical_to_logical.get(&id).copied().unwrap_or(id);
                if let Some(session) = sessions.get_mut(&logical_id) {
                    session.push(&server.engine, &payload).await;
                }
            }
            IngestEvent::Disconnected {
                id,
                phase,
                reason,
                had_error,
            } => {
                tracing::debug!(?id, phase, %reason, had_error, "Rust SRT ingest connection disconnected");
                let logical_id = physical_to_logical.remove(&id).unwrap_or(id);
                if let Some(cancel) = plays.remove(&logical_id) {
                    cancel.cancel();
                }
                let empty_bond = bonded_sessions.iter_mut().find_map(|(key, bond)| {
                    bond.members
                        .remove(&id)
                        .then_some((key.clone(), bond.members.is_empty()))
                });
                if let Some((key, true)) = empty_bond {
                    bonded_sessions.remove(&key);
                }
                let still_bonded = bonded_sessions
                    .values()
                    .any(|bond| bond.logical_id == logical_id);
                if !still_bonded && let Some(session) = sessions.remove(&logical_id) {
                    session
                        .finish(&server.engine, Some(phase), Some(reason), had_error)
                        .await;
                }
            }
        }
    }
    pool.stop();
    for cancel in plays.into_values() {
        cancel.cancel();
    }
    for (_, session) in sessions {
        session
            .finish(
                &server.engine,
                Some("shutdown"),
                Some("Rust SRT ingest listener stopped".to_string()),
                false,
            )
            .await;
    }
}

struct ConnectionAdmission {
    id: ConnectionId,
    peer: std::net::SocketAddr,
    stream_id: String,
    group: Option<shiguredo_srt::GroupExtensionData>,
    peer_socket_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BondKey {
    group_id: u32,
    stream_id: String,
}

struct BondSession {
    logical_id: ConnectionId,
    members: HashSet<ConnectionId>,
}

#[allow(clippy::too_many_arguments)]
async fn admit_connection(
    server: &Arc<SrtServer>,
    pool: &RustIngestPool,
    sessions: &mut HashMap<ConnectionId, RustIngestSession>,
    plays: &mut HashMap<ConnectionId, tokio_util::sync::CancellationToken>,
    physical_to_logical: &mut HashMap<ConnectionId, ConnectionId>,
    bonded_sessions: &mut HashMap<BondKey, BondSession>,
    admission: ConnectionAdmission,
    global: &ResolvedSrtIngestConfig,
) {
    let ConnectionAdmission {
        id,
        peer,
        stream_id,
        group,
        peer_socket_id,
    } = admission;
    if let Some(group) = group {
        tracing::debug!(
            %peer,
            group_id = format_args!("{:#x}", group.group_id),
            ?group.group_type,
            peer_socket_id,
            "Rust SRT ingest completed bonded leg handshake"
        );
    }
    let parsed = parse_srt_stream_id(&stream_id);
    let client_ip = peer.ip().to_string();
    let mut logical_id = id;
    let is_reader = parsed.mode == SrtConnectionMode::Read;
    let accepted = if !matches!(
        parsed.mode,
        SrtConnectionMode::Publish | SrtConnectionMode::Read
    ) || parsed.stream_key.is_empty()
        || (is_reader && group.is_some())
    {
        warn!(peer = %peer, "Rust SRT ingest rejected invalid StreamID or read GROUP");
        false
    } else if server
        .security
        .is_ip_banned_for(
            if is_reader {
                crate::media::security::RateLimitScope::SrtRead
            } else {
                crate::media::security::RateLimitScope::SrtPublish
            },
            &client_ip,
        )
        .is_some()
    {
        warn!(peer = %peer, "Rust SRT ingest rejected banned SRT client");
        false
    } else {
        match authenticate_connection(
            server,
            peer,
            &parsed.stream_key,
            if is_reader {
                PipelineAccessMode::SrtRead
            } else {
                PipelineAccessMode::SrtPublish
            },
            global,
        )
        .await
        {
            None => false,
            Some(pipeline) => {
                if is_reader {
                    let active = server
                        .engine
                        .ingests
                        .active
                        .read()
                        .await
                        .contains_key(&pipeline.id);
                    let sender = pool.command_sender(id);
                    if !active {
                        warn!(
                            peer = %peer,
                            pipeline = %pipeline.id,
                            "Rust SRT read rejected without active ingest"
                        );
                        false
                    } else if let Some(sender) = sender {
                        let cancel = read::spawn(server.clone(), id, pipeline.id, sender);
                        plays.insert(id, cancel);
                        physical_to_logical.insert(id, id);
                        true
                    } else {
                        warn!(peer = %peer, "Rust SRT read rejected without worker command channel");
                        false
                    }
                } else if let Some(group) = group {
                    let key = BondKey {
                        group_id: group.group_id,
                        stream_id: normalize_stream_id(&stream_id),
                    };
                    if let Some(bond) = bonded_sessions.get_mut(&key) {
                        logical_id = bond.logical_id;
                        bond.members.insert(id);
                        physical_to_logical.insert(id, logical_id);
                        true
                    } else {
                        match RustIngestSession::create(
                            server,
                            pipeline,
                            &parsed.stream_key,
                            &peer.to_string(),
                        )
                        .await
                        {
                            None => false,
                            Some(session) => {
                                sessions.insert(id, session);
                                physical_to_logical.insert(id, id);
                                let mut members = HashSet::new();
                                members.insert(id);
                                bonded_sessions.insert(
                                    key,
                                    BondSession {
                                        logical_id: id,
                                        members,
                                    },
                                );
                                true
                            }
                        }
                    }
                } else {
                    match RustIngestSession::create(
                        server,
                        pipeline,
                        &parsed.stream_key,
                        &peer.to_string(),
                    )
                    .await
                    {
                        None => false,
                        Some(session) => {
                            sessions.insert(id, session);
                            physical_to_logical.insert(id, id);
                            true
                        }
                    }
                }
            }
        }
    };
    if !pool.authorize(id, logical_id, accepted) {
        physical_to_logical.remove(&id);
        if let Some(cancel) = plays.remove(&id) {
            cancel.cancel();
        }
        if let Some(session) = sessions.remove(&logical_id) {
            session
                .finish(
                    &server.engine,
                    Some("authorize"),
                    Some("Rust SRT ingest worker command channel closed".to_string()),
                    true,
                )
                .await;
        }
    }
}

async fn authenticate_connection(
    server: &Arc<SrtServer>,
    peer: std::net::SocketAddr,
    stream_key: &str,
    access_mode: PipelineAccessMode,
    global: &ResolvedSrtIngestConfig,
) -> Option<AuthenticatedPipeline> {
    let Some(policy) = server.ingest_policy_store.resolved_policy(stream_key) else {
        warn!(peer = %peer, "Rust SRT ingest rejected unknown stream key");
        return None;
    };
    if policy.crypto != global.crypto || policy.latency_ms != global.latency_ms {
        warn!(peer = %peer, "Rust SRT ingest rejected per-stream policy not representable by current listener");
        return None;
    }
    let pipeline = match server
        .pipeline_access
        .authenticate(access_mode, stream_key, &peer.ip().to_string())
        .await
    {
        Ok(pipeline) => pipeline,
        Err(_) => {
            warn!(peer = %peer, "Rust SRT ingest authentication failed");
            return None;
        }
    };
    Some(pipeline)
}

fn normalize_stream_id(stream_id: &str) -> String {
    stream_id.trim_matches('\0').trim().to_string()
}

fn worker_options(global: &ResolvedSrtIngestConfig) -> Result<WorkerOptions, String> {
    let (passphrase, key_length) = match &global.crypto {
        ResolvedSrtCrypto::Plaintext => (None, KeyLength::Aes128),
        ResolvedSrtCrypto::Encrypted {
            passphrase,
            pbkeylen,
        } => (
            Some(passphrase.clone()),
            match pbkeylen {
                16 => KeyLength::Aes128,
                24 => KeyLength::Aes192,
                32 => KeyLength::Aes256,
                other => return Err(format!("unsupported SRT pbkeylen {other}")),
            },
        ),
    };
    Ok(WorkerOptions {
        passphrase,
        key_length,
        tsbpd_delay: global.latency_ms as u16,
    })
}
