//! HLS PUT fabric shard backend: every HLS PUT output of one HLS store on
//! this shard is a slot that drives the shared upload policy
//! (`media::hls::upload_policy`) over one persistent connection.
//!
//! The shard owns the connections (Compio TCP through the same poller as
//! RTMP, TLS through `egress::tls`, kernel TLS after the handshake) and the
//! clock. A store publish wakes the shard directly (see
//! `engine_hls_egress_fabric.rs`); a small pool of resolve threads keeps
//! DNS off the shard, so one destination with a hanging name server holds
//! one lookup thread, not every output's connects, and every retry
//! resolves again (Akamai). One request is in flight per output, and each
//! output's deadlines (backoff, resolve, connect, request timeout) are
//! shard timers. Slots live in a `LeafArena`: a late DNS answer or poller
//! event for a removed output never reaches the output reusing its slot.
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_rustls::rustls::ClientConfig;

use super::compio_tcp::CompioTcpStream;
use super::hls_put::{Exchange, ExchangeStep, HlsPutTarget};
use super::rtmp_shard_poller::RtmpReadinessPoller;
use super::tcp::{TcpConnectAttempt, TcpReadyLeaf, connect_error};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::leaf_arena::{LeafArena, LeafKey};
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardIdleWake,
};
use crate::media::egress::tls::{TlsCounters, TlsTcpConnection};
use crate::media::hls::HlsStore;
use crate::media::hls::upload_policy::{
    Next, ResultEffect, Stopped, UploadOutcome, UploadPolicy, UploadRequest,
};

/// HTTPS connection and kTLS counters for HLS PUT, reported beside RTMPS.
pub(crate) static HLS_PUT_TLS_COUNTERS: TlsCounters = TlsCounters::new();

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// The end playlist after a remove gets this long.
const END_TIMEOUT: Duration = Duration::from_secs(1);
const RESOLVE_QUEUE_CAPACITY: usize = 1024;
/// Lookup threads per shard: a destination whose name server hangs ties up
/// one, not the shard's other outputs.
const RESOLVE_THREADS: usize = 4;
const SCRATCH_BYTES: usize = 16 * 1024;

// ---------------------------------------------------------------------------
// Resolve worker
// ---------------------------------------------------------------------------

struct ResolveRequest {
    key: LeafKey,
    token: u64,
    host: String,
    port: u16,
}

struct Resolved {
    key: LeafKey,
    token: u64,
    addr: Option<SocketAddr>,
}

/// A few threads resolving names for the shard. Dropping it closes the
/// request queue; each thread ends after its current lookup and is never
/// joined, so a lookup blocked in `getaddrinfo` cannot stall shutdown.
struct Resolver {
    requests: flume::Sender<ResolveRequest>,
    completions: flume::Receiver<Resolved>,
}

impl Resolver {
    fn spawn() -> std::io::Result<Self> {
        let (request_tx, request_rx) = flume::bounded::<ResolveRequest>(RESOLVE_QUEUE_CAPACITY);
        let (done_tx, done_rx) = flume::bounded::<Resolved>(RESOLVE_QUEUE_CAPACITY);
        for _ in 0..RESOLVE_THREADS {
            let (request_rx, done_tx) = (request_rx.clone(), done_tx.clone());
            std::thread::Builder::new()
                .name("hls-put-resolve".to_string())
                .spawn(move || {
                    while let Ok(request) = request_rx.recv() {
                        let addr =
                            crate::media::egress::backends::rtmp_shard::resolve_rtmp_peer_host(
                                &request.host,
                                request.port,
                            );
                        let resolved = Resolved {
                            key: request.key,
                            token: request.token,
                            addr,
                        };
                        if done_tx.send(resolved).is_err() {
                            return;
                        }
                    }
                })?;
        }
        Ok(Self {
            requests: request_tx,
            completions: done_rx,
        })
    }

    fn request(&self, request: ResolveRequest) -> bool {
        self.requests.try_send(request).is_ok()
    }
}

