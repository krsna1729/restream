//! RTMP fabric shard backend: wires [`RtmpFabricEngine`] into
//! [`EgressShardBackend`], mirroring [`crate::media::egress::backends::srt::SrtShardBackend`]'s
//! shape — a real `TcpEgressPoller`-backed poller, leaf slab, ready queue,
//! and a bounded blocking connect on the shard's own OS thread (acceptable
//! there, per `tcp_connect.rs`, since it blocks only that shard's own leaves
//! for at most the connect timeout) — with DNS resolution split onto a
//! dedicated worker thread and completion queue instead, since unlike a
//! bounded `connect_timeout`, `ToSocketAddrs` has no timeout of its own and
//! could otherwise stall the shard indefinitely on a slow or hung resolver.
//!
//! Unlike the SRT fabric (always write-registered — libsrt handles
//! acknowledgement internally), RTMP genuinely alternates between wanting
//! read and write readiness across its handshake, negotiation, and
//! publishing states, so this backend re-registers each leaf's poller
//! interest after every visit based on the `Interest` the engine's last
//! `EngineProgress` carried, rather than registering once at connect time.

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio_rustls::rustls::ClientConfig;

use crate::media::egress::backend::{CloseReason, Interest, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::feed::EgressFeed;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, LeafStallClass, WorkBudget, classify_stall};
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::rtmp::parse_rtmp_url;

use super::rtmp::{RtmpFabricEngine, RtmpPublishStartup};
use super::rtmp_connection::RtmpConnection;
use super::tcp::{TcpEgressInterest, TcpEgressPollError, TcpEgressPoller, TcpReadyLeaf};
use super::tcp_connect::{TcpFabricConnectConfig, connect_fabric_tcp_egress_socket};

// ---------------------------------------------------------------------------
// Publish-startup source
// ---------------------------------------------------------------------------

/// Supplies the immutable [`RtmpPublishStartup`] snapshot for one output.
/// The application layer assembles this (querying `MediaEngine`, output
/// registries, and ring state — none of which a leaf visit may touch) before
/// the output is added to a shard; wiring a real, shared-map-backed source
/// from the application layer is the next slice after this one.
pub(crate) trait RtmpPublishStartupSource {
    fn take_startup(&mut self, output_id: &OutputId) -> Option<RtmpPublishStartup>;
}

/// Always supplies an empty startup snapshot (no metadata, no cached
/// sequence headers). Correct for an output whose ring has not produced any
/// startup state yet, and the default until the application-layer source is
/// wired in.
#[derive(Debug, Default)]
pub(crate) struct EmptyRtmpPublishStartupSource;

impl RtmpPublishStartupSource for EmptyRtmpPublishStartupSource {
    fn take_startup(&mut self, _output_id: &OutputId) -> Option<RtmpPublishStartup> {
        Some(RtmpPublishStartup::default())
    }
}

/// Real source backed by a shared map: the application layer assembles
/// `RtmpFabricStartup` (querying `MediaEngine`, output registries, and ring
/// state), converts it to `RtmpPublishStartup`, and calls
/// [`Self::set`] before dispatching `EgressCommand::Add` for that output —
/// the shard thread only ever reads, via `take_startup`, never queries
/// anything itself. One instance is shared (cloned) across every shard of a
/// fabric runtime, since any output can land on any shard.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedRtmpPublishStartupSource {
    pending: std::sync::Arc<std::sync::Mutex<HashMap<OutputId, RtmpPublishStartup>>>,
}

impl SharedRtmpPublishStartupSource {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(&self, output_id: OutputId, startup: RtmpPublishStartup) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(output_id, startup);
    }

    pub(crate) fn remove(&self, output_id: &OutputId) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(output_id);
    }
}

impl RtmpPublishStartupSource for SharedRtmpPublishStartupSource {
    fn take_startup(&mut self, output_id: &OutputId) -> Option<RtmpPublishStartup> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(output_id)
            .cloned()
    }
}

// ---------------------------------------------------------------------------
// Resolve worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RtmpResolvedConnect {
    pub(crate) output_id: OutputId,
    pub(crate) generation: u64,
    pub(crate) peer_addr: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RtmpResolveWorkerError {
    ResolveFailed { host: String },
    CompletionQueueFull,
    CompletionQueueClosed,
}

