//! The shard's SRT transport: ONE Compio runtime and at most ONE
//! `srt_transport::compio::Owner` per address family (IPv4, IPv6), all born,
//! driven and dropped on the shard OS thread.
//!
//! Each `Owner` owns exactly one shared caller UDP socket (one family), a
//! bounded caller pool, a fixed TX pool/lane set and the receive path. Leaves
//! hold a [`SrtCaller`] (family + `LogicalCallerId`) and never a transport.
//! This module is the only place that touches the runtime or an Owner; it
//! exposes narrow concrete operations, not a runtime abstraction.
//!
//! Phases the backend relies on (see `SrtShardBackend::begin_batch`):
//! [`SrtOwners::service`] runs each existing Owner ONCE under a finite
//! [`OwnerServiceBudget`]; [`SrtOwners::drain_events`] then drains bounded
//! event/failure queues into a reusable scratch vector. Payload submission
//! (`send`) only enqueues into protocol state and never services.

use std::future::Future;
use std::pin::pin;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::future::{Either, pending, select};
use srt_proto::Timestamp;
use srt_transport::advanced::caller::{
    CallerEvent, CallerGroupFault, LogicalCallerId, LogicalCallerState, LogicalCallerStats,
    PoolEvent, PoolOutcome, PoolRequestId,
};
use srt_transport::advanced::driver::OutputDrainBudget;
use srt_transport::advanced::sink::TxAttribution;
use srt_transport::compio::{
    CompioProductionProfile, ManagedRxSubstrate, Owner, OwnerFault, OwnerRxMode,
    OwnerServiceBudget, ProductionRuntimeConfig, RxModePolicy, TxFailureClass, TxFailureEvent,
    observe_production_runtime, production_runtime_builder,
};

use crate::media::egress::command::EgressCommand;
use crate::media::egress::metrics::{OwnerFamilyMetrics, ShardMetrics};
use crate::media::egress::shard::EgressShardIdleWake;
use crate::media::srt::{AddressFamily, SrtConnectKind, SrtConnectRequest, SrtSendResult};

/// Concurrent datagram sends per family Owner (its TX pool and lane count,
/// "K"). The retired native driver allowed 16 concurrent UDP send operations
/// per family; that physical in-flight envelope is kept as the starting point.
/// Unsent protocol output waits in bounded protocol state, not in a Restream
/// transport queue, so this is the whole TX envelope of one family.
pub(crate) const SRT_OWNER_TX_CAPACITY: usize = 16;

/// Runtime TX envelope: both family Owners may exist at once, so the single
/// shard runtime is sized for `AddressFamily::COUNT * K`, not just K.
pub(crate) const SRT_RUNTIME_TX_ENVELOPE: usize = AddressFamily::COUNT * SRT_OWNER_TX_CAPACITY;

/// Largest datagram an Owner will carry: SRT live payload (1316) + header and
/// GCM tag fit under the 1500-byte control ceiling, which therefore dominates.
pub(crate) const SRT_OWNER_WIRE_CEILING: usize = 1500;

/// Per-drain cap on each bounded Owner event queue.
const EVENT_DRAIN: usize = 64;

/// How long a disconnected caller may keep its SHUTDOWN datagram in flight
/// before it is removed from the Owner regardless.
const CLOSING_GRACE: Duration = Duration::from_millis(500);

/// Closing callers examined per batch. Bounds the work at the closing
/// population, never the whole leaf population.
const CLOSING_CHECKS_PER_BATCH: usize = 32;

/// Reserve carved out of the shard drain window for Owner teardown
/// (`shutdown_and_drain`). Leaves flush inside `drain - reserve`, so the whole
/// shutdown stays inside the generic drain deadline. Never more than half the
/// window.
pub(crate) const OWNER_SHUTDOWN_RESERVE: Duration = Duration::from_millis(250);

/// What a leaf needs to address its logical caller. It owns nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SrtCaller {
    pub(crate) family: AddressFamily,
    pub(crate) id: LogicalCallerId,
}

/// Builds a shard's Compio runtime. Production always uses
/// [`production_runtime`] (forced io_uring, no fallback); the type is an
/// injection seam so the typed startup-failure path is testable without a
/// seccomp-restricted host.
pub(crate) type RuntimeBuilder =
    fn(ProductionRuntimeConfig) -> Result<compio::runtime::Runtime, String>;