// ---------------------------------------------------------------------------
// Slots
// ---------------------------------------------------------------------------

enum Conn {
    Idle,
    Resolving {
        token: u64,
        deadline: Instant,
    },
    Connecting {
        stream: CompioTcpStream,
        deadline: Instant,
    },
    Open(TlsTcpConnection),
}

struct Slot {
    output_id: OutputId,
    generation: u64,
    progress: EgressProgressSink,
    target: HlsPutTarget,
    policy: UploadPolicy,
    conn: Conn,
    /// Taken from the policy, waiting for a connection.
    waiting_request: Option<UploadRequest>,
    exchange: Option<Exchange>,
    connect_timeout: Duration,
    /// The policy's backoff instant, when it is waiting for one.
    wake_at: Option<Instant>,
    resolve_token: u64,
    /// In `ready`: an event for a queued slot does not queue it twice.
    queued: bool,
}

impl Slot {
    fn deadline(&self) -> Option<Instant> {
        let connect = match &self.conn {
            Conn::Connecting { deadline, .. } | Conn::Resolving { deadline, .. } => Some(*deadline),
            Conn::Idle | Conn::Open(_) => None,
        };
        [
            self.wake_at,
            connect,
            self.exchange.as_ref().map(|exchange| exchange.deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn fd(&self) -> Option<i32> {
        match &self.conn {
            Conn::Connecting { stream, .. } => Some(stream.as_raw_fd()),
            Conn::Open(connection) => Some(connection.raw_fd()),
            Conn::Idle | Conn::Resolving { .. } => None,
        }
    }
}

pub(crate) struct HlsPutShardBackend<P: RtmpReadinessPoller> {
    poller: P,
    store: Arc<HlsStore>,
    client_config: Arc<ClientConfig>,
    resolver: Resolver,
    slots: LeafArena<Slot>,
    by_output: HashMap<OutputId, LeafKey>,
    /// Reused by `drive_due`.
    due: Vec<LeafKey>,
    /// Shutdown or drain began: keep the shard alive while a slot remains,
    /// so every end playlist gets its chance.
    draining: bool,
    /// Slots with something to do now.
    ready: VecDeque<LeafKey>,
    poll_buffer: Vec<TcpReadyLeaf>,
    scratch: Vec<u8>,
    /// The timer the shard holds for us: `(fire_at, output, generation)`.
    scheduled: Option<(Instant, OutputId, u64)>,
    tx_bytes: u64,
    tx_units: u64,
}

impl<P: RtmpReadinessPoller> HlsPutShardBackend<P> {
    pub(crate) fn new(
        poller: P,
        store: Arc<HlsStore>,
        client_config: Arc<ClientConfig>,
        leaf_capacity: usize,
    ) -> std::io::Result<Self> {
        Ok(Self {
            poller,
            store,
            client_config,
            resolver: Resolver::spawn()?,
            slots: LeafArena::with_capacity(leaf_capacity),
            by_output: HashMap::new(),
            due: Vec::new(),
            draining: false,
            ready: VecDeque::with_capacity(leaf_capacity),
            poll_buffer: Vec::new(),
            scratch: vec![0; SCRATCH_BYTES],
            scheduled: None,
            tx_bytes: 0,
            tx_units: 0,
        })
    }

    fn add(&mut self, spec: OutputSpec) {
        let ProtocolSpec::HlsPut { url } = &spec.protocol else {
            return;
        };
        if let Some(key) = self.by_output.remove(&spec.id) {
            // A new generation replaces the old slot outright.
            self.close_slot(key);
        }
        let Some(target) = HlsPutTarget::parse(url) else {
            tracing::warn!(output_id = %spec.id, "hls put fabric leaf rejected: invalid url");
            spec.progress.mark_terminated_unexpectedly();
            return;
        };
        let mut policy = UploadPolicy::new(crate::media::hls::upload::upload_session_token());
        if let Some(snapshot) = self.store.snapshot() {
            policy.on_publish(&snapshot);
        }
        let inserted = self.slots.insert_with(|_| Slot {
            output_id: spec.id.clone(),
            generation: spec.generation,
            progress: spec.progress.clone(),
            target,
            policy,
            conn: Conn::Idle,
            waiting_request: None,
            exchange: None,
            connect_timeout: spec.policy.connect_timeout,
            wake_at: None,
            resolve_token: 0,
            queued: false,
        });
        let Some(key) = inserted else {
            tracing::warn!(output_id = %spec.id, "hls put fabric leaf rejected: shard leaf capacity exhausted");
            spec.progress.mark_terminated_unexpectedly();
            return;
        };
        self.by_output.insert(spec.id, key);
        self.enqueue(key);
    }

    fn enqueue(&mut self, key: LeafKey) {
        if let Some(slot) = self.slots.get_mut(key)
            && !slot.queued
        {
            slot.queued = true;
            self.ready.push_back(key);
        }
    }

    fn close_slot(&mut self, key: LeafKey) {
        let Some(slot) = self.slots.remove(key) else {
            return;
        };
        if let Some(fd) = slot.fd() {
            let _ = self.poller.remove(fd);
        }
        if self.by_output.get(&slot.output_id) == Some(&key) {
            self.by_output.remove(&slot.output_id);
        }
        // A stale queue entry no longer resolves; dropping it keeps the
        // queue at most one entry per live slot.
        self.ready.retain(|ready| *ready != key);
    }

    fn drop_connection(&mut self, key: LeafKey) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        if let Some(fd) = slot.fd() {
            let _ = self.poller.remove(fd);
        }
        slot.conn = Conn::Idle;
        slot.exchange = None;
    }

    fn start_resolve(&mut self, key: LeafKey) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        slot.resolve_token = slot.resolve_token.wrapping_add(1);
        let token = slot.resolve_token;
        let request = ResolveRequest {
            key,
            token,
            host: slot.target.host.clone(),
            port: slot.target.port,
        };
        slot.conn = Conn::Resolving {
            token,
            deadline: Instant::now() + slot.connect_timeout,
        };
        if !self.resolver.request(request) {
            self.connection_failed(key, "resolver queue full");
        }
    }

    /// The connection attempt for the waiting request failed: it counts as
    /// a transport failure of that request.
    fn connection_failed(&mut self, key: LeafKey, why: &str) {
        self.drop_connection(key);
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        let ending = slot.policy.is_finishing();
        if ending {
            // Reported once as "end playlist not delivered".
            tracing::debug!(output_id = %slot.output_id, error = why, "hls put connect failed");
        } else {
            tracing::warn!(output_id = %slot.output_id, error = why, "hls put connect failed");
        }
        if slot.waiting_request.take().is_some() {
            let effect = slot
                .policy
                .on_result(UploadOutcome::Transport, Instant::now());
            report(slot, effect, Some(why));
        }
        self.enqueue(key);
    }

    fn resolved(&mut self, resolved: Resolved) {
        let key = resolved.key;
        let Some(slot) = self.slots.get(key) else {
            return;
        };
        if !matches!(slot.conn, Conn::Resolving { token, .. } if token == resolved.token) {
            return; // a stale lookup
        }
        let Some(addr) = resolved.addr else {
            self.connection_failed(key, "name resolution failed");
            return;
        };
        let (generation, timeout) = (slot.generation, slot.connect_timeout);
        match self.poller.start_connect(addr, key, generation, timeout) {
            Ok(TcpConnectAttempt::Connected(stream)) => self.activate(key, stream),
            Ok(TcpConnectAttempt::InProgress(stream)) => {
                if let Some(slot) = self.slots.get_mut(key) {
                    slot.conn = Conn::Connecting {
                        stream,
                        deadline: Instant::now() + timeout,
                    };
                }
            }
            Err(error) => self.connection_failed(key, &error.message),
        }
    }

    fn activate(&mut self, key: LeafKey, stream: CompioTcpStream) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        let connection = if slot.target.tls {
            match TlsTcpConnection::tls_with_config(
                stream,
                &slot.target.host,
                self.client_config.clone(),
                &HLS_PUT_TLS_COUNTERS,
            ) {
                Ok(connection) => connection,
                Err(error) => {
                    self.connection_failed(key, &error);
                    return;
                }
            }
        } else {
            TlsTcpConnection::plain(stream)
        };
        let fd = connection.raw_fd();
        if let Err(error) = self.poller.register_connection(
            fd,
            key,
            slot.generation,
            connection.completion_stream(),
        ) {
            self.connection_failed(key, &error.message);
            return;
        }
        slot.conn = Conn::Open(connection);
        self.enqueue(key);
    }