pub(crate) trait RtmpResolveCompletionSource {
    fn drain_resolved(&mut self, resolved: &mut Vec<RtmpResolvedConnect>);
}

#[derive(Debug, Default)]
pub(crate) struct NoopRtmpResolveCompletionSource;

impl RtmpResolveCompletionSource for NoopRtmpResolveCompletionSource {
    fn drain_resolved(&mut self, _resolved: &mut Vec<RtmpResolvedConnect>) {}
}

#[derive(Debug)]
pub(crate) struct RtmpResolveCompletionQueue {
    receiver: Receiver<RtmpResolvedConnect>,
}

pub(crate) fn rtmp_resolve_completion_queue(
    capacity: usize,
) -> (SyncSender<RtmpResolvedConnect>, RtmpResolveCompletionQueue) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (sender, RtmpResolveCompletionQueue { receiver })
}

impl RtmpResolveCompletionSource for RtmpResolveCompletionQueue {
    fn drain_resolved(&mut self, resolved: &mut Vec<RtmpResolvedConnect>) {
        while let Ok(completion) = self.receiver.try_recv() {
            resolved.push(completion);
        }
    }
}

pub(crate) fn spawn_rtmp_resolve_worker(
    output_id: OutputId,
    generation: u64,
    host: String,
    port: u16,
    completion_sender: SyncSender<RtmpResolvedConnect>,
) -> JoinHandle<Result<(), RtmpResolveWorkerError>> {
    thread::spawn(move || {
        let peer_addr = resolve_rtmp_peer_host(&host, port)
            .ok_or_else(|| RtmpResolveWorkerError::ResolveFailed { host: host.clone() })?;
        completion_sender
            .try_send(RtmpResolvedConnect {
                output_id,
                generation,
                peer_addr,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => RtmpResolveWorkerError::CompletionQueueFull,
                TrySendError::Disconnected(_) => RtmpResolveWorkerError::CompletionQueueClosed,
            })
    })
}

fn resolve_rtmp_peer_host(host: &str, port: u16) -> Option<SocketAddr> {
    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(addr, port));
    }
    (host, port).to_socket_addrs().ok()?.next()
}

// ---------------------------------------------------------------------------
// Poller trait (fake-able)
// ---------------------------------------------------------------------------

pub(crate) trait RtmpReadinessPoller {
    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError>;

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError>;

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError>;
}

impl<O> RtmpReadinessPoller for TcpEgressPoller<O>
where
    O: super::tcp::TcpPollOps,
{
    fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        self.register_leaf(fd, key, generation, interest)
    }

    fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        self.remove(fd)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}

// ---------------------------------------------------------------------------
// Leaf
// ---------------------------------------------------------------------------

struct RtmpFabricLeaf {
    common: LeafCommon,
    engine: RtmpFabricEngine,
    transport: RtmpConnection,
    /// What the poller is currently registered to watch for this leaf's fd.
    /// `visit_one_ready_leaf` only calls `register_leaf` (an `epoll_ctl`
    /// syscall) when the engine's next requested interest actually differs
    /// from this — unlike SRT (always `WRITE`), RTMP's interest genuinely
    /// changes across handshake/negotiation/publishing, but consecutive
    /// visits commonly request the *same* interest as last time (e.g. two
    /// `Progress{interest: WRITE}` results in a row while draining a large
    /// batch), and re-registering an unchanged interest is a syscall that
    /// changes nothing.
    registered_interest: TcpEgressInterest,
    /// Fallback "last progress" instant for `observe_stall` when a leaf has
    /// never made any byte/protocol progress at all (e.g. still mid-connect
    /// or mid-handshake) — mirrors `NativeSrtLeaf::observed_since`.
    observed_since: Instant,
    /// Set when this leaf has been asked to close (via `Remove`,
    /// `DrainShard`, or `Shutdown`) but still had queued application bytes
    /// at that moment. While `Some`, the leaf stays registered and visited
    /// normally so it can flush that backlog; it is force-closed once
    /// either `pending_application_bytes` reaches zero or this instant is
    /// more than the backend's drain timeout in the past — whichever comes
    /// first. `None` means "not closing" (the common case).
    draining_since: Option<Instant>,
    /// The reason to report once a draining leaf actually closes, recorded
    /// at the moment draining started so the real cause (removed vs.
    /// shutdown) survives to the eventual close call.
    draining_reason: Option<CloseReason>,
    /// `(tcp_bytes_sent, sampled_at)` from the previous quality sample,
    /// needed to compute `tcp_send_rate_mbps` as a two-sample delta —
    /// mirrors `rtmp/ingest.rs`'s `previous_tcp_bytes` for the receive side.
    previous_tcp_bytes: Option<(u64, Instant)>,
}

