//! The Tokio-facing half of the SRT ingress Owners: [`SrtIngressHandle`],
//! Tokio's end of the bounded bridges, and the start error. The vocabulary
//! the bridges carry lives in `ingress_types`; the Owner threads themselves
//! live in `ingress_owner`.
//!
//! The listener is one Owner thread, or K when `RESTREAM_SRT_INGRESS_OWNERS`
//! asks for a `SO_REUSEPORT` group (one srt-rs `owner_plans` entry each). The
//! bridges carry session lifecycle only (admission, stream probe/topology updates,
//! disconnect). Media runs to completion on the Owner that holds the session
//! (`ingress_media`); nothing per packet crosses any bridge.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use super::ingress_owner::{listener_plans, run_owner_thread};
use super::ingress_quality::QualitySample;
use super::ingress_types::{
    IngressCommand, IngressConfig, IngressExit, OwnerSeat, SrtIngressEvent,
};

/// Why the owner threads could not start.
#[derive(Debug)]
pub(crate) struct IngressStartError(pub(crate) String);

impl std::fmt::Display for IngressStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

struct OwnerLink {
    commands: flume::Sender<IngressCommand>,
    thread: Option<std::thread::JoinHandle<IngressExit>>,
}

/// Tokio's ends of the Owners' bridges out: lifecycle events, and lossy,
/// stamped receive-quality samples. [`SrtIngressHandle::start`] hands them
/// to the loop that receives them.
pub(crate) type IngressReceivers = (
    mpsc::Receiver<SrtIngressEvent>,
    mpsc::Receiver<QualitySample>,
);

/// Tokio's handle to the owner threads.
pub(crate) struct SrtIngressHandle {
    owners: Vec<OwnerLink>,
    local_addr: SocketAddr,
}

impl SrtIngressHandle {
    /// Build each Owner's runtime and listener on its own OS thread and wait
    /// for every verdict. A runtime or listener that cannot be built is a
    /// typed error (the started Owners are stopped); there is no fallback
    /// transport.
    pub(crate) async fn start(
        config: IngressConfig,
    ) -> Result<(Self, IngressReceivers), IngressStartError> {
        let plans = listener_plans(&config).map_err(IngressStartError)?;
        let (events_tx, events_rx) = mpsc::channel(config.event_capacity.max(1));
        let (telemetry_tx, telemetry_rx) = mpsc::channel(config.telemetry_capacity.max(1));
        let (inbox_tx, inbox_rx): (Vec<_>, Vec<_>) =
            plans.iter().map(|_| flume::unbounded()).unzip();
        let epoch = Instant::now();
        let mut owners = Vec::with_capacity(plans.len());
        let mut ready = Vec::with_capacity(plans.len());
        for (index, (plan, inbox)) in plans.into_iter().zip(inbox_rx).enumerate() {
            let (commands_tx, commands_rx) = flume::bounded(config.command_capacity.max(1));
            let (ready_tx, ready_rx) = flume::bounded::<Result<SocketAddr, String>>(1);
            let seat = OwnerSeat {
                index: u16::try_from(index)
                    .map_err(|_| IngressStartError("too many SRT ingress Owners".to_string()))?,
                plan,
                epoch,
                inbox,
                members: inbox_tx.clone(),
            };
            let (owner_config, events, telemetry) =
                (config.clone(), events_tx.clone(), telemetry_tx.clone());
            let spawned = std::thread::Builder::new()
                // `srt-in-<port>[-<i>]`: unique per Owner and within the
                // kernel's 15-byte thread name.
                .name(owner_thread_name(config.bind.port(), index, config.owners))
                .spawn(move || {
                    run_owner_thread(seat, owner_config, commands_rx, events, telemetry, ready_tx)
                });
            let thread = match spawned {
                Ok(thread) => thread,
                Err(error) => {
                    stop_started(owners, None).await;
                    return Err(IngressStartError(format!(
                        "spawn SRT ingress thread: {error}"
                    )));
                }
            };
            owners.push(OwnerLink {
                commands: commands_tx,
                thread: Some(thread),
            });
            ready.push(ready_rx);
        }
        let mut local_addr = None;
        for ready_rx in ready {
            let verdict = match ready_rx.recv_async().await {
                Ok(verdict) => verdict,
                Err(_) => Err("SRT ingress thread exited before reporting readiness".to_string()),
            };
            match verdict {
                Ok(addr) => {
                    local_addr.get_or_insert(addr);
                }
                Err(message) => {
                    stop_started(owners, None).await;
                    return Err(IngressStartError(message));
                }
            }
        }
        let local_addr = local_addr
            .ok_or_else(|| IngressStartError("SRT ingress started no Owner".to_string()))?;
        Ok((Self { owners, local_addr }, (events_rx, telemetry_rx)))
    }