    fn poll(&mut self) {
        if self.poller.poll_leaves(0, &mut self.poll_buffer).is_err() {
            return;
        }
        let mut events = std::mem::take(&mut self.poll_buffer);
        for event in events.drain(..) {
            let Some(slot) = self.slots.get(event.key) else {
                continue;
            };
            if slot.fd() != Some(event.fd) {
                continue; // a closed connection's event
            }
            if let Conn::Connecting { .. } = slot.conn {
                if let Err(error) = connect_error(event.fd) {
                    self.connection_failed(event.key, &error.to_string());
                    continue;
                }
                let Some(Conn::Connecting { stream, .. }) = self
                    .slots
                    .get_mut(event.key)
                    .map(|slot| std::mem::replace(&mut slot.conn, Conn::Idle))
                else {
                    continue;
                };
                // Registered again as an established connection, as RTMP does.
                self.activate(event.key, stream);
                continue;
            }
            self.enqueue(event.key);
        }
        self.poll_buffer = events;
        while let Ok(resolved) = self.resolver.completions.try_recv() {
            self.resolved(resolved);
        }
    }

    /// Drive one slot as far as it can go now.
    fn drive(&mut self, key: LeafKey, now: Instant) {
        if let Some(slot) = self.slots.get_mut(key) {
            slot.queued = false;
        }
        loop {
            let Some(slot) = self.slots.get_mut(key) else {
                return;
            };
            match &slot.conn {
                Conn::Resolving { deadline, .. } if now >= *deadline => {
                    self.connection_failed(key, "name resolution timed out");
                    continue;
                }
                Conn::Connecting { deadline, .. } if now >= *deadline => {
                    self.connection_failed(key, "connect timed out");
                    continue;
                }
                _ => {}
            }
            if let Some(exchange) = slot.exchange.as_mut() {
                let Conn::Open(connection) = &mut slot.conn else {
                    slot.exchange = None;
                    continue;
                };
                let step = exchange.advance(connection, &mut self.scratch);
                connection.resume_receive();
                match step {
                    ExchangeStep::Done(response) => {
                        slot.exchange = None;
                        let effect = slot
                            .policy
                            .on_result(UploadOutcome::Status(response.status), now);
                        if let Some(ResultEffect::Acknowledged { bytes }) = effect {
                            self.tx_bytes = self.tx_bytes.saturating_add(bytes);
                            self.tx_units = self.tx_units.saturating_add(1);
                        }
                        report(slot, effect, None);
                        if !response.keep_alive {
                            self.drop_connection(key);
                        }
                        continue;
                    }
                    ExchangeStep::Blocked if now < exchange.deadline => return,
                    ExchangeStep::Blocked => {
                        self.drop_connection(key);
                        self.fail_exchange(key, now, "request timed out");
                        continue;
                    }
                    ExchangeStep::Failed(error) => {
                        self.drop_connection(key);
                        self.fail_exchange(key, now, &error);
                        continue;
                    }
                }
            }
            if slot.waiting_request.is_some() {
                // Waiting for a connection.
                if matches!(slot.conn, Conn::Idle) {
                    self.start_resolve(key);
                }
                // Take the request only once a connection is open; until
                // then it stays waiting.
                if let Some(slot) = self.slots.get_mut(key)
                    && matches!(slot.conn, Conn::Open(_))
                    && let Some(request) = slot.waiting_request.take()
                {
                    slot.exchange = Some(exchange_for(slot, &request, now));
                    continue;
                }
                return;
            }
            if let Conn::Open(connection) = &mut slot.conn
                && peer_closed_idle(connection, &mut self.scratch)
            {
                self.drop_connection(key);
                continue;
            }
            match slot.policy.next(now) {
                Next::Put(request) => {
                    slot.wake_at = None;
                    if request.fresh_connection && matches!(slot.conn, Conn::Open(_)) {
                        self.drop_connection(key);
                    }
                    let Some(slot) = self.slots.get_mut(key) else {
                        return;
                    };
                    if matches!(slot.conn, Conn::Open(_)) {
                        slot.exchange = Some(exchange_for(slot, &request, now));
                    } else {
                        slot.waiting_request = Some(request);
                    }
                }
                Next::Wait(until) => {
                    slot.wake_at = until;
                    return;
                }
                Next::Stop(stopped) => {
                    if let Stopped::Rejected { status } = stopped {
                        tracing::warn!(output_id = %slot.output_id, status, "HLS ingest rejected the upload; not retrying");
                        slot.progress.mark_terminated_unexpectedly();
                    }
                    self.close_slot(key);
                    return;
                }
            }
        }
    }