impl RtmpFabricLeaf {
    fn visit_ready(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &RingFeed,
        budget: WorkBudget,
    ) -> EngineVisitResult {
        EngineVisit {
            generation,
            common: &mut self.common,
            engine: &mut self.engine,
            transport: &mut self.transport,
            readiness,
            feed,
            budget,
        }
        .run()
    }

    /// Classify this leaf's send-path health from its pending application
    /// bytes (`common.pending_application_bytes`, wired up per-visit by
    /// `RtmpShardBackend::visit_one_ready_leaf`) and how long it's been
    /// since the last byte/protocol progress. Mirrors
    /// `NativeSrtLeaf::observe_stall` exactly (`src/media/egress/backends/srt.rs`)
    /// — RTMP has no native transport backlog to probe, so this is simpler:
    /// no FFI call, just the shared `classify_stall` on `common.progress`,
    /// which every protocol already updates identically via
    /// `EngineVisit::run`'s call to `apply_progress_to_common`.
    fn observe_stall(&self, now: Instant) -> LeafStallClass {
        let last_progress = self
            .common
            .progress
            .last_byte_progress
            .into_iter()
            .chain(self.common.progress.last_protocol_progress)
            .max()
            .unwrap_or(self.observed_since);
        let age = now.saturating_duration_since(last_progress);
        classify_stall(
            self.common.pending_application_bytes as u64,
            age,
            &self.common.limits,
        )
    }

    /// Sample sender-side TCP quality (RTT, retransmits, cwnd, pacing rate,
    /// congestion algorithm) for the once-per-second stall sweep — the same
    /// `TCP_INFO`/`SO_MEMINFO` mechanism and cadence legacy RTMP egress used
    /// for its own quality reporting, and the same conversion `rtmp/ingest.rs`
    /// already uses on the receive side. Returns `None` when `TCP_INFO` is
    /// unavailable (non-Linux, or the getsockopt call itself failed); the
    /// caller should leave the previously published quality in place then.
    fn sample_quality(
        &mut self,
        now: Instant,
    ) -> Option<crate::media::snapshots::PublisherQuality> {
        let stats =
            crate::media::tcp_stats::collect_tcp_stats_by_fd(self.transport.raw_fd()).ok()?;
        let send_rate = stats.tcp_bytes_sent.and_then(|bytes| {
            let rate = self.previous_tcp_bytes.and_then(|(previous, sampled_at)| {
                crate::media::tcp_stats::bytes_delta_rate_mbps(
                    bytes,
                    previous,
                    now.duration_since(sampled_at).as_secs_f64(),
                )
            });
            self.previous_tcp_bytes = Some((bytes, now));
            rate
        });
        Some(stats.into_egress_quality(send_rate))
    }
}

fn requeue_after_rtmp_visit(decision: VisitDecision) -> bool {
    matches!(decision, VisitDecision::Continue)
}

/// Registration interest for the next visit, derived from what the engine's
/// last `EngineProgress` said it needs. Variants that don't carry an
/// `Interest` (`HandshakeComplete`, `FeedOverrun`) are always paired with an
/// immediate requeue (see `requeue_after_rtmp_visit`), so the stale
/// registration is only observed if the shard's visit budget is exhausted
/// before that requeued visit runs — `READ_WRITE` is a safe superset for
/// that brief window.
fn next_registration_interest(
    progress: &crate::media::egress::backend::EngineProgress,
) -> Interest {
    use crate::media::egress::backend::EngineProgress;
    match progress {
        EngineProgress::Progress { wait, .. } | EngineProgress::Needs(wait) => wait.io_interest(),
        _ => Interest::READ_WRITE,
    }
}

