//! System name lookups on the egress resolver OS threads.

use std::net::SocketAddr;

/// Runs one system name lookup (`getaddrinfo`) for `host` on a resolver OS
/// thread. A panic in it fails only that lookup (recorded, `None`), never the
/// resolver thread and the requests queued behind it, each of which must
/// still get its completion.
pub(crate) fn contained_lookup(
    host: &str,
    lookup: impl FnOnce() -> Option<SocketAddr>,
) -> Option<SocketAddr> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        if host.starts_with(INJECTED_LOOKUP_PANIC_HOST) {
            std::panic::resume_unwind(Box::new("injected name lookup panic"));
        }
        lookup()
    }))
    .unwrap_or_else(|payload| {
        let panic = crate::panic_boundary::record_contained(payload.as_ref());
        tracing::warn!(host, panic, "name lookup panicked; only this lookup failed");
        None
    })
}

/// A host whose lookup panics inside [`contained_lookup`].
#[cfg(test)]
pub(crate) const INJECTED_LOOKUP_PANIC_HOST: &str = "restream-test-lookup-panic.invalid";

#[cfg(test)]
mod tests {
    use super::*;

    /// A panicking lookup fails that RTMP or SRT resolution only: `None`,
    /// counted, and the calling thread carries on.
    #[test]
    fn a_panicking_lookup_fails_only_that_resolution() {
        let before = crate::panic_boundary::contained_panics();
        assert_eq!(
            crate::egress::backends::rtmp_shard::resolve_rtmp_peer_host(
                INJECTED_LOOKUP_PANIC_HOST,
                1935
            ),
            None
        );
        assert_eq!(
            crate::egress::backends::srt::resolve_srt_peer_host(&format!(
                "{INJECTED_LOOKUP_PANIC_HOST}:9000"
            )),
            None
        );
        assert!(crate::panic_boundary::contained_panics() >= before + 2);
    }
}