    fn fail_exchange(&mut self, key: LeafKey, now: Instant, why: &str) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        let effect = slot.policy.on_result(UploadOutcome::Transport, now);
        report(slot, effect, Some(why));
    }

    fn drive_ready(&mut self) {
        let now = Instant::now();
        let mut rounds = self.ready.len();
        while rounds > 0
            && let Some(key) = self.ready.pop_front()
        {
            rounds -= 1;
            self.drive(key, now);
        }
    }

    fn drive_due(&mut self, now: Instant) {
        let mut due = std::mem::take(&mut self.due);
        due.extend(
            self.slots
                .iter()
                .filter(|(_, slot)| slot.deadline().is_some_and(|deadline| deadline <= now))
                .map(|(key, _)| key),
        );
        for key in due.drain(..) {
            self.drive(key, now);
        }
        self.due = due;
    }

    /// Keep one shard timer at the earliest slot deadline.
    fn timer_effect(&mut self) -> EgressShardCommandEffect {
        let earliest = self
            .slots
            .iter()
            .filter_map(|(_, slot)| slot.deadline().map(|at| (at, slot)))
            .min_by_key(|(at, _)| *at)
            .map(|(at, slot)| (at, slot.output_id.clone(), slot.generation));
        if earliest == self.scheduled {
            return EgressShardCommandEffect::Continue;
        }
        self.scheduled = earliest.clone();
        match earliest {
            Some((fire_at, output_id, generation)) => EgressShardCommandEffect::ScheduleTimer {
                output_id,
                generation,
                fire_at,
            },
            None => EgressShardCommandEffect::Continue,
        }
    }

    fn after_work(&mut self) -> EgressShardCommandEffect {
        if !self.ready.is_empty() {
            return EgressShardCommandEffect::ScheduleReady { count: 1 };
        }
        let effect = self.timer_effect();
        if self.draining && !self.slots.is_empty() && effect == EgressShardCommandEffect::Continue {
            // The shard stops a drain that reports nothing to do; an end
            // playlist still resolving, connecting or in flight is work.
            // Re-arm the earliest deadline (every busy slot has one).
            self.scheduled = None;
            return self.timer_effect();
        }
        effect
    }
}

