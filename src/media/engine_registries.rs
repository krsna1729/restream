//! Registry structs for `MediaEngine` state ownership, grouping the
//! synchronized maps and sets that back ingest, egress, HLS, and stage lifecycles.

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
    RecentEgressOutcome, RecentIngestOutcome,
};
use crate::media::hls::HlsStore;
use crate::media::hls_fmp4::Fmp4HlsStore;
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_metrics::StageMetrics;
use crate::media::ts_chunk_ring::TsChunkRing;

pub type TranscoderBuffer = (Arc<RingBuffer>, CancellationToken);

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

pub struct StageRegistry {
    pub buffers: TokioRwLock<HashMap<StageKey, TranscoderBuffer>>,
    pub metrics: TokioRwLock<HashMap<StageKey, Arc<StageMetrics>>>,
    pub input_queues: TokioRwLock<HashMap<StageKey, Arc<MemoryQueue>>>,
    pub pipe_metrics: TokioRwLock<HashMap<StageKey, Arc<PipeMetrics>>>,
    pub ts_muxers: TokioRwLock<HashMap<String, Arc<TsChunkRing>>>,
}

impl Default for StageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl StageRegistry {
    pub fn new() -> Self {
        Self {
            buffers: TokioRwLock::new(HashMap::new()),
            metrics: TokioRwLock::new(HashMap::new()),
            input_queues: TokioRwLock::new(HashMap::new()),
            pipe_metrics: TokioRwLock::new(HashMap::new()),
            ts_muxers: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct RuntimeInfra {
    pub listener_stats: Arc<ListenerSocketStats>,
    pub os_threads: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub sender_semaphore: Arc<tokio::sync::Semaphore>,
    pub external_ffmpeg_semaphore: Arc<tokio::sync::Semaphore>,
    pub diag_semaphores: TokioRwLock<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    pub event_log: Arc<EventLog>,
}

impl Default for RuntimeInfra {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeInfra {
    pub fn new() -> Self {
        let external_ffmpeg_permits = external_ffmpeg_child_limit();
        Self {
            listener_stats: Arc::new(ListenerSocketStats::default()),
            os_threads: std::sync::Mutex::new(Vec::new()),
            sender_semaphore: Arc::new(tokio::sync::Semaphore::new(512)),
            external_ffmpeg_semaphore: Arc::new(tokio::sync::Semaphore::new(
                external_ffmpeg_permits,
            )),
            diag_semaphores: TokioRwLock::new(HashMap::new()),
            event_log: Arc::new(EventLog::new()),
        }
    }
}

fn external_ffmpeg_child_limit() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let reserve = std::env::var("RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .min(cpus.saturating_sub(1));
    let per_child = std::env::var("RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2)
        .max(1);
    let derived = cpus
        .saturating_sub(reserve)
        .max(1)
        .div_ceil(per_child)
        .max(1);
    let hard_cap = std::env::var("RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    derived.min(hard_cap).max(1)
}
