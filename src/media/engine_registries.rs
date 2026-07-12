//! Registry structs for `MediaEngine` state ownership, grouping the
//! synchronized maps and sets that back ingest, egress, HLS, and stage lifecycles.
//!
//! Registry access rule: do not hold registry guards across awaits that acquire
//! another registry. Snapshot paths should copy or render the fields they need
//! inside a bounded scope, drop all guards, then perform async enrichment such
//! as stage or HLS lookups. Lifecycle paths that must touch more than one
//! registry keep the critical section short and avoid calling out while guards
//! are live.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::RwLock as TokioRwLock;
use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKey;
use crate::events::EventLog;
use crate::media::avio::MemoryQueue;
use crate::media::engine::{
    ActiveEgress, ActiveIngest, EgressRetryState, HlsConsumers, ListenerSocketStats,
    RecentEgressOutcome, RecentIngestOutcome, RtmpListenerStats,
};
use crate::media::hls::HlsStore;
use crate::media::hls_fmp4::Fmp4HlsStore;
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;
use crate::media::ts_chunk_ring::TsChunkRing;

pub struct IngestRegistry {
    pub pipelines: TokioRwLock<HashMap<String, Arc<RingBuffer>>>,
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub next_attempt_id: AtomicU64,
    pub active: TokioRwLock<HashMap<String, ActiveIngest>>,
    pub recent: TokioRwLock<HashMap<String, RecentIngestOutcome>>,
}

impl Default for IngestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl IngestRegistry {
    pub fn new() -> Self {
        Self {
            pipelines: TokioRwLock::new(HashMap::new()),
            cancel_tokens: TokioRwLock::new(HashMap::new()),
            next_attempt_id: AtomicU64::new(1),
            active: TokioRwLock::new(HashMap::new()),
            recent: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct EgressRegistry {
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub next_attempt_id: AtomicU64,
    pub active: TokioRwLock<HashMap<String, ActiveEgress>>,
    pub queues: TokioRwLock<HashMap<String, Arc<MemoryQueue>>>,
    pub recent: TokioRwLock<HashMap<String, RecentEgressOutcome>>,
    pub retry: TokioRwLock<HashMap<String, EgressRetryState>>,
}

impl Default for EgressRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressRegistry {
    pub fn new() -> Self {
        Self {
            cancel_tokens: TokioRwLock::new(HashMap::new()),
            next_attempt_id: AtomicU64::new(1),
            active: TokioRwLock::new(HashMap::new()),
            queues: TokioRwLock::new(HashMap::new()),
            recent: TokioRwLock::new(HashMap::new()),
            retry: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct RecordingRegistry {
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
}

impl Default for RecordingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingRegistry {
    pub fn new() -> Self {
        Self {
            cancel_tokens: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct HlsRegistry {
    pub stores: TokioRwLock<HashMap<String, Arc<HlsStore>>>,
    pub preview_stores: TokioRwLock<HashMap<String, Arc<Fmp4HlsStore>>>,
    pub consumers: TokioRwLock<HashMap<String, HlsConsumers>>,
}

impl Default for HlsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HlsRegistry {
    pub fn new() -> Self {
        Self {
            stores: TokioRwLock::new(HashMap::new()),
            preview_stores: TokioRwLock::new(HashMap::new()),
            consumers: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct FileIngestRegistry {
    pub children: TokioRwLock<HashMap<String, tokio::process::Child>>,
    pub active: TokioRwLock<HashSet<String>>,
}

impl Default for FileIngestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FileIngestRegistry {
    pub fn new() -> Self {
        Self {
            children: TokioRwLock::new(HashMap::new()),
            active: TokioRwLock::new(HashSet::new()),
        }
    }
}

#[derive(Clone)]
pub struct StageRuntime {
    pub ring: Option<Arc<RingBuffer>>,
    pub cancel: CancellationToken,
    pub lifecycle: Arc<StageLifecycle>,
    pub metrics: Arc<StageMetrics>,
    pub input_queue: Option<Arc<MemoryQueue>>,
    pub pipe_metrics: Option<Arc<PipeMetrics>>,
}

pub struct StageRegistry {
    pub runtimes: TokioRwLock<HashMap<StageKey, StageRuntime>>,
    pub metrics: TokioRwLock<HashMap<StageKey, Arc<StageMetrics>>>,
    pub ts_muxers: TokioRwLock<HashMap<String, Arc<TsChunkRing>>>,
    pub lifecycles: TokioRwLock<HashMap<StageKey, Arc<StageLifecycle>>>,
}

impl Default for StageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl StageRegistry {
    pub fn new() -> Self {
        Self {
            runtimes: TokioRwLock::new(HashMap::new()),
            metrics: TokioRwLock::new(HashMap::new()),
            ts_muxers: TokioRwLock::new(HashMap::new()),
            lifecycles: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct RuntimeInfra {
    pub listener_stats: Arc<ListenerSocketStats>,
    pub rtmp_listener_stats: Arc<RtmpListenerStats>,
    pub os_threads: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub listener_shutdowns: std::sync::Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    pub sender_semaphore: Arc<tokio::sync::Semaphore>,
    pub external_ffmpeg_semaphore: Arc<tokio::sync::Semaphore>,
    pub diag_semaphores: TokioRwLock<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    pub event_log: Arc<EventLog>,
}

impl Default for RuntimeInfra {
    fn default() -> Self {
        Self::new(&crate::AppConfig::default())
    }
}

impl RuntimeInfra {
    pub fn new(config: &crate::AppConfig) -> Self {
        let external_ffmpeg_permits = config.external_ffmpeg_permits;
        Self {
            listener_stats: Arc::new(ListenerSocketStats::default()),
            rtmp_listener_stats: Arc::new(RtmpListenerStats::default()),
            os_threads: std::sync::Mutex::new(Vec::new()),
            listener_shutdowns: std::sync::Mutex::new(Vec::new()),
            sender_semaphore: Arc::new(tokio::sync::Semaphore::new(512)),
            external_ffmpeg_semaphore: Arc::new(tokio::sync::Semaphore::new(
                external_ffmpeg_permits,
            )),
            diag_semaphores: TokioRwLock::new(HashMap::new()),
            event_log: Arc::new(EventLog::new()),
        }
    }
}
