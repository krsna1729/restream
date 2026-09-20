//! The Tokio-facing half of the SRT ingress owner: the command and event
//! vocabulary, the start/exit types, and [`SrtIngressHandle`], Tokio's end of
//! the two bounded bridges. The owner thread itself lives in `ingress_owner`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use srt_transport::advanced::admission::LogicalPeerId;
use tokio::sync::mpsc;

use crate::media::snapshots::ListenerSocketStats;

use super::ingress_admission::ReceiverGroupId;
use super::ingress_owner::run_owner_thread;
use super::ingress_quality::PeerSampleTable;
use super::srt_policy::SrtIngestPolicyStore;

/// Tokio -> Owner command bridge capacity.
pub(crate) const INGRESS_COMMAND_CAPACITY: usize = 256;

/// Owner -> Tokio event bridge capacity.
pub(crate) const INGRESS_EVENT_CAPACITY: usize = 256;

pub(crate) enum IngressCommand {
    /// Send one SRT message payload to a connected reader peer.
    Send {
        logical_peer: LogicalPeerId,
        payload: Bytes,
    },
    /// Begin an orderly protocol disconnect of one peer. The owner retires the
    /// peer when its terminal event arrives (or after a short grace).
    Disconnect { logical_peer: LogicalPeerId },
    /// Disconnect nothing further; flush, drain the Owner and exit.
    Shutdown,
}

/// Narrow application vocabulary from the Owner to Tokio.
pub(crate) enum SrtIngressEvent {
    Connected {
        peer: SocketAddr,
        logical_peer: LogicalPeerId,
        stream_id: String,
    },
    Media {
        logical_peer: LogicalPeerId,
        payload: Bytes,
    },
    Disconnected {
        peer: SocketAddr,
        logical_peer: LogicalPeerId,
        reason: String,
    },
    /// The Owner developed an OWNER-FATAL fault; the listener is gone.
    Fault { detail: String },
}

/// What the owner thread reports when it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngressExit {
    /// `shutdown_and_drain` reached quiescence (or there was nothing to drain).
    pub(crate) quiescent: bool,
    pub(crate) fault: Option<String>,
}

/// Why the owner thread could not start.
#[derive(Debug)]
pub(crate) struct IngressStartError(pub(crate) String);

impl std::fmt::Display for IngressStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything the owner thread is built from.
pub(crate) struct IngressConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) policy_store: Arc<SrtIngestPolicyStore>,
    pub(crate) receiver_group: ReceiverGroupId,
    pub(crate) stats: Arc<ListenerSocketStats>,
    /// Where the owner publishes per-peer receive-quality samples for Tokio.
    pub(crate) samples: Arc<PeerSampleTable>,
    pub(crate) command_capacity: usize,
    pub(crate) event_capacity: usize,
}

/// Tokio's handle to the owner thread.
pub(crate) struct SrtIngressHandle {
    pub(crate) events: mpsc::Receiver<SrtIngressEvent>,
    commands: flume::Sender<IngressCommand>,
    thread: Option<std::thread::JoinHandle<IngressExit>>,
    local_addr: SocketAddr,
}

impl SrtIngressHandle {
    /// Build the runtime and the Owner on a new OS thread and wait for its
    /// verdict. A runtime or listener that cannot be built is a typed error;
    /// there is no fallback transport.
    pub(crate) async fn start(config: IngressConfig) -> Result<Self, IngressStartError> {
        let (commands_tx, commands_rx) = flume::bounded(config.command_capacity.max(1));
        let (events_tx, events_rx) = mpsc::channel(config.event_capacity.max(1));
        let (ready_tx, ready_rx) = flume::bounded::<Result<SocketAddr, String>>(1);
        let thread = std::thread::Builder::new()
            // `srt-in-<port>`: unique per listener and short enough that the
            // kernel's 15-byte thread name keeps the whole port.
            .name(format!("srt-in-{}", config.bind.port()))
            .spawn(move || run_owner_thread(config, commands_rx, events_tx, ready_tx))
            .map_err(|error| IngressStartError(format!("spawn SRT ingress thread: {error}")))?;
        match ready_rx.recv_async().await {
            Ok(Ok(local_addr)) => Ok(Self {
                events: events_rx,
                commands: commands_tx,
                thread: Some(thread),
                local_addr,
            }),
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(IngressStartError(message))
            }
            Err(_) => {
                let _ = thread.join();
                Err(IngressStartError(
                    "SRT ingress thread exited before reporting readiness".to_string(),
                ))
            }
        }
    }

    /// The address the Owner's listener socket is bound to.
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Offer a command without blocking. `Err(command)` hands it back when the
    /// bounded bridge is full (or the thread is gone), so the caller keeps its
    /// media/session state and retries instead of losing it.
    pub(crate) fn try_send(&self, command: IngressCommand) -> Result<(), IngressCommand> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                flume::TrySendError::Full(command) | flume::TrySendError::Disconnected(command) => {
                    command
                }
            })
    }

    /// Whether the owner thread's command receiver is gone (thread exited).
    pub(crate) fn is_closed(&self) -> bool {
        self.commands.is_disconnected()
    }

    /// Orderly stop: deliver `Shutdown` (bounded wait for bridge room), then
    /// join the owner thread and report its truthful verdict.
    pub(crate) async fn shutdown(mut self) -> IngressExit {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut command = IngressCommand::Shutdown;
        loop {
            match self.commands.try_send(command) {
                Ok(()) => break,
                Err(flume::TrySendError::Disconnected(_)) => break,
                Err(flume::TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    command = back;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
        // Dropping the sender also stops the owner thread if the explicit
        // command could not be queued.
        drop(self.commands);
        let Some(thread) = self.thread.take() else {
            return IngressExit {
                quiescent: true,
                fault: None,
            };
        };
        match tokio::task::spawn_blocking(move || thread.join()).await {
            Ok(Ok(exit)) => exit,
            _ => IngressExit {
                quiescent: false,
                fault: Some("SRT ingress owner thread panicked".to_string()),
            },
        }
    }
}