fn exchange_for(slot: &Slot, request: &UploadRequest, now: Instant) -> Exchange {
    let timeout = if matches!(
        request.target,
        crate::media::hls::upload_policy::UploadTarget::Playlist { end: true, .. }
    ) {
        END_TIMEOUT
    } else {
        REQUEST_TIMEOUT
    };
    Exchange::new(&slot.target, request, now + timeout)
}

/// An idle keep-alive connection the peer closed (or wrote to unasked) is
/// finished; noticing now saves the next request a failed attempt.
fn peer_closed_idle(connection: &mut TlsTcpConnection, scratch: &mut [u8]) -> bool {
    if !connection.has_buffered_receive() {
        return false;
    }
    !matches!(
        std::io::Read::read(connection, scratch),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    )
}

fn report(slot: &Slot, effect: Option<ResultEffect>, failure: Option<&str>) {
    match effect {
        Some(ResultEffect::Acknowledged { bytes }) => {
            let now_ms = crate::media::engine::MediaEngine::now_epoch_ms();
            slot.progress.record_sent(bytes, 1, now_ms);
        }
        Some(ResultEffect::WillRetry { attempts }) => {
            tracing::warn!(
                output_id = %slot.output_id,
                attempts,
                error = failure.unwrap_or("upload failed"),
                "HLS upload failed; retrying"
            );
        }
        Some(ResultEffect::Rejected { status }) => {
            tracing::warn!(output_id = %slot.output_id, status, "HLS ingest rejected the upload");
        }
        Some(ResultEffect::EndNotDelivered) => {
            tracing::info!(
                output_id = %slot.output_id,
                error = failure.unwrap_or("upload failed"),
                "HLS end playlist not delivered"
            );
        }
        None => {}
    }
}

