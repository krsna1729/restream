//! Centralized application configuration.
//! Reads environment variables once at startup and stores them in a typed struct.

use crate::capacity::{CapacityLimits, LimitSource, LimitSources};
use crate::media::config::EngineConfig;
use crate::media::config::env::{env_bool, env_present, env_u32, env_u64, env_usize, env_var};
use crate::media::host_capacity::HostCapacity;

/// Default location for media-library files relative to the process working directory.
///
/// Keep this as the single source of truth: tests and harness fallbacks use the same
/// value so they cannot silently create a repository-root `media/` directory.
pub const DEFAULT_MEDIA_DIR: &str = ".restream/media";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPorts {
    pub http: u16,
    pub rtmp: u16,
    pub srt: u16,
    /// The MoQ ingest listener's UDP port, when `EngineConfig::moq_ingest` is on.
    pub moq: Option<u16>,
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
pub struct TokioRuntimeConfig {
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
}

impl Default for TokioRuntimeConfig {
    fn default() -> Self {
        Self {
            worker_threads: HostCapacity::get().tokio_workers(),
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

impl RuntimeTuning {
    pub(crate) fn session_prune_every_ticks(&self) -> u64 {
        let ticks = 3_600_000u64.div_ceil(self.reconciler_interval_ms);
        ticks.max(1)
    }

    pub(crate) fn output_backoff_ms(&self, retries: u32, jitter_key: &str) -> u64 {
        self.output_retry_policy().backoff_ms(retries, jitter_key)
    }

    pub(crate) fn output_retry_policy(&self) -> crate::application::reconcile::OutputRetryPolicy {
        crate::application::reconcile::OutputRetryPolicy {
            max_retries: self.output_max_retries,
            base_ms: self.output_retry_base_ms,
            max_ms: self.output_retry_max_ms,
        }
    }
}

/// The VAPID contact when `RESTREAM_PUSH_SUBJECT` is unset or not a
/// `mailto:`/`https:` URL.
pub const DEFAULT_PUSH_SUBJECT: &str = "mailto:restream@localhost";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ports: ServerPorts,
    pub http_bind_addr: String,
    pub tuning: RuntimeTuning,
    /// Host-specific service-center capacities. These remain observe-only;
    /// admission is not enforced until calibration is validated externally.
    pub capacity_limits: CapacityLimits,
    /// Which `capacity_limits` the operator set, the rest being priors.
    pub capacity_sources: LimitSources,
    /// The CPU count the priors scale with.
    pub capacity_prior_cpus: u32,
    pub tokio_runtime: TokioRuntimeConfig,
    pub db_path: String,
    pub media_dir: String,
    pub log_retention_days: u64,
    pub ffmpeg_threads: Option<u32>,
    pub ffmpeg_bin_path: Option<String>,
    pub log_dir: String,
    pub no_color: bool,
    pub srt_passphrase: Option<String>,
    pub srt_pbkeylen: i32,
    pub initial_admin_password: Option<String>,
    pub secure_session_cookies: bool,
    /// The dashboard's public URL (`RESTREAM_PUBLIC_URL`): notifications
    /// link back to it. `None` sends no links.
    pub public_url: Option<String>,
    /// How long the flight recorder's persisted history reaches back
    /// (`RESTREAM_FLIGHT_HISTORY_HOURS`, default 24, at most a week); 0
    /// keeps only the last 10 minutes, in memory.
    pub flight_history_hours: u64,
    /// The contact in Web Push VAPID tokens (`RESTREAM_PUSH_SUBJECT`): a
    /// `mailto:` or `https:` URL push services may use to reach the operator.
    pub push_subject: String,
    /// The media engine's settings.
    pub engine: EngineConfig,
}

/// A positive limit from `name`, and whether the operator set it; else the
/// prior. An unparseable or non-positive value is the prior, as if unset.
fn capacity_limit(name: &str, prior: f64) -> (f64, LimitSource) {
    match env_var(name)
        .and_then(|value| value.parse().ok())
        .filter(|value: &f64| value.is_finite() && value.is_sign_positive())
    {
        Some(value) => (value, LimitSource::Configured),
        None => (prior, LimitSource::Prior),
    }
}

fn capacity_limits_from_env(parallelism: usize) -> (CapacityLimits, LimitSources) {
    let priors = CapacityLimits::from_parallelism(parallelism.min(u32::MAX as usize) as u32);
    let (ingress_pps, ingress_source) =
        capacity_limit("RESTREAM_CAPACITY_INGRESS_PPS", priors.ingress_pps);
    let (egress_pps, egress_source) =
        capacity_limit("RESTREAM_CAPACITY_EGRESS_PPS", priors.egress_pps);
    let (nic_bps, nic_source) = capacity_limit("RESTREAM_CAPACITY_NIC_BPS", priors.nic_bps);
    let (memory_bytes, memory_source) =
        capacity_limit("RESTREAM_CAPACITY_MEMORY_BYTES", priors.memory_bytes);
    let (ffmpeg_stages, ffmpeg_source) =
        capacity_limit("RESTREAM_CAPACITY_FFMPEG_STAGES", priors.ffmpeg_stages);
    let (disk_bps, disk_source) = capacity_limit("RESTREAM_CAPACITY_DISK_BPS", priors.disk_bps);
    (
        CapacityLimits {
            ingress_pps,
            egress_pps,
            nic_bps,
            memory_bytes,
            ffmpeg_stages,
            disk_bps,
        },
        LimitSources {
            ingress_pps: ingress_source,
            egress_pps: egress_source,
            nic_bps: nic_source,
            memory_bytes: memory_source,
            ffmpeg_stages: ffmpeg_source,
            disk_bps: disk_source,
        },
    )
}

/// The malloc arena environment, read once at startup before any thread
/// exists: `(MALLOC_ARENA_MAX, RESTREAM_MALLOC_ARENA_MAX)`. Interpreted by
/// `crate::malloc_tuning`.
pub fn malloc_arena_env() -> (Option<String>, Option<String>) {
    (
        env_var("MALLOC_ARENA_MAX"),
        env_var("RESTREAM_MALLOC_ARENA_MAX"),
    )
}

/// Port `0` means "let the OS pick" for a bind() call, not a meaningful configured port;
/// accepting it here would silently bind an unpredictable ephemeral port while every API
/// response keeps advertising `:0` as the connect address. Reject it like a parse failure.
fn env_port(name: &str, default: u16) -> u16 {
    match env_var(name).and_then(|v| v.parse::<u16>().ok()) {
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
            moq: None,
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

impl Default for AppConfig {
    fn default() -> Self {
        let ports = ServerPorts {
            http: 3030,
            rtmp: 1935,
            srt: 10080,
            moq: None,
        };
        let tuning = RuntimeTuning::default();
        let tokio_runtime = TokioRuntimeConfig::default();
        let cpus = HostCapacity::get().cpus;

        Self {
            ports,
            http_bind_addr: "127.0.0.1".to_string(),
            tuning,
            capacity_limits: CapacityLimits::from_parallelism(cpus as u32),
            capacity_sources: LimitSources::default(),
            capacity_prior_cpus: cpus as u32,
            tokio_runtime,
            db_path: ".restream/data/restream.db".to_string(),
            media_dir: DEFAULT_MEDIA_DIR.to_string(),
            log_retention_days: 7,
            ffmpeg_threads: None,
            ffmpeg_bin_path: None,
            log_dir: ".restream/logs".to_string(),
            no_color: false,
            srt_passphrase: None,
            srt_pbkeylen: 16,
            initial_admin_password: None,
            secure_session_cookies: false,
            public_url: None,
            flight_history_hours: 24,
            push_subject: DEFAULT_PUSH_SUBJECT.to_string(),
            engine: EngineConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        let engine = EngineConfig::from_env();
        let ports = ServerPorts {
            moq: engine.moq_ingest.port(),
            ..ServerPorts::from_env()
        };
        let http_bind_addr =
            env_var("RESTREAM_HTTP_BIND_ADDR").unwrap_or_else(|| "127.0.0.1".to_string());
        let tuning = RuntimeTuning::from_env();
        let cpus = HostCapacity::get().cpus;
        let (capacity_limits, capacity_sources) = capacity_limits_from_env(cpus);
        let tokio_runtime = TokioRuntimeConfig::from_env();
        let db_path =
            env_var("RESTREAM_DB_PATH").unwrap_or_else(|| ".restream/data/restream.db".to_string());
        let media_dir =
            env_var("RESTREAM_MEDIA_DIR").unwrap_or_else(|| DEFAULT_MEDIA_DIR.to_string());
        let log_retention_days = env_u64("RESTREAM_LOG_RETENTION_DAYS", 7);
        let ffmpeg_threads =
            env_var("RESTREAM_EXTERNAL_FFMPEG_THREADS").and_then(|v| v.parse::<u32>().ok());
        let ffmpeg_bin_path = env_var("FFMPEG_BIN_PATH");
        let log_dir = env_var("RESTREAM_LOG_DIR").unwrap_or_else(|| ".restream/logs".to_string());
        let no_color = env_present("NO_COLOR");
        let srt_passphrase = env_var("RESTREAM_SRT_PASSPHRASE");
        let srt_pbkeylen = env_var("RESTREAM_SRT_PBKEYLEN")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(16);
        let initial_admin_password = env_var("RESTREAM_INITIAL_ADMIN_PASSWORD");
        let secure_session_cookies = env_bool("RESTREAM_SECURE_SESSION_COOKIES").unwrap_or(false);
        let public_url = env_var("RESTREAM_PUBLIC_URL").filter(|value| !value.is_empty());
        let flight_history_hours = env_u64("RESTREAM_FLIGHT_HISTORY_HOURS", 24).min(24 * 7);
        let push_subject = env_var("RESTREAM_PUSH_SUBJECT")
            .filter(|value| value.starts_with("mailto:") || value.starts_with("https://"))
            .unwrap_or_else(|| DEFAULT_PUSH_SUBJECT.to_string());

        Self {
            ports,
            http_bind_addr,
            tuning,
            capacity_limits,
            capacity_sources,
            capacity_prior_cpus: cpus.min(u32::MAX as usize) as u32,
            tokio_runtime,
            db_path,
            media_dir,
            log_retention_days,
            ffmpeg_threads,
            ffmpeg_bin_path,
            log_dir,
            no_color,
            srt_passphrase,
            srt_pbkeylen,
            initial_admin_password,
            secure_session_cookies,
            public_url,
            flight_history_hours,
            push_subject,
            engine,
        }
    }

    pub fn effective_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "ports": {
                "http": self.ports.http,
                "rtmp": self.ports.rtmp,
                "srt": self.ports.srt,
                "moq": self.ports.moq,
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
                "shards": self.engine.egress_fabric.shards,
                "commandChannelCapacity": self.engine.egress_fabric.command_channel_capacity,
                "commandBatchBudget": self.engine.egress_fabric.command_batch_budget,
                "readinessBatchBudget": self.engine.egress_fabric.readiness_batch_budget,
                "timerBatchBudget": self.engine.egress_fabric.timer_batch_budget,
                "maxLeavesPerShard": self.engine.egress_fabric.max_leaves_per_shard,
                "idleWaitMs": self.engine.egress_fabric.idle_wait_ms,
                "tcpPollerMaxEvents": self.engine.egress_fabric.tcp_poller_max_events,
                "visitMaxUnits": self.engine.egress_fabric.visit_max_units,
                "visitMaxBytes": self.engine.egress_fabric.visit_max_bytes,
                "visitMaxUs": self.engine.egress_fabric.visit_max_us,
                "maxPendingBytes": self.engine.egress_fabric.max_pending_bytes,
                "drainTimeoutMs": self.engine.egress_fabric.drain_timeout_ms,
                "hlsPutFabric": self.engine.egress_fabric.hls_put_fabric,
            },
            "capacity": {
                "ingressPps": self.capacity_limits.ingress_pps,
                "egressPps": self.capacity_limits.egress_pps,
                "nicBps": self.capacity_limits.nic_bps,
                "memoryBytes": self.capacity_limits.memory_bytes,
                "ffmpegStages": self.capacity_limits.ffmpeg_stages,
                "diskBps": self.capacity_limits.disk_bps,
            },
            "paths": {
                "db": self.db_path,
                "media": self.media_dir,
                "logs": self.log_dir,
                "ffmpegBin": self.ffmpeg_bin_path,
                "rtmpsExtraTrustRootsPem": self.engine.rtmps_extra_trust_roots_pem_path,
            },
            "logging": {
                "retentionDays": self.log_retention_days,
                "noColor": self.no_color,
            },
            "ffmpeg": {
                "externalPermits": self.engine.external_ffmpeg_permits,
                "threads": self.ffmpeg_threads,
                "recordingThreads": self.engine.recording_threads,
            },
            "buffers": {
                "recordingQueueCapacity": self.engine.recording_queue_capacity,
                "hlsMinSegmentMs": self.engine.hls_min_segment_ms,
                "hlsSegmentCapacityBytes": self.engine.hls_segment_capacity_bytes,
                "hlsMaxSegments": self.engine.hls_max_segments,
                "tsRingCapacity": self.engine.ts_ring_capacity,
                "ringHeadroomSecs": self.engine.ring_headroom_secs,
                "ringCapacity": self.engine.ring_capacity,
                "transcoderRingCapacity": self.engine.transcoder_ring_capacity,
            },
            "srt": {
                "passphraseConfigured": self.srt_passphrase.is_some(),
                "pbkeylen": self.srt_pbkeylen,
                "connectTimeoutMs": self.engine.srt_connect_timeout_ms,
                "egressMuxerMaxOutputsPerShard": self.engine.srt_egress_muxer_max_outputs_per_shard,
                "egressMuxerMaxShards": self.engine.srt_egress_muxer_max_shards,
            },
            "security": {
                "secureSessionCookies": self.secure_session_cookies,
            },
            "rtmp": {
                "backlog": self.engine.rtmp_backlog,
                "maxConnections": self.engine.rtmp_max_connections,
                "ingressOwners": self.engine.rtmp_ingress_owners,
                "maxMessageBytes": self.engine.rtmp_max_message_bytes,
                "ingestParserBudgetBytes": self.engine.rtmp_ingest_parser_budget_bytes,
                "handshakeTimeoutMs": self.engine.rtmp_handshake_timeout_ms,
                "maxConnectionsPerIp": self.engine.rtmp_max_connections_per_ip,
                "preauthTimeoutMs": self.engine.rtmp_preauth_timeout_ms,
                "preauthBufferBytes": self.engine.rtmp_preauth_buffer_bytes,
                "streamBufferBytes": self.engine.rtmp_stream_buffer_bytes,
                "egressChunkSize": self.engine.rtmp_egress_chunk_size,
            },
            "moq": self.engine.moq_ingest.summary(),
        })
    }
}

#[cfg(test)]
#[path = "config/tests/configuration_behavior.rs"]
mod configuration_behavior_tests;