    /// The address every Owner's listener socket is bound to.
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Offer a session command to the Owner that holds the session, without
    /// blocking. `Err(command)` hands it back when that Owner's bounded bridge
    /// is full (or its thread is gone), so the caller keeps its media/session
    /// state and retries instead of losing it.
    pub(crate) fn try_send(&self, command: IngressCommand) -> Result<(), IngressCommand> {
        let Some(link) = command
            .owner()
            .and_then(|owner| self.owners.get(usize::from(owner)))
        else {
            // Shutdown goes through `shutdown`; an unknown Owner holds nothing.
            return Ok(());
        };
        link.commands
            .try_send(command)
            .map_err(|error| match error {
                flume::TrySendError::Full(command) | flume::TrySendError::Disconnected(command) => {
                    command
                }
            })
    }

    /// Whether any Owner thread is gone. One dead member black-holes its share
    /// of the port, so the listener is down as a whole.
    pub(crate) fn is_closed(&self) -> bool {
        self.owners
            .iter()
            .any(|link| link.commands.is_disconnected())
    }

    /// Deliver the already-accepted pending command before `Shutdown`, within
    /// the existing shared shutdown deadline, then join all Owner threads.
    pub(crate) async fn shutdown(self, pending: Option<IngressCommand>) -> IngressExit {
        stop_started(self.owners, pending).await
    }

    /// Controlled transport endpoints for production-control backpressure
    /// tests. Only channel construction differs; routing/queue logic is real.
    #[cfg(test)]
    pub(super) fn test_channels(
        owner_count: usize,
    ) -> (
        Self,
        Vec<flume::Receiver<IngressCommand>>,
        mpsc::Sender<SrtIngressEvent>,
        mpsc::Receiver<SrtIngressEvent>,
    ) {
        let mut owners = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..owner_count {
            let (commands, receiver) = flume::bounded(1);
            owners.push(OwnerLink {
                commands,
                thread: None,
            });
            receivers.push(receiver);
        }
        let (events_tx, events) = mpsc::channel(1);
        (
            Self {
                owners,
                local_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            },
            receivers,
            events_tx,
            events,
        )
    }
}

fn owner_thread_name(port: u16, index: usize, owners: usize) -> String {
    if owners > 1 {
        format!("srt-in-{port}-{index}")
    } else {
        format!("srt-in-{port}")
    }
}

/// Shut down and join every Owner in `owners`: all quiescent, first fault.
async fn stop_started(owners: Vec<OwnerLink>, pending: Option<IngressCommand>) -> IngressExit {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut delivered = true;
    if let Some(command) = pending
        && let Some(link) = command
            .owner()
            .and_then(|index| owners.get(usize::from(index)))
    {
        delivered &= send_before_deadline(&link.commands, command, deadline).await;
    }
    for link in &owners {
        delivered &= send_before_deadline(&link.commands, IngressCommand::Shutdown, deadline).await;
    }
    let mut exit = IngressExit {
        quiescent: delivered,
        fault: None,
    };
    for OwnerLink { commands, thread } in owners {
        // Dropping the sender also stops the owner thread if the explicit
        // command could not be queued.
        drop(commands);
        let Some(thread) = thread else {
            continue;
        };
        let verdict = match tokio::task::spawn_blocking(move || thread.join()).await {
            Ok(Ok(verdict)) => verdict,
            _ => IngressExit {
                quiescent: false,
                fault: Some("SRT ingress owner thread panicked".to_string()),
            },
        };
        exit.quiescent &= verdict.quiescent;
        if exit.fault.is_none() {
            exit.fault = verdict.fault;
        }
    }
    exit
}

async fn send_before_deadline(
    commands: &flume::Sender<IngressCommand>,
    mut command: IngressCommand,
    deadline: Instant,
) -> bool {
    loop {
        match commands.try_send(command) {
            Ok(()) | Err(flume::TrySendError::Disconnected(_)) => return true,
            Err(flume::TrySendError::Full(back)) => {
                if Instant::now() >= deadline {
                    return false;
                }
                command = back;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
}