fn tcp_interest(interest: Interest) -> TcpEgressInterest {
    TcpEgressInterest {
        readable: interest.readable,
        writable: interest.writable,
    }
}

// ---------------------------------------------------------------------------
// Shard backend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RtmpLeafSocket {
    key: LeafKey,
    fd: RawFd,
}

struct PendingRtmpConnect {
    common: LeafCommon,
    parts: crate::media::rtmp::RtmpUrlParts,
    connect_timeout: Duration,
}

pub(crate) struct RtmpShardBackend<
    P,
    R = NoopRtmpResolveCompletionSource,
    S = EmptyRtmpPublishStartupSource,
> where
    P: RtmpReadinessPoller,
    R: RtmpResolveCompletionSource,
    S: RtmpPublishStartupSource,
{
    poller: P,
    resolve_completions: R,
    startup_source: S,
    feed: RingFeed,
    /// Per-visit limits. `WorkBudget::deadline` is an absolute `Instant`
    /// computed at construction time — storing one `WorkBudget` and reusing
    /// it for every visit (as this backend used to) makes `is_exhausted()`
    /// permanently `true` once that one deadline passes, silently stopping
    /// every leaf on this shard from reading or sending anything ever
    /// again. A fresh `WorkBudget` is constructed from these fields for
    /// every visit instead (see `visit_one_ready_leaf`).
    budget_max_units: usize,
    budget_max_bytes: usize,
    budget_window: Duration,
    chunk_size: u32,
    rtmps_client_config: Arc<ClientConfig>,
    leaves: Vec<Option<RtmpFabricLeaf>>,
    output_sockets: HashMap<OutputId, RtmpLeafSocket>,
    ready: VecDeque<TcpReadyLeaf>,
    poll_buffer: Vec<TcpReadyLeaf>,
    pending_connects: HashMap<OutputId, PendingRtmpConnect>,
    last_stall_sweep: Option<Instant>,
    /// Bound on how long a leaf may stay in `draining_since` before it is
    /// force-closed regardless of remaining `pending_application_bytes`.
    /// Defaults to `EgressShardConfig::DEFAULT_DRAIN_TIMEOUT`; tests use
    /// `with_drain_timeout` for fast, deterministic timing.
    drain_timeout: Duration,
    /// Total `EngineProgress::FeedOverrun` resynchronizations observed across
    /// every leaf this backend has ever visited. Read by
    /// `EgressShardRuntime::record_iteration` into `ShardMetrics::feed_resyncs`
    /// for the repeated-resync alert (`derive_alerts`, `src/alerts.rs`).
    resync_count: u64,
}

impl<P> RtmpShardBackend<P, NoopRtmpResolveCompletionSource, EmptyRtmpPublishStartupSource>
where
    P: RtmpReadinessPoller,
{
    // Production always constructs via `with_runtime_components` directly
    // (see rtmp_shard_resolve_runtime.rs); this convenience constructor is
    // only used by tests.
    #[cfg(test)]
    pub(crate) fn new(poller: P, feed: RingFeed, budget: WorkBudget, chunk_size: u32) -> Self {
        Self::with_runtime_components(
            poller,
            feed,
            budget,
            chunk_size,
            crate::media::rtmp::rustls_client_config(),
            NoopRtmpResolveCompletionSource,
            EmptyRtmpPublishStartupSource,
        )
    }
}

