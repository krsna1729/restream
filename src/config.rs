//! Centralized application configuration.
//! Reads environment variables once at startup and stores them in a typed struct.

use crate::planner::backend_policy::BackendPolicy;

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
pub struct TokioRuntimeConfig {
    pub worker_threads: usize,
    pub max_blocking_threads: usize,
}

impl Default for TokioRuntimeConfig {
    fn default() -> Self {
        let effective_cpus = effective_cpu_count();
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

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub ports: ServerPorts,
    pub http_bind_addr: String,
    pub tuning: RuntimeTuning,
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

fn available_parallelism_count() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

fn parse_cpu_list_count(value: &str) -> Option<usize> {
    let mut count = 0usize;
    for item in value.trim().split(',').filter(|item| !item.is_empty()) {
        let (start, end) = match item.split_once('-') {
            Some((start, end)) => (start.trim(), end.trim()),
            None => {
                item.trim().parse::<usize>().ok()?;
                count = count.checked_add(1)?;
                continue;
            }
        };
        let start = start.parse::<usize>().ok()?;
        let end = end.parse::<usize>().ok()?;
        if end < start {
            return None;
        }
        count = count.checked_add(end - start + 1)?;
    }
    (count > 0).then_some(count)
}

fn parse_cpu_allowed_list(status: &str) -> Option<usize> {
    status.lines().find_map(|line| {
        line.strip_prefix("Cpus_allowed_list:")
            .and_then(parse_cpu_list_count)
    })
}

fn process_cpu_mask_count() -> Option<usize> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| parse_cpu_allowed_list(&status))
}

fn parse_cpu_max_quota(value: &str) -> Option<usize> {
    let mut parts = value.split_whitespace();
    let quota = parts.next()?;
    let period = parts.next()?.parse::<usize>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<usize>().ok()?;
    Some(quota.div_ceil(period).max(1))
}

fn cgroup_cpu_quota_count() -> Option<usize> {
    std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
        .ok()
        .and_then(|cpu_max| parse_cpu_max_quota(&cpu_max))
}

