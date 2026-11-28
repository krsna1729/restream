//! Registry structs for `MediaEngine` state ownership, grouping the
//! synchronized maps and sets that back egress, fabric, HLS, recording,
//! stream-QA and file-ingest lifecycles, and the egress records they store.
//! Ingest state is in [`crate::ingest_registry`], shared-stage state in
//! [`crate::stage_registry`].
//!
//! Registry access rule: do not hold registry guards across awaits that acquire
//! another registry. Snapshot paths should copy or render the fields they need
//! inside a bounded scope, drop all guards, then perform async enrichment such
//! as stage or HLS lookups. Lifecycle paths that must touch more than one
//! registry keep the critical section short and avoid calling out while guards
//! are live.
//!
//! Tokio's `RwLock` is fair: once a writer waits, new readers queue behind it.
//! A task that holds a guard and then asks for the same lock again, or for a
//! lock some writer holds while it waits on the first, deadlocks with that
//! writer. The few sections that must hold more than one guard take them in
//! one order per registry, and never the reverse:
//!
//! - ingest: in the [`crate::ingest_registry`] module docs. Across
//!   registries: `ingests.active` is read inside egress unregistration with
//!   `try_read` only, and a MoQ origin's per-pipeline attach lock comes
//!   before that pipeline's `ingests.selection`;
//! - egress: `retry` -> `cancel_tokens` -> `active` -> `recent` (a settled
//!   outcome is written under the same `active` guard that removes its
//!   attempt, so a late settlement cannot follow a replacement);
//! - stage: in the [`crate::stage_registry`] module docs;
//! - file ingest: `ingests.sessions` -> `children` -> `active` -> `positions`
//!   (any subsequence); `active` is never held while taking `children`.
//!
//! Fabric protocol guards protect only map/refcount transactions and snapshots.
//! Construction runs off-lock; publication rechecks the registry generation and
//! winner. `SharedFabricRuntime` serializes one feed's mutations with an owned
//! async mutex guard transferred to the blocking pool. No protocol guard crosses
//! that wait. Health reads use independent snapshot handles, and periodic sizing
//! skips a busy feed instead of delaying the other feeds or housekeeping.
//!
//! `clippy::await_holding_invalid_type` (`clippy.toml`) flags a tokio guard
//! or a [`restream_sync::keyed_lock::KeyedGuard`] bound directly to a local
//! across an await; each such section carries an `#[expect]` naming its
//! order. It does not see a guard inside an `Option`, a tuple or a struct
//! field: today the `selected_inputs` and `active` guards of
//! `try_register_pipeline_input_attempt` (already under an `#[expect]`) and
//! `ObjectBody`'s `segment.growing` claim lock in `hls_ingest.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock};
use tokio_util::sync::CancellationToken;

use crate::egress::FeedId;
use crate::egress::runtime::{FeedWakeSubscription, SharedFabricRuntime};
use crate::events::EventLog;
use crate::hls::HlsStore;
use crate::hls_fmp4::Fmp4HlsStore;
use crate::snapshots::{ListenerSocketStats, PublisherQuality, RtmpListenerStats};
use crate::stage_metrics::{StageMetrics, StageMetricsSnapshot};
use restream_domain::domain::stage::StageKey;
use restream_domain::domain::state::{EgressPhase, EgressRuntimeStatus, EgressStatus};

pub(crate) const EGRESS_FLAP_WINDOW_MS: u64 = 30_000;

/// Where a fabric output runs: its feed and the shard index assigned to it.
/// `(feed, shard)` names one shard heartbeat (feed ids carry a per-protocol
/// prefix, so they are unique across fabric registries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricSlot {
    pub feed: FeedId,
    pub shard: u32,
}

