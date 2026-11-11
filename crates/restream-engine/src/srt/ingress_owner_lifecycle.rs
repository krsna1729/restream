//! An ingress Owner thread's start and end: the srt-rs listener plan, the
//! thread entry, building the runtime, Owner and loop on the thread, and the
//! canonical teardown that reports the exit verdict.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use srt_transport::advanced::admission::BondedInputPolicy;
use srt_transport::compio::{
    Owner, OwnerRxMode, OwnerServiceBudget, ProductionRuntimeConfig, RxModePolicy,
    observe_production_runtime,
};
use srt_transport::{ListenerConfig, OwnerListenerPlan, RuntimeFlavor, owner_plans};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::egress::backends::srt::owner_set::{
    SRT_OWNER_WIRE_CEILING, production_runtime, substrate_diagnosis,
};

use super::super::ingress_admission::ingress_resolver;
use super::{
    CALLS_IN_FLIGHT, COMMANDS_PER_VISIT, DeferredSends, INGRESS_TX_CAPACITY, IngressCommand,
    IngressConfig, IngressExit, IngressMedia, OwnerLoop, OwnerSeat, PAYLOAD_WORK_CAPACITY,
    Published, QualitySample, READER_SENDS_PER_VISIT, SAMPLE_INTERVAL, SHUTDOWN_DRAIN_DEADLINE,
    SHUTDOWN_FLUSH_VISITS, SrtIngressEvent, listener_shape, publish,
};

/// The srt-rs listener this ingress runs, split into one plan per Owner
/// (see [`listener_shape`]).
pub(in crate::srt) fn listener_plans(
    config: &IngressConfig,
) -> Result<Vec<OwnerListenerPlan>, String> {
    let owners = std::num::NonZeroUsize::new(config.owners).unwrap_or(std::num::NonZeroUsize::MIN);
    let (topology, promotion) = listener_shape(owners);
    let listener_config = ListenerConfig::builder(config.bind)
        .topology(topology)
        .bonded_inputs(BondedInputPolicy::Accept)
        .configure_admission(|admission| {
            // srt-rs defaults this to `max_peers` (no per-client bound).
            admission.limits.max_peers_per_ip =
                config.max_peers_per_ip.clamp(1, admission.limits.max_peers);
            // Each established publisher reserves its worst-case window
            // memory here; one that does not fit is rejected (OVERLOAD).
            admission.limits.max_window_memory_bytes = config.window_memory_bytes;
        })
        .configure_transport(|transport| {
            // Same receive buffer as egress Owner sockets (`desired_udp_buf`,
            // 8 MiB unless RESTREAM_SRT_UDP_BUF_BYTES overrides). With the
            // kernel default (208 KB) the listener dropped 5,876 publisher
            // datagrams in one SRT x150 run (`ss -uam`), which ARQ then had to
            // recover.
            if let Some(bytes) = std::num::NonZeroUsize::new(super::super::desired_udp_buf()) {
                transport.socket_buffers = srt_transport::SocketBufferConfig::Bytes(bytes);
            }
            transport.promotion = promotion;
        })
        .build()
        .map_err(|error| format!("failed to build srt-rs listener config: {error}"))?;
    owner_plans(&listener_config, RuntimeFlavor::Compio)
        .map_err(|error| format!("failed to plan srt-rs listener Owners: {error}"))
}

pub(in crate::srt) fn run_owner_thread(
    seat: OwnerSeat,
    config: IngressConfig,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
    telemetry: mpsc::Sender<QualitySample>,
    ready: flume::Sender<Result<SocketAddr, String>>,
) -> IngressExit {
    match build(seat, config, commands, events, telemetry) {
        Ok((owner_loop, local_addr)) => {
            let _ = ready.send(Ok(local_addr));
            serve_contained(owner_loop)
        }
        Err(message) => {
            error!(error = %message, "SRT ingress Owner failed to start");
            let _ = ready.send(Err(message));
            IngressExit {
                quiescent: true,
                fault: None,
            }
        }
    }
}

