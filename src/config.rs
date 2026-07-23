//! Centralized application configuration.
//! Reads environment variables once at startup and stores them in a typed struct.

use std::num::NonZeroU32;
use std::time::Duration;

use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::EgressShardConfig;
use crate::planner::BackendPolicy;

/// Default location for media-library files relative to the process working directory.
///
/// Keep this as the single source of truth: tests and harness fallbacks use the same
/// value so they cannot silently recreate the legacy repository-root `media/` directory.
pub const DEFAULT_MEDIA_DIR: &str = ".restream/media";
const EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPorts {
    pub http: u16,
    pub rtmp: u16,
    pub srt: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeTuning {
    pub nofile_limit: u64,
    pub reconciler_interval_ms: u64,
    pub ingest_disconnect_grace_ms: u64,
    pub output_max_retries: u32,
    pub output_retry_base_ms: u64,
    pub output_retry_max_ms: u64,
    pub hls_idle_timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressFabricConfig {
    pub rollout: EgressRolloutMode,
    pub shards: u32,
    pub command_channel_capacity: usize,
    pub command_batch_budget: usize,
    pub readiness_batch_budget: usize,
    pub timer_batch_budget: usize,
    pub idle_wait_ms: u64,
    pub srt_poller_max_events: usize,
    pub visit_max_units: usize,
    pub visit_max_bytes: usize,
    pub visit_max_us: u64,
    pub max_pending_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokioRuntimeConfig {
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
}

impl Default for TokioRuntimeConfig {
    fn default() -> Self {
        let effective_cpus = crate::system_sampling::effective_cpu_count();
        Self {
            worker_threads: default_tokio_worker_threads(effective_cpus),
            max_blocking_threads: 512,
        }
    }
}

impl Default for RuntimeTuning {
    fn default() -> Self {
        Self {
            nofile_limit: 65_536,
            reconciler_interval_ms: 1_000,
            ingest_disconnect_grace_ms: 5_000,
            output_max_retries: 10,
            output_retry_base_ms: 5_000,
            output_retry_max_ms: 300_000,
            hls_idle_timeout_ms: 60_000,
        }
    }
}

/// Protocol-selective egress fabric rollout mode.
///
/// Exactly one runtime owns any given output: modes route whole protocols to
/// the fabric while every other protocol stays on the legacy path.
/// `ShadowMetrics` may instantiate model and assignment calculations but must
/// never establish duplicate network connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressRolloutMode {
    Off,
    Srt,
    Rtmp,
    All,
    ShadowMetrics,
}

impl EgressRolloutMode {
    fn parse(value: &str) -> Option<Self> {
        // Legacy boolean spellings map to the historical behavior of the
        // RESTREAM_EGRESS_FABRIC flag: enabling it routed SRT only.
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" => Some(Self::Off),
            "srt" | "1" | "true" | "yes" => Some(Self::Srt),
            "rtmp" => Some(Self::Rtmp),
            "all" => Some(Self::All),
            "shadow-metrics" | "shadow_metrics" => Some(Self::ShadowMetrics),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Srt => "srt",
            Self::Rtmp => "rtmp",
            Self::All => "all",
            Self::ShadowMetrics => "shadow-metrics",
        }
    }

    /// SRT outputs route to the fabric runtime.
    pub fn routes_srt(self) -> bool {
        matches!(self, Self::Srt | Self::All)
    }

    /// RTMP and RTMPS outputs route to the fabric runtime.
    pub fn routes_rtmp(self) -> bool {
        matches!(self, Self::Rtmp | Self::All)
    }

    /// Any fabric machinery (including shadow calculations) is active.
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Off)
    }
}

impl Default for EgressFabricConfig {
    fn default() -> Self {
        Self {
            rollout: EgressRolloutMode::Off,
            shards: 4,
            command_channel_capacity: 1024,
            command_batch_budget: 32,
            readiness_batch_budget: 64,
            timer_batch_budget: 64,
            idle_wait_ms: 1,
            srt_poller_max_events: 1024,
            visit_max_units: 32,
            visit_max_bytes: 256 * 1024,
            visit_max_us: 2_000,
            max_pending_bytes: 256 * 1024,
        }
    }
}

impl RuntimeTuning {
    pub(crate) fn session_prune_every_ticks(&self) -> u64 {
        let ticks = 3_600_000u64.div_ceil(self.reconciler_interval_ms);
        ticks.max(1)
    }