/// The one production runtime: forced io_uring. There is no fallback driver;
/// when io_uring cannot be created the shard does not start.
pub(crate) fn production_runtime(
    config: ProductionRuntimeConfig,
) -> Result<compio::runtime::Runtime, String> {
    production_runtime_builder(config)?
        .build()
        .map_err(|error| format!("SRT egress Compio runtime failed to build: {error}"))
}

/// Attribution for the managed-RX substrate observation, kept distinct so an
/// operator can tell a container/seccomp policy denial (`EPERM`/`EACCES`) from
/// an unsupported kernel feature (`EINVAL`). Log-only; never a metric label.
pub(crate) fn substrate_diagnosis(substrate: ManagedRxSubstrate) -> &'static str {
    match substrate {
        ManagedRxSubstrate::Available => "available",
        ManagedRxSubstrate::NotIoUring => "runtime-not-io_uring",
        ManagedRxSubstrate::MultishotRecvUnsupported => "recvmsg-multishot-unsupported",
        ManagedRxSubstrate::BufferRingRegistrationFailed(1 | 13) => "buffer-ring-denied-by-policy",
        ManagedRxSubstrate::BufferRingRegistrationFailed(22) => "buffer-ring-unsupported-by-kernel",
        ManagedRxSubstrate::BufferRingRegistrationFailed(_) => "buffer-ring-registration-failed",
    }
}

/// Configuration that is fixed for the life of a shard.
///
/// The Owner is a resource governor: it owns pool CAPACITY and the finite
/// service budget, never a request's timeout. Each output's connect deadline
/// travels on its own `CallerConfig`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SrtOwnerSettings {
    /// Per-Owner caller-pool `max_in_flight`: the transport's connect
    /// admission bound (replaces the old application connect semaphore).
    pub(crate) caller_max_in_flight: std::num::NonZeroUsize,
    /// Upstream defaults, independent of pool capacity and fan-out: protocol
    /// TX (`max_actions`) and pool/lifecycle maintenance
    /// (`max_maintenance_actions`) progress on separate finite axes.
    pub(crate) service_budget: OwnerServiceBudget,
    /// `ManagedPreferred` keeps a host without provided-buffer rings running
    /// on the readiness receiver; the selected mode is always observable
    /// (`OwnerRxMode`, shard metrics, startup log), never silently claimed.
    pub(crate) rx_policy: RxModePolicy,
    /// How the shard's Compio runtime is built (production: forced io_uring).
    pub(crate) runtime_builder: RuntimeBuilder,
}

