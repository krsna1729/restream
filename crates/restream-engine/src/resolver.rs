//! The process's one name resolver. Every hostname lookup in the engine goes
//! through it; clippy's `disallowed-methods` bans the system entry points
//! (`ToSocketAddrs::to_socket_addrs`, `tokio::net::lookup_host`) everywhere
//! else.
//!
//! - `getaddrinfo` blocks, so it runs on a pool of dedicated OS threads,
//!   never on a Tokio worker, a Compio `Owner` or an egress shard. The pool
//!   keeps [`RESOLVER_THREADS`] threads *available*: when a request's
//!   deadline passes while its lookup is still inside `getaddrinfo`, that
//!   thread is marked hung and a replacement starts, up to
//!   [`RESOLVER_MAX_THREADS`] in total. A hung thread exits when its lookup
//!   returns if the pool has its full complement without it. So distinct
//!   names that hang (a tenant's own name server can produce any number)
//!   never starve other names until the cap; at the cap, names wait in the
//!   queue until their deadline, counted as `cap_reached`.
//! - Concurrent requests for one `(host, port)` share one `getaddrinfo` call.
//!   A request made while that call is still running joins it, so a retrying
//!   output never stacks lookups of its hung name.
//! - Every request carries a deadline. A timer thread answers it with
//!   [`ResolveError::TimedOut`] at that instant even while the lookup is still
//!   running; the late answer then serves only requests still waiting.
//! - Names waiting for a thread sit in a bounded queue. A full queue answers
//!   [`ResolveError::Overflow`] at once and counts it ([`stats`]). Every
//!   request is answered exactly once.
//! - Threads are never joined. Dropping the last handle answers the waiting
//!   requests with [`ResolveError::Unavailable`]; each thread exits after its
//!   current lookup.
//!
//! Async code awaits [`Resolver::resolve`]. Shard threads pass
//! [`Resolver::submit`] a completion that pushes into their own queue, which
//! they drain on their media tick.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Lookup threads the pool keeps available (not stuck in a lookup past its
/// deadline).
pub(crate) const RESOLVER_THREADS: usize = 16;
/// Hard cap on lookup threads, hung ones included: each costs a thread
/// stack, and a name server can hold `getaddrinfo` for its full retry
/// schedule (resolv.conf `timeout` x `attempts`, tens of seconds).
pub(crate) const RESOLVER_MAX_THREADS: usize = 128;
/// Distinct names that may wait for a lookup thread.
pub(crate) const RESOLVER_QUEUE_CAPACITY: usize = 1024;
/// The deadline of a lookup whose caller has no per-output resolve timeout
/// (`LeafPolicy::resolve_timeout` serves the egress fabrics): HLS pull, the
/// SRT caller and MoQ relay connects.
pub(crate) const DEFAULT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The blocking lookup the pool threads run. Production passes
/// [`system_lookup`]; tests inject one that hangs, counts or panics.
pub(crate) type Lookup =
    Arc<dyn Fn(&str, u16) -> std::io::Result<Vec<SocketAddr>> + Send + Sync + 'static>;