impl<P, R, S> RtmpShardBackend<P, R, S>
where
    P: RtmpReadinessPoller,
    R: RtmpResolveCompletionSource,
    S: RtmpPublishStartupSource,
{
    pub(crate) fn with_runtime_components(
        poller: P,
        feed: RingFeed,
        budget: WorkBudget,
        chunk_size: u32,
        rtmps_client_config: Arc<ClientConfig>,
        resolve_completions: R,
        startup_source: S,
    ) -> Self {
        let budget_window = budget
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        Self {
            poller,
            resolve_completions,
            startup_source,
            feed,
            budget_max_units: budget.max_units,
            budget_max_bytes: budget.max_bytes,
            budget_window,
            chunk_size,
            rtmps_client_config,
            leaves: Vec::new(),
            output_sockets: HashMap::new(),
            ready: VecDeque::new(),
            poll_buffer: Vec::new(),
            pending_connects: HashMap::new(),
            last_stall_sweep: None,
            drain_timeout: crate::media::egress::shard::EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
            resync_count: 0,
        }
    }

    /// Override the per-leaf drain deadline. Production threads the
    /// configured `EgressFabricConfig::drain_timeout_ms` through here (see
    /// `resolving_rtmp_shard_backend`); tests use it for fast, deterministic
    /// timing instead of the constructor's multi-second default.
    pub(crate) fn with_drain_timeout(mut self, drain_timeout: Duration) -> Self {
        self.drain_timeout = drain_timeout;
        self
    }

    fn queue_pending_rtmp_connect(&mut self, spec: OutputSpec, target_url: &str) {
        let Some(parts) = parse_rtmp_url(target_url) else {
            tracing::warn!(output_id = %spec.id, "rtmp fabric leaf rejected: invalid url");
            return;
        };
        let output_id = spec.id.clone();
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress.clone());
        self.pending_connects.insert(
            output_id,
            PendingRtmpConnect {
                common,
                parts,
                connect_timeout: spec.policy.connect_timeout,
            },
        );
    }

    #[cfg(test)]
    fn pending_connect(&self, output_id: &OutputId) -> Option<&PendingRtmpConnect> {
        self.pending_connects.get(output_id)
    }

    /// Complete a pending connect with an already-resolved peer address:
    /// dials (bounded, blocking on this shard thread — see module docs),
    /// registers the connected socket with the poller, and constructs the
    /// engine. Errors are logged and drop the pending connect; the retry
    /// policy at the application layer owns reconnection.
    /// Returns `true` when a leaf actually became connected and registered
    /// this call — the caller uses that to know whether it needs to give
    /// the new leaf its first look at readiness (see `on_media_tick`'s doc
    /// comment on the shard trait: nothing else will discover a fresh
    /// leaf's I/O readiness on its own).
    fn complete_pending_connect(
        &mut self,
        output_id: &OutputId,
        generation: u64,
        peer_addr: SocketAddr,
    ) -> bool {
        let Some(pending) = self.pending_connects.remove(output_id) else {
            return false;
        };
        if pending.common.generation != generation {
            self.pending_connects.insert(output_id.clone(), pending);
            return false;
        }

        // Any early return below means the application never sees a leaf
        // at all for this attempt — nothing else will tell it the attempt
        // died, so mark it the same way an established leaf's unexpected
        // close does (see `EgressProgressSink::terminated_unexpectedly`).
        let progress_sink = pending.common.progress_sink.clone();

        let tcp_stream = match connect_fabric_tcp_egress_socket(TcpFabricConnectConfig {
            peer_addr,
            connect_timeout: pending.connect_timeout,
        }) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf connect failed");
                progress_sink.mark_terminated_unexpectedly();
                return false;
            }
        };
        let stream = if pending.parts.tls {
            match RtmpConnection::tls_with_config(
                tcp_stream,
                &pending.parts.host,
                self.rtmps_client_config.clone(),
            ) {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf tls init failed");
                    progress_sink.mark_terminated_unexpectedly();
                    return false;
                }
            }
        } else {
            RtmpConnection::plain(tcp_stream)
        };

        let Some(publish_startup) = self.startup_source.take_startup(output_id) else {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf rejected: no publish startup available");
            progress_sink.mark_terminated_unexpectedly();
            return false;
        };

        let engine = match RtmpFabricEngine::new_client(
            pending.parts,
            self.chunk_size,
            false,
            publish_startup,
        ) {
            Ok(engine) => engine,
            Err(error) => {
                tracing::warn!(output_id = %output_id, error = %error, "rtmp fabric leaf init failed");
                progress_sink.mark_terminated_unexpectedly();
                return false;
            }
        };

        let fd = stream.raw_fd();
        let key = LeafKey(self.leaves.len());
        if self
            .poller
            .register_leaf(fd, key, pending.common.generation, TcpEgressInterest::WRITE)
            .is_err()
        {
            tracing::warn!(output_id = %output_id, "rtmp fabric leaf poller registration failed");
            progress_sink.mark_terminated_unexpectedly();
            return false;
        }

        let leaf = RtmpFabricLeaf {
            common: pending.common,
            engine,
            transport: stream,
            registered_interest: TcpEgressInterest::WRITE,
            observed_since: Instant::now(),
            draining_since: None,
            draining_reason: None,
            previous_tcp_bytes: None,
        };
        self.leaves.push(Some(leaf));
        if let Some(previous) = self
            .output_sockets
            .insert(output_id.clone(), RtmpLeafSocket { key, fd })
        {
            self.remove_leaf_socket(previous, CloseReason::Removed);
        }
        tracing::info!(output_id = %output_id, leaf_key = key.0, "rtmp fabric leaf connected");
        true
    }

    fn remove_leaf_by_output(&mut self, output_id: &OutputId) -> bool {
        self.pending_connects.remove(output_id);
        let Some(socket_ref) = self.output_sockets.remove(output_id) else {
            return false;
        };
        self.remove_leaf_socket(socket_ref, CloseReason::Removed)
    }

    fn remove_leaf_socket(&mut self, socket_ref: RtmpLeafSocket, reason: CloseReason) -> bool {
        let _ = self.poller.remove(socket_ref.fd);
        let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) else {
            return false;
        };
        let mut leaf = leaf;
        leaf.engine.close(&mut leaf.transport, reason);
        true
    }

    fn leaf_mut(&mut self, key: LeafKey) -> Option<&mut RtmpFabricLeaf> {
        self.leaves.get_mut(key.0).and_then(Option::as_mut)
    }

    /// Minimum interval between stall sweeps — no per-leaf FFI probe to
    /// throttle here (unlike SRT's native bstats call), but there is no
    /// reason to walk every leaf on every media tick either.
    const STALL_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

    /// Directly enqueue every connected leaf whose last `WaitCondition`
    /// wants a feed wake (`Feed`/`FeedOrIo`) — set in
    /// `apply_progress_to_common` (`visit.rs`) from its own most recent
    /// `EngineProgress` — without any poller call.
    ///
    /// Mirrors `poll_ready()`'s push-with-dedup shape exactly (same
    /// `enqueued` check and set), using `self.ready` directly instead of a
    /// real `epoll_wait()`. Replaces the previous `epoll_ctl`-widening
    /// implementation of this method, which forced every leaf's
    /// registration to `READ_WRITE` on the (mistaken — RTMP's steady-state
    /// publisher always keeps at least `READ` registered, see
    /// `MediaPublisher::advance`'s `FeedRead::Empty` handling) assumption
    /// that a drained leaf's registration had gone empty; the *actual*
    /// effect of that widening was manufacturing a spurious writable event
    /// for the next real `epoll_wait()` to discover, since a TCP socket is
    /// almost always writable — an indirect, syscall-costly wake signal
    /// this replaces with a direct one.
    ///
    /// Safe against the regression an earlier direct-enqueue attempt hit
    /// (documented in this method's prior history): that attempt pushed a
    /// synthetic ready entry unconditionally, which starved
    /// handshake/negotiation leaves — those always want *real* I/O, and a
    /// synthetic no-readiness event kept winning the race to be visited
    /// before `poll_ready()` ever ran. This can't happen here because
    /// handshake/negotiation sub-state-machines only ever report
    /// `WaitCondition::Io(_)` (see `RtmpFabricEngine::advance`'s
    /// `Handshaking`/`Negotiating` arms), never `Feed`/`FeedOrIo`, so
    /// `wants_feed_wake` is structurally `false` for them and this loop
    /// never touches them — they remain discoverable only via real
    /// `poll_ready()`, exactly as before.
    fn enqueue_feed_waiting_leaves(&mut self) {
        let sockets: Vec<RtmpLeafSocket> = self.output_sockets.values().copied().collect();
        for socket_ref in sockets {
            let Some(leaf) = self
                .leaves
                .get_mut(socket_ref.key.0)
                .and_then(Option::as_mut)
            else {
                continue;
            };
            if !leaf.common.schedule.wants_feed_wake || leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            self.ready.push_back(TcpReadyLeaf {
                fd: socket_ref.fd,
                key: socket_ref.key,
                generation: leaf.common.generation,
                readable: false,
                writable: false,
            });
        }
    }

    fn poll_ready(&mut self) {
        if self.poller.poll_leaves(0, &mut self.poll_buffer).is_err() {
            return;
        }
        let events: Vec<_> = self.poll_buffer.drain(..).collect();
        for event in events {
            let Some(leaf) = self.leaf_mut(event.key) else {
                continue;
            };
            if leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            self.ready.push_back(event);
        }
    }

    /// Visit the next ready leaf, then re-register its poller interest to
    /// match what the engine's returned progress says it needs next (unlike
    /// SRT's always-write registration, RTMP's interest genuinely changes
    /// across handshake/negotiation/publishing — see module docs).
    ///
    /// `OutputId` wraps a `String`, so cloning it is a heap allocation; the
    /// caller only ever uses it on `VisitDecision::Close` (to remove the
    /// leaf), so it's only cloned then — every other visit (the overwhelming
    /// majority in steady state) pays nothing for it.
    fn visit_one_ready_leaf(&mut self) -> Option<(Option<OutputId>, VisitDecision)> {
        let event = self.ready.pop_front()?;
        let budget = WorkBudget::new(
            self.budget_max_units,
            self.budget_max_bytes,
            self.budget_window,
        );
        let feed = &self.feed;
        let leaf = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)?;
        let result = leaf.visit_ready(
            event.generation,
            Readiness {
                readable: event.readable,
                writable: event.writable,
            },
            feed,
            budget,
        );

        let (progress, decision) = match result {
            EngineVisitResult::StaleGeneration => return Some((None, VisitDecision::Suspend)),
            EngineVisitResult::Visited(outcome) => (outcome.progress, outcome.decision),
        };
        if matches!(
            progress,
            crate::media::egress::backend::EngineProgress::FeedOverrun
        ) {
            self.resync_count = self.resync_count.saturating_add(1);
        }
        leaf.common.pending_application_bytes = leaf
            .engine
            .pending_application_bytes()
            .saturating_add(leaf.transport.rustls_pending_bytes_estimate());

        // A draining leaf (see `begin_graceful_close`) that has now flushed
        // everything it had queued closes right here — no need to wait for
        // the next `sweep_draining_leaves` tick. One still stuck past its
        // deadline force-closes the same way, so a peer that stops reading
        // mid-drain can't hang this leaf open forever.
        if let Some(draining_since) = leaf.draining_since {
            let flushed = leaf.common.pending_application_bytes == 0;
            let expired = draining_since.elapsed() >= self.drain_timeout;
            if flushed || expired {
                let reason = leaf.draining_reason.unwrap_or(CloseReason::Removed);
                let output_id = leaf.common.output_id.clone();
                if let Some(socket_ref) = self.output_sockets.remove(&output_id) {
                    self.remove_leaf_socket(socket_ref, reason);
                }
                return Some((None, VisitDecision::Suspend));
            }
        }

        if matches!(decision, VisitDecision::Close) {
            return Some((Some(leaf.common.output_id.clone()), decision));
        }

        {
            let interest = tcp_interest(next_registration_interest(&progress));
            // `register_leaf` is an `epoll_ctl(EPOLL_CTL_MOD)` syscall; skip
            // it when the requested interest already matches what's
            // registered (common across consecutive visits of the same
            // leaf — e.g. several `Progress{interest: WRITE}` results in a
            // row while draining a large batch).
            if let Some(leaf) = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)
                && leaf.registered_interest != interest
            {
                // Discarding this Result and updating `registered_interest`
                // unconditionally would desync tracked state from the real
                // kernel registration on failure: the leaf would believe
                // it's watching (e.g.) writable readiness forever while the
                // kernel never actually does, and would never be
                // rediscovered by `poll_ready()` again — silent, permanent
                // starvation, indistinguishable from a healthy idle leaf
                // (the root cause of the recurring "RTMP fabric leaf
                // terminated unexpectedly" CI flake). Treat a failed
                // re-registration as leaf-fatal instead, same as a failed
                // initial registration at connect time: close and let the
                // existing retry/reconnect path recover it.
                if self
                    .poller
                    .register_leaf(event.fd, event.key, event.generation, interest)
                    .is_err()
                {
                    tracing::warn!(
                        output_id = %leaf.common.output_id,
                        leaf_key = event.key.0,
                        "rtmp fabric leaf re-registration failed; closing for retry"
                    );
                    return Some((Some(leaf.common.output_id.clone()), VisitDecision::Close));
                }
                leaf.registered_interest = interest;
            }
        }

        Some((None, decision))
    }
}

