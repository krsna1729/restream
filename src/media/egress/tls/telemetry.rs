//! Per-user TLS connection counters. Each TLS egress protocol owns one
//! static [`TlsCounters`] and passes it to every connection it opens, so the
//! counters stay per protocol while the connection code is shared.
use std::sync::atomic::{AtomicU64, Ordering};

use tokio_rustls::rustls::{CipherSuite, ProtocolVersion};

use super::ktls;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TlsTelemetrySnapshot {
    pub(crate) connections: u64,
    pub(crate) tls12: u64,
    pub(crate) tls13: u64,
    pub(crate) ktls_requested: u64,
    pub(crate) ktls_attempts: u64,
    pub(crate) ktls_success: u64,
    pub(crate) ktls_unsupported: u64,
    pub(crate) ktls_error: u64,
    pub(crate) userspace_tls_connections: u64,
    pub(crate) ktls_tls12_aes128_gcm: bool,
    pub(crate) ktls_tls12_aes256_gcm: bool,
    pub(crate) ktls_tls13_aes128_gcm: bool,
    pub(crate) ktls_tls13_aes256_gcm: bool,
}

pub(crate) struct TlsCounters {
    pub(super) connections: AtomicU64,
    pub(super) tls12: AtomicU64,
    pub(super) tls13: AtomicU64,
    pub(super) ktls_requested: AtomicU64,
    pub(super) ktls_attempts: AtomicU64,
    pub(super) ktls_success: AtomicU64,
    pub(super) ktls_unsupported: AtomicU64,
    pub(super) ktls_error: AtomicU64,
    pub(super) userspace_tls_connections: AtomicU64,
}

impl TlsCounters {
    pub(crate) const fn new() -> Self {
        Self {
            connections: AtomicU64::new(0),
            tls12: AtomicU64::new(0),
            tls13: AtomicU64::new(0),
            ktls_requested: AtomicU64::new(0),
            ktls_attempts: AtomicU64::new(0),
            ktls_success: AtomicU64::new(0),
            ktls_unsupported: AtomicU64::new(0),
            ktls_error: AtomicU64::new(0),
            userspace_tls_connections: AtomicU64::new(0),
        }
    }

    pub(super) fn add(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// The counters plus which kTLS ciphers this kernel accepts.
    pub(crate) fn snapshot(&self) -> TlsTelemetrySnapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        TlsTelemetrySnapshot {
            connections: load(&self.connections),
            tls12: load(&self.tls12),
            tls13: load(&self.tls13),
            ktls_requested: load(&self.ktls_requested),
            ktls_attempts: load(&self.ktls_attempts),
            ktls_success: load(&self.ktls_success),
            ktls_unsupported: load(&self.ktls_unsupported),
            ktls_error: load(&self.ktls_error),
            userspace_tls_connections: load(&self.userspace_tls_connections),
            ktls_tls12_aes128_gcm: ktls::supports(
                ProtocolVersion::TLSv1_2,
                CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
            ),
            ktls_tls12_aes256_gcm: ktls::supports(
                ProtocolVersion::TLSv1_2,
                CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
            ),
            ktls_tls13_aes128_gcm: ktls::supports(
                ProtocolVersion::TLSv1_3,
                CipherSuite::TLS13_AES_128_GCM_SHA256,
            ),
            ktls_tls13_aes256_gcm: ktls::supports(
                ProtocolVersion::TLSv1_3,
                CipherSuite::TLS13_AES_256_GCM_SHA384,
            ),
        }
    }
}
