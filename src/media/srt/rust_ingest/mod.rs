use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use shiguredo_srt::KeyLength;
use tokio::sync::mpsc::{self, Receiver};
use tracing::{error, info, warn};

use crate::config::rust_srt_ingest_worker_count;
use crate::domain::srt_ingest::{ResolvedSrtCrypto, ResolvedSrtIngestConfig};
use crate::media::ingest_auth::PipelineAccessMode;

use super::SrtServer;
use super::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};
use session::RustIngestSession;
use types::{ConnectionId, IngestEvent, WorkerCommand};
use worker::WorkerOptions;

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

    fn authorize(&self, id: ConnectionId, accepted: bool) -> bool {
        self.commands.get(id.worker).is_some_and(|sender| {
            sender
                .try_send(WorkerCommand::Authorize { id, accepted })
                .is_ok()
        })
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
    while let Some(event) = events.recv().await {
        match event {
            IngestEvent::Connected {
                id,
                peer,
                stream_id,
            } => {
                admit_connection(server, &pool, &mut sessions, id, peer, stream_id, &global).await;
            }
            IngestEvent::Data { id, payload } => {
                if let Some(session) = sessions.get_mut(&id) {
                    session.push(&server.engine, &payload).await;
                }
            }
            IngestEvent::Disconnected {
                id,
                phase,
                reason,
                had_error,
            } => {
                if let Some(session) = sessions.remove(&id) {
                    session
                        .finish(&server.engine, Some(phase), Some(reason), had_error)
                        .await;
                }
            }
        }
    }
    pool.stop();
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

async fn admit_connection(
    server: &Arc<SrtServer>,
    pool: &RustIngestPool,
    sessions: &mut HashMap<ConnectionId, RustIngestSession>,
    id: ConnectionId,
    peer: std::net::SocketAddr,
    stream_id: String,
    global: &ResolvedSrtIngestConfig,
) {
    let parsed = parse_srt_stream_id(&stream_id);
    let client_ip = peer.ip().to_string();
    let accepted = if parsed.mode != SrtConnectionMode::Publish || parsed.stream_key.is_empty() {
        warn!(peer = %peer, "Rust SRT ingest rejected invalid publish StreamID");
        false
    } else if server
        .security
        .is_ip_banned_for(
            crate::media::security::RateLimitScope::SrtPublish,
            &client_ip,
        )
        .is_some()
    {
        warn!(peer = %peer, "Rust SRT ingest rejected banned publisher");
        false
    } else {
        admit_authenticated(server, sessions, id, peer, &parsed.stream_key, global).await
    };
    if !pool.authorize(id, accepted)
        && let Some(session) = sessions.remove(&id)
    {
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

async fn admit_authenticated(
    server: &Arc<SrtServer>,
    sessions: &mut HashMap<ConnectionId, RustIngestSession>,
    id: ConnectionId,
    peer: std::net::SocketAddr,
    stream_key: &str,
    global: &ResolvedSrtIngestConfig,
) -> bool {
    let Some(policy) = server.ingest_policy_store.resolved_policy(stream_key) else {
        warn!(peer = %peer, "Rust SRT ingest rejected unknown stream key");
        return false;
    };
    if policy.crypto != global.crypto || policy.latency_ms != global.latency_ms {
        warn!(peer = %peer, "Rust SRT ingest rejected per-stream policy not representable by current listener");
        return false;
    }
    let pipeline = match server
        .pipeline_access
        .authenticate(
            PipelineAccessMode::SrtPublish,
            stream_key,
            &peer.ip().to_string(),
        )
        .await
    {
        Ok(pipeline) => pipeline,
        Err(_) => {
            warn!(peer = %peer, "Rust SRT ingest authentication failed");
            return false;
        }
    };
    let Some(session) =
        RustIngestSession::create(server, pipeline, stream_key, &peer.to_string()).await
    else {
        warn!(peer = %peer, "Rust SRT ingest rejected duplicate publisher");
        return false;
    };
    sessions.insert(id, session);
    true
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