pub struct ActiveEgress {
    pub attempt_id: u64,
    pub output_id: String,
    pub pipeline_id: String,
    pub protocol: String,
    pub target_url: String,
    pub target_addr: Arc<std::sync::Mutex<Option<String>>>,
    pub status: EgressStatus,
    pub phase: Arc<std::sync::Mutex<EgressPhase>>,
    pub started_at: String,
    pub start_instant: Instant,
    pub bytes_sent: Arc<AtomicU64>,
    pub metrics: Arc<StageMetrics>,
    pub last_progress_ms: Arc<AtomicU64>,
    pub last_error: Arc<std::sync::Mutex<Option<String>>>,
    pub last_error_ms: Arc<AtomicU64>,
    pub failure_phase: Arc<std::sync::Mutex<Option<String>>>,
    pub quality: Arc<std::sync::Mutex<PublisherQuality>>,
    pub prev_bytes_sent: AtomicU64,
    pub prev_sample_time: std::sync::Mutex<Instant>,
    pub bitrate_kbps: std::sync::Mutex<Option<f64>>,
    pub terminal_stage_key: Option<StageKey>,
    pub output_name: String,
    pub encoding: String,
    /// Whether this output is owned by the egress fabric's shard runtime
    /// (RTMP, RTMPS, SRT, sink discard, and pipeline recirculation — every
    /// network-egress output type). `false` for output types the fabric
    /// does not cover (HLS PUT upload, recording), which have their own
    /// task shape and no shard/leaf concept. Set once, right after
    /// registration, from the same routing decision the bootstrap egress
    /// reconciler already made — never inferred from `protocol`. `None`
    /// for output types without a shard/leaf (the Reqwest HLS uploader,
    /// recording).
    pub fabric: Option<FabricSlot>,
    pub resync_count: Arc<AtomicU64>,
    /// Feed units the leaf's cursor is currently behind the feed head.
    /// Fabric outputs only; stays 0 for non-fabric output types, which have
    /// no shared feed cursor to report against.
    pub feed_lag_units: Arc<AtomicU64>,
    /// Reason for the leaf's current send-path health (`None` when
    /// idle/healthy). Fabric outputs only.
    pub backpressure_reason: Arc<std::sync::Mutex<Option<&'static str>>>,
}