impl<P, R, S> EgressShardBackend for RtmpShardBackend<P, R, S>
where
    P: RtmpReadinessPoller + Send + 'static,
    R: RtmpResolveCompletionSource + Send + 'static,
    S: RtmpPublishStartupSource + Send + 'static,
{
    fn resync_count(&self) -> u64 {
        self.resync_count
    }

    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                if let ProtocolSpec::Rtmp { url, .. } = spec.protocol.clone() {
                    self.queue_pending_rtmp_connect(spec, &url);
                }
            }
            EgressCommand::Remove(output_id) => {
                self.begin_graceful_close(&output_id, CloseReason::Removed);
            }
            EgressCommand::FeedWake => self.enqueue_feed_waiting_leaves(),
            // Both mean "every leaf here should close, gracefully" —
            // `DrainShard` for future shard-count reconfiguration (the
            // shard itself keeps running afterward), `Shutdown` because the
            // whole process is going down (the shard-runtime layer keeps
            // this shard's loop alive long enough to let leaves flush; see
            // `EgressShardRuntime::run`'s drain window in `shard.rs`).
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {
                let output_ids: Vec<OutputId> = self.output_sockets.keys().cloned().collect();
                let reason = if matches!(command, EgressCommand::Shutdown) {
                    CloseReason::ShardShutdown
                } else {
                    CloseReason::Removed
                };
                for output_id in output_ids {
                    self.begin_graceful_close(&output_id, reason);
                }
            }
        }
        EgressShardCommandEffect::Continue
    }

    /// Visit one ready leaf, then decide whether to ask for another
    /// `on_ready` pass immediately.
    ///
    /// `poll_ready()` can enqueue several ready leaves from one poll; if
    /// the leaf visited *this* call suspends (needs more I/O readiness) or
    /// closes, that alone must not stop the shard from draining the rest
    /// of an already-nonempty `self.ready` queue — those leaves were
    /// already reported ready and would otherwise sit stranded until some
    /// unrelated future command or feed wake happened to touch this shard
    /// again. Requeuing whenever `self.ready` is still nonempty (in
    /// addition to the existing "this leaf wants to continue" case) fixes
    /// that: a blocked leaf never blocks its already-ready neighbors.
    fn on_ready(&mut self) -> EgressShardCommandEffect {
        if self.ready.is_empty() {
            self.poll_ready();
        }

        let outcome = self.visit_one_ready_leaf();
        if let Some((Some(output_id), VisitDecision::Close)) = &outcome {
            // `VisitDecision::Close` here means either
            // `EngineProgress::PeerClosed`/`Failed` (see `visit.rs`) or a
            // failed poller re-registration inside `visit_one_ready_leaf`
            // — an explicit `EgressCommand::Remove` never reaches this
            // path — so every close observed here is unexpected from the
            // application's point of view.
            if let Some(socket_ref) = self.output_sockets.get(output_id)
                && let Some(leaf) = self.leaves.get(socket_ref.key.0).and_then(Option::as_ref)
            {
                leaf.common.progress_sink.mark_terminated_unexpectedly();
            }
            self.remove_leaf_by_output(output_id);
        }

        let leaf_wants_more =
            matches!(&outcome, Some((_, decision)) if requeue_after_rtmp_visit(*decision));
        if leaf_wants_more || !self.ready.is_empty() {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        let mut resolved = Vec::new();
        self.resolve_completions.drain_resolved(&mut resolved);
        let mut connected_any = false;
        for completion in resolved {
            let connected = self.complete_pending_connect(
                &completion.output_id,
                completion.generation,
                completion.peer_addr,
            );
            connected_any |= connected;
        }
        self.sweep_stalled_leaves(Instant::now());
        if connected_any {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_shutdown(&mut self) {
        let sockets: Vec<_> = self
            .output_sockets
            .drain()
            .map(|(_, socket_ref)| socket_ref)
            .collect();
        for socket_ref in sockets {
            let _ = self.poller.remove(socket_ref.fd);
            if let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::ShardShutdown,
                );
            }
        }
    }
}

#[path = "rtmp_shard_drain.rs"]
mod rtmp_shard_drain;

#[cfg(test)]
#[path = "rtmp_shard_tests.rs"]
mod tests;