    pub(crate) fn output_backoff_ms(&self, retries: u32) -> u64 {
        self.output_retry_policy().backoff_ms(retries)
    }

    pub(crate) fn output_retry_policy(&self) -> crate::application::reconcile::OutputRetryPolicy {
        crate::application::reconcile::OutputRetryPolicy {
            max_retries: self.output_max_retries,
            base_ms: self.output_retry_base_ms,
            max_ms: self.output_retry_max_ms,
        }
    }
}

impl EgressFabricConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            rollout: std::env::var("RESTREAM_EGRESS_FABRIC")
                .ok()
                .and_then(|value| EgressRolloutMode::parse(&value))
                .unwrap_or(defaults.rollout),
            shards: env_u32("RESTREAM_EGRESS_SHARDS", defaults.shards).clamp(1, 1024),
            command_channel_capacity: env_usize(
                "RESTREAM_EGRESS_COMMAND_CAPACITY",
                defaults.command_channel_capacity,
            )
            .clamp(1, 65_536),
            command_batch_budget: env_usize(
                "RESTREAM_EGRESS_COMMAND_BATCH",
                defaults.command_batch_budget,
            )
            .clamp(1, 4096),
            readiness_batch_budget: env_usize(
                "RESTREAM_EGRESS_READY_BATCH",
                defaults.readiness_batch_budget,
            )
            .clamp(1, 4096),
            timer_batch_budget: env_usize(
                "RESTREAM_EGRESS_TIMER_BATCH",
                defaults.timer_batch_budget,
            )
            .clamp(1, 4096),
            idle_wait_ms: env_u64("RESTREAM_EGRESS_IDLE_WAIT_MS", defaults.idle_wait_ms)
                .clamp(1, 1_000),
            srt_poller_max_events: env_usize(
                "RESTREAM_EGRESS_SRT_POLLER_MAX_EVENTS",
                defaults.srt_poller_max_events,
            )
            .clamp(1, 65_536),
            visit_max_units: env_usize("RESTREAM_EGRESS_VISIT_MAX_UNITS", defaults.visit_max_units)
                .clamp(1, 4096),
            visit_max_bytes: env_usize("RESTREAM_EGRESS_VISIT_MAX_BYTES", defaults.visit_max_bytes)
                .clamp(188, 16 * 1024 * 1024),
            visit_max_us: env_u64("RESTREAM_EGRESS_VISIT_MAX_US", defaults.visit_max_us)
                .clamp(1, 1_000_000),
            max_pending_bytes: env_usize(
                "RESTREAM_EGRESS_MAX_PENDING_BYTES",
                defaults.max_pending_bytes,
            )
            .clamp(1, 16 * 1024 * 1024),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn shard_count(&self) -> NonZeroU32 {
        NonZeroU32::new(self.shards).expect("egress fabric shard count is clamped nonzero")
    }

    #[allow(dead_code)]
    pub(crate) fn shard_config(&self) -> EgressShardConfig {
        EgressShardConfig::new(
            self.command_channel_capacity,
            self.command_batch_budget,
            self.readiness_batch_budget,
            self.timer_batch_budget,
            Duration::from_millis(self.idle_wait_ms),
        )
        .expect("egress fabric shard config is clamped nonzero")
    }

    #[allow(dead_code)]
    pub(crate) fn work_budget(&self) -> WorkBudget {
        WorkBudget::new(
            self.visit_max_units,
            self.visit_max_bytes,
            Duration::from_micros(self.visit_max_us),
        )
    }
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ports: ServerPorts,
    pub http_bind_addr: String,
    pub tuning: RuntimeTuning,
    pub egress_fabric: EgressFabricConfig,
    pub tokio_runtime: TokioRuntimeConfig,
    pub db_path: String,
    pub media_dir: String,
    pub log_retention_days: u64,
    pub backend_policy: BackendPolicy,
    pub rtmp_backlog: u32,
    pub rtmp_max_connections: usize,
    pub rtmp_handshake_timeout_ms: u64,
    pub rtmp_preauth_buffer_bytes: usize,
    pub rtmp_stream_buffer_bytes: usize,
    pub rtmp_egress_chunk_size: u32,
    pub ffmpeg_threads: Option<u32>,
    pub avio_capacity: usize,
    pub hls_min_segment_ms: f64,
    pub hls_segment_capacity_bytes: usize,
    pub hls_max_segments: usize,
    pub recording_threads: Option<u32>,
    pub ts_ring_capacity: usize,
    pub ring_capacity: usize,
    pub transcoder_ring_capacity: usize,
    pub require_srt_bonding: bool,
    pub external_ffmpeg_permits: usize,
    pub ffmpeg_bin_path: Option<String>,
    pub log_dir: String,
    pub no_color: bool,
    pub srt_passphrase: Option<String>,
    pub srt_pbkeylen: i32,
    pub srt_connect_timeout_ms: u64,
    pub srt_egress_reuse_local_port: bool,
    pub srt_egress_muxer_max_outputs_per_shard: usize,
    pub srt_egress_muxer_max_shards: usize,
    pub use_internal_file_ingest: bool,
    pub initial_admin_password: Option<String>,
    pub secure_session_cookies: bool,
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn default_tokio_worker_threads(effective_cpus: usize) -> usize {
    let effective_cpus = effective_cpus.max(1);
    if effective_cpus <= 2 {
        effective_cpus
    } else {
        effective_cpus.div_ceil(3).clamp(2, 8)
    }
}