/// Thread-entry fault boundary. A panic anywhere in the serve loop (srt-rs
/// service and the admission resolver it runs, command apply, reader sends)
/// is caught here, after the unwind, and taken down the Owner fault path:
/// counted, `faulted` set, the `Fault` event sent (Tokio stops the listener
/// and `SupervisedListener` restarts it), and the Owner torn down canonically
/// rather than dropped mid-unwind (compio aborts the process if a task is
/// polled while the thread is panicking, and a live managed-RX Owner is never
/// just dropped). If that teardown panics too, the Owner and its runtime are
/// leaked, not dropped; only the bridge endpoints are released, so Tokio and
/// the other Owners still see this one gone.
fn serve_contained(mut owner_loop: OwnerLoop) -> IngressExit {
    let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner_loop.serve()));
    let payload = match served {
        Ok(exit) => return exit,
        Err(payload) => payload,
    };
    let detail = format!(
        "SRT ingress Owner panicked: {}",
        crate::panic_boundary::record_contained(payload.as_ref())
    );
    error!(fault = %detail, "SRT ingress Owner faulted; the listener is stopping");
    owner_loop
        .stats
        .ingress_owner
        .faulted
        .store(true, Ordering::Relaxed);
    let finished = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        owner_loop.finish(Some(detail.clone()))
    }));
    finished.unwrap_or_else(|payload| {
        let panic = crate::panic_boundary::record_contained(payload.as_ref());
        error!(
            panic,
            "SRT ingress Owner teardown panicked; leaking the Owner"
        );
        owner_loop.report_fault(&detail);
        owner_loop.release_bridges();
        owner_loop.withdraw_gauges();
        std::mem::forget(owner_loop);
        IngressExit {
            quiescent: false,
            fault: Some(detail),
        }
    })
}

