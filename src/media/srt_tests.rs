use super::*;
use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use crate::domain::srt_ingest::SrtGlobalIngestConfig;
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::security::IngestSecurityService;
use crate::media::ts_chunk_ring::TsChunkRing;
use proptest::prelude::*;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tokio_util::sync::CancellationToken;

fn srt_test_runtime_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serializes tests that mutate libsrt process-global state and guarantees
/// cleanup even when an assertion unwinds.
struct SrtTestRuntime {
    _guard: MutexGuard<'static, ()>,
}

impl SrtTestRuntime {
    fn lock() -> Self {
        Self {
            _guard: srt_test_runtime_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }

    fn startup() -> Self {
        let runtime = Self::lock();
        // SAFETY: the process-global test lock excludes concurrent
        // srt_startup/srt_cleanup calls for the lifetime of this fixture.
        unsafe {
            assert_eq!(srt_startup(), 0);
        }
        runtime
    }
}

impl Drop for SrtTestRuntime {
    fn drop(&mut self) {
        // SAFETY: every socket/config created by the test is closed before the
        // fixture is dropped, and the process-global test lock is still held.
        unsafe {
            srt_cleanup();
        }
    }
}

include!("srt_tests/policy.rs");
include!("srt_tests/quality.rs");
include!("srt_tests/muxing.rs");
include!("srt_tests/socket_runtime.rs");
include!("srt_tests/readiness.rs");

#[path = "srt_tests/shared_muxer.rs"]
mod shared_muxer;