fn env_bool(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_bool_default_true(name: &str) -> bool {
    !matches!(
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

fn derive_external_ffmpeg_permits(
    cpus: usize,
    reserve: usize,
    per_child: usize,
    hard_cap: usize,
) -> usize {
    let cpu_budgeted = cpus
        .saturating_sub(reserve)
        .max(1)
        .div_ceil(per_child.max(1))
        .max(1);
    cpu_budgeted
        .max(EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR)
        .min(hard_cap)
        .max(1)
}

/// Port `0` means "let the OS pick" for a bind() call, not a meaningful configured port;
/// accepting it here would silently bind an unpredictable ephemeral port while every API
/// response keeps advertising `:0` as the connect address. Reject it like a parse failure.
fn env_port(name: &str, default: u16) -> u16 {
    match std::env::var(name).ok().and_then(|v| v.parse::<u16>().ok()) {
        Some(0) => {
            tracing::warn!(env = name, "port 0 is not valid; using default {default}");
            default
        }
        Some(port) => port,
        None => default,
    }
}

impl ServerPorts {
    pub fn from_env() -> Self {
        Self {
            http: env_port("RESTREAM_HTTP_PORT", 3030),
            rtmp: env_port("RESTREAM_RTMP_PORT", 1935),
            srt: env_port("RESTREAM_SRT_PORT", 10080),
        }
    }
}

impl RuntimeTuning {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            nofile_limit: env_u64("RESTREAM_NOFILE_LIMIT", defaults.nofile_limit).max(1),
            reconciler_interval_ms: env_u64(
                "RESTREAM_RECONCILE_INTERVAL_MS",
                defaults.reconciler_interval_ms,
            )
            .max(100),
            ingest_disconnect_grace_ms: env_u64(
                "RESTREAM_INGEST_DISCONNECT_GRACE_MS",
                defaults.ingest_disconnect_grace_ms,
            ),
            output_max_retries: env_u32("RESTREAM_OUTPUT_MAX_RETRIES", defaults.output_max_retries),
            output_retry_base_ms: env_u64(
                "RESTREAM_OUTPUT_RETRY_BASE_MS",
                defaults.output_retry_base_ms,
            )
            .max(1),
            output_retry_max_ms: env_u64(
                "RESTREAM_OUTPUT_RETRY_MAX_MS",
                defaults.output_retry_max_ms,
            )
            .max(1),
            hls_idle_timeout_ms: env_u64(
                "RESTREAM_HLS_IDLE_TIMEOUT_MS",
                defaults.hls_idle_timeout_ms,
            )
            .max(1),
        }
    }
}

impl TokioRuntimeConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            worker_threads: env_usize("RESTREAM_TOKIO_WORKER_THREADS", defaults.worker_threads)
                .max(1),
            max_blocking_threads: env_usize(
                "RESTREAM_TOKIO_MAX_BLOCKING_THREADS",
                defaults.max_blocking_threads,
            )
            .max(1),
        }
    }
}