#[derive(Debug, Clone)]
pub struct RecentEgressOutcome {
    pub output_id: String,
    pub pipeline_id: String,
    pub protocol: String,
    pub target_url: String,
    pub target_addr: Option<String>,
    pub status: EgressRuntimeStatus,
    pub raw_status: EgressStatus,
    pub phase: EgressPhase,
    pub started_at: String,
    pub uptime_secs: f64,
    pub bytes_sent: u64,
    pub last_progress_ms: u64,
    pub resync_count: u64,
    pub feed_lag_units: u64,
    pub backpressure_reason: Option<&'static str>,
    pub last_error: Option<String>,
    pub last_error_ms: u64,
    pub failure_phase: Option<String>,
    pub first_failure_at_ms: u64,
    pub failure_count: u32,
    pub quality: PublisherQuality,
    pub metrics: StageMetricsSnapshot,
    pub ended_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct EgressRetryState {
    pub attempts: u32,
    pub backoff_ms: u64,
    pub next_retry_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct EgressRegistration {
    pub cancel_token: CancellationToken,
    pub attempt_id: u64,
}

pub struct EgressRegistry {
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub next_attempt_id: AtomicU64,
    pub active: TokioRwLock<HashMap<String, ActiveEgress>>,
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
            recent: TokioRwLock::new(HashMap::new()),
            retry: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct SrtFabricRegistry {
    pub runtimes: HashMap<FeedId, SharedFabricRuntime>,
    /// Fences constructors started before a whole-registry shutdown.
    pub(crate) generation: u64,
    pub(crate) active_outputs: HashMap<FeedId, u64>,
    /// One publication wake subscription per feed runtime; dropped on release.
    pub(crate) feed_wakes: HashMap<FeedId, FeedWakeSubscription>,
    /// A dedicated reader per feed runtime, held only so
    /// `EgressFabricRuntime::rescale` can mint a fresh reader for a shard
    /// grown after startup — see `RtmpFabricRegistry::feeds`.
    pub(crate) feeds: HashMap<FeedId, crate::egress::journal::TsFeed>,
}

impl SrtFabricRegistry {
    fn new() -> Self {
        Self {
            runtimes: HashMap::new(),
            generation: 0,
            active_outputs: HashMap::new(),
            feed_wakes: HashMap::new(),
            feeds: HashMap::new(),
        }
    }
}

pub struct RtmpFabricRegistry {
    pub runtimes: HashMap<FeedId, SharedFabricRuntime>,
    pub(crate) generation: u64,
    pub(crate) active_outputs: HashMap<FeedId, u64>,
    /// One publish-startup source per feed runtime, shared (cloned) across
    /// that runtime's shards; the bootstrap path writes into it before
    /// dispatching `EgressCommand::Add` for an output on that feed.
    pub(crate) startup_sources:
        HashMap<FeedId, crate::egress::backends::rtmp_shard::SharedRtmpPublishStartupSource>,
    /// One publication wake subscription per feed runtime; dropped on release.
    pub(crate) feed_wakes: HashMap<FeedId, FeedWakeSubscription>,
    /// A dedicated reader per feed runtime, held only so
    /// `EgressFabricRuntime::rescale` can mint a fresh reader
    /// (`RingFeed::clone_reader`) for a shard grown after startup — the
    /// same construction the initial `spawn_rtmp_fabric_shard_group` call
    /// used per shard, just invoked later. `RingFeed` isn't `Clone` (its
    /// `cached_oldest` counter is per-reader), hence storing a dedicated
    /// instance rather than reusing another reader.
    pub(crate) feeds: HashMap<FeedId, crate::egress::journal::RingFeed>,
    /// The RTMPS trust-root config resolved once at creation time
    /// (`resolve_rtmps_client_config` parses PEM files — not something to
    /// redo on every rescale call), reused for any shard grown later.
    pub(crate) rtmps_client_configs: HashMap<FeedId, Arc<tokio_rustls::rustls::ClientConfig>>,
}

impl RtmpFabricRegistry {
    fn new() -> Self {
        Self {
            runtimes: HashMap::new(),
            generation: 0,
            active_outputs: HashMap::new(),
            startup_sources: HashMap::new(),
            feed_wakes: HashMap::new(),
            feeds: HashMap::new(),
            rtmps_client_configs: HashMap::new(),
        }
    }
}

/// MoQ fabric runtimes, one per feed — RTMP's shape: a shared startup
/// source the bootstrap path writes before `Add`, one wake subscription and
/// one dedicated reader (for shards grown later) per runtime.
pub struct MoqFabricRegistry {
    pub runtimes: HashMap<FeedId, SharedFabricRuntime>,
    pub(crate) generation: u64,
    pub(crate) active_outputs: HashMap<FeedId, u64>,
    pub(crate) startup_sources:
        HashMap<FeedId, crate::egress::backends::moq::SharedMoqStartupSource>,
    pub(crate) feed_wakes: HashMap<FeedId, FeedWakeSubscription>,
    pub(crate) feeds: HashMap<FeedId, crate::egress::journal::RingFeed>,
}

impl MoqFabricRegistry {
    fn new() -> Self {
        Self {
            runtimes: HashMap::new(),
            generation: 0,
            active_outputs: HashMap::new(),
            startup_sources: HashMap::new(),
            feed_wakes: HashMap::new(),
            feeds: HashMap::new(),
        }
    }
}

pub struct SinkFabricRegistry {
    pub runtimes: HashMap<FeedId, SharedFabricRuntime>,
    pub(crate) generation: u64,
    pub(crate) active_outputs: HashMap<FeedId, u64>,
    /// One publication wake subscription per feed runtime; dropped on release.
    pub(crate) feed_wakes: HashMap<FeedId, FeedWakeSubscription>,
    pub(crate) feeds: HashMap<FeedId, crate::egress::journal::RingFeed>,
}

impl SinkFabricRegistry {
    fn new() -> Self {
        Self {
            runtimes: HashMap::new(),
            generation: 0,
            active_outputs: HashMap::new(),
            feed_wakes: HashMap::new(),
            feeds: HashMap::new(),
        }
    }
}

pub struct PipelineFabricRegistry {
    pub runtimes: HashMap<FeedId, SharedFabricRuntime>,
    pub(crate) generation: u64,
    pub(crate) active_outputs: HashMap<FeedId, u64>,
    /// One target source per feed runtime, shared (cloned) across that
    /// runtime's shards — see `RtmpFabricRegistry::startup_sources` for
    /// why this split exists (claiming the target input is async and
    /// fallible, so it cannot happen on a shard thread).
    pub(crate) target_sources:
        HashMap<FeedId, crate::egress::backends::pipeline_shard::SharedPipelineTargetSource>,
    /// One publication wake subscription per feed runtime; dropped on release.
    pub(crate) feed_wakes: HashMap<FeedId, FeedWakeSubscription>,
    pub(crate) feeds: HashMap<FeedId, crate::egress::journal::RingFeed>,
}

impl PipelineFabricRegistry {
    fn new() -> Self {
        Self {
            runtimes: HashMap::new(),
            generation: 0,
            active_outputs: HashMap::new(),
            target_sources: HashMap::new(),
            feed_wakes: HashMap::new(),
            feeds: HashMap::new(),
        }
    }
}

/// HLS PUT fabric runtimes, one per HLS store.
#[derive(Default)]
pub struct HlsPutFabricRegistry {
    pub runtimes: HashMap<FeedId, SharedFabricRuntime>,
    pub(crate) generation: u64,
    pub(crate) active_outputs: HashMap<FeedId, u64>,
    /// One store publication wake subscription per runtime; dropped on release.
    pub(crate) feed_wakes: HashMap<FeedId, FeedWakeSubscription>,
    /// The store and TLS client config every shard of the runtime shares.
    pub(crate) shared: HashMap<
        FeedId,
        (
            std::sync::Arc<crate::hls::HlsStore>,
            std::sync::Arc<tokio_rustls::rustls::ClientConfig>,
        ),
    >,
}

pub struct FabricRegistry {
    pub srt: TokioMutex<SrtFabricRegistry>,
    pub rtmp: TokioMutex<RtmpFabricRegistry>,
    pub sink: TokioMutex<SinkFabricRegistry>,
    pub pipeline: TokioMutex<PipelineFabricRegistry>,
    pub hls_put: TokioMutex<HlsPutFabricRegistry>,
    pub moq: TokioMutex<MoqFabricRegistry>,
}

impl Default for FabricRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FabricRegistry {
    pub fn new() -> Self {
        Self {
            srt: TokioMutex::new(SrtFabricRegistry::new()),
            rtmp: TokioMutex::new(RtmpFabricRegistry::new()),
            sink: TokioMutex::new(SinkFabricRegistry::new()),
            pipeline: TokioMutex::new(PipelineFabricRegistry::new()),
            hls_put: TokioMutex::new(HlsPutFabricRegistry::default()),
            moq: TokioMutex::new(MoqFabricRegistry::new()),
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

/// Each checked pipeline's sampler: its stop token and its findings.
pub struct StreamQaEntry {
    pub cancel: CancellationToken,
    pub findings: Arc<std::sync::Mutex<crate::stream_qa::StreamQa>>,
}

#[derive(Default)]
pub struct StreamQaRegistry {
    pub entries: std::sync::Mutex<HashMap<String, StreamQaEntry>>,
}

/// Tracks HLS consumers for a pipeline. Persistent consumers (egress outputs)
/// hold a [`PersistentLease`]. Transient consumers (browser preview) keep the
/// segmenter alive via playlist fetch heartbeats.
pub struct HlsConsumers {
    /// Live [`PersistentLease`]s taken from this entry; only leases change it.
    persistent: Arc<AtomicU64>,
    /// Monotonic reference time.
    pub reference_instant: Instant,
    /// Monotonic elapsed millis since reference_instant for the last access.
    pub last_access_ms: AtomicU64,
    /// Cancel token for the segmenter task.
    pub cancel_token: CancellationToken,
}

impl HlsConsumers {
    pub fn new(cancel_token: CancellationToken) -> Self {
        Self {
            persistent: Arc::new(AtomicU64::new(0)),
            reference_instant: Instant::now(),
            last_access_ms: AtomicU64::new(0),
            cancel_token,
        }
    }

    fn now_ms(&self) -> u64 {
        self.reference_instant.elapsed().as_millis() as u64
    }

    pub fn touch(&self) {
        self.last_access_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    /// Register a persistent consumer of this entry until the lease drops.
    pub fn lease_persistent(&self) -> PersistentLease {
        self.persistent.fetch_add(1, Ordering::Relaxed);
        PersistentLease {
            count: Arc::clone(&self.persistent),
        }
    }

    pub fn persistent_count(&self) -> u64 {
        self.persistent.load(Ordering::Relaxed)
    }

    pub fn is_idle(&self, timeout_ms: u64) -> bool {
        let persistent = self.persistent.load(Ordering::Relaxed);
        if persistent > 0 {
            return false;
        }
        let last = self.last_access_ms.load(Ordering::Relaxed);
        let now = self.now_ms();
        now.saturating_sub(last) >= timeout_ms
    }
}

/// One persistent HLS consumer (an HLS egress output). While it lives, the
/// segmenter entry it came from is never idle; dropping it releases exactly
/// that registration, on that entry, once. It is not `Clone`, so there is no
/// unmatched release, and a lease from a segmenter that has since been
/// replaced cannot release the replacement's consumers.
#[must_use = "dropping the lease releases the consumer at once"]
pub struct PersistentLease {
    count: Arc<AtomicU64>,
}

impl Drop for PersistentLease {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
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

/// Lock order `children` -> `active`: the running check holds `children`
/// through its `active` update, so a stop or reap (which must take
/// `children` to remove the child) clears `active` only after it. Nothing
/// holds `active` while taking `children`; the dependency snapshot reads one
/// map at a time (the reverse nesting deadlocked against the running check).
pub struct FileIngestRegistry {
    pub children: TokioRwLock<HashMap<String, tokio::process::Child>>,
    pub active: TokioRwLock<HashSet<String>>,
    /// The item each running ingest is playing (0 for a single file).
    pub positions: TokioRwLock<HashMap<String, usize>>,
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
            positions: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct RuntimeInfra {
    pub listener_stats: Arc<ListenerSocketStats>,
    pub rtmp_listener_stats: Arc<RtmpListenerStats>,
    /// Restarts of the MoQ ingest listener after it ended without a
    /// shutdown request.
    pub moq_listener_restarts: std::sync::atomic::AtomicU64,
    pub os_threads: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub listener_shutdowns: std::sync::Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    pub sender_semaphore: Arc<tokio::sync::Semaphore>,
    pub external_ffmpeg_semaphore: Arc<tokio::sync::Semaphore>,
    pub diag_semaphores: TokioRwLock<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    pub event_log: Arc<EventLog>,
}

impl Default for RuntimeInfra {
    fn default() -> Self {
        Self::new(&crate::config::EngineConfig::default())
    }
}

impl RuntimeInfra {
    pub fn new(config: &crate::config::EngineConfig) -> Self {
        let external_ffmpeg_permits = config.external_ffmpeg_permits;
        Self {
            listener_stats: Arc::new(ListenerSocketStats::default()),
            rtmp_listener_stats: Arc::new(RtmpListenerStats::default()),
            moq_listener_restarts: std::sync::atomic::AtomicU64::new(0),
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

#[cfg(test)]
mod tests {
    use super::HlsConsumers;
    use std::sync::atomic::Ordering;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn is_idle_treats_never_touched_consumer_as_idle_at_zero_timeout() {
        // last_access_ms starts at 0; a fresh consumer with no persistent
        // outputs and a zero grace timeout must be considered idle
        // immediately, with no implicit startup grace period.
        let hc = HlsConsumers::new(CancellationToken::new());
        assert!(hc.is_idle(0));
    }

    #[test]
    fn a_held_lease_vetoes_idle_and_its_drop_releases_it() {
        // A persistent (egress) consumer must veto idle shutdown outright,
        // even though the heartbeat was never touched and the timeout is 0.
        let hc = HlsConsumers::new(CancellationToken::new());
        let first = hc.lease_persistent();
        let second = hc.lease_persistent();
        drop(first);
        assert!(!hc.is_idle(0), "one lease is still held");
        drop(second);
        assert_eq!(hc.persistent_count(), 0);
        assert!(hc.is_idle(0));
    }

    // `is_idle` compares `now_ms()` against `last_access_ms` with
    // `saturating_sub`, not plain subtraction. If `last_access_ms` is ever
    // ahead of `now_ms()` (state corruption, or clock skew across a
    // hypothetical restart of `reference_instant`), plain subtraction on
    // `u64` would panic in debug builds and wrap to a huge value in release,
    // both of which would misreport a live consumer as idle or crash the
    // engine. The guard must instead treat "reads as behind" as "not idle".
    #[test]
    fn is_idle_does_not_underflow_when_last_access_is_ahead_of_now() {
        let hc = HlsConsumers::new(CancellationToken::new());
        hc.last_access_ms.store(u64::MAX, Ordering::Relaxed);
        assert!(!hc.is_idle(1000));
    }
}