impl<P: RtmpReadinessPoller + 'static> EgressShardBackend for HlsPutShardBackend<P> {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => self.add(spec),
            EgressCommand::Remove(output_id) => {
                if let Some(key) = self.by_output.get(&output_id).copied()
                    && let Some(slot) = self.slots.get_mut(key)
                {
                    slot.policy.finish();
                    slot.wake_at = None;
                    self.enqueue(key);
                }
            }
            EgressCommand::FeedWake => {
                if let Some(snapshot) = self.store.snapshot() {
                    for (key, slot) in self.slots.iter_mut() {
                        slot.policy.on_publish(&snapshot);
                        if !slot.queued {
                            slot.queued = true;
                            self.ready.push_back(key);
                        }
                    }
                }
            }
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {
                self.draining = true;
                let keys: Vec<LeafKey> = self.by_output.values().copied().collect();
                for key in keys {
                    if let Some(slot) = self.slots.get_mut(key) {
                        slot.policy.finish();
                        slot.wake_at = None;
                    }
                    self.enqueue(key);
                }
            }
        }
        self.drive_ready();
        self.after_work()
    }

    fn timer_generation(&self, output_id: &OutputId) -> Option<u64> {
        let key = self.by_output.get(output_id)?;
        self.slots.get(*key).map(|slot| slot.generation)
    }

    fn on_timer(&mut self, _output_id: OutputId, _generation: u64) -> EgressShardCommandEffect {
        self.scheduled = None;
        self.drive_due(Instant::now());
        self.after_work()
    }

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        self.poll();
        self.drive_ready();
        self.after_work()
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        let mut resolved_any = false;
        while let Ok(resolved) = self.resolver.completions.try_recv() {
            self.resolved(resolved);
            resolved_any = true;
        }
        if resolved_any {
            self.drive_ready();
        }
        self.drive_due(Instant::now());
        self.after_work()
    }

    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: Duration,
    ) -> EgressShardIdleWake {
        if let Some(wake) = self.poller.wait_idle(commands, max_wait) {
            return wake;
        }
        match commands.recv_timeout(max_wait) {
            Ok(command) => EgressShardIdleWake::Command(command),
            Err(flume::RecvTimeoutError::Timeout) => EgressShardIdleWake::Timeout,
            Err(flume::RecvTimeoutError::Disconnected) => EgressShardIdleWake::Disconnected,
        }
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        let (completions, stale_completions) = self.poller.completion_counts();
        metrics.tx_packets = self.tx_units;
        metrics.tx_bytes = self.tx_bytes;
        metrics.cqes = completions;
        metrics.stale_completions = stale_completions;
    }

    fn on_shutdown(&mut self) {
        let keys: Vec<LeafKey> = self.by_output.values().copied().collect();
        for key in keys {
            self.close_slot(key);
        }
    }
}

#[cfg(test)]
#[path = "hls_put_shard_tests.rs"]
mod tests;