pub fn backend_policy_from_env() -> BackendPolicy {
    BackendPolicy {
        internal_video_presets: env_bool("RESTREAM_INTERNAL_VIDEO_PRESETS").unwrap_or(false),
        internal_hevc_to_h264: env_bool("RESTREAM_INTERNAL_HEVC_TO_H264").unwrap_or(false),
        internal_hls_preview: env_bool("RESTREAM_INTERNAL_HLS_PREVIEW").unwrap_or(false),
        internal_complex_audio: env_bool("RESTREAM_INTERNAL_AUDIO_COMPLEX").unwrap_or(false),
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let ports = ServerPorts {
            http: 3030,
            rtmp: 1935,
            srt: 10080,
        };
        let tuning = RuntimeTuning::default();
        let tokio_runtime = TokioRuntimeConfig::default();
        let cpus = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        let derived_permits =
            derive_external_ffmpeg_permits(cpus, 2.min(cpus.saturating_sub(1)), 2, usize::MAX);

        Self {
            ports,
            http_bind_addr: "127.0.0.1".to_string(),
            tuning,
            egress_fabric: EgressFabricConfig::default(),
            tokio_runtime,
            db_path: ".restream/data/restream.db".to_string(),
            media_dir: DEFAULT_MEDIA_DIR.to_string(),
            log_retention_days: 7,
            backend_policy: BackendPolicy {
                internal_video_presets: false,
                internal_hevc_to_h264: false,
                internal_hls_preview: false,
                internal_complex_audio: false,
            },
            rtmp_backlog: 1024,
            rtmp_max_connections: 512,
            rtmp_handshake_timeout_ms: 10_000,
            rtmp_preauth_buffer_bytes: 128 * 1024,
            rtmp_stream_buffer_bytes: 8 * 1024 * 1024,
            rtmp_egress_chunk_size: 16 * 1024,
            ffmpeg_threads: None,
            avio_capacity: 512 * 1024,
            hls_min_segment_ms: 1.0,
            hls_segment_capacity_bytes: 8 * 1024 * 1024,
            hls_max_segments: 20,
            recording_threads: None,
            ts_ring_capacity: 256,
            ring_capacity: 1024,
            transcoder_ring_capacity: 512,
            require_srt_bonding: false,
            external_ffmpeg_permits: derived_permits,
            ffmpeg_bin_path: None,
            log_dir: ".restream/logs".to_string(),
            no_color: false,
            srt_passphrase: None,
            srt_pbkeylen: 16,
            srt_connect_timeout_ms: 3_000,
            srt_egress_reuse_local_port: true,
            srt_egress_muxer_max_outputs_per_shard: 0,
            srt_egress_muxer_max_shards: 64,
            use_internal_file_ingest: false,
            initial_admin_password: None,
            secure_session_cookies: false,
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        let ports = ServerPorts::from_env();
        let http_bind_addr =
            std::env::var("RESTREAM_HTTP_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
        let tuning = RuntimeTuning::from_env();
        let egress_fabric = EgressFabricConfig::from_env();
        let tokio_runtime = TokioRuntimeConfig::from_env();
        let db_path = std::env::var("RESTREAM_DB_PATH")
            .unwrap_or_else(|_| ".restream/data/restream.db".to_string());
        let media_dir =
            std::env::var("RESTREAM_MEDIA_DIR").unwrap_or_else(|_| DEFAULT_MEDIA_DIR.to_string());
        let log_retention_days = env_u64("RESTREAM_LOG_RETENTION_DAYS", 7);
        let backend_policy = backend_policy_from_env();
        let rtmp_backlog = env_u32("RESTREAM_RTMP_LISTENER_BACKLOG", 1024);
        let rtmp_max_connections = env_usize("RESTREAM_RTMP_MAX_CONNECTIONS", 512).clamp(1, 16384);
        let rtmp_handshake_timeout_ms =
            env_u64("RESTREAM_RTMP_HANDSHAKE_TIMEOUT_MS", 10_000).clamp(100, 300_000);
        let rtmp_preauth_buffer_bytes = env_usize("RESTREAM_RTMP_PREAUTH_BUFFER_BYTES", 128 * 1024)
            .clamp(16 * 1024, 1024 * 1024);
        let rtmp_stream_buffer_bytes =
            env_usize("RESTREAM_RTMP_STREAM_BUFFER_BYTES", 8 * 1024 * 1024)
                .clamp(128 * 1024, 64 * 1024 * 1024);
        let rtmp_egress_chunk_size =
            env_u32("RESTREAM_RTMP_EGRESS_CHUNK_SIZE", 16 * 1024).clamp(128, 1024 * 1024);
        let ffmpeg_threads = std::env::var("RESTREAM_EXTERNAL_FFMPEG_THREADS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let avio_capacity = env_usize("RESTREAM_AVIO_QUEUE_CAPACITY", 512 * 1024)
            .clamp(64 * 1024, 16 * 1024 * 1024);

        let hls_min_segment_ms = env_u64("RESTREAM_HLS_MIN_SEGMENT_MS", 1000) as f64 / 1000.0;
        let hls_segment_capacity_bytes =
            env_usize("RESTREAM_HLS_SEGMENT_CAPACITY_BYTES", 8 * 1024 * 1024).max(188);
        let hls_max_segments = env_usize("RESTREAM_HLS_MAX_SEGMENTS", 20).max(1);

        let recording_threads = std::env::var("RESTREAM_RECORDING_FFMPEG_THREADS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok());
        let ts_ring_capacity = env_usize("RESTREAM_TS_RING_CAPACITY", 256).clamp(32, 16384);
        let ring_capacity = env_usize("RESTREAM_RING_CAPACITY", 1024).clamp(64, 16384);
        let transcoder_ring_capacity =
            env_usize("RESTREAM_TRANSCODER_RING_CAPACITY", 512).clamp(64, 16384);
        let require_srt_bonding = std::env::var_os("RESTREAM_REQUIRE_SRT_BONDING").is_some();
        let ffmpeg_bin_path = std::env::var("FFMPEG_BIN_PATH").ok();
        let log_dir =
            std::env::var("RESTREAM_LOG_DIR").unwrap_or_else(|_| ".restream/logs".to_string());
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let srt_passphrase = std::env::var("RESTREAM_SRT_PASSPHRASE").ok();
        let srt_pbkeylen = std::env::var("RESTREAM_SRT_PBKEYLEN")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(16);
        let srt_connect_timeout_ms = env_u64("RESTREAM_SRT_CONNECT_TIMEOUT_MS", 3_000);
        let srt_egress_reuse_local_port =
            env_bool_default_true("RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT");
        let srt_egress_muxer_max_outputs_per_shard =
            env_usize("RESTREAM_SRT_EGRESS_MUXER_MAX_OUTPUTS_PER_SHARD", 0).min(10_000);
        let srt_egress_muxer_max_shards =
            env_usize("RESTREAM_SRT_EGRESS_MUXER_MAX_SHARDS", 64).clamp(1, 64);
        let use_internal_file_ingest =
            std::env::var_os("RESTREAM_USE_INTERNAL_FILE_INGEST").is_some();
        let initial_admin_password = std::env::var("RESTREAM_INITIAL_ADMIN_PASSWORD").ok();
        let secure_session_cookies = env_bool("RESTREAM_SECURE_SESSION_COOKIES").unwrap_or(false);

        // Calculate external_ffmpeg_permits:
        let permits = if let Ok(value) = std::env::var("RESTREAM_EXTERNAL_FFMPEG_PERMITS")
            && let Some(v) = value.parse::<usize>().ok().filter(|&v| v >= 1)
        {
            v
        } else {
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
            let hard_cap = std::env::var("RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            derive_external_ffmpeg_permits(cpus, reserve, per_child, hard_cap)
        };

        Self {
            ports,
            http_bind_addr,
            tuning,
            egress_fabric,
            tokio_runtime,
            db_path,
            media_dir,
            log_retention_days,
            backend_policy,
            rtmp_backlog,
            rtmp_max_connections,
            rtmp_handshake_timeout_ms,
            rtmp_preauth_buffer_bytes,
            rtmp_stream_buffer_bytes,
            rtmp_egress_chunk_size,
            ffmpeg_threads,
            avio_capacity,
            hls_min_segment_ms,
            hls_segment_capacity_bytes,
            hls_max_segments,
            recording_threads,
            ts_ring_capacity,
            ring_capacity,
            transcoder_ring_capacity,
            require_srt_bonding,
            external_ffmpeg_permits: permits,
            ffmpeg_bin_path,
            log_dir,
            no_color,
            srt_passphrase,
            srt_pbkeylen,
            srt_connect_timeout_ms,
            srt_egress_reuse_local_port,
            srt_egress_muxer_max_outputs_per_shard,
            srt_egress_muxer_max_shards,
            use_internal_file_ingest,
            initial_admin_password,
            secure_session_cookies,
        }
    }

    pub fn effective_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "ports": {
                "http": self.ports.http,
                "rtmp": self.ports.rtmp,
                "srt": self.ports.srt,
                "httpBindAddr": self.http_bind_addr,
            },
            "tuning": {
                "nofileLimit": self.tuning.nofile_limit,
                "reconcilerIntervalMs": self.tuning.reconciler_interval_ms,
                "ingestDisconnectGraceMs": self.tuning.ingest_disconnect_grace_ms,
                "outputMaxRetries": self.tuning.output_max_retries,
                "outputRetryBaseMs": self.tuning.output_retry_base_ms,
                "outputRetryMaxMs": self.tuning.output_retry_max_ms,
                "hlsIdleTimeoutMs": self.tuning.hls_idle_timeout_ms,
            },
            "tokio": {
                "workerThreads": self.tokio_runtime.worker_threads,
                "maxBlockingThreads": self.tokio_runtime.max_blocking_threads,
            },
            "egressFabric": {
                "enabled": self.egress_fabric.rollout.is_active(),
                "rollout": self.egress_fabric.rollout.as_str(),
                "shards": self.egress_fabric.shards,
                "commandChannelCapacity": self.egress_fabric.command_channel_capacity,
                "commandBatchBudget": self.egress_fabric.command_batch_budget,
                "readinessBatchBudget": self.egress_fabric.readiness_batch_budget,
                "timerBatchBudget": self.egress_fabric.timer_batch_budget,
                "idleWaitMs": self.egress_fabric.idle_wait_ms,
                "srtPollerMaxEvents": self.egress_fabric.srt_poller_max_events,
                "visitMaxUnits": self.egress_fabric.visit_max_units,
                "visitMaxBytes": self.egress_fabric.visit_max_bytes,
                "visitMaxUs": self.egress_fabric.visit_max_us,
                "maxPendingBytes": self.egress_fabric.max_pending_bytes,
            },
            "paths": {
                "db": self.db_path,
                "media": self.media_dir,
                "logs": self.log_dir,
                "ffmpegBin": self.ffmpeg_bin_path,
            },
            "logging": {
                "retentionDays": self.log_retention_days,
                "noColor": self.no_color,
            },
            "backendPolicy": {
                "internalVideoPresets": self.backend_policy.internal_video_presets,
                "internalHevcToH264": self.backend_policy.internal_hevc_to_h264,
                "internalHlsPreview": self.backend_policy.internal_hls_preview,
                "internalComplexAudio": self.backend_policy.internal_complex_audio,
                "useInternalFileIngest": self.use_internal_file_ingest,
            },
            "ffmpeg": {
                "externalPermits": self.external_ffmpeg_permits,
                "threads": self.ffmpeg_threads,
                "recordingThreads": self.recording_threads,
            },
            "buffers": {
                "avioCapacity": self.avio_capacity,
                "hlsMinSegmentMs": self.hls_min_segment_ms,
                "hlsSegmentCapacityBytes": self.hls_segment_capacity_bytes,
                "hlsMaxSegments": self.hls_max_segments,
                "tsRingCapacity": self.ts_ring_capacity,
                "ringCapacity": self.ring_capacity,
                "transcoderRingCapacity": self.transcoder_ring_capacity,
            },
            "srt": {
                "requireBonding": self.require_srt_bonding,
                "passphraseConfigured": self.srt_passphrase.is_some(),
                "pbkeylen": self.srt_pbkeylen,
                "connectTimeoutMs": self.srt_connect_timeout_ms,
                "egressMuxerMaxOutputsPerShard": self.srt_egress_muxer_max_outputs_per_shard,
                "egressMuxerMaxShards": self.srt_egress_muxer_max_shards,
            },
            "security": {
                "secureSessionCookies": self.secure_session_cookies,
            },
            "rtmp": {
                "backlog": self.rtmp_backlog,
                "maxConnections": self.rtmp_max_connections,
                "handshakeTimeoutMs": self.rtmp_handshake_timeout_ms,
                "preauthBufferBytes": self.rtmp_preauth_buffer_bytes,
                "streamBufferBytes": self.rtmp_stream_buffer_bytes,
                "egressChunkSize": self.rtmp_egress_chunk_size,
            },
        })
    }
}

#[cfg(test)]
#[path = "config/tests/configuration_behavior.rs"]
mod configuration_behavior_tests;
