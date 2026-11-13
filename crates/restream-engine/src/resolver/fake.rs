//! A production [`Resolver`] whose injected lookup is scripted by name, for
//! tests that need a name server that hangs, answers slowly or panics.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{RESOLVER_MAX_THREADS, RESOLVER_QUEUE_CAPACITY, RESOLVER_THREADS, Resolver};

/// Names starting with `hung` block until [`FakeDns::release_hung`] or this
/// value drops; `slow` ones answer after [`FakeDns::SLOW`]; `panic` ones
/// panic; `fail` ones fail; every other name answers 127.0.0.1.
pub(crate) struct FakeDns {
    pub(crate) resolver: Arc<Resolver>,
    calls: Arc<AtomicUsize>,
    release: Mutex<Option<flume::Sender<()>>>,
}

impl FakeDns {
    pub(crate) const SLOW: Duration = Duration::from_millis(200);

    /// The production pool: sizes, cap and queue.
    pub(crate) fn new() -> Self {
        Self::build(
            RESOLVER_THREADS,
            RESOLVER_MAX_THREADS,
            RESOLVER_QUEUE_CAPACITY,
        )
    }

    /// A pool already at its cap (`threads` total, never replaced), to
    /// exercise queueing and overflow.
    pub(crate) fn with_pool(threads: usize, queue_capacity: usize) -> Self {
        Self::build(threads, threads, queue_capacity)
    }

    pub(crate) fn build(threads: usize, max_threads: usize, queue_capacity: usize) -> Self {
        let (release, released) = flume::bounded::<()>(0);
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let lookup = move |host: &str, port: u16| {
            counted.fetch_add(1, Ordering::SeqCst);
            if host.starts_with("hung") {
                // Disconnects when the `FakeDns` drops.
                let _ = released.recv_timeout(Duration::from_secs(60));
                return Err(std::io::Error::other("released"));
            }
            if host.starts_with("slow") {
                std::thread::sleep(Self::SLOW);
            }
            if host.starts_with("panic") {
                // `resume_unwind` skips the panic hook: test logs stay quiet.
                std::panic::resume_unwind(Box::new("injected name lookup panic"));
            }
            if host.starts_with("fail") {
                return Err(std::io::Error::other("no such name"));
            }
            Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)])
        };
        Self {
            resolver: Arc::new(Resolver::new(
                threads,
                max_threads,
                queue_capacity,
                Arc::new(lookup),
            )),
            calls,
            release: Mutex::new(Some(release)),
        }
    }

    /// Lookups the pool has run so far.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Lets every hung lookup (and any later one) return.
    pub(crate) fn release_hung(&self) {
        restream_sync::lock::lock(&self.release).take();
    }

    /// Drops the resolver while its hung lookups stay hung; they return
    /// when the result drops.
    pub(crate) fn drop_resolver(self) -> impl Sized {
        let Self {
            resolver, release, ..
        } = self;
        drop(resolver);
        release
    }

    /// Waits until `count` lookups have started.
    pub(crate) fn wait_for_calls(&self, count: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.calls() < count {
            assert!(
                std::time::Instant::now() < deadline,
                "only {} of {count} lookups started",
                self.calls()
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