fn effective_cpu_count() -> usize {
    let mut cpus = available_parallelism_count().max(1);
    if let Some(mask_cpus) = process_cpu_mask_count() {
        cpus = cpus.min(mask_cpus.max(1));
    }
    if let Some(quota_cpus) = cgroup_cpu_quota_count() {
        cpus = cpus.min(quota_cpus.max(1));
    }
    cpus.max(1)
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

impl ServerPorts {
    pub fn from_env() -> Self {
        Self {
            http: std::env::var("RESTREAM_HTTP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3030),
            rtmp: std::env::var("RESTREAM_RTMP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1935),
            srt: std::env::var("RESTREAM_SRT_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10080),
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

impl BackendPolicy {
    pub fn from_env() -> Self {
        Self {
            internal_video_presets: env_bool("RESTREAM_INTERNAL_VIDEO_PRESETS").unwrap_or(false),
            internal_hevc_to_h264: env_bool("RESTREAM_INTERNAL_HEVC_TO_H264").unwrap_or(false),
            internal_hls_preview: env_bool("RESTREAM_INTERNAL_HLS_PREVIEW").unwrap_or(false),
            internal_complex_audio: env_bool("RESTREAM_INTERNAL_AUDIO_COMPLEX").unwrap_or(false),
        }
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
        let tokio_runtime = TokioRuntimeConfig::from_env();
        let db_path = std::env::var("RESTREAM_DB_PATH")
            .unwrap_or_else(|_| ".restream/data/restream.db".to_string());
        let media_dir =
            std::env::var("RESTREAM_MEDIA_DIR").unwrap_or_else(|_| DEFAULT_MEDIA_DIR.to_string());
        let log_retention_days = env_u64("RESTREAM_LOG_RETENTION_DAYS", 7);
        let backend_policy = BackendPolicy::from_env();
        let rtmp_backlog = env_u32("RESTREAM_RTMP_LISTENER_BACKLOG", 1024);
        let rtmp_max_connections = env_usize("RESTREAM_RTMP_MAX_CONNECTIONS", 512).clamp(1, 16384);
        let rtmp_handshake_timeout_ms =
            env_u64("RESTREAM_RTMP_HANDSHAKE_TIMEOUT_MS", 10_000).clamp(100, 300_000);
        let rtmp_preauth_buffer_bytes = env_usize("RESTREAM_RTMP_PREAUTH_BUFFER_BYTES", 128 * 1024)
            .clamp(16 * 1024, 1024 * 1024);
        let rtmp_stream_buffer_bytes =
            env_usize("RESTREAM_RTMP_STREAM_BUFFER_BYTES", 8 * 1024 * 1024)
                .clamp(128 * 1024, 64 * 1024 * 1024);
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
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env_vars(vars: &[(&str, &str)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = vars
            .iter()
            .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        unsafe {
            for (name, value) in vars {
                std::env::set_var(name, value);
            }
        }
        f();
        unsafe {
            for (name, value) in previous {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    fn with_env_overlay(vars: &[(&str, &str)], removed: &[&str], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous_vars = vars
            .iter()
            .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        let previous_removed = removed
            .iter()
            .map(|name| ((*name).to_string(), std::env::var(name).ok()))
            .collect::<Vec<_>>();
        unsafe {
            for (name, value) in vars {
                std::env::set_var(name, value);
            }
            for name in removed {
                std::env::remove_var(name);
            }
        }
        f();
        unsafe {
            for (name, value) in previous_vars.into_iter().chain(previous_removed) {
                if let Some(value) = value {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }

    #[test]
    fn server_ports_are_loaded_by_config_module() {
        with_env_vars(
            &[
                ("RESTREAM_HTTP_PORT", "4040"),
                ("RESTREAM_RTMP_PORT", "2935"),
                ("RESTREAM_SRT_PORT", "11080"),
            ],
            || {
                let ports = ServerPorts::from_env();
                assert_eq!(ports.http, 4040);
                assert_eq!(ports.rtmp, 2935);
                assert_eq!(ports.srt, 11080);
            },
        );
    }

    #[test]
    fn http_bind_addr_defaults_to_loopback_and_can_be_overridden() {
        with_env_overlay(&[], &["RESTREAM_HTTP_BIND_ADDR"], || {
            assert_eq!(AppConfig::from_env().http_bind_addr, "127.0.0.1");
        });
        with_env_vars(&[("RESTREAM_HTTP_BIND_ADDR", "0.0.0.0")], || {
            assert_eq!(AppConfig::from_env().http_bind_addr, "0.0.0.0");
        });
    }

    #[test]
    fn runtime_layout_is_owned_and_each_path_can_be_overridden() {
        with_env_overlay(
            &[],
            &["RESTREAM_DB_PATH", "RESTREAM_MEDIA_DIR", "RESTREAM_LOG_DIR"],
            || {
                let config = AppConfig::from_env();
                assert_eq!(config.db_path, ".restream/data/restream.db");
                assert_eq!(config.media_dir, DEFAULT_MEDIA_DIR);
                assert_eq!(config.log_dir, ".restream/logs");
            },
        );

        with_env_vars(
            &[
                ("RESTREAM_DB_PATH", "/state/custom.db"),
                ("RESTREAM_MEDIA_DIR", "/assets"),
                ("RESTREAM_LOG_DIR", "/var/log/restream"),
            ],
            || {
                let config = AppConfig::from_env();
                assert_eq!(config.db_path, "/state/custom.db");
                assert_eq!(config.media_dir, "/assets");
                assert_eq!(config.log_dir, "/var/log/restream");
            },
        );
    }

    #[test]
    fn secure_session_cookie_flag_is_opt_in() {
        with_env_overlay(&[], &["RESTREAM_SECURE_SESSION_COOKIES"], || {
            assert!(!AppConfig::from_env().secure_session_cookies);
        });
        with_env_vars(&[("RESTREAM_SECURE_SESSION_COOKIES", "true")], || {
            assert!(AppConfig::from_env().secure_session_cookies);
        });
    }

    #[test]
    fn external_ffmpeg_derivation_keeps_live_dependency_graph_moving() {
        assert_eq!(
            derive_external_ffmpeg_permits(6, 2, 2, usize::MAX),
            EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR
        );
        assert_eq!(
            derive_external_ffmpeg_permits(2, 1, 2, usize::MAX),
            EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR
        );
        assert_eq!(derive_external_ffmpeg_permits(64, 2, 2, usize::MAX), 31);
        assert_eq!(derive_external_ffmpeg_permits(6, 2, 2, 3), 3);
    }

    #[test]
    fn external_ffmpeg_env_override_and_hard_cap_are_preserved() {
        with_env_overlay(
            &[("RESTREAM_EXTERNAL_FFMPEG_PERMITS", "2")],
            &[
                "RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN",
                "RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE",
                "RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD",
            ],
            || {
                assert_eq!(AppConfig::from_env().external_ffmpeg_permits, 2);
            },
        );

        with_env_overlay(
            &[("RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN", "3")],
            &[
                "RESTREAM_EXTERNAL_FFMPEG_PERMITS",
                "RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE",
                "RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD",
            ],
            || {
                assert_eq!(AppConfig::from_env().external_ffmpeg_permits, 3);
            },
        );
    }

    #[test]
    fn rtmp_preauth_limits_are_loaded_and_clamped() {
        with_env_vars(
            &[
                ("RESTREAM_RTMP_MAX_CONNECTIONS", "0"),
                ("RESTREAM_RTMP_HANDSHAKE_TIMEOUT_MS", "10"),
                ("RESTREAM_RTMP_PREAUTH_BUFFER_BYTES", "1024"),
                ("RESTREAM_RTMP_STREAM_BUFFER_BYTES", "65536"),
            ],
            || {
                let config = AppConfig::from_env();
                assert_eq!(config.rtmp_max_connections, 1);
                assert_eq!(config.rtmp_handshake_timeout_ms, 100);
                assert_eq!(config.rtmp_preauth_buffer_bytes, 16 * 1024);
                assert_eq!(config.rtmp_stream_buffer_bytes, 128 * 1024);
            },
        );
    }

    #[test]
    fn runtime_tuning_is_loaded_by_config_module() {
        with_env_vars(
            &[
                ("RESTREAM_NOFILE_LIMIT", "1234"),
                ("RESTREAM_RECONCILE_INTERVAL_MS", "5"),
                ("RESTREAM_OUTPUT_RETRY_BASE_MS", "0"),
                ("RESTREAM_HLS_IDLE_TIMEOUT_MS", "90000"),
            ],
            || {
                let tuning = RuntimeTuning::from_env();
                assert_eq!(tuning.nofile_limit, 1234);
                assert_eq!(tuning.reconciler_interval_ms, 100);
                assert_eq!(tuning.output_retry_base_ms, 1);
                assert_eq!(tuning.hls_idle_timeout_ms, 90000);
            },
        );
    }

    #[test]
    fn tokio_runtime_config_tracks_cpu_limits_and_overrides() {
        assert_eq!(parse_cpu_list_count("0"), Some(1));
        assert_eq!(parse_cpu_list_count("0-5"), Some(6));
        assert_eq!(parse_cpu_list_count("0-1,4,6-7"), Some(5));
        assert_eq!(parse_cpu_list_count("3-1"), None);
        assert_eq!(parse_cpu_list_count(""), None);
        assert_eq!(
            parse_cpu_allowed_list("Name:\trestream\nCpus_allowed_list:\t0-1,4\n"),
            Some(3)
        );
        assert_eq!(parse_cpu_max_quota("max 100000"), None);
        assert_eq!(parse_cpu_max_quota("100000 100000"), Some(1));
        assert_eq!(parse_cpu_max_quota("150000 100000"), Some(2));
        assert_eq!(parse_cpu_max_quota("250000 100000"), Some(3));

        assert_eq!(default_tokio_worker_threads(1), 1);
        assert_eq!(default_tokio_worker_threads(2), 2);
        assert_eq!(default_tokio_worker_threads(6), 2);
        assert_eq!(default_tokio_worker_threads(12), 4);
        assert_eq!(default_tokio_worker_threads(64), 8);

        with_env_vars(
            &[
                ("RESTREAM_TOKIO_WORKER_THREADS", "3"),
                ("RESTREAM_TOKIO_MAX_BLOCKING_THREADS", "32"),
            ],
            || {
                let runtime = TokioRuntimeConfig::from_env();
                assert_eq!(runtime.worker_threads, 3);
                assert_eq!(runtime.max_blocking_threads, 32);
                assert_eq!(AppConfig::from_env().tokio_runtime, runtime);
            },
        );
    }

    #[test]
    fn backend_policy_is_loaded_by_config_module() {
        with_env_vars(
            &[
                ("RESTREAM_INTERNAL_VIDEO_PRESETS", "true"),
                ("RESTREAM_INTERNAL_HEVC_TO_H264", "1"),
                ("RESTREAM_INTERNAL_HLS_PREVIEW", "yes"),
                ("RESTREAM_INTERNAL_AUDIO_COMPLEX", "off"),
            ],
            || {
                let policy = BackendPolicy::from_env();
                assert!(policy.internal_video_presets);
                assert!(policy.internal_hevc_to_h264);
                assert!(policy.internal_hls_preview);
                assert!(!policy.internal_complex_audio);
            },
        );
    }

    #[test]
    fn legacy_global_internal_transcoder_env_does_not_enable_stage_families() {
        with_env_overlay(
            &[("RESTREAM_USE_INTERNAL_TRANSCODER", "1")],
            &[
                "RESTREAM_INTERNAL_VIDEO_PRESETS",
                "RESTREAM_INTERNAL_HEVC_TO_H264",
                "RESTREAM_INTERNAL_HLS_PREVIEW",
                "RESTREAM_INTERNAL_AUDIO_COMPLEX",
            ],
            || {
                let policy = BackendPolicy::from_env();
                assert_eq!(policy, BackendPolicy::default());
            },
        );
    }

    #[test]
    fn backend_policy_does_not_use_global_internal_switch_for_all_stages() {
        with_env_overlay(
            &[("RESTREAM_USE_INTERNAL_TRANSCODER", "1")],
            &[
                "RESTREAM_INTERNAL_VIDEO_PRESETS",
                "RESTREAM_INTERNAL_HEVC_TO_H264",
                "RESTREAM_INTERNAL_HLS_PREVIEW",
                "RESTREAM_INTERNAL_AUDIO_COMPLEX",
            ],
            || {
                let policy = BackendPolicy::from_env();
                assert!(!policy.internal_video_presets);
                assert!(!policy.internal_hevc_to_h264);
                assert!(!policy.internal_hls_preview);
                assert!(!policy.internal_complex_audio);
            },
        );
    }

    #[test]
    fn initial_admin_password_is_loaded_by_config_module() {
        with_env_vars(&[("RESTREAM_INITIAL_ADMIN_PASSWORD", "dev-secret")], || {
            let config = AppConfig::from_env();
            assert_eq!(config.initial_admin_password.as_deref(), Some("dev-secret"));
        });
    }

    #[test]
    fn effective_summary_covers_runtime_knobs_without_secret_values() {
        let config = AppConfig {
            srt_passphrase: Some("super-secret".to_string()),
            initial_admin_password: Some("admin-secret".to_string()),
            ffmpeg_bin_path: Some("/usr/bin/ffmpeg".to_string()),
            ..AppConfig::default()
        };

        let summary = config.effective_summary();
        assert_eq!(summary["ports"]["http"], 3030);
        assert_eq!(summary["tuning"]["reconcilerIntervalMs"], 1000);
        assert_eq!(
            summary["tokio"]["workerThreads"],
            config.tokio_runtime.worker_threads
        );
        assert_eq!(
            summary["tokio"]["maxBlockingThreads"],
            config.tokio_runtime.max_blocking_threads
        );
        assert_eq!(summary["paths"]["ffmpegBin"], "/usr/bin/ffmpeg");
        assert_eq!(summary["backendPolicy"]["internalHlsPreview"], false);
        assert_eq!(
            summary["ffmpeg"]["externalPermits"],
            config.external_ffmpeg_permits
        );
        assert_eq!(summary["buffers"]["ringCapacity"], 1024);
        assert_eq!(summary["srt"]["passphraseConfigured"], true);
        assert_eq!(summary["srt"]["pbkeylen"], 16);
        assert!(!summary.to_string().contains("super-secret"));
        assert!(!summary.to_string().contains("admin-secret"));
    }
}