/// Every address the name resolved to, in resolver order (never empty).
pub(crate) type Resolution = Result<Vec<SocketAddr>, ResolveError>;
type Deliver = Box<dyn FnOnce(Resolution) + Send + 'static>;
type Name = (String, u16);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The deadline passed before the lookup answered.
    TimedOut,
    /// The queue of names waiting for a lookup thread was full.
    Overflow,
    /// The lookup succeeded with no address.
    NoAddress,
    /// The lookup failed (or panicked).
    Failed(String),
    /// The resolver has no thread to run the lookup or has shut down.
    Unavailable,
    /// The text is not `host:port`.
    InvalidAddress(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => formatter.write_str("name resolution timed out"),
            Self::Overflow => formatter.write_str("name resolver queue full"),
            Self::NoAddress => formatter.write_str("name resolved to no address"),
            Self::Failed(error) => write!(formatter, "name resolution failed: {error}"),
            Self::Unavailable => formatter.write_str("name resolver unavailable"),
            Self::InvalidAddress(text) => write!(formatter, "{text} is not host:port"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Counters for the health surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolverStats {
    /// `getaddrinfo` calls started.
    pub lookups: u64,
    /// Requests answered at their deadline.
    pub timeouts: u64,
    /// Requests refused because the queue was full.
    pub overflows: u64,
    /// Live lookup threads, hung ones included (gauge).
    pub threads: u64,
    /// Live lookup threads still inside a lookup past a request's deadline
    /// (gauge).
    pub hung_threads: u64,
    /// Times a hung thread could not be replaced because the pool was at
    /// [`RESOLVER_MAX_THREADS`].
    pub cap_reached: u64,
}

struct Waiter {
    name: Name,
    deliver: Deliver,
}

/// One queued or running lookup.
#[derive(Default)]
struct Pending {
    /// The requests waiting on it.
    ids: Vec<u64>,
    /// A thread is inside the lookup.
    running: bool,
    /// A request's deadline passed while it ran: its thread is not
    /// counted as available, and a replacement was started.
    hung: bool,
}

#[derive(Default)]
struct State {
    /// Requests not yet answered, by id.
    waiters: HashMap<u64, Waiter>,
    /// Each queued or running lookup. An entry lives until its lookup
    /// returns (or a thread dequeues it with nobody left waiting), so a
    /// later request for the name joins it.
    names: HashMap<Name, Pending>,
    deadlines: BinaryHeap<Reverse<(Instant, u64)>>,
    next_id: u64,
    /// Live lookup threads, hung ones included.
    workers: usize,
    /// Live lookup threads marked hung (`workers - hung` are available).
    hung: usize,
    timer: bool,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    timer_wake: Condvar,
    lookup: Lookup,
    queue: flume::Receiver<Name>,
    threads: usize,
    max_threads: usize,
    lookups: AtomicU64,
    timeouts: AtomicU64,
    overflows: AtomicU64,
    cap_reached: AtomicU64,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        restream_sync::lock::lock(&self.state)
    }
}

pub struct Resolver {
    shared: Arc<Shared>,
    queue: flume::Sender<Name>,
}

static SHARED: LazyLock<Arc<Resolver>> = LazyLock::new(|| {
    Arc::new(Resolver::new(
        RESOLVER_THREADS,
        RESOLVER_MAX_THREADS,
        RESOLVER_QUEUE_CAPACITY,
        Arc::new(system_lookup),
    ))
});

/// The process resolver.
pub(crate) fn shared() -> Arc<Resolver> {
    Arc::clone(&SHARED)
}

/// The process resolver's counters.
pub fn stats() -> ResolverStats {
    SHARED.stats()
}

/// The system lookup (`getaddrinfo`). Runs only on the resolver's threads.
#[expect(
    clippy::disallowed_methods,
    reason = "the resolver's pool threads are the one place a system lookup runs"
)]
fn system_lookup(host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    std::net::ToSocketAddrs::to_socket_addrs(&(host, port)).map(Iterator::collect)
}

/// Splits `host:port` (an SRT peer; `[v6]:port` included) the way `&str`'s
/// `ToSocketAddrs` does.
pub(crate) fn split_host_port(text: &str) -> Result<(&str, u16), ResolveError> {
    text.rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse().ok()?)))
        .ok_or_else(|| ResolveError::InvalidAddress(text.to_string()))
}

/// An IP literal needs no lookup (`[v6]` as URLs carry it, too).
fn ip_literal(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|bare| bare.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

/// Runs a completion; a panic in it is that caller's alone, never the
/// thread that answers everyone else.
fn answer(deliver: Deliver, resolution: Resolution) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| deliver(resolution))) {
        let panic = crate::panic_boundary::record_contained(payload.as_ref());
        tracing::warn!(panic, "name resolution completion panicked");
    }
}