impl SrtOwnerSettings {
    pub(crate) fn new(caller_max_in_flight: usize) -> Self {
        Self {
            caller_max_in_flight: std::num::NonZeroUsize::new(caller_max_in_flight.max(1))
                .expect("clamped nonzero"),
            service_budget: OwnerServiceBudget::default(),
            rx_policy: RxModePolicy::ManagedPreferred,
            runtime_builder: production_runtime,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_runtime_builder(mut self, runtime_builder: RuntimeBuilder) -> Self {
        self.runtime_builder = runtime_builder;
        self
    }
}

/// One observed transport event, already attributed to a family.
pub(crate) enum SrtOwnerEvent {
    /// A queued connect was admitted and now has a logical caller.
    Admitted {
        family: AddressFamily,
        request_id: PoolRequestId,
        caller: LogicalCallerId,
    },
    /// An admitted attempt reached its deadline before `Connected`.
    Expired {
        family: AddressFamily,
        caller: LogicalCallerId,
    },
    /// A queued connect failed to admit.
    RequestFailed {
        family: AddressFamily,
        request_id: PoolRequestId,
        reason: String,
    },
    Connected {
        family: AddressFamily,
        caller: LogicalCallerId,
    },
    Disconnected {
        family: AddressFamily,
        caller: LogicalCallerId,
    },
    /// A peer-local or transient send failure, attributed to one caller
    /// (and leg). Never an Owner-wide fault.
    TxFailure {
        family: AddressFamily,
        attribution: TxAttribution,
        class: TxFailureClass,
    },
    /// Protocol output for a caller/leg could not be materialized; the
    /// session/leg is quarantined until the application retires it.
    OutputFailure {
        family: AddressFamily,
        attribution: TxAttribution,
    },
    /// A bonded leg answered from a different remote receiving group than the
    /// one its logical caller is bound to: the output is not an SRT bond.
    /// The upstream typed fault is kept whole (caller, leg peer, member and
    /// both group ids).
    PeerGroupCollision {
        family: AddressFamily,
        fault: CallerGroupFault,
    },
}

/// Result of one batch service across the existing Owners.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ServiceSummary {
    /// Any Owner reported remaining bounded work, or an event queue was full:
    /// the backend schedules another ordinary ready visit.
    pub(crate) work_remaining: bool,
    /// Datagrams received or completions reaped: window/ACK state may have
    /// changed for parked callers.
    pub(crate) activity: bool,
    /// Families whose Owner faulted for the first time in this batch.
    pub(crate) newly_faulted: [bool; AddressFamily::COUNT],
}

#[derive(Debug, Clone, Copy, Default)]
struct FamilyCounters {
    rx_packets: u64,
    rx_bytes: u64,
    tx_packets: u64,
    tx_bytes: u64,
    completed_ok: u64,
    short_sends: u64,
    failed_sends: u64,
    peer_local_failures: u64,
    transient_failures: u64,
    protocol_output_failures: u64,
    service_visits: u64,
    budget_exhausted_visits: u64,
    /// Protocol (TX) output actions, and pool/lifecycle maintenance actions:
    /// separate upstream budget axes, never mixed.
    service_actions: u64,
    maintenance_actions: u64,
    peer_group_collisions: u64,
}

struct FamilyOwner {
    owner: Owner,
    fault_reported: bool,
    /// The selected receive mode was logged once, when the Owner attached.
    attach_logged: bool,
    counters: FamilyCounters,
}

struct ClosingCaller {
    family: AddressFamily,
    id: LogicalCallerId,
    since: Instant,
}

/// The shard's runtime and family Owners. Field order matters: Owners (and
/// the tasks they own) drop before the runtime that runs them.
pub(crate) struct SrtOwners {
    owners: [Option<FamilyOwner>; AddressFamily::COUNT],
    closing: std::collections::VecDeque<ClosingCaller>,
    pool_scratch: Vec<PoolEvent>,
    caller_scratch: Vec<CallerEvent>,
    failure_scratch: Vec<TxFailureEvent>,
    output_failure_scratch: Vec<srt_transport::advanced::sink::ProtocolOutputFailure>,
    group_fault_scratch: Vec<CallerGroupFault>,
    runtime: compio::runtime::Runtime,
    epoch: Instant,
    profile: CompioProductionProfile,
    substrate: ManagedRxSubstrate,
    settings: SrtOwnerSettings,
    /// Cumulative `shutdown_and_drain` outcomes, for observability.
    shutdown_incomplete: u64,
    /// The OS thread this runtime and its Owners were built on. Every use and
    /// the drop must be on it (checked in debug builds; the types are `!Send`
    /// so a cross-thread move cannot compile).
    home_thread: std::thread::ThreadId,
}

impl SrtOwners {
    /// Build the shard's production Compio runtime and observe THAT runtime
    /// (never a probe) for its managed-RX substrate. Must run on the shard OS
    /// thread. Fails with a typed message when the production runtime cannot
    /// be built (e.g. io_uring unavailable): the shard then does not start.
    pub(crate) fn new(settings: SrtOwnerSettings) -> Result<Self, String> {
        let config =
            ProductionRuntimeConfig::for_owner(SRT_RUNTIME_TX_ENVELOPE, SRT_OWNER_WIRE_CEILING);
        let runtime = (settings.runtime_builder)(config)?;
        let profile = runtime.block_on(observe_production_runtime(
            &runtime,
            SRT_RUNTIME_TX_ENVELOPE,
            SRT_OWNER_WIRE_CEILING,
        ));
        let substrate = profile.managed_rx_substrate();
        tracing::info!(
            driver = %profile.driver_type,
            compio = %profile.compio_version,
            io_uring = profile.is_io_uring,
            managed_rx = ?substrate,
            substrate_diagnosis = substrate_diagnosis(substrate),
            tx_capacity_per_family = SRT_OWNER_TX_CAPACITY,
            runtime_tx_envelope = SRT_RUNTIME_TX_ENVELOPE,
            wire_ceiling = SRT_OWNER_WIRE_CEILING,
            "srt egress shard runtime ready"
        );
        Ok(Self {
            owners: [None, None],
            closing: std::collections::VecDeque::new(),
            pool_scratch: Vec::with_capacity(EVENT_DRAIN),
            caller_scratch: Vec::with_capacity(EVENT_DRAIN),
            failure_scratch: Vec::with_capacity(EVENT_DRAIN),
            output_failure_scratch: Vec::with_capacity(EVENT_DRAIN),
            group_fault_scratch: Vec::with_capacity(EVENT_DRAIN),
            runtime,
            epoch: Instant::now(),
            profile,
            substrate,
            settings,
            shutdown_incomplete: 0,
            home_thread: std::thread::current().id(),
        })
    }

