//! The process's one name resolver. Every hostname lookup in the engine goes
//! through it; clippy's `disallowed-methods` bans the system entry points
//! (`ToSocketAddrs::to_socket_addrs`, `tokio::net::lookup_host`) everywhere
//! else.
//!
//! - `getaddrinfo` blocks, so it runs on a fixed pool of [`RESOLVER_THREADS`]
//!   dedicated OS threads, never on a Tokio worker, a Compio `Owner` or an
//!   egress shard. One name whose server black-holes holds one thread; other
//!   names keep resolving on the rest.
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

/// Lookup threads in the process pool: how many distinct hung names it takes
/// before another name waits for a thread (it still gets its deadline).
pub(crate) const RESOLVER_THREADS: usize = 16;
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
}

struct Waiter {
    name: Name,
    deliver: Deliver,
}

#[derive(Default)]
struct State {
    /// Requests not yet answered, by id.
    waiters: HashMap<u64, Waiter>,
    /// The ids waiting on each queued or running lookup. An entry lives
    /// until its lookup returns (or a thread dequeues it with nobody left
    /// waiting), so a later request for the name joins it.
    names: HashMap<Name, Vec<u64>>,
    deadlines: BinaryHeap<Reverse<(Instant, u64)>>,
    next_id: u64,
    workers: usize,
    timer: bool,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    timer_wake: Condvar,
    lookup: Lookup,
    lookups: AtomicU64,
    timeouts: AtomicU64,
    overflows: AtomicU64,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        restream_sync::lock::lock(&self.state)
    }
}

pub struct Resolver {
    shared: Arc<Shared>,
    queue: flume::Sender<Name>,
    queue_rx: flume::Receiver<Name>,
    threads: usize,
}

static SHARED: LazyLock<Arc<Resolver>> = LazyLock::new(|| {
    Arc::new(Resolver::new(
        RESOLVER_THREADS,
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
    /// A resolver with `threads` lookup threads and room for
    /// `queue_capacity` names waiting for one. Threads start on first use.
    pub(crate) fn new(threads: usize, queue_capacity: usize, lookup: Lookup) -> Self {
        let (queue, queue_rx) = flume::bounded(queue_capacity);
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                timer_wake: Condvar::new(),
                lookup,
                lookups: AtomicU64::new(0),
                timeouts: AtomicU64::new(0),
                overflows: AtomicU64::new(0),
            }),
            queue,
            queue_rx,
            threads: threads.max(1),
        }
    }

    pub fn stats(&self) -> ResolverStats {
        ResolverStats {
            lookups: self.shared.lookups.load(Ordering::Relaxed),
            timeouts: self.shared.timeouts.load(Ordering::Relaxed),
            overflows: self.shared.overflows.load(Ordering::Relaxed),
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
        self.ensure_threads(&mut state);
        if state.closed || state.workers == 0 || !state.timer {
            drop(state);
            answer(deliver, Err(ResolveError::Unavailable));
            return;
        }
        let name = (host.to_string(), port);
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        if let Some(ids) = state.names.get_mut(&name) {
            ids.push(id);
        } else {
            if self.queue.try_send(name.clone()).is_err() {
                drop(state);
                self.shared.overflows.fetch_add(1, Ordering::Relaxed);
                answer(deliver, Err(ResolveError::Overflow));
                return;
            }
            state.names.insert(name.clone(), vec![id]);
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

    /// Starts missing threads: on first use, and again after a spawn
    /// failure (thread or memory limits) or a thread that died.
    fn ensure_threads(&self, state: &mut State) {
        while state.workers < self.threads {
            let (shared, queue) = (Arc::clone(&self.shared), self.queue_rx.clone());
            match std::thread::Builder::new()
                .name("dns-resolve".to_string())
                .spawn(move || run_worker(&shared, &queue))
            {
                Ok(_) => state.workers += 1,
                Err(error) => {
                    tracing::warn!(%error, "name resolver thread did not start");
                    break;
                }
            }
        }
        if !state.timer {
            let shared = Arc::clone(&self.shared);
            match std::thread::Builder::new()
                .name("dns-deadline".to_string())
                .spawn(move || run_timer(&shared))
            {
                Ok(_) => state.timer = true,
                Err(error) => tracing::warn!(%error, "name resolver timer did not start"),
            }
        }
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.shared.state().closed = true;
        self.shared.timer_wake.notify_one();
    }
}

fn run_worker(shared: &Shared, queue: &flume::Receiver<Name>) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        while let Ok(name) = queue.recv() {
            {
                let mut state = shared.state();
                if state.closed {
                    return;
                }
                if state.names.get(&name).is_some_and(Vec::is_empty) {
                    // Everyone waiting gave up while it queued.
                    state.names.remove(&name);
                    continue;
                }
            }
            shared.lookups.fetch_add(1, Ordering::Relaxed);
            let resolution = contained_lookup(shared, &name);
            let answered: Vec<Deliver> = {
                let mut state = shared.state();
                let ids = state.names.remove(&name).unwrap_or_default();
                ids.into_iter()
                    .filter_map(|id| state.waiters.remove(&id))
                    .map(|waiter| waiter.deliver)
                    .collect()
            };
            for deliver in answered {
                answer(deliver, resolution.clone());
            }
        }
    }));
    shared.state().workers -= 1;
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
fn run_timer(shared: &Shared) {
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
                    if let Some(ids) = state.names.get_mut(&waiter.name) {
                        ids.retain(|waiting| *waiting != id);
                    }
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

#[cfg(test)]
pub(crate) mod fake;
#[cfg(test)]
mod tests;