impl Resolver {
    /// A resolver keeping `threads` lookup threads available, at most
    /// `max_threads` in total, with room for `queue_capacity` names waiting
    /// for one. Threads start on first use.
    pub(crate) fn new(
        threads: usize,
        max_threads: usize,
        queue_capacity: usize,
        lookup: Lookup,
    ) -> Self {
        let (queue, queue_rx) = flume::bounded(queue_capacity);
        let threads = threads.max(1);
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                timer_wake: Condvar::new(),
                lookup,
                queue: queue_rx,
                threads,
                max_threads: max_threads.max(threads),
                lookups: AtomicU64::new(0),
                timeouts: AtomicU64::new(0),
                overflows: AtomicU64::new(0),
                cap_reached: AtomicU64::new(0),
            }),
            queue,
        }
    }

    pub fn stats(&self) -> ResolverStats {
        let (threads, hung_threads) = {
            let state = self.shared.state();
            (state.workers as u64, state.hung as u64)
        };
        ResolverStats {
            lookups: self.shared.lookups.load(Ordering::Relaxed),
            timeouts: self.shared.timeouts.load(Ordering::Relaxed),
            overflows: self.shared.overflows.load(Ordering::Relaxed),
            threads,
            hung_threads,
            cap_reached: self.shared.cap_reached.load(Ordering::Relaxed),
        }
    }

    /// Resolves `host` (a name or IP literal) for `port`, answering by
    /// `deadline` at the latest. Dropping the future abandons the request.
    pub(crate) async fn resolve(&self, host: &str, port: u16, deadline: Instant) -> Resolution {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.submit(host, port, deadline, move |resolution| {
            let _ = sender.send(resolution);
        });
        receiver.await.unwrap_or(Err(ResolveError::Unavailable))
    }

    /// Never blocks: `deliver` runs exactly once, inline for an IP literal,
    /// an overflow or an unavailable resolver, else on a resolver thread
    /// when the lookup answers or at `deadline`, whichever is first. It
    /// must not block (push into a queue, send on a oneshot).
    pub(crate) fn submit(
        &self,
        host: &str,
        port: u16,
        deadline: Instant,
        deliver: impl FnOnce(Resolution) + Send + 'static,
    ) {
        if let Some(ip) = ip_literal(host) {
            answer(Box::new(deliver), Ok(vec![SocketAddr::new(ip, port)]));
            return;
        }
        let deliver: Deliver = Box::new(deliver);
        let mut state = self.shared.state();
        ensure_threads(&self.shared, &mut state);
        if state.closed || state.workers == 0 || !state.timer {
            drop(state);
            answer(deliver, Err(ResolveError::Unavailable));
            return;
        }
        let name = (host.to_string(), port);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        if let Some(pending) = state.names.get_mut(&name) {
            pending.ids.push(id);
        } else {
            if self.queue.try_send(name.clone()).is_err() {
                drop(state);
                self.shared.overflows.fetch_add(1, Ordering::Relaxed);
                answer(deliver, Err(ResolveError::Overflow));
                return;
            }
            let pending = Pending {
                ids: vec![id],
                ..Pending::default()
            };
            state.names.insert(name.clone(), pending);
        }
        state.waiters.insert(id, Waiter { name, deliver });
        let earliest = state
            .deadlines
            .peek()
            .is_none_or(|Reverse((first, _))| deadline < *first);
        state.deadlines.push(Reverse((deadline, id)));
        drop(state);
        if earliest {
            self.shared.timer_wake.notify_one();
        }
    }
}

/// Starts threads until `threads` are available (hung ones do not count) or
/// the pool holds `max_threads`: on first use, after a lookup is marked
/// hung, and again after a spawn failure (thread or memory limits) or a
/// thread that died. Returns whether `threads` are available.
fn ensure_threads(shared: &Arc<Shared>, state: &mut State) -> bool {
    while state.workers - state.hung < shared.threads && state.workers < shared.max_threads {
        let worker = Arc::clone(shared);
        match std::thread::Builder::new()
            .name("dns-resolve".to_string())
            .spawn(move || run_worker(&worker))
        {
            Ok(_) => state.workers += 1,
            Err(error) => {
                tracing::warn!(%error, "name resolver thread did not start");
                break;
            }
        }
    }
    if !state.timer {
        let timer = Arc::clone(shared);
        match std::thread::Builder::new()
            .name("dns-deadline".to_string())
            .spawn(move || run_timer(&timer))
        {
            Ok(_) => state.timer = true,
            Err(error) => tracing::warn!(%error, "name resolver timer did not start"),
        }
    }
    state.workers - state.hung >= shared.threads
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.shared.state().closed = true;
        self.shared.timer_wake.notify_one();
    }
}