pub(super) fn build(
    seat: OwnerSeat,
    config: IngressConfig,
    commands: flume::Receiver<IngressCommand>,
    events: mpsc::Sender<SrtIngressEvent>,
    telemetry: mpsc::Sender<QualitySample>,
) -> Result<(OwnerLoop, SocketAddr), String> {
    // The runtime and the Owner are born on this thread and never leave it.
    let runtime_config =
        ProductionRuntimeConfig::for_owner(INGRESS_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    let runtime = production_runtime(runtime_config)?;
    let profile = runtime.block_on(observe_production_runtime(
        &runtime,
        INGRESS_TX_CAPACITY,
        SRT_OWNER_WIRE_CEILING,
    ));
    let substrate = profile.managed_rx_substrate();
    let rx_policy = RxModePolicy::ManagedPreferred;
    let OwnerSeat {
        index,
        plan,
        epoch,
        inbox,
        members,
    } = seat;
    let mut owner = Owner::new_with_ceiling(INGRESS_TX_CAPACITY, SRT_OWNER_WIRE_CEILING);
    // Install the observation from this Owner's runtime before its first attach.
    owner
        .set_rx_substrate(substrate)
        .map_err(|error| format!("failed to set SRT ingress RX substrate: {error}"))?;
    owner.set_rx_mode_policy(rx_policy);
    // Before any socket attaches: the caller pool `srt_call` inputs dial from.
    owner
        .set_caller_pool_capacity(
            std::num::NonZeroUsize::new(CALLS_IN_FLIGHT).expect("non-zero call capacity"),
        )
        .map_err(|error| format!("failed to size the SRT ingress caller pool: {error}"))?;
    let resolver = ingress_resolver(config.policy_store, config.receiver_group);
    runtime
        .block_on(async { owner.listen_planned(&plan, Some(resolver)) })
        .map_err(|error| format!("failed to attach srt-rs listener: {error}"))?;
    let local_addr = owner
        .listener_local_addr()
        .ok_or_else(|| "srt-rs listener has no local address".to_string())?;
    let rx_mode = owner.rx_mode();
    info!(
        driver = %profile.driver_type,
        compio = %profile.compio_version,
        io_uring = profile.is_io_uring,
        managed_rx = ?substrate,
        substrate_diagnosis = substrate_diagnosis(substrate),
        rx_mode = ?rx_mode,
        rx_policy = ?rx_policy,
        bind = %local_addr,
        owner = index,
        owners = config.owners,
        receiver_group_id = format_args!("{:#010x}", config.receiver_group.wire_id()),
        tx_capacity = INGRESS_TX_CAPACITY,
        wire_ceiling = SRT_OWNER_WIRE_CEILING,
        rx_ring_entries = runtime_config.rx_ring_entries,
        command_capacity = config.command_capacity,
        event_capacity = config.event_capacity,
        commands_per_visit = COMMANDS_PER_VISIT,
        "srt ingress owner attached"
    );
    let stats = config.stats;
    let mut published = Published::default();
    publish(&stats.ingress_owner.owners, &mut published.owners, 1);
    publish(
        &stats.ingress_owner.tx_capacity,
        &mut published.tx_capacity,
        u64::try_from(INGRESS_TX_CAPACITY).unwrap_or(u64::MAX),
    );
    // Every Owner of a listener observes the same runtime substrate.
    stats.ingress_owner.managed_rx.store(
        rx_mode == Some(OwnerRxMode::ManagedMultishot),
        Ordering::Relaxed,
    );
    Ok((
        OwnerLoop {
            owner,
            runtime,
            commands,
            events,
            telemetry,
            stats,
            live: std::collections::HashSet::new(),
            sample_queue: VecDeque::new(),
            next_sample_round: Instant::now() + SAMPLE_INTERVAL,
            epoch,
            pending_events: VecDeque::with_capacity(64),
            scratch: Vec::with_capacity(64),
            caller_scratch: Vec::with_capacity(64),
            pool_scratch: Vec::with_capacity(16),
            queued_calls: HashMap::new(),
            payload_work: VecDeque::with_capacity(PAYLOAD_WORK_CAPACITY),
            active_payloads: std::collections::HashSet::with_capacity(PAYLOAD_WORK_CAPACITY),
            media: IngressMedia::default(),
            addrs: HashMap::new(),
            reader_scratch: Vec::with_capacity(READER_SENDS_PER_VISIT),
            deferred: DeferredSends::default(),
            closing: VecDeque::new(),
            overloaded: std::collections::HashSet::new(),
            peers: 0,
            stash: None,
            shutting_down: false,
            home_thread: std::thread::current().id(),
            index,
            inbox,
            members,
            transfers: Vec::new(),
            transfer_stash: None,
            published,
        },
        local_addr,
    ))
}

impl OwnerLoop {
    /// Orderly exit: flush queued protocol output (SHUTDOWN datagrams), report
    /// a fault to Tokio, then run the canonical Owner teardown and report its
    /// verdict truthfully. A live managed-RX Owner is never just dropped.
    pub(super) fn finish(&mut self, fault: Option<String>) -> IngressExit {
        self.assert_home_thread();
        // Flush every publisher's last media into its ring.
        for peer in self.media.publisher_peers() {
            self.media.detach(peer);
        }
        if let Some(detail) = &fault {
            self.report_fault(detail);
        } else {
            for _ in 0..SHUTDOWN_FLUSH_VISITS {
                self.runtime.enter(|| {
                    self.runtime.poll_with(Some(Duration::ZERO));
                    self.runtime.run();
                });
                let now = self.timestamp();
                let owner = &mut self.owner;
                let report = self.runtime.block_on(async {
                    let report = owner.service(now, OwnerServiceBudget::default()).await;
                    if !report.work_remaining && owner.tx_in_flight() > 0 {
                        owner.wait_for_activity(Duration::from_millis(5)).await;
                    }
                    report
                });
                if !report.work_remaining && self.owner.tx_in_flight() == 0 {
                    break;
                }
            }
        }
        let owner = &mut self.owner;
        let quiescent = self
            .runtime
            .block_on(owner.shutdown_and_drain(SHUTDOWN_DRAIN_DEADLINE));
        if !quiescent {
            warn!(
                fault = ?self.owner.fault(),
                in_flight = self.owner.tx_in_flight(),
                "srt ingress Owner did not reach quiescence within its shutdown bound"
            );
        }
        self.withdraw_gauges();
        IngressExit { quiescent, fault }
    }

    /// Tell Tokio the listener died. Bounded: a stalled consumer cannot hold
    /// the owner thread past one second.
    fn report_fault(&mut self, detail: &str) {
        let mut event = SrtIngressEvent::Fault {
            detail: detail.to_string(),
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.events.try_send(event) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => return,
                Err(mpsc::error::TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        return;
                    }
                    event = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// Drop this Owner's ends of every bridge (replacing them with detached
    /// ones), for an Owner that is leaked instead of dropped: Tokio's
    /// `is_closed` and the other Owners' transfer sends must still see it gone.
    fn release_bridges(&mut self) {
        self.commands = flume::bounded(1).1;
        self.inbox = flume::bounded(1).1;
        self.events = mpsc::channel(1).0;
        self.telemetry = mpsc::channel(1).0;
        self.members.clear();
    }
}
