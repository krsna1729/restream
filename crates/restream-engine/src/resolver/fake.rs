//! A production [`Resolver`] whose injected lookup is scripted by name, for
//! tests that need a name server that hangs, answers slowly or panics.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::{RESOLVER_QUEUE_CAPACITY, RESOLVER_THREADS, Resolver};

/// Names starting with `hung` block until this value drops; `slow` ones
/// answer after [`FakeDns::SLOW`]; `panic` ones panic; `fail` ones fail;
/// every other name answers 127.0.0.1.
pub(crate) struct FakeDns {
    pub(crate) resolver: Arc<Resolver>,
    calls: Arc<AtomicUsize>,
    _release: flume::Sender<()>,
}

impl FakeDns {
    pub(crate) const SLOW: Duration = Duration::from_millis(200);

    /// The production pool size and queue.
    pub(crate) fn new() -> Self {
        Self::with_pool(RESOLVER_THREADS, RESOLVER_QUEUE_CAPACITY)
    }

    pub(crate) fn with_pool(threads: usize, queue_capacity: usize) -> Self {
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
            resolver: Arc::new(Resolver::new(threads, queue_capacity, Arc::new(lookup))),
            calls,
            _release: release,
        }
    }

    /// Lookups the pool has run so far.
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
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