fn run_worker(shared: &Shared) {
    // `Ok(true)`: the thread left the pool as surplus and already
    // uncounted itself.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        while let Ok(name) = shared.queue.recv() {
            {
                let mut state = shared.state();
                if state.closed {
                    return false;
                }
                match state.names.get_mut(&name) {
                    Some(pending) if !pending.ids.is_empty() => pending.running = true,
                    _ => {
                        // Everyone waiting gave up while it queued.
                        state.names.remove(&name);
                        continue;
                    }
                }
            }
            shared.lookups.fetch_add(1, Ordering::Relaxed);
            let resolution = contained_lookup(shared, &name);
            let (answered, surplus) = {
                let mut state = shared.state();
                let pending = state.names.remove(&name).unwrap_or_default();
                // A hung thread is back: it rejoins the pool only if the
                // pool is short of available threads without it.
                let mut surplus = false;
                if pending.hung {
                    state.hung -= 1;
                    if state.workers - state.hung > shared.threads {
                        state.workers -= 1;
                        surplus = true;
                    }
                }
                let answered: Vec<Deliver> = pending
                    .ids
                    .into_iter()
                    .filter_map(|id| state.waiters.remove(&id))
                    .map(|waiter| waiter.deliver)
                    .collect();
                (answered, surplus)
            };
            for deliver in answered {
                answer(deliver, resolution.clone());
            }
            if surplus {
                return true;
            }
        }
        false
    }));
    if !matches!(outcome, Ok(true)) {
        shared.state().workers -= 1;
    }
    if let Err(payload) = outcome {
        let panic = crate::panic_boundary::record_contained(payload.as_ref());
        tracing::warn!(
            panic,
            "name resolver thread panicked; the next request restarts it"
        );
    }
}

/// One lookup. A panic in it fails that name only (counted), never the
/// thread and the names queued behind it.
fn contained_lookup(shared: &Shared, (host, port): &Name) -> Resolution {
    match catch_unwind(AssertUnwindSafe(|| (shared.lookup)(host, *port))) {
        Ok(Ok(addresses)) if addresses.is_empty() => Err(ResolveError::NoAddress),
        Ok(Ok(addresses)) => Ok(addresses),
        Ok(Err(error)) => Err(ResolveError::Failed(error.to_string())),
        Err(payload) => {
            let panic = crate::panic_boundary::record_contained(payload.as_ref());
            tracing::warn!(host, panic, "name lookup panicked; only this lookup failed");
            Err(ResolveError::Failed("lookup panicked".to_string()))
        }
    }
}

/// Answers each request at its deadline, and everyone once the resolver
/// is dropped.
fn run_timer(shared: &Arc<Shared>) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut state = shared.state();
        loop {
            if state.closed {
                let waiting: Vec<Deliver> = state
                    .waiters
                    .drain()
                    .map(|(_, waiter)| waiter.deliver)
                    .collect();
                state.names.clear();
                state.deadlines.clear();
                drop(state);
                for deliver in waiting {
                    answer(deliver, Err(ResolveError::Unavailable));
                }
                return;
            }
            let now = Instant::now();
            let mut expired = Vec::new();
            while let Some(&Reverse((at, id))) = state.deadlines.peek() {
                if at > now {
                    break;
                }
                state.deadlines.pop();
                // An id already answered by its lookup is gone: skip it.
                if let Some(waiter) = state.waiters.remove(&id) {
                    mark_hung(shared, &mut state, &waiter.name, id);
                    expired.push(waiter.deliver);
                }
            }
            if !expired.is_empty() {
                drop(state);
                shared
                    .timeouts
                    .fetch_add(expired.len() as u64, Ordering::Relaxed);
                for deliver in expired {
                    answer(deliver, Err(ResolveError::TimedOut));
                }
                state = shared.state();
                continue;
            }
            let next = state.deadlines.peek().map(|Reverse((at, _))| *at);
            state = match next {
                Some(at) => {
                    shared
                        .timer_wake
                        .wait_timeout(state, at.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => shared
                    .timer_wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }));
    shared.state().timer = false;
    if let Err(payload) = outcome {
        let panic = crate::panic_boundary::record_contained(payload.as_ref());
        tracing::warn!(
            panic,
            "name resolver timer panicked; the next request restarts it"
        );
    }
}

/// Request `id` for `name` passed its deadline. If a thread is inside that
/// lookup and not yet marked, it is hung: uncount it and start a
/// replacement, so other names keep their `threads` available ones.
fn mark_hung(shared: &Arc<Shared>, state: &mut State, name: &Name, id: u64) {
    let Some(pending) = state.names.get_mut(name) else {
        return;
    };
    pending.ids.retain(|waiting| *waiting != id);
    if !pending.running || pending.hung {
        return;
    }
    pending.hung = true;
    state.hung += 1;
    if !ensure_threads(shared, state) && state.workers >= shared.max_threads {
        shared.cap_reached.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