    /// One monotonic shard-local protocol time shared by every Owner here.
    fn assert_home_thread(&self) {
        debug_assert_eq!(
            std::thread::current().id(),
            self.home_thread,
            "SRT shard runtime used off its shard thread"
        );
    }

    pub(crate) fn timestamp(&self) -> Timestamp {
        Timestamp::from_micros(self.epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
    }

    pub(crate) fn has_owner(&self) -> bool {
        self.owners.iter().any(Option::is_some)
    }

    #[cfg(test)]
    pub(crate) fn owner_count(&self) -> usize {
        self.owners.iter().flatten().count()
    }

    #[cfg(test)]
    pub(crate) fn rx_mode(&self, family: AddressFamily) -> Option<OwnerRxMode> {
        self.owners[family.index()].as_ref()?.owner.rx_mode()
    }

    fn new_family_owner(&self) -> Result<FamilyOwner, String> {
        let mut owner = Owner::new_with_ceiling(SRT_OWNER_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
        owner
            .set_rx_substrate(self.substrate)
            .map_err(|error| error.to_string())?;
        owner.set_rx_mode_policy(self.settings.rx_policy);
        owner
            .set_caller_pool_capacity(self.settings.caller_max_in_flight)
            .map_err(|error| error.to_string())?;
        Ok(FamilyOwner {
            owner,
            fault_reported: false,
            attach_logged: false,
            counters: FamilyCounters::default(),
        })
    }

    /// Admit one output's connect request into its family Owner, creating the
    /// Owner lazily on the first output of that family. `Owner::connect` /
    /// `connect_bonded` are transactional, so a refusal leaves nothing behind
    /// (a freshly created but never-attached Owner is kept for the next try).
    pub(crate) fn connect(&mut self, request: SrtConnectRequest) -> Result<PoolOutcome, String> {
        self.assert_home_thread();
        let index = request.family.index();
        if self.owners[index].is_none() {
            self.owners[index] = Some(self.new_family_owner()?);
        }
        let family_owner = self.owners[index].as_mut().expect("just created");
        if let Some(fault) = family_owner.owner.fault() {
            return Err(format!(
                "SRT {:?} Owner is faulted ({fault:?}); no new callers are admitted",
                request.family
            ));
        }
        let now = self.epoch.elapsed();
        let now = Timestamp::from_micros(now.as_micros().min(u128::from(u64::MAX)) as u64);
        let owner = &mut family_owner.owner;
        // Socket creation and registration need the runtime context.
        let outcome = self.runtime.block_on(async {
            match &request.kind {
                SrtConnectKind::Direct(config) => owner.connect(config, now),
                SrtConnectKind::Bonded(config) => owner.connect_bonded(config, now),
            }
            .map_err(|error| error.to_string())
        });
        // Once, when the family Owner has actually attached and selected its
        // receive mode (never per packet): make a RawReadiness fallback --
        // and why the managed substrate was unavailable -- impossible to miss.
        if outcome.is_ok()
            && !family_owner.attach_logged
            && let Some(rx_mode) = family_owner.owner.rx_mode()
        {
            family_owner.attach_logged = true;
            tracing::info!(
                family = ?request.family,
                driver = %self.profile.driver_type,
                io_uring = self.profile.is_io_uring,
                substrate = ?self.substrate,
                substrate_diagnosis = substrate_diagnosis(self.substrate),
                rx_mode = ?rx_mode,
                rx_policy = ?self.settings.rx_policy,
                "srt egress owner attached"
            );
        }
        outcome
    }

    /// Submit one payload fragment to a logical caller. Only enqueues into
    /// protocol state; transport service is the batch phase, never here.
    pub(crate) fn send(
        &mut self,
        caller: &SrtCaller,
        payload: &Bytes,
        now: Timestamp,
    ) -> SrtSendResult {
        let Some(family_owner) = self.owners[caller.family.index()].as_mut() else {
            return SrtSendResult::Failed {
                reason: "srt-owner-missing",
                detail: "the family Owner for this caller no longer exists".to_string(),
                retryable: false,
            };
        };
        let Some(mut logical) = family_owner.owner.logical_caller_mut(&caller.id) else {
            return SrtSendResult::PeerClosed;
        };
        match logical.state() {
            None | Some(LogicalCallerState::Disconnected) => SrtSendResult::PeerClosed,
            Some(LogicalCallerState::Connecting) => SrtSendResult::WouldBlock,
            Some(LogicalCallerState::Connected) => {
                if !logical.can_send() {
                    return SrtSendResult::WouldBlock;
                }
                match logical.send_shared(payload.clone(), now) {
                    Ok(0) => SrtSendResult::WouldBlock,
                    Ok(_) => SrtSendResult::Accepted {
                        bytes: payload.len(),
                    },
                    Err(error) if error.kind == srt_proto::ErrorKind::InvalidState => {
                        SrtSendResult::PeerClosed
                    }
                    Err(error) => SrtSendResult::Failed {
                        reason: "srt-owner-send",
                        detail: error.to_string(),
                        retryable: true,
                    },
                }
            }
        }
    }

    /// Public logical-caller statistics (sender backlog, RTT, loss, ...).
    pub(crate) fn stats(&self, caller: &SrtCaller) -> Option<LogicalCallerStats> {
        self.owners[caller.family.index()]
            .as_ref()?
            .owner
            .logical_caller(&caller.id)?
            .stats()
    }

    /// Begin an orderly close: queue the protocol SHUTDOWN and let the next
    /// batches flush it. The Owner-side caller is removed once it reaches
    /// `Disconnected` or after [`CLOSING_GRACE`].
    pub(crate) fn begin_close(&mut self, caller: &SrtCaller) {
        let now = self.timestamp();
        let Some(family_owner) = self.owners[caller.family.index()].as_mut() else {
            return;
        };
        // An attempt that never connected has no SHUTDOWN to flush and holds a
        // pool permit: retire it now so a queued request can be admitted.
        let established = family_owner
            .owner
            .logical_caller(&caller.id)
            .and_then(|logical| logical.state())
            == Some(LogicalCallerState::Connected);
        if !established {
            drop(family_owner.owner.remove_caller(caller.id));
            return;
        }
        if let Some(mut logical) = family_owner.owner.logical_caller_mut(&caller.id) {
            logical.disconnect(now);
            self.closing.push_back(ClosingCaller {
                family: caller.family,
                id: caller.id,
                since: Instant::now(),
            });
        }
    }

    /// Remove a caller that has no pending protocol obligation (an attempt
    /// that never connected, a stale queued admission).
    pub(crate) fn remove_now(&mut self, caller: &SrtCaller) {
        if let Some(family_owner) = self.owners[caller.family.index()].as_mut() {
            drop(family_owner.owner.remove_caller(caller.id));
        }
    }

    /// Callers awaiting removal after `begin_close`.
    #[cfg(test)]
    pub(crate) fn closing_len(&self) -> usize {
        self.closing.len()
    }

    fn reap_closing(&mut self) {
        let mut checked = 0;
        let len = self.closing.len();
        while checked < CLOSING_CHECKS_PER_BATCH.min(len) {
            checked += 1;
            let Some(entry) = self.closing.pop_front() else {
                break;
            };
            let Some(family_owner) = self.owners[entry.family.index()].as_mut() else {
                continue;
            };
            let done = match family_owner.owner.logical_caller(&entry.id) {
                None => true,
                Some(logical) => {
                    matches!(
                        logical.state(),
                        None | Some(LogicalCallerState::Disconnected)
                    ) || entry.since.elapsed() >= CLOSING_GRACE
                }
            };
            if done {
                drop(family_owner.owner.remove_caller(entry.id));
            } else {
                self.closing.push_back(entry);
            }
        }
    }

    /// Service every existing Owner exactly once under the finite budget.
    /// The single place transport service happens per ready batch.
    pub(crate) fn service(&mut self) -> ServiceSummary {
        self.assert_home_thread();
        let mut summary = ServiceSummary::default();
        if !self.has_owner() {
            return summary;
        }
        let now = self.timestamp();
        let budget = self.settings.service_budget;
        // `block_on` returns as soon as its future is ready without touching
        // the driver, so a shard that never idles would never reap TX
        // completions or receive datagrams. One non-blocking driver poll per
        // ready batch keeps both flowing.
        self.runtime.poll_with(Some(Duration::ZERO));
        self.runtime.run();
        let owners = &mut self.owners;
        self.runtime.block_on(async {
            for (index, slot) in owners.iter_mut().enumerate() {
                let Some(family_owner) = slot.as_mut() else {
                    continue;
                };
                let report = family_owner.owner.service(now, budget).await;
                let counters = &mut family_owner.counters;
                counters.service_visits += 1;
                counters.service_actions += report.actions as u64;
                counters.maintenance_actions += report.maintenance_actions as u64;
                counters.rx_packets += report.rx_packets as u64;
                counters.rx_bytes += report.rx_bytes as u64;
                counters.tx_packets += report.tx_packets_submitted as u64;
                counters.tx_bytes += report.tx_bytes_submitted as u64;
                counters.completed_ok += report.tx_completed_ok as u64;
                counters.short_sends += report.tx_short_sends as u64;
                counters.failed_sends += report.tx_failed_sends as u64;
                counters.peer_local_failures += report.tx_peer_local_failures as u64;
                counters.transient_failures += report.tx_transient_failures as u64;
                counters.protocol_output_failures += report.protocol_output_failures as u64;
                if report.budget_exhausted {
                    counters.budget_exhausted_visits += 1;
                }
                summary.work_remaining |= report.work_remaining;
                summary.activity |= report.rx_packets > 0 || report.completions_reaped > 0;
                if family_owner.owner.fault().is_some() && !family_owner.fault_reported {
                    family_owner.fault_reported = true;
                    summary.newly_faulted[index] = true;
                }
            }
        });
        self.reap_closing();
        summary
    }

    /// Drain bounded Owner event/failure queues into `out` (cleared first).
    /// Returns `true` when any queue filled its cap, i.e. more may be pending
    /// and the backend should schedule another ordinary ready visit.
    pub(crate) fn drain_events(&mut self, out: &mut Vec<SrtOwnerEvent>) -> bool {
        out.clear();
        let mut more = false;
        let cap = OutputDrainBudget::default().max_actions.min(EVENT_DRAIN);
        for (index, slot) in self.owners.iter_mut().enumerate() {
            let Some(family_owner) = slot.as_mut() else {
                continue;
            };
            let family = if index == 0 {
                AddressFamily::V4
            } else {
                AddressFamily::V6
            };
            let owner = &mut family_owner.owner;
            owner.poll_caller_pool_events(&mut self.pool_scratch);
            let pool_drained_fully = self.pool_scratch.len() < cap;
            more |= !pool_drained_fully;
            for event in self.pool_scratch.drain(..) {
                match event {
                    PoolEvent::Admitted {
                        request_id,
                        caller_id,
                    } => out.push(SrtOwnerEvent::Admitted {
                        family,
                        request_id,
                        caller: caller_id,
                    }),
                    PoolEvent::Expired { caller_id, .. } => out.push(SrtOwnerEvent::Expired {
                        family,
                        caller: caller_id,
                    }),
                    PoolEvent::Failed { request_id, reason } => {
                        out.push(SrtOwnerEvent::RequestFailed {
                            family,
                            request_id,
                            reason,
                        });
                    }
                    PoolEvent::Queued { .. } | PoolEvent::Cancelled { .. } => {}
                }
            }
            owner.poll_caller_events(&mut self.caller_scratch);
            more |= self.caller_scratch.len() >= cap;
            for event in self.caller_scratch.drain(..) {
                match event.event {
                    srt_proto::ConnectionEvent::Connected => out.push(SrtOwnerEvent::Connected {
                        family,
                        caller: event.id,
                    }),
                    srt_proto::ConnectionEvent::Disconnected { .. } => {
                        out.push(SrtOwnerEvent::Disconnected {
                            family,
                            caller: event.id,
                        });
                    }
                    _ => {}
                }
            }
            owner.poll_tx_failures(cap, &mut self.failure_scratch);
            more |= self.failure_scratch.len() >= cap;
            for failure in self.failure_scratch.drain(..) {
                out.push(SrtOwnerEvent::TxFailure {
                    family,
                    attribution: failure.attribution,
                    class: failure.class,
                });
            }
            owner.poll_caller_output_failures(cap, &mut self.output_failure_scratch);
            more |= self.output_failure_scratch.len() >= cap;
            for failure in self.output_failure_scratch.drain(..) {
                out.push(SrtOwnerEvent::OutputFailure {
                    family,
                    attribution: failure.attribution,
                });
            }
            // A peer-group fault names a LOGICAL CALLER, so it may only be
            // delivered after every pool `Admitted` that could introduce that
            // caller to the backend: they precede it in `out` when the pool
            // queue drained fully this pass. If it did not, the (non-lossy)
            // upstream fault queue simply waits for the next pass.
            if pool_drained_fully {
                owner.poll_caller_group_faults(cap, &mut self.group_fault_scratch);
                more |= self.group_fault_scratch.len() >= cap;
                family_owner.counters.peer_group_collisions +=
                    self.group_fault_scratch.len() as u64;
                for fault in self.group_fault_scratch.drain(..) {
                    out.push(SrtOwnerEvent::PeerGroupCollision { family, fault });
                }
            }
        }
        more
    }

    /// The longest the shard may park: `max_wait` shortened to each Owner's
    /// next protocol/pool deadline. Saturating; SRT protocol deadlines stay
    /// inside the Owners and are never mirrored into Restream's timer wheel.
    pub(crate) fn park_bound(&mut self, max_wait: Duration) -> Duration {
        let now = self.timestamp();
        let default_us = u64::try_from(max_wait.as_micros()).unwrap_or(u64::MAX);
        let mut wait_us = default_us;
        for family_owner in self.owners.iter_mut().flatten() {
            wait_us = wait_us.min(family_owner.owner.time_until_next_deadline(now, default_us));
        }
        Duration::from_micros(wait_us).min(max_wait)
    }

    /// Park the shard thread INSIDE the shard's Compio runtime until a
    /// command arrives, an Owner has network/completion activity, or `wait`
    /// elapses. Commands win ties. Nothing here services an Owner or touches
    /// protocol state: a wake only says what woke it.
    pub(crate) fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: Duration,
    ) -> EgressShardIdleWake {
        self.assert_home_thread();
        let wait = self.park_bound(max_wait);
        let [v4, v6] = &mut self.owners;
        self.runtime.block_on(async {
            let command = pin!(async {
                match commands.recv_async().await {
                    Ok(command) => EgressShardIdleWake::Command(command),
                    Err(_) => EgressShardIdleWake::Disconnected,
                }
            });
            let activity_v4 = pin!(owner_activity(v4.as_mut(), wait));
            let activity_v6 = pin!(owner_activity(v6.as_mut(), wait));
            let timeout = pin!(async {
                compio::time::sleep(wait).await;
                EgressShardIdleWake::Timeout
            });
            // Commands are polled first so they win ties.
            first_ready(
                command,
                pin!(first_ready(
                    pin!(first_ready(activity_v4, activity_v6)),
                    timeout
                )),
            )
            .await
        })
    }

    /// Canonical teardown of every instantiated Owner, each bounded by
    /// `timeout`. Removes lingering closing callers first. Returns whether
    /// every Owner reached quiescence (`tx_in_flight == 0`, pool whole, lanes
    /// joined, managed RX consumer gone); a miss is counted, never hidden.
    pub(crate) fn shutdown(&mut self, timeout: Duration) -> bool {
        self.assert_home_thread();
        self.log_final_counters();
        while let Some(entry) = self.closing.pop_front() {
            if let Some(family_owner) = self.owners[entry.family.index()].as_mut() {
                drop(family_owner.owner.remove_caller(entry.id));
            }
        }
        let mut all = true;
        let owners = &mut self.owners;
        let incomplete = &mut self.shutdown_incomplete;
        self.runtime.block_on(async {
            for family_owner in owners.iter_mut().flatten() {
                if !family_owner.owner.shutdown_and_drain(timeout).await {
                    all = false;
                    *incomplete += 1;
                    tracing::warn!(
                        fault = ?family_owner.owner.fault(),
                        in_flight = family_owner.owner.tx_in_flight(),
                        "srt egress Owner did not reach quiescence within its shutdown bound"
                    );
                }
            }
        });
        all
    }

    /// One line per attached family at shard shutdown: the cumulative Owner
    /// counters, so a run's receive mode, service rate and TX pool behavior
    /// are on record (low-cardinality; no caller identities).
    fn log_final_counters(&self) {
        let mut metrics = ShardMetrics::default();
        self.observe(&mut metrics);
        for (index, owner) in metrics.srt_owners.iter().enumerate() {
            if !owner.present {
                continue;
            }
            tracing::info!(
                family = if index == 0 { "v4" } else { "v6" },
                managed_rx = owner.managed_rx,
                service_visits = owner.service_visits,
                service_actions = owner.service_actions,
                maintenance_actions = owner.maintenance_actions,
                service_budget_exhausted = owner.service_budget_exhausted,
                rx_packets = owner.rx_packets,
                rx_truncated = owner.rx_truncated,
                rx_ring_dropped = owner.rx_ring_dropped,
                tx_packets = owner.tx_packets,
                tx_completed_ok = owner.tx_completed_ok,
                tx_high_water = owner.tx_high_water,
                tx_exhaustions = owner.tx_exhaustions,
                caller_expired = owner.caller_expired,
                peer_group_collisions = owner.peer_group_collisions,
                "srt egress owner final counters"
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn owner(&self, family: AddressFamily) -> Option<&Owner> {
        Some(&self.owners[family.index()].as_ref()?.owner)
    }

    pub(crate) fn fault(&self, family: AddressFamily) -> Option<&OwnerFault> {
        self.owners[family.index()].as_ref()?.owner.fault()
    }

    /// Fill the low-cardinality per-family Owner gauges/counters. Cheap and
    /// allocation-free: fixed structs from public Owner observability.
    pub(crate) fn observe(&self, metrics: &mut ShardMetrics) {
        metrics.srt_runtime_io_uring = self.profile.is_io_uring;
        metrics.srt_managed_rx_available = self.substrate.is_available();
        metrics.srt_owner_shutdown_incomplete = self.shutdown_incomplete;
        for (index, slot) in self.owners.iter().enumerate() {
            let Some(family_owner) = slot.as_ref() else {
                metrics.srt_owners[index] = OwnerFamilyMetrics::default();
                continue;
            };
            let owner = &family_owner.owner;
            let counters = &family_owner.counters;
            let tx = owner.tx_pool_snapshot();
            let pool = owner.caller_pool_stats().unwrap_or_default();
            let rx = owner.rx_stats().caller;
            metrics.srt_owners[index] = OwnerFamilyMetrics {
                present: true,
                faulted: owner.fault().is_some(),
                managed_rx: owner.rx_mode() == Some(OwnerRxMode::ManagedMultishot),
                tx_capacity: tx.capacity as u32,
                tx_free: tx.free as u32,
                tx_high_water: tx.high_water as u32,
                tx_exhaustions: tx.exhaustions,
                tx_in_flight: owner.tx_in_flight() as u32,
                tx_failures_dropped: owner.tx_failures_dropped(),
                rx_packets: counters.rx_packets,
                rx_bytes: counters.rx_bytes,
                tx_packets: counters.tx_packets,
                tx_bytes: counters.tx_bytes,
                tx_completed_ok: counters.completed_ok,
                tx_short_sends: counters.short_sends,
                tx_failed_sends: counters.failed_sends,
                tx_peer_local_failures: counters.peer_local_failures,
                tx_transient_failures: counters.transient_failures,
                protocol_output_failures: counters.protocol_output_failures,
                service_visits: counters.service_visits,
                service_budget_exhausted: counters.budget_exhausted_visits,
                service_actions: counters.service_actions,
                maintenance_actions: counters.maintenance_actions,
                peer_group_collisions: counters.peer_group_collisions,
                caller_in_flight: pool.in_flight as u32,
                caller_queued: pool.queued as u32,
                caller_expired: pool.expired,
                caller_failed: pool.failed,
                caller_cancelled: pool.cancelled,
                rx_ring_depth: rx.map_or(0, |stats| stats.depth as u32),
                rx_ring_dropped: rx.map_or(0, |stats| stats.dropped),
                rx_truncated: rx.map_or(0, |stats| stats.truncated),
            };
        }
    }
}

/// The output of whichever future finishes first (the left one on a tie); the
/// loser is dropped, which for `recv_async`/`wait_for_activity` only cancels a
/// registered waker.
async fn first_ready<T>(
    a: impl Future<Output = T> + Unpin,
    b: impl Future<Output = T> + Unpin,
) -> T {
    match select(a, b).await {
        Either::Left((value, _)) | Either::Right((value, _)) => value,
    }
}

/// Owner activity as a future; an absent Owner never wakes.
async fn owner_activity(owner: Option<&mut FamilyOwner>, wait: Duration) -> EgressShardIdleWake {
    match owner {
        Some(family_owner) => {
            // `wait_for_activity` returns both on activity and on its own
            // timeout; only a return before the bound is activity.
            let started = Instant::now();
            family_owner.owner.wait_for_activity(wait).await;
            if started.elapsed() >= wait {
                EgressShardIdleWake::Timeout
            } else {
                EgressShardIdleWake::BackendActivity
            }
        }
        None => pending::<EgressShardIdleWake>().await,
    }
}

impl Drop for SrtOwners {
    fn drop(&mut self) {
        self.assert_home_thread();
    }
}

#[cfg(test)]
impl SrtOwners {
    pub(crate) fn shutdown_incomplete(&self) -> u64 {
        self.shutdown_incomplete
    }
}
